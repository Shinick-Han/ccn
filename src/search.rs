//! Workspace text search (Ctrl+F).
//!
//! [`search`] performs a case-insensitive substring scan over the scan-relative
//! source paths that the caller already holds in memory. Files are read from
//! `root.join(path)` and every matching line becomes one [`SearchMatch`];
//! binary files, unreadable files and lines beyond `limit` are skipped.
//!
//! [`SearchState`] owns the popup query, results and selection. The caller
//! feeds characters to [`SearchState::type_char`], which reruns the scan, and
//! draws the popup with [`render`].

use std::fs;
use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::AURA;

/// Maximum number of hits the caller should keep for the popup.
pub const MAX_RESULTS: usize = 200;

/// Maximum number of result rows drawn at once.
pub const MAX_ROWS: usize = 12;

/// Maximum number of characters kept from a matched line in [`SearchMatch`].
pub const MAX_TEXT_CHARS: usize = 160;

/// One matching line in one source file.
///
/// `line` is 1-based and `text` is the truncated line content without its
/// trailing newline. `path` is the scan-relative path as supplied by the
/// caller, using the same separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Search `paths` under `root` for `query`, keeping at most `limit` hits.
///
/// The scan is an ASCII/Unicode case-insensitive substring match over whole
/// lines. Paths are visited in input order and lines in file order, so results
/// are deterministic. An empty query, `limit == 0`, missing files, files with
/// an embedded NUL byte and files that are not valid UTF-8 all yield no hits.
pub fn search(root: &str, paths: &[String], query: &str, limit: usize) -> Vec<SearchMatch> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }

    let base = Path::new(root);
    let mut results = Vec::new();

    for path in paths {
        if results.len() >= limit {
            break;
        }
        let Ok(bytes) = fs::read(base.join(path)) else {
            continue;
        };
        // A NUL byte is the cheapest reliable binary marker and also makes
        // the file useless as a line-oriented text document.
        if bytes.contains(&0) {
            continue;
        }
        let Ok(contents) = String::from_utf8(bytes) else {
            continue;
        };

        for (index, line) in contents.lines().enumerate() {
            if results.len() >= limit {
                break;
            }
            if line.to_lowercase().contains(&needle) {
                results.push(SearchMatch {
                    path: path.clone(),
                    line: index + 1,
                    text: truncate_chars(line, MAX_TEXT_CHARS),
                });
            }
        }
    }

    results
}

/// Popup state: query, hits and the highlighted row.
#[derive(Debug, Default)]
pub struct SearchState {
    open: bool,
    query: String,
    results: Vec<SearchMatch>,
    selected: usize,
}

impl SearchState {
    /// Create a closed search popup with an empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the popup should capture input and be drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the popup and reset the transient query, hits and selection.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
    }

    /// Hide the popup and drop the transient query, hits and selection.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
    }

    /// Current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Append a character, rerun the scan and select the first hit.
    pub fn type_char(&mut self, ch: char, root: &str, paths: &[String]) {
        self.query.push(ch);
        self.rerun(root, paths);
    }

    /// Remove the last character, rerun the scan and select the first hit.
    pub fn backspace(&mut self, root: &str, paths: &[String]) {
        self.query.pop();
        self.rerun(root, paths);
    }

    /// Move the selection by `delta` rows, clamped to the hit list. The
    /// selection does not wrap and stays at zero when there are no hits.
    pub fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.results.len() - 1;
        let current = self.selected.min(last) as isize;
        self.selected = (current + delta).clamp(0, last as isize) as usize;
    }

    /// The highlighted hit, or `None` when nothing matched.
    pub fn selected(&self) -> Option<&SearchMatch> {
        self.results.get(self.selected)
    }

    /// All hits for the current query, in scan order.
    pub fn results(&self) -> &[SearchMatch] {
        &self.results
    }

    /// Rerun the scan for the current query, capped at [`MAX_RESULTS`], and
    /// return the selection to the first hit.
    pub fn rerun(&mut self, root: &str, paths: &[String]) {
        self.results = search(root, paths, &self.query, MAX_RESULTS);
        self.selected = 0;
    }
}

/// Draw a centered `SEARCH` popup over `area`.
///
/// The popup is only drawn while the state is open. It shows the query line and
/// up to [`MAX_ROWS`] hits formatted as `path:line  text`, with the selected
/// row drawn as a pill. Areas too small for the frame are ignored.
pub fn render(frame: &mut Frame, area: Rect, state: &SearchState) {
    if !state.is_open() || area.width < 12 || area.height < 5 {
        return;
    }

    let row_count = state.results.len().max(1) as u16;
    let height = (row_count + 3).min(area.height);
    let width = area.width.saturating_sub(4).clamp(12, 86);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(AURA.card))
        .border_style(Style::default().fg(AURA.border_focus))
        .title(Line::from(Span::styled(
            " SEARCH · Esc closes ",
            Style::default().fg(AURA.accent),
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
        truncate_chars(&prompt, width),
        Style::default().fg(AURA.text),
    )));

    if state.results.is_empty() {
        if lines.len() < inner.height as usize {
            let hint = if state.query.is_empty() {
                "  type to search"
            } else {
                "  no matches"
            };
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(AURA.muted),
            )));
        }
    } else {
        for (position, hit) in state.results.iter().enumerate().take(MAX_ROWS) {
            if lines.len() >= inner.height as usize {
                break;
            }
            let text = truncate_chars(&format!("  {}:{}  {}", hit.path, hit.line, hit.text), width);
            if position == state.selected {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(AURA.text)
                        .bg(AURA.pill)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(AURA.text),
                )));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
        inner,
    );
}

/// Cut `text` to `width` characters without splitting a character.
fn truncate_chars(text: &str, width: usize) -> String {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    /// Temporary fixture directory that removes itself on drop.
    struct Fixture {
        root: String,
    }

    impl Fixture {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let unique = NEXT_DIR.fetch_add(1, Ordering::SeqCst);
            let mut root = std::env::temp_dir();
            root.push(format!("ccn-search-{}-{unique}", std::process::id()));
            fs::create_dir_all(&root).expect("create fixture dir");
            for (path, body) in files {
                let full = root.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).expect("create fixture parent");
                }
                fs::write(&full, body).expect("write fixture file");
            }
            Self {
                root: root.to_string_lossy().into_owned(),
            }
        }

        fn root(&self) -> &str {
            &self.root
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
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

    #[test]
    fn finds_matches_in_multiple_files_and_lines() {
        let fixture = Fixture::new(&[
            ("src/alpha.rs", b"fn main() {}\nlet needle = 1;\n"),
            ("src/beta.rs", b"// nothing\nneedle();\n"),
            ("README.md", b"plain\n"),
        ]);
        let paths = strings(&["src/alpha.rs", "src/beta.rs", "README.md"]);
        let hits = search(fixture.root(), &paths, "needle", MAX_RESULTS);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/alpha.rs");
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "let needle = 1;");
        assert_eq!(hits[1].path, "src/beta.rs");
        assert_eq!(hits[1].line, 2);
        assert_eq!(hits[1].text, "needle();");
    }

    #[test]
    fn matching_is_case_insensitive() {
        let fixture = Fixture::new(&[("a.txt", b"Hello World\nhello again\n")]);
        let paths = strings(&["a.txt"]);

        assert_eq!(
            search(fixture.root(), &paths, "HELLO", MAX_RESULTS).len(),
            2
        );
        assert_eq!(
            search(fixture.root(), &paths, "world", MAX_RESULTS).len(),
            1
        );
        assert_eq!(
            search(fixture.root(), &paths, "hElLo", MAX_RESULTS)[0].line,
            1
        );
    }

    #[test]
    fn empty_query_and_zero_limit_return_nothing() {
        let fixture = Fixture::new(&[("a.txt", b"anything\n")]);
        let paths = strings(&["a.txt"]);

        assert!(search(fixture.root(), &paths, "", MAX_RESULTS).is_empty());
        assert!(search(fixture.root(), &paths, "anything", 0).is_empty());
    }

    #[test]
    fn limit_caps_hits_across_files() {
        let fixture = Fixture::new(&[("a.txt", b"hit\nhit\nhit\n"), ("b.txt", b"hit\nhit\n")]);
        let paths = strings(&["a.txt", "b.txt"]);

        let hits = search(fixture.root(), &paths, "hit", 3);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].path, "a.txt");
        assert_eq!(hits[1].line, 2);
        assert_eq!(hits[2].line, 3);
    }

    #[test]
    fn binary_missing_and_invalid_utf8_files_are_skipped() {
        let fixture = Fixture::new(&[
            ("ok.txt", b"needle\n"),
            ("binary.bin", b"needle\0tail\n"),
            (
                "invalid.txt",
                &[b'n', b'e', b'e', b'd', b'l', b'e', 0xff, b'\n'],
            ),
        ]);
        let paths = strings(&["binary.bin", "missing.txt", "invalid.txt", "ok.txt"]);

        let hits = search(fixture.root(), &paths, "needle", MAX_RESULTS);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "ok.txt");
    }

    #[test]
    fn long_lines_are_truncated_but_path_and_line_survive() {
        let mut body = "a".repeat(300);
        body.push_str("needle\n");
        let fixture = Fixture::new(&[("long.txt", body.as_bytes())]);
        let paths = strings(&["long.txt"]);

        let hits = search(fixture.root(), &paths, "needle", MAX_RESULTS);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "long.txt");
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].text.chars().count(), MAX_TEXT_CHARS);
        assert!(hits[0].text.ends_with('~'));
    }

    #[test]
    fn state_tracks_query_results_and_clamps_selection() {
        let fixture = Fixture::new(&[("a.txt", b"hit one\nhit two\nhit three\n")]);
        let paths = strings(&["a.txt"]);

        let mut state = SearchState::new();
        assert!(!state.is_open());
        assert!(state.selected().is_none());

        state.open();
        assert!(state.is_open());
        assert_eq!(state.query(), "");
        assert!(state.results().is_empty());

        for ch in "hit".chars() {
            state.type_char(ch, fixture.root(), &paths);
        }
        assert_eq!(state.query(), "hit");
        assert_eq!(state.results().len(), 3);
        assert_eq!(state.selected().map(|hit| hit.line), Some(1));

        state.move_selection(1);
        assert_eq!(state.selected().map(|hit| hit.line), Some(2));
        state.move_selection(99);
        assert_eq!(state.selected().map(|hit| hit.line), Some(3));
        state.move_selection(-99);
        assert_eq!(state.selected().map(|hit| hit.line), Some(1));

        // A query change resets the selection to the first hit.
        state.type_char(' ', fixture.root(), &paths);
        assert_eq!(state.selected().map(|hit| hit.line), Some(1));

        state.backspace(fixture.root(), &paths);
        assert_eq!(state.query(), "hit");
        assert_eq!(state.results().len(), 3);

        state.close();
        assert!(!state.is_open());
        assert_eq!(state.query(), "");
        assert!(state.results().is_empty());
        assert!(state.selected().is_none());
        state.move_selection(5);
        assert!(state.selected().is_none());
    }

    #[test]
    fn rerun_refreshes_hits_for_the_same_query() {
        let fixture = Fixture::new(&[("a.txt", b"alpha\nbeta\n")]);
        let paths = strings(&["a.txt"]);

        let mut state = SearchState::new();
        state.open();
        state.type_char('a', fixture.root(), &paths);
        assert_eq!(state.selected().map(|hit| hit.line), Some(1));

        state.move_selection(1);
        assert_eq!(state.selected().map(|hit| hit.line), Some(2));

        state.rerun(fixture.root(), &paths);
        assert_eq!(state.results().len(), 2);
        assert_eq!(state.selected().map(|hit| hit.line), Some(1));
    }

    #[test]
    fn renders_popup_with_query_and_selected_pill() {
        let fixture = Fixture::new(&[("src/a.rs", b"first needle\nsecond needle\n")]);
        let paths = strings(&["src/a.rs"]);

        let mut state = SearchState::new();
        state.open();
        for ch in "needle".chars() {
            state.type_char(ch, fixture.root(), &paths);
        }
        state.move_selection(1);

        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &state))
            .expect("draw search");
        let text = buffer_text(terminal.backend().buffer());

        assert!(text.contains("SEARCH"), "{text}");
        assert!(text.contains("Esc closes"), "{text}");
        assert!(text.contains("> needle"), "{text}");
        assert!(text.contains("src/a.rs:1  first needle"), "{text}");
        assert!(text.contains("src/a.rs:2  second needle"), "{text}");
    }

    #[test]
    fn tiny_or_closed_popup_is_ignored_without_panicking() {
        let fixture = Fixture::new(&[("a.txt", b"hit\n")]);
        let paths = strings(&["a.txt"]);

        let mut state = SearchState::new();
        state.open();
        state.type_char('h', fixture.root(), &paths);

        for (width, height) in [(1, 1), (11, 4), (12, 5)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test backend");
            terminal
                .draw(|frame| render(frame, frame.area(), &state))
                .expect("draw tiny search");
        }

        let closed = SearchState::new();
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &closed))
            .expect("draw closed search");
        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains("SEARCH"), "{text}");
    }
}
