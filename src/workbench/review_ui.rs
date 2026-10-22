//! Baseline review panel for the workbench.
//!
//! The panel is a compact, read-only list of functions that became complex
//! since a revision. It owns no state: the caller passes the selected index on
//! every frame and the renderer derives a local scroll window from it.

use crate::baseline::{Change, Kind};
use crate::theme::AURA;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

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
        Kind::Added => AURA.accent_alt,
        Kind::Worsened => AURA.danger,
    };
    Style::default().fg(fg).add_modifier(Modifier::BOLD)
}

/// Build one styled row. The caller guarantees `width > 0`.
fn row_line(change: &Change, width: u16, selected: bool) -> Line<'static> {
    let bg = if selected { AURA.pill } else { AURA.card };
    let text_style = if selected {
        Style::default()
            .fg(AURA.text)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(AURA.text).bg(bg)
    };
    let muted = Style::default().fg(AURA.muted).bg(bg);
    let width_chars = usize::from(width);

    let mark = marker(change);
    let prefix = format!("{mark} {:>5} ", change.line);
    let suffix = format!(" {} {}", ccn_text(change), delta_text(change));
    let base_len = prefix.chars().count() + suffix.chars().count();

    // At very narrow widths a single truncated span is the only sane output.
    if width_chars <= base_len + 1 {
        return Line::from(Span::styled(
            truncate_chars(&change_line(change), width),
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

    let mut spans = vec![
        Span::styled(format!("{mark} "), marker_style(change).bg(bg)),
        Span::styled(format!("{:>5} ", change.line), muted),
        Span::styled(name, text_style),
        Span::styled(
            suffix,
            Style::default()
                .fg(match change.kind {
                    Kind::Added => AURA.danger,
                    Kind::Worsened => AURA.warn,
                })
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
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
            title_text(rev),
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
    let subtitle = truncate_chars(&summary_line(changes, threshold), inner.width);
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

    if changes.is_empty() {
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
    let selected = selected.min(changes.len() - 1);
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let end = (start + visible).min(changes.len());

    let lines: Vec<Line> = changes[start..end]
        .iter()
        .enumerate()
        .map(|(offset, change)| row_line(change, list_area.width, start + offset == selected))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
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
                if cell.bg == AURA.pill {
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
