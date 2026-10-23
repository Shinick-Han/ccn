//! Baseline diff panel for the workbench.
//!
//! The panel is a read-only diff. It owns no state: the caller passes the
//! selected row on every frame and the renderer derives a local scroll window
//! from it, so the selection is always kept visible.
//!
//! Two presentations share the same input rows: the classic unified list
//! ([`render`] / [`render_with_hunks`]) and a two-column side-by-side view
//! ([`render_side_by_side`]) that pairs each removal with the addition that
//! replaced it.
//!
//! Text hygiene: tabs expand to four spaces and every other control character
//! (C0, DEL and C1, including `\r`/`\n`) becomes a single space, so source
//! text can never move the cursor or corrupt the surrounding layout.

use crate::diff::{hunks, DiffKind, DiffLine, Hunk};
use crate::theme::palette;
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

/// The hunk the caller asked to highlight, resolved against the current rows.
///
/// A stale or partially specified hunk falls back to the hunk containing its
/// start row, so navigation never highlights unrelated context.
fn resolve_hunk(rows: &[DiffLine], hunk: Option<Hunk>) -> Option<Hunk> {
    let hunk = hunk?;
    let all = hunks(rows);
    all.iter()
        .find(|candidate| candidate.start == hunk.start && candidate.end == hunk.end)
        .or_else(|| all.iter().find(|candidate| candidate.contains(hunk.start)))
        .copied()
}

/// `hunk N/M` for the active hunk, or `None` when there is nothing to label.
fn hunk_label(rows: &[DiffLine], active: Option<Hunk>) -> Option<String> {
    let all = hunks(rows);
    let active = resolve_hunk(rows, active)?;
    let index = all.iter().position(|hunk| *hunk == active)?;
    Some(format!("hunk {}/{}", index + 1, all.len()))
}

/// Summary plus an optional hunk position, e.g. `... \u{b7} hunk 2/5`.
fn summary_line_with_hunk(rows: &[DiffLine], selected: usize, active: Option<Hunk>) -> String {
    let base = summary_line(rows, selected);
    match hunk_label(rows, active) {
        Some(label) => format!("{base} \u{b7} {label}"),
        None => base,
    }
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
        DiffKind::Added => palette().ok,
        DiffKind::Removed => palette().danger,
        DiffKind::Context => palette().muted,
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
#[allow(dead_code)]
fn row_line(row: &DiffLine, num_width: usize, width: u16, selected: bool) -> Line<'static> {
    row_line_with(row, num_width, width, selected, false)
}

/// Build one styled row, optionally dimmed as part of the active hunk.
fn row_line_with(
    row: &DiffLine,
    num_width: usize,
    width: u16,
    selected: bool,
    active_hunk: bool,
) -> Line<'static> {
    let bg = if selected {
        palette().pill
    } else if active_hunk {
        // A shade darker than the card so the hunk reads as a group without
        // competing with the selected-row pill.
        palette().canvas
    } else {
        palette().card
    };
    let fg = kind_color(&row.kind);
    let kind_style = if selected {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).bg(bg)
    };
    let muted = Style::default().fg(palette().muted).bg(bg);
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
            Style::default().fg(palette().muted).bg(bg),
        ),
        Span::styled(" ", muted),
        Span::styled(
            format!("{new:>num_width$}"),
            Style::default().fg(palette().muted).bg(bg),
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
    render_with_hunks(frame, area, rows, selected, focused, title, None);
}

/// Render the diff panel, optionally labelling and highlighting one hunk.
///
/// `hunk` is a row range previously obtained from [`crate::diff::hunks`]. When
/// supplied, the subtitle gains `hunk N/M` and those rows use a slightly darker
/// background; when `None`, the output is identical to [`render`].
#[allow(dead_code)]
pub(crate) fn render_with_hunks(
    frame: &mut Frame,
    area: Rect,
    rows: &[DiffLine],
    selected: usize,
    focused: bool,
    title: &str,
    hunk: Option<Hunk>,
) {
    let active_hunk = resolve_hunk(rows, hunk);
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
            title_text(title),
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
        &summary_line_with_hunk(rows, selected, active_hunk),
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

    if rows.is_empty() {
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
        .map(|(offset, row)| {
            let index = start + offset;
            let selected = index == selected;
            let in_hunk = active_hunk.is_some_and(|hunk| hunk.contains(index));
            row_line_with(
                row,
                num_width,
                list_area.width,
                selected,
                in_hunk && !selected,
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        list_area,
    );
}

/// One visual row of the side-by-side view.
///
/// Each field is an index into the unified `rows` slice, or `None` when that
/// side has no line for the visual row. Old lines are always on the left and
/// new lines on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PairRow {
    /// Index of the old-side row, when the left column has a line.
    pub(crate) left: Option<usize>,
    /// Index of the new-side row, when the right column has a line.
    pub(crate) right: Option<usize>,
}

impl PairRow {
    /// Whether `index` is one of the unified rows shown by this visual row.
    fn contains(&self, index: usize) -> bool {
        self.left == Some(index) || self.right == Some(index)
    }

    /// Whether either side of this visual row lies inside `hunk`.
    fn touches(&self, hunk: Hunk) -> bool {
        self.left.is_some_and(|index| hunk.contains(index))
            || self.right.is_some_and(|index| hunk.contains(index))
    }
}

/// Pair unified rows into aligned, side-by-side visual rows.
///
/// Rules:
/// - A context row is one visual row with the same index on both sides.
/// - A maximal run of changed rows (no context between them) is split into its
///   removals and additions, and those are paired by position: the first
///   removal shares a visual row with the first addition, and so on. Since
///   [`crate::diff::diff_lines`] emits removals before additions, a replacement
///   therefore shows old on the left and new on the right on the same row.
/// - When one side of a run is longer, the surplus rows appear alone on their
///   side (`left`/`right` are `None` for the empty column).
///
/// Rows in different runs are never paired: a removal and an addition separated
/// by context (or by a different hunk) remain on separate visual rows.
pub(crate) fn pair_rows(rows: &[DiffLine]) -> Vec<PairRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut index = 0;
    while index < rows.len() {
        if rows[index].kind == DiffKind::Context {
            out.push(PairRow {
                left: Some(index),
                right: Some(index),
            });
            index += 1;
            continue;
        }

        let start = index;
        while index < rows.len() && rows[index].kind != DiffKind::Context {
            index += 1;
        }
        let removed: Vec<usize> = (start..index)
            .filter(|&row| rows[row].kind == DiffKind::Removed)
            .collect();
        let added: Vec<usize> = (start..index)
            .filter(|&row| rows[row].kind == DiffKind::Added)
            .collect();
        for slot in 0..removed.len().max(added.len()) {
            out.push(PairRow {
                left: removed.get(slot).copied(),
                right: added.get(slot).copied(),
            });
        }
    }
    out
}

/// Map a unified row index to the visual row that contains it.
///
/// The orchestrator tracks selection in unified rows, but each visual row may
/// show two unified rows (a removal and its replacement addition). The selected
/// index is clamped to the available rows, and an empty diff maps to visual row
/// `0`. Selecting either half of a replacement therefore highlights the same
/// visual row.
#[allow(dead_code)]
pub(crate) fn visual_row_for(rows: &[DiffLine], selected: usize) -> usize {
    visual_index(&pair_rows(rows), rows.len(), selected)
}

/// [`visual_row_for`] for an already-computed pair list.
fn visual_index(pairs: &[PairRow], row_count: usize, selected: usize) -> usize {
    if row_count == 0 {
        return 0;
    }
    let selected = selected.min(row_count - 1);
    pairs
        .iter()
        .position(|pair| pair.contains(selected))
        .unwrap_or(0)
}

/// Old or new side of the split view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

impl Side {
    /// The line number this side shows for `row`.
    fn number(self, row: &DiffLine) -> Option<usize> {
        match self {
            Side::Old => row.old_line,
            Side::New => row.new_line,
        }
    }
}

/// Narrowest list area that still renders two usable columns. Below this the
/// renderer falls back to a truncated unified row.
const MIN_SIDE_BY_SIDE_WIDTH: u16 = 5;

/// Width of one side's line-number column, at least one digit.
fn side_number_width(rows: &[DiffLine], side: Side) -> usize {
    rows.iter()
        .filter_map(|row| side.number(row))
        .map(|number| number.to_string().len())
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Subtitle for the split view: `+a -r · side-by-side[ · hunk N/M]`.
fn side_summary_line(rows: &[DiffLine], active: Option<Hunk>) -> String {
    let (added, removed, _) = counts(rows);
    let base = format!("+{added} -{removed} \u{b7} side-by-side");
    match hunk_label(rows, active) {
        Some(label) => format!("{base} \u{b7} {label}"),
        None => base,
    }
}

/// Background for a visual row, shared by both columns and the divider.
fn pair_bg(selected: bool, in_hunk: bool) -> Color {
    if selected {
        palette().pill
    } else if in_hunk {
        palette().canvas
    } else {
        palette().card
    }
}

/// One column cell, padded to exactly `width` characters.
///
/// A row without content for this side becomes blank; a row that has content
/// shows `number marker text`. Text is sanitised and truncated by characters,
/// so no control character or over-long line can disturb neighbouring cells.
fn side_cell(
    row: Option<&DiffLine>,
    side: Side,
    num_width: usize,
    width: u16,
    bg: Color,
    selected: bool,
) -> Vec<Span<'static>> {
    let width_chars = usize::from(width);
    if width_chars == 0 {
        return Vec::new();
    }

    let Some(row) = row else {
        return vec![Span::styled(
            " ".repeat(width_chars),
            Style::default().bg(bg),
        )];
    };

    let fg = kind_color(&row.kind);
    let text_style = if selected {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(fg).bg(bg)
    };
    let muted = Style::default().fg(palette().muted).bg(bg);
    let number = side
        .number(row)
        .map(|number| number.to_string())
        .unwrap_or_default();
    let marker = kind_marker(&row.kind);

    // Number, separator, marker and the space before the source text.
    let gutter = num_width.saturating_add(3);
    if width_chars <= gutter {
        let prefix = truncate_chars(&format!("{number:>num_width$} {marker}"), width);
        let used = prefix.chars().count();
        let mut spans = vec![Span::styled(prefix, text_style)];
        if used < width_chars {
            spans.push(Span::styled(
                " ".repeat(width_chars - used),
                Style::default().bg(bg),
            ));
        }
        return spans;
    }

    let text_budget = width_chars - gutter;
    let text = truncate_chars(
        &sanitize_text(&row.text),
        u16::try_from(text_budget).unwrap_or(u16::MAX),
    );
    let mut spans = vec![
        Span::styled(format!("{number:>num_width$}"), muted),
        Span::styled(" ", muted),
        Span::styled(marker.to_string(), text_style),
        Span::styled(" ", muted),
        Span::styled(text, text_style),
    ];
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if used < width_chars {
        spans.push(Span::styled(
            " ".repeat(width_chars - used),
            Style::default().bg(bg),
        ));
    }
    spans
}

/// Compose one visual row: left cell, divider, right cell.
///
/// Both cells and the divider use the same background, so a selected visual row
/// reads as one pill spanning the whole width.
#[allow(clippy::too_many_arguments)]
fn pair_line(
    left: Option<&DiffLine>,
    right: Option<&DiffLine>,
    left_num_width: usize,
    right_num_width: usize,
    left_width: u16,
    right_width: u16,
    divider_width: u16,
    bg: Color,
    selected: bool,
) -> Line<'static> {
    let mut spans = side_cell(left, Side::Old, left_num_width, left_width, bg, selected);
    let divider_style = if selected {
        Style::default().fg(palette().muted).bg(bg)
    } else {
        Style::default().fg(palette().border).bg(bg)
    };
    spans.push(Span::styled(
        "\u{2502}".repeat(usize::from(divider_width)),
        divider_style,
    ));
    spans.extend(side_cell(
        right,
        Side::New,
        right_num_width,
        right_width,
        bg,
        selected,
    ));
    Line::from(spans)
}

/// Unified rendering used when the list area is too narrow to split.
fn render_unified_fallback(
    frame: &mut Frame,
    area: Rect,
    rows: &[DiffLine],
    selected: usize,
    active: Option<Hunk>,
) {
    if rows.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let visible = usize::from(area.height).max(1);
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
        .map(|(offset, row)| {
            let index = start + offset;
            let selected = index == selected;
            let in_hunk = active.is_some_and(|hunk| hunk.contains(index));
            row_line_with(row, num_width, area.width, selected, in_hunk && !selected)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        area,
    );
}

/// Render the empty-diff state inside the list area.
fn render_empty_state(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = empty_lines()
        .iter()
        .map(|text| {
            Line::from(Span::styled(
                truncate_chars(text, area.width),
                Style::default().fg(palette().muted).bg(palette().card),
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        area,
    );
}

/// Render the diff as two aligned columns inside `area`.
///
/// Old lines and old line numbers are on the left, new lines and new line
/// numbers on the right, separated by a one-column divider. Removals are paired
/// with the additions that replaced them (see [`pair_rows`]) and context rows
/// occupy both columns.
///
/// `selected` is a unified row index. It is converted to the visual row that
/// contains it ([`visual_row_for`]), so either half of a replacement selects the
/// same visual row, and the local scroll window follows that visual row. When
/// the list area is too narrow for two columns the renderer falls back to a
/// truncated unified row.
#[allow(dead_code)]
pub(crate) fn render_side_by_side(
    frame: &mut Frame,
    area: Rect,
    rows: &[DiffLine],
    selected: usize,
    focused: bool,
    title: &str,
    hunk: Option<Hunk>,
) {
    let active_hunk = resolve_hunk(rows, hunk);
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
            title_text(title),
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

    // Row 0 is always the subtitle; the columns get what remains.
    let subtitle = truncate_chars(&side_summary_line(rows, active_hunk), inner.width);
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

    if rows.is_empty() {
        render_empty_state(frame, list_area);
        return;
    }

    if list_area.width < MIN_SIDE_BY_SIDE_WIDTH {
        render_unified_fallback(frame, list_area, rows, selected, active_hunk);
        return;
    }

    let pairs = pair_rows(rows);
    if pairs.is_empty() {
        return;
    }

    let visible = usize::from(list_area.height).max(1);
    let selected_visual = visual_index(&pairs, rows.len(), selected);
    let start = if selected_visual >= visible {
        selected_visual + 1 - visible
    } else {
        0
    };
    let end = (start + visible).min(pairs.len());

    let left_num_width = side_number_width(rows, Side::Old);
    let right_num_width = side_number_width(rows, Side::New);
    let divider_width = 1u16;
    let left_width = (list_area.width - divider_width) / 2;
    let right_width = list_area.width - divider_width - left_width;

    let lines: Vec<Line> = pairs[start..end]
        .iter()
        .enumerate()
        .map(|(offset, pair)| {
            let visual = start + offset;
            let selected = visual == selected_visual;
            let bg = pair_bg(selected, active_hunk.is_some_and(|hunk| pair.touches(hunk)));
            pair_line(
                pair.left.map(|index| &rows[index]),
                pair.right.map(|index| &rows[index]),
                left_num_width,
                right_num_width,
                left_width,
                right_width,
                divider_width,
                bg,
                selected,
            )
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
                if buffer[(x, y)].bg == palette().pill {
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

    fn two_hunk_sample() -> Vec<DiffLine> {
        vec![
            row(DiffKind::Context, Some(1), Some(1), "a"),
            row(DiffKind::Added, None, Some(2), "b"),
            row(DiffKind::Context, Some(2), Some(3), "c"),
            row(DiffKind::Removed, Some(3), None, "d"),
            row(DiffKind::Context, Some(4), Some(4), "e"),
        ]
    }

    #[test]
    fn hunk_label_reports_position_and_total() {
        let rows = sample();
        assert_eq!(hunk_label(&rows, None), None);
        assert_eq!(
            hunk_label(&rows, Some(Hunk { start: 1, end: 2 })),
            Some("hunk 1/1".to_string())
        );
        // A stale range still resolves through its start row.
        assert_eq!(
            hunk_label(&rows, Some(Hunk { start: 1, end: 99 })),
            Some("hunk 1/1".to_string())
        );

        let rows = two_hunk_sample();
        assert_eq!(
            hunk_label(&rows, Some(Hunk { start: 1, end: 1 })),
            Some("hunk 1/2".to_string())
        );
        assert_eq!(
            hunk_label(&rows, Some(Hunk { start: 3, end: 3 })),
            Some("hunk 2/2".to_string())
        );
        // No changed rows: no label even if a hunk was supplied.
        assert_eq!(hunk_label(&[], Some(Hunk { start: 0, end: 0 })), None);
    }

    #[test]
    fn summary_appends_hunk_position_without_changing_the_old_form() {
        let rows = sample();
        assert_eq!(
            summary_line_with_hunk(&rows, 2, None),
            summary_line(&rows, 2)
        );
        assert_eq!(
            summary_line_with_hunk(&rows, 2, Some(Hunk { start: 1, end: 2 })),
            "+1 -1 2 context \u{b7} 3/4 \u{b7} hunk 1/1"
        );
    }

    #[test]
    fn hunk_subtitle_and_highlight_render() {
        let rows = sample();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|frame| {
                render_with_hunks(
                    frame,
                    frame.area(),
                    &rows,
                    0,
                    true,
                    "main.rs",
                    Some(Hunk { start: 1, end: 2 }),
                );
            })
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("hunk 1/1"), "{text}");

        // The active hunk rows use the subtle canvas background.
        let buffer = terminal.backend().buffer();
        let mut found = false;
        for y in 0..8 {
            for x in 0..60 {
                if buffer[(x, y)].bg == palette().canvas {
                    found = true;
                }
            }
        }
        assert!(found, "active hunk should be highlighted: {text}");
    }

    #[test]
    fn tiny_areas_render_with_a_hunk_without_panicking() {
        for (w, h) in [(1, 1), (1, 3), (3, 2), (5, 2), (12, 3), (19, 4)] {
            let rows = sample();
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| {
                    render_with_hunks(
                        frame,
                        frame.area(),
                        &rows,
                        1,
                        false,
                        "x",
                        Some(Hunk { start: 1, end: 2 }),
                    );
                })
                .unwrap();
            // Empty rows with a hunk hint must also stay safe.
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| {
                    render_with_hunks(
                        frame,
                        frame.area(),
                        &[],
                        0,
                        false,
                        "",
                        Some(Hunk { start: 0, end: 0 }),
                    );
                })
                .unwrap();
        }
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

    fn draw_side(
        width: u16,
        height: u16,
        rows: &[DiffLine],
        selected: usize,
        hunk: Option<Hunk>,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render_side_by_side(frame, frame.area(), rows, selected, true, "main.rs", hunk);
            })
            .unwrap();
        terminal
    }

    fn pair_shape(rows: &[DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
        pair_rows(rows)
            .into_iter()
            .map(|pair| (pair.left, pair.right))
            .collect()
    }

    #[test]
    fn side_by_side_pairs_replacements_by_position() {
        let rows = sample();
        // Context, then the removed/added pair, then context.
        assert_eq!(
            pair_shape(&rows),
            vec![(Some(0), Some(0)), (Some(1), Some(2)), (Some(3), Some(3)),]
        );

        // More removals than additions: the surplus removal stays alone.
        let rows = vec![
            row(DiffKind::Removed, Some(1), None, "a"),
            row(DiffKind::Removed, Some(2), None, "b"),
            row(DiffKind::Added, None, Some(1), "c"),
        ];
        assert_eq!(pair_shape(&rows), vec![(Some(0), Some(2)), (Some(1), None)]);

        // More additions than removals: the surplus addition stays alone.
        let rows = vec![
            row(DiffKind::Removed, Some(1), None, "a"),
            row(DiffKind::Added, None, Some(1), "b"),
            row(DiffKind::Added, None, Some(2), "c"),
        ];
        assert_eq!(pair_shape(&rows), vec![(Some(0), Some(1)), (None, Some(2))]);
    }

    #[test]
    fn context_rows_align_on_both_sides() {
        let rows = vec![
            row(DiffKind::Context, Some(1), Some(1), "a"),
            row(DiffKind::Context, Some(2), Some(2), "b"),
        ];
        assert_eq!(
            pair_shape(&rows),
            vec![(Some(0), Some(0)), (Some(1), Some(1))]
        );
    }

    #[test]
    fn changed_rows_in_separate_hunks_are_not_paired() {
        let rows = vec![
            row(DiffKind::Removed, Some(1), None, "gone"),
            row(DiffKind::Context, Some(2), Some(2), "between"),
            row(DiffKind::Added, None, Some(3), "new"),
        ];
        assert_eq!(
            pair_shape(&rows),
            vec![(Some(0), None), (Some(1), Some(1)), (None, Some(2))]
        );
    }

    #[test]
    fn visual_row_mapping_covers_both_halves_of_a_replacement() {
        let rows = sample();
        assert_eq!(visual_row_for(&rows, 0), 0);
        // Selecting either the removal or its replacement highlights row 1.
        assert_eq!(visual_row_for(&rows, 1), 1);
        assert_eq!(visual_row_for(&rows, 2), 1);
        assert_eq!(visual_row_for(&rows, 3), 2);
        // Out-of-range selections clamp to the last visual row.
        assert_eq!(visual_row_for(&rows, 99), 2);
        assert_eq!(visual_row_for(&[], 0), 0);
        assert_eq!(visual_row_for(&[], 99), 0);

        let rows = vec![
            row(DiffKind::Removed, Some(1), None, "a"),
            row(DiffKind::Removed, Some(2), None, "b"),
            row(DiffKind::Added, None, Some(1), "c"),
        ];
        assert_eq!(visual_row_for(&rows, 0), 0);
        assert_eq!(visual_row_for(&rows, 1), 1);
        assert_eq!(visual_row_for(&rows, 2), 0);
    }

    #[test]
    fn side_by_side_subtitle_names_the_mode_and_hunk() {
        let rows = sample();
        let terminal = draw_side(70, 8, &rows, 1, Some(Hunk { start: 1, end: 2 }));
        let text = screen(&terminal);
        assert!(text.contains("+1 -1"), "{text}");
        assert!(text.contains("side-by-side"), "{text}");
        assert!(text.contains("hunk 1/1"), "{text}");

        let without_hunk = draw_side(70, 8, &rows, 1, None);
        let text = screen(&without_hunk);
        assert!(text.contains("side-by-side"), "{text}");
        assert!(!text.contains("hunk "), "{text}");
    }

    #[test]
    fn replacement_renders_old_left_and_new_right_on_the_same_row() {
        let terminal = draw_side(70, 8, &sample(), 1, None);
        let text = screen(&terminal);
        let line = text
            .lines()
            .find(|line| line.contains("old_call();") && line.contains("new_call();"))
            .unwrap_or_else(|| panic!("replacement should share one visual row: {text}"));
        assert!(
            line.find("old_call();").unwrap() < line.find("new_call();").unwrap(),
            "old text must be left of new text: {line}"
        );
    }

    #[test]
    fn unpaired_rows_render_on_their_own_side() {
        let rows = vec![row(DiffKind::Added, None, Some(1), "added_only")];
        let text = screen(&draw_side(60, 6, &rows, 0, None));
        let line = text
            .lines()
            .find(|line| line.contains("added_only"))
            .unwrap_or_else(|| panic!("added row should render: {text}"));
        let column = line[..line.find("added_only").unwrap()].chars().count();
        assert!(column > 27, "added row belongs in the right column: {line}");

        let rows = vec![row(DiffKind::Removed, Some(1), None, "removed_only")];
        let text = screen(&draw_side(60, 6, &rows, 0, None));
        let line = text
            .lines()
            .find(|line| line.contains("removed_only"))
            .unwrap_or_else(|| panic!("removed row should render: {text}"));
        let column = line[..line.find("removed_only").unwrap()].chars().count();
        assert!(
            column < 27,
            "removed row belongs in the left column: {line}"
        );
    }

    #[test]
    fn context_text_appears_in_both_columns() {
        let rows = vec![row(DiffKind::Context, Some(1), Some(1), "shared_ctx")];
        let text = screen(&draw_side(70, 6, &rows, 0, None));
        let line = text
            .lines()
            .find(|line| line.contains("shared_ctx"))
            .unwrap_or_else(|| panic!("context row should render: {text}"));
        assert_eq!(line.matches("shared_ctx").count(), 2, "{line}");
    }

    #[test]
    fn selected_visual_row_stays_visible_and_is_pill_backed() {
        let rows: Vec<DiffLine> = (0..10)
            .map(|index| {
                row(
                    DiffKind::Context,
                    Some(index + 1),
                    Some(index + 1),
                    &format!("line{index}"),
                )
            })
            .collect();
        // Border (2 rows) + subtitle (1) leaves two list rows for ten pairs.
        let terminal = draw_side(40, 5, &rows, 8, None);
        let text = screen(&terminal);
        assert!(
            text.contains("line8"),
            "selected pair should be visible: {text}"
        );
        assert!(
            !text.contains("line0"),
            "scroll should drop early rows: {text}"
        );

        let buffer = terminal.backend().buffer();
        let mut found_pill = false;
        for y in 0..5 {
            for x in 0..40 {
                if buffer[(x, y)].bg == palette().pill {
                    found_pill = true;
                }
            }
        }
        assert!(
            found_pill,
            "selected visual row should use the pill background"
        );
    }

    #[test]
    fn tiny_side_by_side_widths_fall_back_without_panicking() {
        for (width, height) in [(1, 1), (1, 3), (3, 2), (4, 4), (5, 2), (12, 3), (19, 4)] {
            draw_side(width, height, &sample(), 1, None);
            draw_side(width, height, &[], 0, None);
            draw_side(
                width,
                height,
                &sample(),
                99,
                Some(Hunk { start: 1, end: 2 }),
            );
        }

        // Very wide line numbers must not overflow a narrow two-column panel.
        let rows = vec![row(DiffKind::Added, None, Some(123_456), "text")];
        draw_side(8, 4, &rows, 0, None);
    }

    #[test]
    fn side_by_side_clamps_out_of_range_selection() {
        draw_side(50, 8, &sample(), usize::MAX, None);
        draw_side(50, 8, &[], 7, None);
    }
}
