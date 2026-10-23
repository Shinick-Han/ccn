//! Baseline review panel for the workbench.
//!
//! The panel is a compact, read-only list of functions that became complex
//! since a revision. It owns no state: the caller passes the selected index on
//! every frame and the renderer derives a local scroll window from it.

use crate::baseline::{Change, Kind};
use crate::review::key;
use crate::theme::palette;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::collections::BTreeSet;

/// Title shown in the panel border.
fn title_text(rev: &str) -> String {
    if rev.is_empty() {
        " Review ".to_string()
    } else {
        format!(" Review \u{b7} {rev} ")
    }
}

/// Compact subtitle: counts plus the aggregate CCN growth.
fn summary_line(changes: &[Change], threshold: u32) -> String {
    let added = changes
        .iter()
        .filter(|change| change.kind == Kind::Added)
        .count();
    let worsened = changes
        .iter()
        .filter(|change| change.kind == Kind::Worsened)
        .count();
    let growth: u64 = changes
        .iter()
        .map(|change| u64::from(change.ccn.saturating_sub(change.base_ccn.unwrap_or(0))))
        .sum();
    format!(
        "{} changes \u{b7} added {} \u{b7} worsened {} \u{b7} +{} CCN \u{b7} threshold {}",
        changes.len(),
        added,
        worsened,
        growth,
        threshold
    )
}

/// Subtitle with an optional `added-only`/`worsened-only` marker.
fn summary_line_filtered(changes: &[Change], threshold: u32, kind: Option<Kind>) -> String {
    let mut line = summary_line(changes, threshold);
    match kind {
        Some(Kind::Added) => line.push_str(" \u{b7} added-only"),
        Some(Kind::Worsened) => line.push_str(" \u{b7} worsened-only"),
        None => {}
    }
    line
}

/// Subtitle that additionally reports reviewed progress when the caller
/// supplies reviewed state. With `None` the line is unchanged.
fn summary_line_with_reviewed(
    changes: &[Change],
    threshold: u32,
    kind: Option<Kind>,
    reviewed: Option<&BTreeSet<String>>,
) -> String {
    let mut line = summary_line_filtered(changes, threshold, kind);
    if let Some(reviewed) = reviewed {
        let count = changes
            .iter()
            .filter(|change| reviewed.contains(&key(change)))
            .count();
        line.push_str(&format!(" \u{b7} reviewed {count}/{}", changes.len()));
    }
    line
}

/// The CCN transition: `new->n` for an added function, `base->n` otherwise.
fn ccn_text(change: &Change) -> String {
    match change.base_ccn {
        Some(base) => format!("{base}->{}", change.ccn),
        None => format!("new->{}", change.ccn),
    }
}

/// Signed CCN delta, always carrying an explicit sign.
fn delta_text(change: &Change) -> String {
    let delta = i64::from(change.ccn) - i64::from(change.base_ccn.unwrap_or(0));
    if delta >= 0 {
        format!("(+{delta})")
    } else {
        format!("(-{})", delta.unsigned_abs())
    }
}

/// One plain-text row: marker, line, name, transition and delta.
fn change_line(change: &Change) -> String {
    let marker = match change.kind {
        Kind::Added => '+',
        Kind::Worsened => '~',
    };
    format!(
        "{marker} {:>5} {} {} {}",
        change.line,
        change.name,
        ccn_text(change),
        delta_text(change)
    )
}

/// Two-line empty state.
fn empty_lines() -> [&'static str; 2] {
    [
        "No baseline changes",
        "pass --baseline <rev> to populate this view",
    ]
}

/// Truncate by characters (never bytes), with an ASCII ellipsis when possible.
fn truncate_chars(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width <= 3 {
        return text.chars().take(width).collect();
    }
    let mut out: String = text.chars().take(width - 3).collect();
    out.push_str("...");
    out
}

fn marker(change: &Change) -> char {
    match change.kind {
        Kind::Added => '+',
        Kind::Worsened => '~',
    }
}

fn marker_style(change: &Change) -> Style {
    let fg = match change.kind {
        Kind::Added => palette().accent_alt,
        Kind::Worsened => palette().danger,
    };
    Style::default().fg(fg).add_modifier(Modifier::BOLD)
}

/// Build one styled row. The caller guarantees `width > 0`.
#[allow(dead_code)]
fn row_line(change: &Change, width: u16, selected: bool) -> Line<'static> {
    row_line_with_check(change, width, selected, None)
}

/// Build one styled row, optionally prefixing the reviewed checkbox.
///
/// `checked == None` keeps the original row untouched; `Some` renders `[x]` or
/// `[ ]` before the kind marker. The selected pill background still wins.
fn row_line_with_check(
    change: &Change,
    width: u16,
    selected: bool,
    checked: Option<bool>,
) -> Line<'static> {
    let bg = if selected {
        palette().pill
    } else {
        palette().card
    };
    let text_style = if selected {
        Style::default()
            .fg(palette().text)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().text).bg(bg)
    };
    let muted = Style::default().fg(palette().muted).bg(bg);
    let width_chars = usize::from(width);

    let check_text = match checked {
        Some(true) => "[x] ",
        Some(false) => "[ ] ",
        None => "",
    };
    let mark = marker(change);
    let prefix = format!("{check_text}{mark} {:>5} ", change.line);
    let suffix = format!(" {} {}", ccn_text(change), delta_text(change));
    let base_len = prefix.chars().count() + suffix.chars().count();

    // At very narrow widths a single truncated span is the only sane output.
    if width_chars <= base_len + 1 {
        return Line::from(Span::styled(
            truncate_chars(&format!("{check_text}{}", change_line(change)), width),
            text_style,
        ));
    }

    // Give the reason whatever is left after reserving a readable name.
    let mut reason_text = String::new();
    if let Some(reason) = change.reasons.first() {
        let spare = width_chars.saturating_sub(base_len + 2 + 8);
        if spare >= 8 {
            reason_text = truncate_chars(reason, u16::try_from(spare).unwrap_or(u16::MAX));
        }
    }
    let reason_len = if reason_text.is_empty() {
        0
    } else {
        2 + reason_text.chars().count()
    };
    let name_budget = width_chars.saturating_sub(base_len + reason_len).max(1);
    let name = truncate_chars(&change.name, u16::try_from(name_budget).unwrap_or(u16::MAX));

    let mut spans: Vec<Span> = Vec::with_capacity(6);
    if let Some(checked) = checked {
        spans.push(Span::styled(
            check_text,
            if checked {
                Style::default()
                    .fg(palette().ok)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                muted
            },
        ));
    }
    spans.extend([
        Span::styled(format!("{mark} "), marker_style(change).bg(bg)),
        Span::styled(format!("{:>5} ", change.line), muted),
        Span::styled(name, text_style),
        Span::styled(
            suffix,
            Style::default()
                .fg(match change.kind {
                    Kind::Added => palette().danger,
                    Kind::Worsened => palette().warn,
                })
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    if !reason_text.is_empty() {
        spans.push(Span::styled(format!("  {reason_text}"), muted));
    }

    if selected {
        let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
        if used < width_chars {
            spans.push(Span::styled(
                " ".repeat(width_chars - used),
                Style::default().bg(bg),
            ));
        }
    }
    Line::from(spans)
}

/// One rendered list row, either a group header or a change.
enum Row {
    Header { path: String, count: usize },
    Change(usize),
}

/// Normalize path separators the same way the review model does.
fn normalized(path: &str) -> String {
    path.replace('\\', "/")
}

/// Build the row list, injecting a file header when several paths are present.
fn review_rows(changes: &[Change]) -> Vec<Row> {
    let mut keys: Vec<String> = Vec::new();
    for change in changes {
        let key = normalized(&change.path);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    if keys.len() <= 1 {
        return (0..changes.len()).map(Row::Change).collect();
    }

    let mut rows = Vec::with_capacity(changes.len() + keys.len());
    let mut previous: Option<String> = None;
    for (index, change) in changes.iter().enumerate() {
        let key = normalized(&change.path);
        if previous.as_deref() != Some(key.as_str()) {
            let count = changes
                .iter()
                .filter(|other| normalized(&other.path) == key)
                .count();
            rows.push(Row::Header {
                path: change.path.clone(),
                count,
            });
            previous = Some(key);
        }
        rows.push(Row::Change(index));
    }
    rows
}

/// One muted, bold file header row.
fn header_line(path: &str, count: usize, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        truncate_chars(&format!("{path} ({count})"), width),
        Style::default()
            .fg(palette().muted)
            .bg(palette().card)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Render the review panel inside `area`.
#[allow(dead_code)]
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    changes: &[Change],
    selected: usize,
    focused: bool,
    rev: &str,
    threshold: u32,
) {
    render_impl(
        frame, area, changes, selected, focused, rev, threshold, None, None,
    );
}

/// Render the review panel with an optional active kind filter.
///
/// The marker only decorates the subtitle; `changes` is expected to already be
/// filtered by the caller (see [`ReviewQueue::changes`]).
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_filtered(
    frame: &mut Frame,
    area: Rect,
    changes: &[Change],
    selected: usize,
    focused: bool,
    rev: &str,
    threshold: u32,
    kind: Option<Kind>,
) {
    render_impl(
        frame, area, changes, selected, focused, rev, threshold, kind, None,
    );
}

/// Render the review panel with kind filter and session reviewed marks.
///
/// `reviewed` holds [`crate::review::key`] values; rows gain a `[x]`/`[ ]`
/// checkbox and the subtitle gains `reviewed N/M`. Passing an empty set still
/// shows the checkbox column and `reviewed 0/M`, because the caller has opted
/// into reviewed state.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_filtered_with_reviewed(
    frame: &mut Frame,
    area: Rect,
    changes: &[Change],
    selected: usize,
    focused: bool,
    rev: &str,
    threshold: u32,
    kind: Option<Kind>,
    reviewed: &BTreeSet<String>,
) {
    render_impl(
        frame,
        area,
        changes,
        selected,
        focused,
        rev,
        threshold,
        kind,
        Some(reviewed),
    );
}

/// Shared renderer. `reviewed` is `None` for the legacy API, which keeps the
/// old rows and subtitle byte-for-byte identical.
#[allow(clippy::too_many_arguments)]
fn render_impl(
    frame: &mut Frame,
    area: Rect,
    changes: &[Change],
    selected: usize,
    focused: bool,
    rev: &str,
    threshold: u32,
    kind: Option<Kind>,
    reviewed: Option<&BTreeSet<String>>,
) {
    let border = if focused {
        palette().border_focus
    } else {
        palette().border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(palette().card))
        .title(Span::styled(
            title_text(rev),
            Style::default()
                .fg(if focused {
                    palette().accent
                } else {
                    palette().text
                })
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Row 0 is always the subtitle; the list gets what remains.
    let subtitle = truncate_chars(
        &summary_line_with_reviewed(changes, threshold, kind, reviewed),
        inner.width,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            subtitle,
            Style::default().fg(palette().muted).bg(palette().card),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let list_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(1),
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if list_area.height == 0 {
        return;
    }

    if changes.is_empty() {
        let lines: Vec<Line> = empty_lines()
            .iter()
            .map(|text| {
                Line::from(Span::styled(
                    truncate_chars(text, list_area.width),
                    Style::default().fg(palette().muted).bg(palette().card),
                ))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(palette().card)),
            list_area,
        );
        return;
    }

    // Headers shift every change row down, so scroll over the local row list.
    let rows = review_rows(changes);
    let selected = selected.min(changes.len() - 1);
    let selected_row = rows
        .iter()
        .position(|row| matches!(row, Row::Change(index) if *index == selected))
        .unwrap_or(0);

    let visible = usize::from(list_area.height).max(1);
    let start = if selected_row >= visible {
        selected_row + 1 - visible
    } else {
        0
    };
    let end = (start + visible).min(rows.len());

    let lines: Vec<Line> = rows[start..end]
        .iter()
        .map(|row| match row {
            Row::Header { path, count } => header_line(path, *count, list_area.width),
            Row::Change(index) => row_line_with_check(
                &changes[*index],
                list_area.width,
                *index == selected,
                reviewed.map(|set| set.contains(&key(&changes[*index]))),
            ),
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        list_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::CcnTier;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn change(kind: Kind, name: &str, line: usize, ccn: u32, base: Option<u32>) -> Change {
        Change {
            path: "src/example.rs".to_string(),
            name: name.to_string(),
            identity: format!("{name}(int)"),
            line,
            ccn,
            base_ccn: base,
            nloc: 42,
            base_nloc: base.map(|_| 20),
            max_nesting: 4,
            base_max_nesting: base.map(|_| 2),
            kind,
            reasons: vec!["ccn +3".to_string(), "nesting +1".to_string()],
            tier: CcnTier::Exact,
        }
    }

    fn sample() -> Vec<Change> {
        vec![
            change(Kind::Added, "brand_new", 12, 15, None),
            change(Kind::Worsened, "already_bad", 40, 21, Some(18)),
            change(Kind::Worsened, "regressed", 77, 9, Some(11)),
        ]
    }

    fn change_at(
        path: &str,
        kind: Kind,
        name: &str,
        line: usize,
        ccn: u32,
        base: Option<u32>,
    ) -> Change {
        let mut change = change(kind, name, line, ccn, base);
        change.path = path.to_string();
        change
    }

    fn multifile() -> Vec<Change> {
        vec![
            change_at("src/a.rs", Kind::Added, "a_new", 5, 14, None),
            change_at("src/a.rs", Kind::Worsened, "a_worse", 9, 20, Some(15)),
            change_at("src/b.rs", Kind::Worsened, "b_worse", 3, 30, Some(25)),
        ]
    }

    fn row_symbols(width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &multifile(), 2, true, "main", 10);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn grouping_headers_are_shown_for_multiple_files() {
        let rows = row_symbols(40, 8);
        let joined = rows.join("\n");
        assert!(joined.contains("src/a.rs (2)"), "rows: {joined}");
        assert!(joined.contains("src/b.rs (1)"), "rows: {joined}");
        assert!(joined.contains("a_new"));
        assert!(joined.contains("b_worse"));
    }

    #[test]
    fn single_file_still_renders_flat_without_header() {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &sample(), 0, true, "main", 10);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..6 {
            for x in 0..40 {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(!text.contains("src/example.rs ("), "flat list: {text}");
    }

    #[test]
    fn selected_row_stays_visible_with_injected_headers() {
        let changes = multifile();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &changes, 2, false, "r1", 10);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut found_selected = false;
        let mut found_pill = false;
        for y in 0..5 {
            let mut row = String::new();
            for x in 0..40 {
                let cell = &buffer[(x, y)];
                row.push_str(cell.symbol());
                if cell.bg == palette().pill {
                    found_pill = true;
                }
            }
            if row.contains("b_worse") {
                found_selected = true;
            }
        }
        assert!(
            found_selected,
            "selected change after a header should be scrolled into view"
        );
        assert!(found_pill, "selected row should use the pill background");
    }

    #[test]
    fn summary_counts_both_kinds_and_positive_growth() {
        let line = summary_line(&sample(), 10);
        assert_eq!(
            line,
            "3 changes \u{b7} added 1 \u{b7} worsened 2 \u{b7} +18 CCN \u{b7} threshold 10"
        );
        assert_eq!(
            summary_line(&[], 8),
            "0 changes \u{b7} added 0 \u{b7} worsened 0 \u{b7} +0 CCN \u{b7} threshold 8"
        );
        assert_eq!(
            summary_line_filtered(&sample(), 10, Some(Kind::Worsened)),
            "3 changes \u{b7} added 1 \u{b7} worsened 2 \u{b7} +18 CCN \u{b7} threshold 10 \u{b7} worsened-only"
        );
        assert_eq!(
            summary_line_filtered(&sample(), 10, Some(Kind::Added)),
            "3 changes \u{b7} added 1 \u{b7} worsened 2 \u{b7} +18 CCN \u{b7} threshold 10 \u{b7} added-only"
        );
        assert_eq!(
            summary_line_filtered(&sample(), 10, None),
            summary_line(&sample(), 10)
        );
    }

    #[test]
    fn change_lines_render_transition_and_signed_delta() {
        let rows = sample();
        assert_eq!(change_line(&rows[0]), "+    12 brand_new new->15 (+15)");
        assert_eq!(change_line(&rows[1]), "~    40 already_bad 18->21 (+3)");
        assert_eq!(change_line(&rows[2]), "~    77 regressed 11->9 (-2)");
    }

    #[test]
    fn truncation_is_by_chars_not_bytes() {
        assert_eq!(truncate_chars("abcdef", 6), "abcdef");
        assert_eq!(truncate_chars("abcdef", 5), "ab...");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abcdef", 0), "");
        // Four CJK characters are 12 bytes but only 4 columns/chars.
        assert_eq!(
            truncate_chars("\u{51fd}\u{6570}\u{540d}\u{79f0}", 4),
            "\u{51fd}\u{6570}\u{540d}\u{79f0}"
        );
        assert_eq!(
            truncate_chars("\u{51fd}\u{6570}\u{540d}\u{79f0}", 2),
            "\u{51fd}\u{6570}"
        );
    }

    #[test]
    fn title_and_empty_state_are_stable() {
        assert_eq!(title_text("abc123"), " Review \u{b7} abc123 ");
        assert_eq!(title_text(""), " Review ");
        assert_eq!(empty_lines()[0], "No baseline changes");
        assert_eq!(
            empty_lines()[1],
            "pass --baseline <rev> to populate this view"
        );
    }

    fn draw(width: u16, height: u16, changes: &[Change], selected: usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), changes, selected, true, "main", 10);
            })
            .unwrap();
    }

    #[test]
    fn tiny_areas_render_without_panicking() {
        for (w, h) in [(1, 1), (1, 3), (5, 2), (12, 3), (19, 4)] {
            draw(w, h, &sample(), 1);
            draw(w, h, &[], 0);
        }
    }

    fn reviewed_set(changes: &[Change], indices: &[usize]) -> BTreeSet<String> {
        indices.iter().map(|index| key(&changes[*index])).collect()
    }

    fn reviewed_rows(
        width: u16,
        height: u16,
        changes: &[Change],
        selected: usize,
        reviewed: &BTreeSet<String>,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render_filtered_with_reviewed(
                    frame,
                    frame.area(),
                    changes,
                    selected,
                    true,
                    "main",
                    10,
                    None,
                    reviewed,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn reviewed_rows_show_checked_and_unchecked_markers() {
        let changes = multifile();
        let reviewed = reviewed_set(&changes, &[0]);
        let joined = reviewed_rows(100, 8, &changes, 2, &reviewed).join("\n");

        assert!(joined.contains("[x]"), "rows: {joined}");
        assert!(joined.contains("[ ]"), "rows: {joined}");
        // The selected pill still wins over the checkbox decoration.
        assert!(joined.contains("b_worse"), "rows: {joined}");
    }

    #[test]
    fn reviewed_subtitle_reports_the_checked_count() {
        let changes = multifile();
        let reviewed = reviewed_set(&changes, &[0, 1]);
        let joined = reviewed_rows(100, 8, &changes, 0, &reviewed).join("\n");

        assert!(joined.contains("reviewed 2/3"), "rows: {joined}");
    }

    #[test]
    fn grouping_still_works_with_reviewed_state() {
        let changes = multifile();
        let reviewed = reviewed_set(&changes, &[0, 1, 2]);
        let joined = reviewed_rows(100, 8, &changes, 0, &reviewed).join("\n");

        assert!(joined.contains("src/a.rs (2)"), "rows: {joined}");
        assert!(joined.contains("src/b.rs (1)"), "rows: {joined}");
        assert!(joined.contains("reviewed 3/3"), "rows: {joined}");
    }

    #[test]
    fn legacy_render_is_unchanged_by_reviewed_support() {
        let changes = multifile();
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        terminal
            .draw(|frame| {
                render_filtered(frame, frame.area(), &changes, 0, true, "main", 10, None);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..8 {
            for x in 0..48 {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            !text.contains("[x]"),
            "legacy rows have no checkbox: {text}"
        );
        assert!(
            !text.contains("[ ]"),
            "legacy rows have no checkbox: {text}"
        );
        assert!(
            !text.contains("reviewed"),
            "legacy subtitle is unchanged: {text}"
        );
    }

    #[test]
    fn summary_with_reviewed_is_opt_in() {
        let changes = sample();
        assert_eq!(
            summary_line_with_reviewed(&changes, 10, None, None),
            summary_line(&changes, 10)
        );
        let reviewed = reviewed_set(&changes, &[1]);
        assert_eq!(
            summary_line_with_reviewed(&changes, 10, Some(Kind::Worsened), Some(&reviewed)),
            "3 changes \u{b7} added 1 \u{b7} worsened 2 \u{b7} +18 CCN \u{b7} threshold 10 \u{b7} worsened-only \u{b7} reviewed 1/3"
        );
    }

    #[test]
    fn tiny_areas_render_with_reviewed_state_without_panicking() {
        let changes = multifile();
        let reviewed = reviewed_set(&changes, &[2]);
        for (w, h) in [(1, 1), (1, 3), (5, 2), (12, 3), (19, 4)] {
            let _ = reviewed_rows(w, h, &changes, 2, &reviewed);
            let _ = reviewed_rows(w, h, &[], 0, &reviewed);
        }
    }

    #[test]
    fn selected_row_stays_visible_and_is_pill_backed() {
        let changes = sample();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &changes, 2, false, "r1", 10);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut found_regressed = false;
        let mut found_pill = false;
        for y in 0..5 {
            let mut row = String::new();
            for x in 0..40 {
                let cell = &buffer[(x, y)];
                row.push_str(cell.symbol());
                if cell.bg == palette().pill {
                    found_pill = true;
                }
            }
            if row.contains("regressed") {
                found_regressed = true;
            }
        }
        assert!(found_regressed, "selected row should be scrolled into view");
        assert!(found_pill, "selected row should use the pill background");
    }
}
