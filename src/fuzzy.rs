//! Fuzzy file open (Ctrl+P).
//!
//! The matcher is dependency free and works on scan-relative paths that use
//! forward slashes. Matching is an ASCII case-insensitive subsequence check:
//! every query character must appear in order, but not contiguously. Scoring
//! prefers matches at the start of the path, after a path separator or another
//! word boundary, and runs of consecutive characters; long paths are penalised
//! only lightly so they still win when the match itself is stronger.
//!
//! `rank` is a single matching pass over the paths slice. It does not compile a
//! regex and allocates only the ranked result vector, which keeps a 20k-path
//! scan-in-memory cheap enough to run on every keystroke.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::AURA;

/// Maximum number of ranked rows kept by [`FuzzyState`] and drawn by
/// [`render`].
pub const MAX_RESULTS: usize = 12;

/// Popup state: the query, the ranked path indices and the current selection.
///
/// `selected` stores a position in the match list; [`FuzzyState::selected`]
/// converts it to an index into the original `paths` slice.
#[derive(Debug, Default)]
pub struct FuzzyState {
    query: String,
    /// Position inside `matches`, not a path index.
    selected: usize,
    matches: Vec<usize>,
    open: bool,
}

impl FuzzyState {
    /// Create a closed palette with an empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the palette and rank `paths` against an empty query.
    pub fn open(&mut self, paths: &[String]) {
        self.open = true;
        self.query.clear();
        self.refresh(paths);
    }

    /// Whether the palette should capture input and be drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Append a character and re-rank. Selection returns to the best match.
    pub fn type_char(&mut self, ch: char, paths: &[String]) {
        self.query.push(ch);
        self.refresh(paths);
    }

    /// Remove the last character and re-rank. Selection returns to the best
    /// match.
    pub fn backspace(&mut self, paths: &[String]) {
        self.query.pop();
        self.refresh(paths);
    }

    /// Move the selection by `delta` rows, clamped to the match list (it does
    /// not wrap).
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.matches.len() - 1;
        let current = self.selected.min(last) as isize;
        self.selected = (current + delta).clamp(0, last as isize) as usize;
    }

    /// Path index of the highlighted row, or `None` when nothing matches.
    pub fn selected(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// Ranked path indices, capped at [`MAX_RESULTS`].
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// Hide the palette and drop the transient query and results.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.matches.clear();
        self.selected = 0;
    }

    fn refresh(&mut self, paths: &[String]) {
        self.matches = rank(paths, &self.query, MAX_RESULTS);
        self.selected = 0;
    }
}

/// Rank path indices for `query`, best first, keeping at most `limit` rows.
///
/// An empty query keeps the input order. Equal scores keep the earlier path.
/// Results index the original `paths` slice.
pub fn rank(paths: &[String], query: &str, limit: usize) -> Vec<usize> {
    if limit == 0 {
        return Vec::new();
    }
    if query.is_empty() {
        return (0..paths.len().min(limit)).collect();
    }

    let needle: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut scored: Vec<(i32, usize)> = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if let Some(score) = score_path(path, &needle) {
            scored.push((score, index));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, index)| index).collect()
}

/// Score one subsequence match, or `None` when a query character is missing.
///
/// The path is walked once from left to right. Matching stops as soon as the
/// query is exhausted, so a late match never rescans the path.
fn score_path(path: &str, needle: &[char]) -> Option<i32> {
    let mut score = 0i32;
    let mut matched = 0usize;
    let mut previous: Option<usize> = None;
    let mut previous_char: Option<char> = None;

    for (position, (_, raw)) in path.char_indices().enumerate() {
        let ch = raw.to_ascii_lowercase();
        if matched < needle.len() && ch == needle[matched] {
            let gap = match previous {
                Some(before) => position.saturating_sub(before + 1),
                None => position,
            };
            if previous.is_some() {
                if gap == 0 {
                    // Consecutive characters are a strong signal.
                    score += 15;
                } else {
                    score -= gap.min(8) as i32 * 2;
                }
            } else {
                // A match far into the path is weaker than one near the front.
                score -= position.min(16) as i32;
            }

            if position == 0 {
                score += 20;
            } else {
                match previous_char.unwrap_or('/') {
                    '/' => score += 12,
                    '_' | '-' | '.' => score += 8,
                    _ => {}
                }
            }

            previous = Some(position);
            matched += 1;
            if matched == needle.len() {
                break;
            }
        }
        previous_char = Some(ch);
    }

    if matched < needle.len() {
        return None;
    }
    // Light long-path penalty: one point per 20 bytes.
    score -= (path.len() / 20) as i32;
    Some(score)
}

/// Draw a centered palette over `area`.
///
/// The popup is only drawn while the state is open. It shows the query line,
/// one highlighted row for the selection and up to [`MAX_RESULTS`] paths. Areas
/// too small for the frame are ignored rather than panicking.
pub fn render(frame: &mut Frame, area: Rect, state: &FuzzyState, paths: &[String]) {
    if !state.is_open() || area.width < 12 || area.height < 5 {
        return;
    }

    let row_count = state.matches().len().max(1) as u16;
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
        .style(Style::default().bg(AURA.card))
        .border_style(Style::default().fg(AURA.border_focus))
        .title(Line::from(Span::styled(
            " OPEN FILE - Esc closes ",
            Style::default().fg(AURA.accent),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    let prompt = if state.query().is_empty() {
        "> ".to_string()
    } else {
        format!("> {}", state.query())
    };
    lines.push(Line::from(Span::styled(
        truncate(&prompt, width),
        Style::default().fg(AURA.text),
    )));

    if state.matches().is_empty() {
        if lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                "  no matches",
                Style::default().fg(AURA.muted),
            )));
        }
    } else {
        for (position, &path_index) in state.matches().iter().enumerate() {
            if lines.len() >= inner.height as usize {
                break;
            }
            let Some(path) = paths.get(path_index) else {
                continue;
            };
            let text = format!("  {path}");
            if position == state.selected {
                lines.push(Line::from(Span::styled(
                    truncate(&text, width),
                    Style::default()
                        .fg(AURA.text)
                        .bg(AURA.pill)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    truncate(&text, width),
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

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn subsequence_matches_engine_cpp() {
        let all = paths(&["src/view.rs", "src/engine.cpp", "README.md"]);
        let ranked = rank(&all, "engcpp", MAX_RESULTS);
        assert_eq!(ranked.first(), Some(&1));
    }

    #[test]
    fn path_boundary_beats_inner_match() {
        let all = paths(&["src/xfoobar.rs", "src/foo_bar.rs"]);
        let ranked = rank(&all, "fb", MAX_RESULTS);
        assert_eq!(ranked, vec![1, 0]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let all = paths(&["SRC/Engine.CPP"]);
        assert_eq!(rank(&all, "engcpp", MAX_RESULTS), vec![0]);
        assert_eq!(rank(&all, "ENGCpp", MAX_RESULTS), vec![0]);
    }

    #[test]
    fn empty_query_returns_first_page() {
        let all: Vec<String> = (0..20).map(|i| format!("file_{i}.rs")).collect();
        assert_eq!(rank(&all, "", 5), vec![0, 1, 2, 3, 4]);
        assert!(rank(&all, "", 0).is_empty());
    }

    #[test]
    fn missing_character_yields_no_match() {
        let all = paths(&["src/engine.cpp"]);
        assert!(rank(&all, "engz", MAX_RESULTS).is_empty());
        assert!(rank(&all, "cppengine", MAX_RESULTS).is_empty());
    }

    #[test]
    fn state_tracks_query_selection_and_close() {
        let all = paths(&["src/alpha.rs", "src/beta.rs", "src/gamma.rs"]);
        let mut state = FuzzyState::new();
        assert!(!state.is_open());
        assert!(state.selected().is_none());

        state.open(&all);
        assert!(state.is_open());
        assert_eq!(state.query(), "");
        assert_eq!(state.matches().len(), 3);
        assert_eq!(state.selected(), Some(0));

        state.type_char('b', &all);
        assert_eq!(state.query(), "b");
        assert_eq!(state.selected(), Some(1));
        state.type_char('e', &all);
        assert_eq!(state.query(), "be");
        assert_eq!(state.selected(), Some(1));

        state.backspace(&all);
        assert_eq!(state.query(), "b");
        assert_eq!(state.selected(), Some(1));
        state.backspace(&all);
        assert_eq!(state.query(), "");
        assert_eq!(state.matches().len(), 3);

        state.move_selection(1);
        assert_eq!(state.selected(), Some(state.matches()[1]));
        state.move_selection(99);
        let last = state.matches().len() - 1;
        assert_eq!(state.selected(), Some(state.matches()[last]));
        state.move_selection(-99);
        assert_eq!(state.selected(), Some(state.matches()[0]));

        state.close();
        assert!(!state.is_open());
        assert!(state.matches().is_empty());
        assert!(state.selected().is_none());
    }

    #[test]
    fn typing_no_match_clears_selection() {
        let all = paths(&["src/engine.cpp"]);
        let mut state = FuzzyState::new();
        state.open(&all);
        state.type_char('z', &all);
        assert!(state.matches().is_empty());
        assert!(state.selected().is_none());
        state.move_selection(1);
        assert!(state.selected().is_none());
        state.backspace(&all);
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn selected_is_a_path_index_after_ranking() {
        let all = paths(&["aaa.rs", "src/beta.rs", "src/foo_bar.rs"]);
        let mut state = FuzzyState::new();
        state.open(&all);
        state.type_char('f', &all);
        state.type_char('b', &all);
        assert_eq!(state.matches(), &[2]);
        assert_eq!(state.selected(), Some(2));
    }

    #[test]
    fn twenty_thousand_paths_are_handled() {
        let all: Vec<String> = (0..20_000)
            .map(|i| format!("src/module_{i}/some_file_{i}.rs"))
            .collect();
        let ranked = rank(&all, "smf19999", MAX_RESULTS);
        assert!(!ranked.is_empty());
        assert!(ranked.len() <= MAX_RESULTS);
        assert!(ranked.iter().all(|&index| index < all.len()));
    }
}
