//! Diagnostics panel for the workbench.
//!
//! The panel is a compact, read-only list. It owns no state: the caller passes
//! the selected index on every frame and the renderer derives a local scroll
//! window from it, so the selection is always kept visible.

use crate::diagnostics::{Area, Diagnostic, Diagnostics, Filter, Severity};
use crate::theme::palette;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Title shown in the panel border, including the E/W/I counts.
fn title_text(diagnostics: &Diagnostics) -> String {
    let (errors, warnings, infos) = diagnostics.counts();
    format!(" Diagnostics \u{b7} {errors}E/{warnings}W/{infos}I ")
}

/// Severity marker, colour and letter.
fn severity_marker(severity: Severity) -> (char, ratatui::style::Color) {
    match severity {
        Severity::Error => ('E', palette().danger),
        Severity::Warn => ('W', palette().warn),
        Severity::Info => ('I', palette().accent),
    }
}

fn area_label(area: Area) -> &'static str {
    match area {
        Area::Scan => "scan",
        Area::Baseline => "baseline",
        Area::Editor => "editor",
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warning",
        Severity::Info => "info",
    }
}

/// Human-readable active filter, or `None` when nothing is narrowed.
fn filter_label(filter: Filter) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(severity) = filter.severity {
        parts.push(severity_label(severity));
    }
    if let Some(area) = filter.area {
        parts.push(area_label(area));
    }
    (!parts.is_empty()).then(|| parts.join(" + "))
}

/// Counts, visible/total, and the active filter when one is set.
fn subtitle_text(diagnostics: &Diagnostics, filter: Filter) -> String {
    let (errors, warnings, infos) = diagnostics.counts();
    let shown = diagnostics.filtered(filter).len();
    let mut text = format!(
        "{errors}E/{warnings}W/{infos}I \u{b7} {shown}/{} shown",
        diagnostics.len()
    );
    if let Some(label) = filter_label(filter) {
        text.push_str(&format!(" \u{b7} filter: {label}"));
    }
    text
}

/// `path:line`, `path`, or empty when the finding is not about a file.
fn location(item: &Diagnostic) -> String {
    match (&item.path, item.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.clone(),
        (None, _) => String::new(),
    }
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

/// Build one styled row: `[E] scan path:line message`.
///
/// The marker keeps its severity colour at every width; the rest is truncated
/// into whatever columns remain. The caller guarantees `width > 0`.
fn row_line(item: &Diagnostic, width: u16, selected: bool) -> Line<'static> {
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

    let (letter, colour) = severity_marker(item.severity);
    let mark = format!("[{letter}] ");
    let mark_style = Style::default()
        .fg(colour)
        .bg(bg)
        .add_modifier(Modifier::BOLD);

    // A single truncated span is the only sane output below the marker width.
    if width <= 4 {
        return Line::from(Span::styled(
            truncate_chars(&format!("{mark}{}", body_text(item)), width),
            mark_style,
        ));
    }

    let mut spans = vec![Span::styled(mark, mark_style)];
    let remaining = width - 4;

    // Below ~10 usable columns, drop the area column rather than the message.
    if remaining < 10 {
        spans.push(Span::styled(
            truncate_chars(&body_text(item), remaining),
            text_style,
        ));
        return finish_row(spans, width, selected);
    }

    let area = format!("{:<8} ", area_label(item.area));
    spans.push(Span::styled(area.clone(), muted));

    let location = location(item);
    let message = if location.is_empty() {
        item.message.clone()
    } else {
        format!("{location} {}", item.message)
    };
    let used = area.chars().count();
    spans.push(Span::styled(
        truncate_chars(&message, remaining - used as u16),
        text_style,
    ));
    finish_row(spans, width, selected)
}

/// Area, location and message without the severity marker.
fn body_text(item: &Diagnostic) -> String {
    let location = location(item);
    if location.is_empty() {
        format!("{} {}", area_label(item.area), item.message)
    } else {
        format!("{} {location} {}", area_label(item.area), item.message)
    }
}

/// Fill the row background when selected so the pill reaches the panel edge.
fn finish_row(mut spans: Vec<Span<'static>>, width: u16, selected: bool) -> Line<'static> {
    if selected {
        let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
        let width = usize::from(width);
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(palette().pill),
            ));
        }
    }
    Line::from(spans)
}

/// Two-line empty state when there is nothing to show at all.
fn empty_lines() -> [&'static str; 2] {
    ["No diagnostics", "scan and baseline look clean"]
}

/// Two-line empty state when diagnostics exist but the filter hides them.
fn no_matches_lines() -> [&'static str; 2] {
    ["No matches for filter", "adjust or clear the filter"]
}

/// Render the diagnostics panel inside `area`.
#[allow(dead_code)]
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    diagnostics: &Diagnostics,
    selected: usize,
    focused: bool,
) {
    render_filtered(
        frame,
        area,
        diagnostics,
        selected,
        focused,
        Filter::default(),
    );
}

/// Render the diagnostics panel with a `filter` applied to the rows.
///
/// Selection indices refer to the filtered rows. The caller is expected to
/// clamp them; an out-of-range index is clamped here as a safety net.
#[allow(dead_code)]
pub(crate) fn render_filtered(
    frame: &mut Frame,
    area: Rect,
    diagnostics: &Diagnostics,
    selected: usize,
    focused: bool,
    filter: Filter,
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
            title_text(diagnostics),
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
    let subtitle = truncate_chars(&subtitle_text(diagnostics, filter), inner.width);
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

    let items = diagnostics.filtered(filter);
    let empty = if diagnostics.is_empty() {
        empty_lines()
    } else if items.is_empty() {
        no_matches_lines()
    } else {
        let visible = usize::from(list_area.height).max(1);
        let selected = selected.min(items.len() - 1);
        let start = if selected >= visible {
            selected + 1 - visible
        } else {
            0
        };
        let end = (start + visible).min(items.len());

        let lines: Vec<Line> = items[start..end]
            .iter()
            .enumerate()
            .map(|(offset, item)| row_line(item, list_area.width, start + offset == selected))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(palette().card)),
            list_area,
        );
        return;
    };

    let lines: Vec<Line> = empty
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn item(
        severity: Severity,
        area: Area,
        path: Option<&str>,
        code: &str,
        message: &str,
    ) -> Diagnostic {
        Diagnostic {
            area,
            severity,
            path: path.map(str::to_string),
            line: path.map(|_| 42),
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    fn sample() -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        diagnostics.push(item(
            Severity::Error,
            Area::Scan,
            Some("src/broken.rs"),
            "read-error",
            "could not read Rust",
        ));
        diagnostics.push(item(
            Severity::Warn,
            Area::Scan,
            Some("src/big.rs"),
            "oversize",
            "skipped by the CCN size guard",
        ));
        diagnostics.push(item(
            Severity::Info,
            Area::Baseline,
            None,
            "baseline-unmapped",
            "2 file(s) outside the version-control view",
        ));
        diagnostics
    }

    fn draw(width: u16, height: u16, diagnostics: &Diagnostics, selected: usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), diagnostics, selected, true);
            })
            .unwrap();
    }

    fn draw_filtered(
        width: u16,
        height: u16,
        diagnostics: &Diagnostics,
        selected: usize,
        filter: Filter,
    ) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render_filtered(frame, frame.area(), diagnostics, selected, true, filter);
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
    fn empty_state_is_rendered() {
        let diagnostics = Diagnostics::new();
        let mut terminal = Terminal::new(TestBackend::new(36, 6)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), &diagnostics, 0, false);
            })
            .unwrap();
        let text = screen(&terminal);

        assert!(text.contains("No diagnostics"), "{text}");
        assert!(text.contains("scan and baseline look clean"), "{text}");
        assert!(text.contains("0E/0W/0I"), "{text}");
    }

    #[test]
    fn title_and_summary_carry_counts() {
        let diagnostics = sample();
        assert_eq!(title_text(&diagnostics), " Diagnostics \u{b7} 1E/1W/1I ");
        assert_eq!(diagnostics.summary(), "1 error(s), 1 warning(s), 1 info");
    }

    #[test]
    fn row_format_is_marker_area_location_message() {
        let diagnostics = sample();
        let item = &diagnostics.items()[0];
        let line = row_line(item, 60, false);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "[E] scan     src/broken.rs:42 could not read Rust");
    }

    #[test]
    fn tiny_areas_render_without_panicking() {
        for (w, h) in [(1, 1), (1, 3), (3, 2), (5, 2), (12, 3), (19, 4)] {
            draw(w, h, &sample(), 1);
            draw(w, h, &Diagnostics::new(), 0);
        }
    }

    #[test]
    fn selected_row_stays_visible_and_is_pill_backed() {
        let diagnostics = sample();
        let mut terminal = Terminal::new(TestBackend::new(44, 5)).unwrap();
        terminal
            .draw(|frame| {
                // Border (2 rows) + summary (1) leaves one list row.
                render(frame, frame.area(), &diagnostics, 2, false);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = screen(&terminal);

        assert!(
            text.contains("outside the vers"),
            "selected row should be scrolled into view: {text}"
        );
        let mut found_pill = false;
        for y in 0..5 {
            for x in 0..44 {
                if buffer[(x, y)].bg == palette().pill {
                    found_pill = true;
                }
            }
        }
        assert!(found_pill, "selected row should use the pill background");
    }

    #[test]
    fn filter_changes_the_visible_row_count() {
        let diagnostics = sample();
        let mut all = Terminal::new(TestBackend::new(60, 8)).unwrap();
        all.draw(|frame| {
            render_filtered(
                frame,
                frame.area(),
                &diagnostics,
                0,
                true,
                Filter::default(),
            );
        })
        .unwrap();
        let all_text = screen(&all);
        assert!(all_text.contains("3/3 shown"), "{all_text}");
        assert!(all_text.contains("src/big.rs"), "{all_text}");
        assert!(all_text.contains("outside the vers"), "{all_text}");

        let errors = Filter {
            severity: Some(Severity::Error),
            area: None,
        };
        let mut filtered = Terminal::new(TestBackend::new(60, 8)).unwrap();
        filtered
            .draw(|frame| {
                render_filtered(frame, frame.area(), &diagnostics, 0, true, errors);
            })
            .unwrap();
        let filtered_text = screen(&filtered);
        assert!(filtered_text.contains("1/3 shown"), "{filtered_text}");
        assert!(filtered_text.contains("filter: error"), "{filtered_text}");
        assert!(filtered_text.contains("src/broken.rs"), "{filtered_text}");
        assert!(!filtered_text.contains("src/big.rs"), "{filtered_text}");
        assert!(
            !filtered_text.contains("outside the vers"),
            "{filtered_text}"
        );
    }

    #[test]
    fn selected_row_scrolls_within_filtered_rows() {
        let diagnostics = sample();
        let filter = Filter {
            severity: None,
            area: Some(Area::Scan),
        };
        let mut terminal = Terminal::new(TestBackend::new(44, 4)).unwrap();
        terminal
            .draw(|frame| {
                // One list row: selecting the second filtered row scrolls to it.
                render_filtered(frame, frame.area(), &diagnostics, 1, false, filter);
            })
            .unwrap();
        let text = screen(&terminal);
        let buffer = terminal.backend().buffer();

        assert!(text.contains("2/3 shown"), "{text}");
        assert!(text.contains("filter: scan"), "{text}");
        assert!(text.contains("src/big.rs"), "{text}");
        assert!(!text.contains("src/broken.rs"), "{text}");
        let mut found_pill = false;
        for y in 0..4 {
            for x in 0..44 {
                if buffer[(x, y)].bg == palette().pill {
                    found_pill = true;
                }
            }
        }
        assert!(found_pill, "filtered selected row should be pill-backed");
    }

    #[test]
    fn empty_filter_state_differs_from_empty_diagnostics() {
        let diagnostics = sample();
        let filter = Filter {
            severity: Some(Severity::Info),
            area: Some(Area::Editor),
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 6)).unwrap();
        terminal
            .draw(|frame| {
                render_filtered(frame, frame.area(), &diagnostics, 0, false, filter);
            })
            .unwrap();
        let text = screen(&terminal);

        assert!(text.contains("No matches for filter"), "{text}");
        assert!(text.contains("adjust or clear the filter"), "{text}");
        assert!(!text.contains("No diagnostics"), "{text}");
        assert!(text.contains("0/3 shown"), "{text}");
        assert!(text.contains("filter: info + editor"), "{text}");

        // With no diagnostics at all the original empty copy still wins.
        let empty = Diagnostics::new();
        let mut clean = Terminal::new(TestBackend::new(48, 6)).unwrap();
        clean
            .draw(|frame| {
                render_filtered(frame, frame.area(), &empty, 0, false, filter);
            })
            .unwrap();
        let clean_text = screen(&clean);
        assert!(clean_text.contains("No diagnostics"), "{clean_text}");
        assert!(
            !clean_text.contains("No matches for filter"),
            "{clean_text}"
        );
    }

    #[test]
    fn filtered_selected_index_is_clamped() {
        let diagnostics = sample();
        let filter = Filter {
            severity: None,
            area: Some(Area::Scan),
        };
        // No panic even though 99 is past the end of the filtered rows.
        draw_filtered(40, 8, &diagnostics, 99, filter);
    }

    #[test]
    fn selected_index_is_clamped() {
        let diagnostics = sample();
        // Must not panic even when the caller passes an index past the end.
        draw(40, 8, &diagnostics, 99);
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
