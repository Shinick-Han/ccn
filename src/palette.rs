//! Command palette (Ctrl+Shift+P style) for the unified TUI.
//!
//! The palette is a small, dependency-free list of app actions. Filtering
//! reuses [`crate::fuzzy::rank`] over the command labels, so typing `diag`
//! narrows the list to `Diagnostics` with the same scoring as file search.
//! The palette owns its query, selection and ranked match indices; rendering
//! is a centered popup with a selected pill.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::fuzzy::rank;
use crate::theme::palette;

/// Maximum number of command rows drawn at once.
pub const MAX_ROWS: usize = 12;

/// Action requested by a palette command.
///
/// The caller maps these onto its own key handling; the palette itself never
/// mutates application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    OpenFilePicker,
    ToggleReview,
    ToggleDiagnostics,
    ShowHelp,
    NextTab,
    PrevTab,
    CloseTab,
    FocusFiles,
    FocusEditor,
    FilterFiles,
    Rescan,
    Quit,
}

/// One entry in the command list.
#[derive(Debug, Clone)]
pub struct Command {
    /// Stable identifier, useful for tests and key bindings.
    pub id: &'static str,
    /// ASCII label shown in the popup and matched by the fuzzy filter.
    pub label: &'static str,
    /// ASCII shortcut hint shown on the right of the row.
    pub shortcut: &'static str,
    /// Action emitted when the row is accepted.
    pub action: PaletteAction,
}

/// The command catalog in declaration order.
///
/// Declaration order is the order shown for an empty query. The index into
/// this slice is what [`Palette`] stores as a match.
const COMMANDS: &[Command] = &[
    Command {
        id: "open_file",
        label: "Open file",
        shortcut: "Ctrl+P",
        action: PaletteAction::OpenFilePicker,
    },
    Command {
        id: "baseline_review",
        label: "Baseline review",
        shortcut: "F6",
        action: PaletteAction::ToggleReview,
    },
    Command {
        id: "diagnostics",
        label: "Diagnostics",
        shortcut: "F7",
        action: PaletteAction::ToggleDiagnostics,
    },
    Command {
        id: "help",
        label: "Help",
        shortcut: "?",
        action: PaletteAction::ShowHelp,
    },
    Command {
        id: "next_tab",
        label: "Next tab",
        shortcut: "F3",
        action: PaletteAction::NextTab,
    },
    Command {
        id: "prev_tab",
        label: "Previous tab",
        shortcut: "F2",
        action: PaletteAction::PrevTab,
    },
    Command {
        id: "close_tab",
        label: "Close tab",
        shortcut: "F4",
        action: PaletteAction::CloseTab,
    },
    Command {
        id: "focus_files",
        label: "Focus files",
        shortcut: "F8",
        action: PaletteAction::FocusFiles,
    },
    Command {
        id: "focus_editor",
        label: "Focus editor",
        shortcut: "F8",
        action: PaletteAction::FocusEditor,
    },
    Command {
        id: "filter_files",
        label: "Filter files",
        shortcut: "/",
        action: PaletteAction::FilterFiles,
    },
    Command {
        id: "rescan",
        label: "Rescan file",
        shortcut: "r",
        action: PaletteAction::Rescan,
    },
    Command {
        id: "quit",
        label: "Quit",
        shortcut: "q",
        action: PaletteAction::Quit,
    },
];

/// Return a copy of the full command catalog in declaration order.
pub fn commands() -> Vec<Command> {
    COMMANDS.to_vec()
}

/// Popup state: query, selected row and ranked command indices.
///
/// `selected` indexes `matches`, not [`COMMANDS`]; [`Palette::selected`]
/// resolves it to the actual command.
#[derive(Debug, Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    matches: Vec<usize>,
}

impl Palette {
    /// Create a closed palette with an empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the palette should capture input and be drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the palette, reset the query and selection, and show all commands.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    /// Hide the palette and drop transient query and results.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
        self.matches.clear();
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

    /// The highlighted command, or `None` when nothing matches.
    pub fn selected(&self) -> Option<&Command> {
        self.matches
            .get(self.selected)
            .and_then(|&index| COMMANDS.get(index))
    }

    /// Ranked commands for the current query, best first.
    ///
    /// This returns references to the static catalog rather than indices so
    /// callers can read labels and actions directly. The order matches the
    /// popup rows.
    pub fn matches(&self) -> Vec<&Command> {
        self.matches
            .iter()
            .filter_map(|&index| COMMANDS.get(index))
            .collect()
    }

    /// Re-rank the catalog, keeping at most [`MAX_ROWS`] entries.
    fn refresh(&mut self) {
        let labels: Vec<String> = COMMANDS
            .iter()
            .map(|command| command.label.to_string())
            .collect();
        self.matches = rank(&labels, &self.query, MAX_ROWS);
        self.selected = 0;
    }
}

/// Draw a centered `COMMANDS` popup over `area`.
///
/// The popup is only drawn while the palette is open. It shows the query line
/// followed by up to [`MAX_ROWS`] commands with right-aligned shortcut hints.
/// Areas too small for the popup are ignored rather than panicking.
pub fn render(frame: &mut Frame, area: Rect, state: &Palette) {
    if !state.is_open() || area.width < 12 || area.height < 5 {
        return;
    }

    let row_count = state.matches.len().max(1) as u16;
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
            " COMMANDS - Esc closes ",
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
                "  no matches",
                Style::default().fg(palette().muted),
            )));
        }
    } else {
        for (position, &index) in state.matches.iter().enumerate().take(MAX_ROWS) {
            if lines.len() >= inner.height as usize {
                break;
            }
            let Some(command) = COMMANDS.get(index) else {
                continue;
            };
            let text = row_text(command.label, command.shortcut, width);
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

/// Compose one row: padded label on the left, shortcut on the right.
fn row_text(label: &str, shortcut: &str, width: usize) -> String {
    let shortcut_width = shortcut.chars().count();
    let label_space = width.saturating_sub(shortcut_width + 3);
    let mut text = format!("  {label}");
    if text.chars().count() > label_space {
        text = truncate(&text, label_space);
    }
    let padding = width.saturating_sub(text.chars().count() + shortcut_width + 1);
    format!("{text}{}{}", " ".repeat(padding), shortcut)
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

    fn filtered(query: &str) -> Palette {
        let mut palette = Palette::new();
        palette.open();
        for ch in query.chars() {
            palette.type_char(ch);
        }
        palette
    }

    #[test]
    fn open_and_close_reset_transient_state() {
        let mut palette = Palette::new();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());

        palette.open();
        assert!(palette.is_open());
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), commands().len());

        palette.type_char('q');
        assert_eq!(palette.query(), "q");
        palette.close();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());
        assert!(palette.matches().is_empty());

        palette.open();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.selected().unwrap().id, "open_file");
    }

    #[test]
    fn filters_diagnostics_from_diag() {
        let palette = filtered("diag");
        assert_eq!(palette.query(), "diag");
        assert_eq!(palette.matches().len(), 1);
        assert_eq!(palette.selected().unwrap().id, "diagnostics");
        assert_eq!(palette.selected().unwrap().label, "Diagnostics");
    }

    #[test]
    fn every_command_has_a_label_and_shortcut() {
        let all = commands();
        assert_eq!(all.len(), 12);
        for command in &all {
            assert!(!command.id.is_empty());
            assert!(!command.label.is_empty());
            assert!(!command.shortcut.is_empty());
            assert!(command.label.is_ascii());
            assert!(command.shortcut.is_ascii());
        }
        let shortcuts: Vec<&str> = all.iter().map(|command| command.shortcut).collect();
        assert!(shortcuts.contains(&"Ctrl+P"));
        assert!(shortcuts.contains(&"F6"));
        assert!(shortcuts.contains(&"F7"));
        assert!(shortcuts.contains(&"?"));
        assert!(shortcuts.contains(&"/"));
        assert!(shortcuts.contains(&"r"));
        assert!(shortcuts.contains(&"q"));
    }

    #[test]
    fn empty_query_keeps_declaration_order() {
        let palette = filtered("");
        let shown: Vec<&str> = palette.matches().iter().map(|command| command.id).collect();
        let expected: Vec<&str> = commands().iter().map(|command| command.id).collect();
        assert_eq!(shown, expected);
        assert_eq!(palette.selected().unwrap().id, "open_file");

        let mut palette = Palette::new();
        palette.open();
        assert_eq!(palette.matches().len(), expected.len());
    }

    #[test]
    fn selection_clamps_and_never_wraps() {
        let mut palette = filtered("");
        assert_eq!(palette.selected().unwrap().id, "open_file");

        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().id, "baseline_review");
        palette.move_selection(-99);
        assert_eq!(palette.selected().unwrap().id, "open_file");
        palette.move_selection(999);
        assert_eq!(palette.selected().unwrap().id, "quit");
        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().id, "quit");

        let mut empty = filtered("zzzz");
        assert!(empty.selected().is_none());
        empty.move_selection(5);
        assert!(empty.selected().is_none());
    }

    #[test]
    fn backspace_restores_broader_matches() {
        let mut palette = filtered("diag");
        assert_eq!(palette.matches().len(), 1);
        palette.backspace();
        palette.backspace();
        palette.backspace();
        palette.backspace();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), commands().len());
    }

    #[test]
    fn renders_popup_with_title_and_shortcuts() {
        let palette = filtered("");
        let backend = TestBackend::new(50, 20);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &palette))
            .expect("draw palette");
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("COMMANDS"), "{text}");
        assert!(text.contains("Open file"), "{text}");
        assert!(text.contains("Ctrl+P"), "{text}");
        assert!(text.contains("Diagnostics"), "{text}");
        assert!(text.contains("F7"), "{text}");
    }

    #[test]
    fn tiny_area_is_ignored_without_panicking() {
        let palette = filtered("");
        for (width, height) in [(1, 1), (11, 4), (12, 5)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test backend");
            terminal
                .draw(|frame| render(frame, frame.area(), &palette))
                .expect("draw tiny palette");
        }
    }

    #[test]
    fn closed_palette_draws_nothing() {
        let palette = Palette::new();
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &palette))
            .expect("draw closed palette");
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("COMMANDS"), "{text}");
    }
}
