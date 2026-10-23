//! Current-file symbol palette (Alt+O).
//!
//! The palette snapshots the functions found in the active file and filters
//! them with the same subsequence matcher used by file search
//! ([`crate::fuzzy::rank`]). Labels combine the breadcrumb trail with the
//! function name, so `proc` finds `demo > process(int)`.
//!
//! `selected` indexes the ranked `matches` list; [`SymbolPalette::selected`]
//! resolves it back to the actual [`FunctionCcn`] that was passed to
//! [`SymbolPalette::open`].

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::breadcrumb;
use crate::ccn::types::FunctionCcn;
use crate::fuzzy::rank;
use crate::theme::palette;

/// Maximum number of symbol rows drawn at once.
pub const MAX_ROWS: usize = 12;

/// Popup state: the snapshot of functions, query, selection and ranked
/// indices.
///
/// `matches` holds indices into `functions`; `selected` is a position inside
/// `matches`.
#[derive(Debug, Default)]
pub struct SymbolPalette {
    open: bool,
    query: String,
    /// Position inside `matches`, not a function index.
    selected: usize,
    matches: Vec<usize>,
    functions: Vec<FunctionCcn>,
    /// Filter text for each function, parallel to `functions`.
    labels: Vec<String>,
}

impl SymbolPalette {
    /// Create a closed palette with an empty query and no functions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the palette should capture input and be drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Snapshot the current file's functions and show all of them.
    ///
    /// The query and selection are reset. Later changes to the source slice do
    /// not affect the palette until the next `open`.
    pub fn open(&mut self, functions: &[FunctionCcn]) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.functions = functions.to_vec();
        self.labels = self.functions.iter().map(label).collect();
        self.refresh();
    }

    /// Hide the palette and drop the snapshot, query and results.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
        self.matches.clear();
        self.functions.clear();
        self.labels.clear();
    }

    /// Current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Append a character and re-rank. Selection returns to the best match.
    pub fn type_char(&mut self, ch: char) {
        self.query.push(ch);
        self.refresh();
    }

    /// Remove the last character and re-rank. Selection returns to the best
    /// match.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    /// Move the selection by `delta` rows, clamped to the match list. The
    /// selection does not wrap.
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.matches.len() - 1;
        let current = self.selected.min(last) as isize;
        self.selected = (current + delta).clamp(0, last as isize) as usize;
    }

    /// The highlighted function, or `None` when nothing matches.
    pub fn selected(&self) -> Option<&FunctionCcn> {
        self.matches
            .get(self.selected)
            .and_then(|&index| self.functions.get(index))
    }

    /// Functions matching the current query, best first.
    ///
    /// References point into the snapshot taken by [`SymbolPalette::open`], so
    /// the order is the same as the popup rows.
    pub fn matches(&self) -> Vec<&FunctionCcn> {
        self.matches
            .iter()
            .filter_map(|&index| self.functions.get(index))
            .collect()
    }

    /// Re-rank the snapshot. An empty query keeps the incoming order.
    fn refresh(&mut self) {
        let limit = self.labels.len();
        self.matches = rank(&self.labels, &self.query, limit);
        self.selected = 0;
    }
}

/// Build the filter text for one function: breadcrumb trail plus name.
///
/// The breadcrumb usually already ends in the name; it is appended only when
/// the identity and the name disagree (older or hand-built data).
fn label(function: &FunctionCcn) -> String {
    let trail = breadcrumb::line(function);
    if trail.contains(&function.name) || function.name.is_empty() {
        trail
    } else {
        format!("{trail} {}", function.name)
    }
}

/// Draw a centered `SYMBOLS` popup over `area`.
///
/// The popup is only drawn while the palette is open. It shows the query line
/// followed by up to [`MAX_ROWS`] functions, each with its breadcrumb on the
/// left and `CCN` plus line number on the right. Areas too small for the
/// popup are ignored rather than panicking.
pub fn render(frame: &mut Frame, area: Rect, state: &SymbolPalette) {
    if !state.is_open() || area.width < 12 || area.height < 5 {
        return;
    }

    let row_count = state.matches.len().clamp(1, MAX_ROWS) as u16;
    let height = (row_count + 3).min(area.height);
    let width = area.width.saturating_sub(4).clamp(12, 76);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(palette().card))
        .border_style(Style::default().fg(palette().border_focus))
        .title(Line::from(Span::styled(
            " SYMBOLS - Esc closes ",
            Style::default().fg(palette().accent),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    let prompt = format!("> {}", state.query);
    lines.push(Line::from(Span::styled(
        truncate(&prompt, width),
        Style::default().fg(palette().text),
    )));

    if state.matches.is_empty() {
        if lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                "  no symbols",
                Style::default().fg(palette().muted),
            )));
        }
    } else {
        for (position, &index) in state.matches.iter().take(MAX_ROWS).enumerate() {
            if lines.len() >= inner.height as usize {
                break;
            }
            let Some(function) = state.functions.get(index) else {
                continue;
            };
            let text = row_text(&breadcrumb::line(function), function, width);
            if position == state.selected {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(palette().text)
                        .bg(palette().pill)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(palette().text),
                )));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

/// Compose one row: breadcrumb on the left, `CCN n` and line on the right.
fn row_text(trail: &str, function: &FunctionCcn, width: usize) -> String {
    let right = format!("CCN {}  L{}", function.ccn, function.line);
    let right_width = right.chars().count();
    let left = format!("  {trail}");

    // Too narrow for two columns: fall back to a single truncated line so the
    // CCN and line number stay visible.
    if width <= right_width + 3 {
        return truncate(&format!("{left} {right}"), width);
    }

    let label_space = width - right_width - 1;
    let left = if left.chars().count() > label_space {
        truncate(&left, label_space)
    } else {
        left
    };
    let padding = width.saturating_sub(left.chars().count() + right_width);
    format!("{left}{}{right}", " ".repeat(padding))
}

/// Cut `text` to `width` characters without splitting a character.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn function(name: &str, identity: &str, line: usize, ccn: u32) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            identity: identity.to_string(),
            line,
            end_line: line + 10,
            nloc: 11,
            ccn,
            params: 0,
            max_nesting: 0,
        }
    }

    fn demo() -> Vec<FunctionCcn> {
        vec![
            function("process", "demo::process(int)", 7, 4),
            function("helper", "demo::helper()", 21, 2),
            function("render", "demo::View::render()", 40, 11),
        ]
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn filtered(query: &str) -> SymbolPalette {
        let mut palette = SymbolPalette::new();
        palette.open(&demo());
        for ch in query.chars() {
            palette.type_char(ch);
        }
        palette
    }

    #[test]
    fn open_and_close_reset_transient_state() {
        let mut palette = SymbolPalette::new();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());
        assert!(palette.matches().is_empty());

        palette.open(&demo());
        assert!(palette.is_open());
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), 3);
        assert_eq!(palette.selected().unwrap().name, "process");

        palette.type_char('r');
        assert_eq!(palette.query(), "r");
        palette.close();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());
        assert!(palette.matches().is_empty());

        palette.open(&demo());
        assert_eq!(palette.query(), "");
        assert_eq!(palette.selected().unwrap().name, "process");
    }

    #[test]
    fn search_proc_finds_process() {
        let palette = filtered("proc");
        assert_eq!(palette.query(), "proc");
        assert!(!palette.matches().is_empty());
        assert_eq!(palette.selected().unwrap().name, "process");
        assert!(palette.matches().iter().any(|f| f.name == "process"));
    }

    #[test]
    fn empty_query_keeps_incoming_order() {
        let palette = filtered("");
        let names: Vec<&str> = palette.matches().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["process", "helper", "render"]);
        assert_eq!(palette.selected().unwrap().name, "process");
    }

    #[test]
    fn selection_clamps_and_never_wraps() {
        let mut palette = filtered("");
        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().name, "helper");
        palette.move_selection(-99);
        assert_eq!(palette.selected().unwrap().name, "process");
        palette.move_selection(999);
        assert_eq!(palette.selected().unwrap().name, "render");
        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().name, "render");

        let mut empty = filtered("zzzz");
        assert!(empty.selected().is_none());
        empty.move_selection(5);
        assert!(empty.selected().is_none());
    }

    #[test]
    fn selection_maps_back_to_the_function_snapshot() {
        let palette = filtered("helper");
        assert_eq!(palette.matches().len(), 1);
        let selected = palette.selected().unwrap();
        assert_eq!(selected.identity, "demo::helper()");
        assert_eq!(selected.line, 21);
        assert_eq!(selected.ccn, 2);
    }

    #[test]
    fn backspace_restores_broader_matches() {
        let mut palette = filtered("rend");
        assert_eq!(palette.selected().unwrap().name, "render");
        palette.backspace();
        palette.backspace();
        palette.backspace();
        palette.backspace();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), 3);
    }

    #[test]
    fn renders_title_query_line_and_ccn() {
        let palette = filtered("proc");
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &palette))
            .expect("draw symbols");
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("SYMBOLS"), "{text}");
        assert!(text.contains("> proc"), "{text}");
        assert!(text.contains("demo > process(int)"), "{text}");
        assert!(text.contains("CCN 4"), "{text}");
        assert!(text.contains("L7"), "{text}");
    }

    #[test]
    fn tiny_area_is_ignored_without_panicking() {
        let palette = filtered("");
        for (width, height) in [(1, 1), (11, 4), (12, 5), (20, 6)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test backend");
            terminal
                .draw(|frame| render(frame, frame.area(), &palette))
                .expect("draw tiny symbols");
        }
    }

    #[test]
    fn closed_palette_draws_nothing() {
        let palette = SymbolPalette::new();
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &palette))
            .expect("draw closed symbols");
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("SYMBOLS"), "{text}");
    }

    #[test]
    fn no_functions_shows_empty_state() {
        let mut palette = SymbolPalette::new();
        palette.open(&[]);
        assert!(palette.is_open());
        assert!(palette.matches().is_empty());
        assert!(palette.selected().is_none());
        palette.type_char('x');
        assert!(palette.selected().is_none());

        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &palette))
            .expect("draw empty symbols");
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("SYMBOLS"), "{text}");
        assert!(text.contains("no symbols"), "{text}");
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("café", 10), "café");
        assert_eq!(truncate("café", 3), "ca~");
        assert_eq!(truncate("café", 0), "");
        assert_eq!(truncate("日本語", 2), "日~");
    }
}
