//! Explain panel for the workbench.
//!
//! Read-only view over an [`Explanation`](crate::explain::Explanation). It owns
//! no state: the caller builds the explanation for the selected function and
//! the renderer fits it into whatever area it is given. The copy deliberately
//! says `heuristic breakdown`, because the engine's CCN remains the exact score
//! and these counts are only an explanation aid.

use crate::explain::Explanation;
use crate::theme::{bin_color, AURA};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Maximum decision sites echoed in the bottom section.
const TOP_LINES: usize = 6;

/// Width of the label column inside a breakdown row.
const LABEL_WIDTH: usize = 8;

/// Never let one proportional bar dominate an unusually wide panel.
const BAR_CAP: usize = 24;

fn title_text(function: &str) -> String {
    format!(" Explain \u{b7} {function} ")
}

fn subtitle_text(explanation: &Explanation) -> String {
    format!(
        "heuristic breakdown \u{b7} CCN {} \u{b7} nesting {} \u{b7} {} LOC",
        explanation.ccn, explanation.nesting, explanation.nloc
    )
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

/// One breakdown row: `label █████ count Lfirst`.
///
/// The bar is proportional to the largest count; every non-zero decision gets
/// at least one cell so it stays visible. The row degrades to `label count`
/// when the panel is too narrow for a bar and a line hint.
fn decision_row(
    decision: &crate::explain::Decision,
    rank: usize,
    total: usize,
    max_count: usize,
    width: u16,
) -> Line<'static> {
    let hint = decision
        .lines
        .first()
        .map(|line| format!("  L{line}"))
        .unwrap_or_default();
    let head = format!(" {:<LABEL_WIDTH$}", decision.label);
    let count = format!(" {:>3}", decision.count);
    let fixed = head.chars().count() + count.chars().count() + hint.chars().count();

    if usize::from(width) < fixed + 3 {
        return Line::from(Span::styled(
            truncate_chars(&format!("{} {}", decision.label, decision.count), width),
            Style::default().fg(AURA.text).bg(AURA.card),
        ));
    }

    let bar_room = (usize::from(width) - fixed).min(BAR_CAP);
    let bar_len = decision
        .count
        .saturating_mul(bar_room)
        .checked_div(max_count)
        .map_or(0, |len| len.clamp(1, bar_room));

    Line::from(vec![
        Span::styled(
            head,
            Style::default()
                .fg(AURA.accent_alt)
                .bg(AURA.card)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "\u{2588}".repeat(bar_len),
            Style::default().fg(bin_color(rank, total)).bg(AURA.card),
        ),
        Span::styled(
            count,
            Style::default()
                .fg(AURA.text)
                .bg(AURA.card)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(hint, Style::default().fg(AURA.muted).bg(AURA.card)),
    ])
}

fn top_line(line: usize, label: &'static str, width: u16) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            truncate_chars(&format!("L{line}"), width),
            Style::default().fg(AURA.muted).bg(AURA.card),
        ),
        Span::styled(
            format!(" {label}"),
            Style::default().fg(AURA.accent_alt).bg(AURA.card),
        ),
    ])
}

/// Render the explanation panel inside `area`.
#[allow(dead_code)]
pub(crate) fn render(frame: &mut Frame, area: Rect, explanation: &Explanation, focused: bool) {
    let border = if focused {
        AURA.border_focus
    } else {
        AURA.border
    };
    let title = truncate_chars(
        &title_text(&explanation.function),
        area.width.saturating_sub(2),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(AURA.card))
        .title(Span::styled(
            title,
            Style::default()
                .fg(if focused { AURA.accent } else { AURA.text })
                .bg(AURA.card)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Row 0 is the labelled subtitle; the breakdown gets what remains.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_chars(&subtitle_text(explanation), inner.width),
            Style::default().fg(AURA.muted).bg(AURA.card),
        )))
        .style(Style::default().bg(AURA.card)),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let remaining = inner.height.saturating_sub(1);
    if remaining == 0 {
        return;
    }
    let body_y = inner.y.saturating_add(1);

    if explanation.decisions.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars("No decision points found", inner.width),
                Style::default().fg(AURA.muted).bg(AURA.card),
            )))
            .style(Style::default().bg(AURA.card)),
            Rect {
                x: inner.x,
                y: body_y,
                width: inner.width,
                height: 1,
            },
        );
        return;
    }

    let top_count = explanation.top_lines.len().min(TOP_LINES);
    let top_height = if top_count == 0 {
        0
    } else {
        1 + top_count as u16
    };
    // Keep at least one breakdown row before deciding to show the top section.
    let show_top = top_height > 0 && remaining > top_height;
    let rows_height = if show_top {
        remaining - top_height
    } else {
        remaining
    };

    let max_count = explanation
        .decisions
        .iter()
        .map(|decision| decision.count)
        .max()
        .unwrap_or(0);
    let total = explanation.decisions.len();
    let rows: Vec<Line> = explanation
        .decisions
        .iter()
        .take(usize::from(rows_height))
        .enumerate()
        .map(|(rank, decision)| decision_row(decision, rank, total, max_count, inner.width))
        .collect();
    frame.render_widget(
        Paragraph::new(rows).style(Style::default().bg(AURA.card)),
        Rect {
            x: inner.x,
            y: body_y,
            width: inner.width,
            height: rows_height,
        },
    );

    if !show_top {
        return;
    }

    let top_y = body_y.saturating_add(rows_height);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_chars("Top decision lines", inner.width),
            Style::default()
                .fg(AURA.muted)
                .bg(AURA.card)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(AURA.card)),
        Rect {
            x: inner.x,
            y: top_y,
            width: inner.width,
            height: 1,
        },
    );

    let lines: Vec<Line> = explanation
        .top_lines
        .iter()
        .take(top_count)
        .map(|(line, label)| {
            let mut rendered = top_line(*line, label, inner.width);
            let used: usize = rendered
                .spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum();
            if used > usize::from(inner.width) {
                rendered.spans.truncate(1);
            }
            rendered
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
        Rect {
            x: inner.x,
            y: top_y.saturating_add(1),
            width: inner.width,
            height: top_count as u16,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explain::Decision;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn sample() -> Explanation {
        Explanation {
            function: "compute".to_string(),
            ccn: 9,
            nesting: 3,
            nloc: 12,
            decisions: vec![
                Decision {
                    label: "if",
                    count: 3,
                    lines: vec![4, 8, 12],
                },
                Decision {
                    label: "&&",
                    count: 2,
                    lines: vec![5, 9],
                },
            ],
            top_lines: vec![(4, "if"), (5, "&&"), (8, "if")],
        }
    }

    fn draw(width: u16, height: u16, explanation: &Explanation) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), explanation, true);
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
    fn renders_title_subtitle_counts_and_top_lines() {
        let explanation = sample();
        let mut terminal = Terminal::new(TestBackend::new(64, 18)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &explanation, true);
            })
            .unwrap();
        let text = screen(&terminal);

        assert!(text.contains("Explain \u{b7} compute"), "{text}");
        assert!(
            text.contains("heuristic breakdown \u{b7} CCN 9 \u{b7} nesting 3 \u{b7} 12 LOC"),
            "{text}"
        );
        assert!(text.contains("Top decision lines"), "{text}");
        assert!(text.contains("L4 if"), "{text}");
        assert!(text.contains("L5 &&"), "{text}");
        assert!(text.contains('\u{2588}'), "{text}");
        assert!(text.contains("  3"), "{text}");
        assert!(text.contains("L4"), "{text}");
    }

    #[test]
    fn empty_breakdown_says_so() {
        let explanation = Explanation {
            function: "plain".to_string(),
            ccn: 1,
            nesting: 0,
            nloc: 2,
            decisions: Vec::new(),
            top_lines: Vec::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(44, 7)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &explanation, false);
            })
            .unwrap();
        let text = screen(&terminal);

        assert!(text.contains("No decision points found"), "{text}");
        assert!(text.contains("CCN 1"), "{text}");
        assert!(!text.contains("Top decision lines"), "{text}");
    }

    #[test]
    fn tiny_areas_render_without_panicking() {
        let explanation = sample();
        let empty = Explanation {
            function: String::new(),
            ccn: 1,
            nesting: 0,
            nloc: 0,
            decisions: Vec::new(),
            top_lines: Vec::new(),
        };
        for (w, h) in [(0, 0), (1, 1), (1, 3), (3, 2), (5, 2), (12, 3), (19, 4)] {
            draw(w, h, &explanation);
            draw(w, h, &empty);
        }
    }

    #[test]
    fn narrow_panel_truncates_instead_of_panicking() {
        let explanation = Explanation {
            function: "a_rather_long_function_name_that_will_not_fit".to_string(),
            ccn: 30,
            nesting: 8,
            nloc: 240,
            decisions: vec![Decision {
                label: "while",
                count: 120,
                lines: vec![42],
            }],
            top_lines: vec![(42, "while")],
        };
        draw(20, 8, &explanation);
        draw(9, 6, &explanation);
        draw(4, 4, &explanation);

        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &explanation, false);
            })
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("..."), "title should be truncated: {text}");
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

    #[test]
    fn focused_border_uses_the_focus_colour() {
        let explanation = sample();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &explanation, true);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut found = false;
        for x in 0..40 {
            if buffer[(x, 0)].fg == AURA.border_focus {
                found = true;
            }
        }
        assert!(found, "focused panel should use the focus border colour");
    }
}
