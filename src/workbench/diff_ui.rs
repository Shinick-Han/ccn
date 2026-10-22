//! Baseline diff panel for the workbench.
//!
//! The panel is a read-only unified diff. It owns no state: the caller passes
//! the selected row on every frame and the renderer derives a local scroll
//! window from it, so the selection is always kept visible.
//!
//! Text hygiene: tabs expand to four spaces and every other control character
//! (C0, DEL and C1, including `\r`/`\n`) becomes a single space, so source
//! text can never move the cursor or corrupt the surrounding layout.

use crate::diff::{DiffKind, DiffLine};
use crate::theme::AURA;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Panel title: `Diff · <title>`, or just `Diff` when there is no title.
fn title_text(title: &str) -> String {
    if title.trim().is_empty() {
        " Diff ".to_string()
    } else {
        format!(" Diff \u{b7} {} ", title.trim())
    }
}

/// Counts of each kind in `rows`.
fn counts(rows: &[DiffLine]) -> (usize, usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    let mut context = 0;
    for row in rows {
        match &row.kind {
            DiffKind::Added => added += 1,
            DiffKind::Removed => removed += 1,
            DiffKind::Context => context += 1,
        }
    }
    (added, removed, context)
}

/// Subtitle: kind totals plus the selected row position.
fn summary_line(rows: &[DiffLine], selected: usize) -> String {
    let (added, removed, context) = counts(rows);
    let position = if rows.is_empty() {
        0
    } else {
        selected.min(rows.len() - 1) + 1
    };
    format!(
        "+{added} -{removed} {context} context \u{b7} {position}/{}",
        rows.len()
    )
}

/// Marker shown between the line numbers and the source text.
fn kind_marker(kind: &DiffKind) -> char {
    match kind {
        DiffKind::Added => '+',
        DiffKind::Removed => '-',
        DiffKind::Context => ' ',
    }
}

/// Foreground colour for a row's marker and text.
fn kind_color(kind: &DiffKind) -> Color {
    match kind {
        DiffKind::Added => AURA.ok,
        DiffKind::Removed => AURA.danger,
        DiffKind::Context => AURA.muted,
    }
}

/// Width of one line-number column, at least one digit.
fn number_width(rows: &[DiffLine]) -> usize {
    rows.iter()
        .flat_map(|row| row.old_line.into_iter().chain(row.new_line))
        .map(|number| number.to_string().len())
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Replace tabs with four spaces and every other control character with a
/// space. The result contains no cursor-moving or layout-breaking characters.
fn sanitize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            other if other.is_control() => out.push(' '),
            other => out.push(other),
        }
    }
    out
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

/// Plain-text rendering of one row: `old new marker text`.
///
/// Line numbers are right-aligned inside `num_width` columns; a missing side
/// is left blank. This is the canonical formatting helper used by the renderer
/// and the tests.
fn format_row(row: &DiffLine, num_width: usize) -> String {
    let old = row.old_line.map(|n| n.to_string()).unwrap_or_default();
    let new = row.new_line.map(|n| n.to_string()).unwrap_or_default();
    format!(
        "{old:>num_width$} {new:>num_width$} {} {}",
        kind_marker(&row.kind),
        sanitize_text(&row.text)
    )
}

/// Build one styled row. The caller guarantees `width > 0`.
fn row_line(row: &DiffLine, num_width: usize, width: u16, selected: bool) -> Line<'static> {
    let bg = if selected { AURA.pill } else { AURA.card };
    let fg = kind_color(&row.kind);
    let kind_style = if selected {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).bg(bg)
    };
    let muted = Style::default().fg(AURA.muted).bg(bg);
    let width_chars = usize::from(width);

    // Two number columns, two separators, the marker and its trailing space.
    let gutter = num_width.saturating_mul(2).saturating_add(4);
    if width_chars <= gutter {
        return Line::from(Span::styled(
            truncate_chars(&format_row(row, num_width), width),
            kind_style,
        ));
    }

    let old = row.old_line.map(|n| n.to_string()).unwrap_or_default();
    let new = row.new_line.map(|n| n.to_string()).unwrap_or_default();
    let text_budget = width_chars.saturating_sub(gutter);
    let text = truncate_chars(
        &sanitize_text(&row.text),
        u16::try_from(text_budget).unwrap_or(u16::MAX),
    );

    let mut spans = vec![
        Span::styled(
            format!("{old:>num_width$}"),
            Style::default().fg(AURA.muted).bg(bg),
        ),
        Span::styled(" ", muted),
        Span::styled(
            format!("{new:>num_width$}"),
            Style::default().fg(AURA.muted).bg(bg),
        ),
        Span::styled(" ", muted),
        Span::styled(kind_marker(&row.kind).to_string(), kind_style),
        Span::styled(" ", muted),
        Span::styled(text, kind_style),
    ];

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

/// Two-line empty state.
fn empty_lines() -> [&'static str; 2] {
    ["No diff rows", "baseline and current are identical"]
}

/// Render the diff panel inside `area`.
#[allow(dead_code)]
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    rows: &[DiffLine],
    selected: usize,
    focused: bool,
    title: &str,
) {
    let border = if focused {
        AURA.border_focus
    } else {
        AURA.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(AURA.card))
        .title(Span::styled(
            title_text(title),
            Style::default()
                .fg(if focused { AURA.accent } else { AURA.text })
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Row 0 is always the subtitle; the list gets what remains.
    let subtitle = truncate_chars(&summary_line(rows, selected), inner.width);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            subtitle,
            Style::default().fg(AURA.muted).bg(AURA.card),
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

    if rows.is_empty() {
        let lines: Vec<Line> = empty_lines()
            .iter()
            .map(|text| {
                Line::from(Span::styled(
                    truncate_chars(text, list_area.width),
                    Style::default().fg(AURA.muted).bg(AURA.card),
                ))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(AURA.card)),
            list_area,
        );
        return;
    }

    let visible = usize::from(list_area.height).max(1);
    let selected = selected.min(rows.len() - 1);
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let end = (start + visible).min(rows.len());
    let num_width = number_width(rows);

    let lines: Vec<Line> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| row_line(row, num_width, list_area.width, start + offset == selected))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
        list_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn row(
        kind: DiffKind,
        old_line: Option<usize>,
        new_line: Option<usize>,
        text: &str,
    ) -> DiffLine {
        DiffLine {
            kind,
            old_line,
            new_line,
            text: text.to_string(),
        }
    }

    fn sample() -> Vec<DiffLine> {
        vec![
            row(DiffKind::Context, Some(1), Some(1), "fn main() {"),
            row(DiffKind::Removed, Some(2), None, "    old_call();"),
            row(DiffKind::Added, None, Some(2), "    new_call();"),
            row(DiffKind::Context, Some(3), Some(3), "}"),
        ]
    }

    fn draw(width: u16, height: u16, rows: &[DiffLine], selected: usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), rows, selected, true, "main.rs");
            })
            .unwrap();
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn title_and_summary_carry_counts_and_position() {
        assert_eq!(title_text("main.rs"), " Diff \u{b7} main.rs ");
        assert_eq!(title_text(""), " Diff ");
        assert_eq!(title_text("   "), " Diff ");
        assert_eq!(summary_line(&sample(), 2), "+1 -1 2 context \u{b7} 3/4");
        assert_eq!(summary_line(&[], 0), "+0 -0 0 context \u{b7} 0/0");
        // Out-of-range selections clamp instead of showing a bogus position.
        assert_eq!(summary_line(&sample(), 99), "+1 -1 2 context \u{b7} 4/4");
    }

    #[test]
    fn row_format_aligns_numbers_and_marks_kinds() {
        let rows = sample();
        let width = number_width(&rows);
        assert_eq!(width, 1);
        assert_eq!(format_row(&rows[0], width), "1 1   fn main() {");
        assert_eq!(format_row(&rows[1], width), "2   -     old_call();");
        assert_eq!(format_row(&rows[2], width), "  2 +     new_call();");
    }

    #[test]
    fn wide_line_numbers_share_one_column_width() {
        let rows = vec![
            row(DiffKind::Context, Some(9), Some(9), "a"),
            row(DiffKind::Context, Some(1000), Some(1002), "b"),
        ];
        assert_eq!(number_width(&rows), 4);
        assert_eq!(format_row(&rows[0], 4), "   9    9   a");
    }

    #[test]
    fn tabs_expand_and_control_chars_are_neutralised() {
        assert_eq!(sanitize_text("a\tb"), "a    b");
        assert_eq!(sanitize_text("a\r\nb"), "a  b");
        assert_eq!(sanitize_text("a\u{1b}[31mb"), "a [31mb");
        assert_eq!(sanitize_text("ok\u{7f}"), "ok ");

        let rows = vec![row(DiffKind::Added, None, Some(1), "\tlet x\t= 1;")];
        assert_eq!(format_row(&rows[0], 1), "  1 +     let x    = 1;");
        // No raw control characters may reach the buffer.
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &rows, 0, true, "x");
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..4 {
            for x in 0..40 {
                assert!(
                    !buffer[(x, y)].symbol().chars().any(char::is_control),
                    "control char leaked at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn empty_state_is_rendered() {
        let mut terminal = Terminal::new(TestBackend::new(44, 6)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &[], 0, false, "");
            })
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Diff"), "{text}");
        assert!(text.contains("+0 -0 0 context"), "{text}");
        assert!(text.contains("No diff rows"), "{text}");
        assert!(
            text.contains("baseline and current are identical"),
            "{text}"
        );
    }

    #[test]
    fn selected_row_stays_visible_and_is_pill_backed() {
        let rows = sample();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| {
                // Border (2 rows) + subtitle (1) leaves two list rows.
                render(frame, frame.area(), &rows, 3, false, "main.rs");
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = screen(&terminal);

        assert!(text.contains("}"), "selected row should be visible: {text}");
        let mut found_pill = false;
        for y in 0..5 {
            for x in 0..40 {
                if buffer[(x, y)].bg == AURA.pill {
                    found_pill = true;
                }
            }
        }
        assert!(found_pill, "selected row should use the pill background");
    }

    #[test]
    fn selected_index_is_clamped() {
        // Must not panic even when the caller passes an index past the end.
        draw(40, 8, &sample(), 99);
    }

    #[test]
    fn tiny_areas_render_without_panicking() {
        for (w, h) in [(1, 1), (1, 3), (3, 2), (5, 2), (12, 3), (19, 4)] {
            draw(w, h, &sample(), 1);
            draw(w, h, &[], 0);
        }
        // Very wide line numbers must not overflow a narrow panel.
        let rows = vec![row(DiffKind::Added, None, Some(123_456), "text")];
        draw(8, 4, &rows, 0);
    }

    #[test]
    fn truncation_is_by_chars_not_bytes() {
        assert_eq!(truncate_chars("abcdef", 6), "abcdef");
        assert_eq!(truncate_chars("abcdef", 5), "ab...");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(
            truncate_chars("\u{51fd}\u{6570}\u{540d}\u{79f0}", 2),
            "\u{51fd}\u{6570}"
        );
    }
}
