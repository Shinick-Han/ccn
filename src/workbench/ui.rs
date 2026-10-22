//! Layout and rendering for the workbench spike.
//!
//! The layout is deliberately boring: header, editor | sidebar, footer. The
//! editor pane is a real vt100 screen rendered by `tui-term`; everything else
//! is plain ratatui.

use crate::breadcrumb;
use crate::theme::{grade_color, AURA};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use vt100::Screen;

use super::{App, Focus, LiveRow, RpcStatus};

/// Named regions of the workbench, recomputed on every resize.
pub struct LayoutRects {
    pub header: Rect,
    pub editor: Rect,
    pub sidebar: Rect,
    pub footer: Rect,
}

pub fn layout(area: Rect) -> LayoutRects {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(area);
    let sidebar_width = sidebar_width(area.width);
    let [editor, sidebar] =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(sidebar_width)]).areas(body);
    LayoutRects {
        header,
        editor,
        sidebar,
        footer,
    }
}

fn sidebar_width(total: u16) -> u16 {
    let preferred = (total / 3).clamp(24, 42);
    preferred.min(total.saturating_sub(12)).max(8)
}

/// The area available to the child process: the editor block's inner rect.
pub fn editor_inner(editor: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(editor)
}

/// Split the sidebar into its summary block and its function-list block.
pub(crate) fn sidebar_regions(sidebar: Rect) -> (Rect, Rect) {
    let inner = Block::default().borders(Borders::ALL).inner(sidebar);
    let [summary, list] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).areas(inner);
    (summary, list)
}

/// Height in terminal rows of one function entry (name row + identity row).
pub(crate) const FUNCTION_ROWS: u16 = 2;

/// Number of function entries that fit in the sidebar list area.
pub(crate) fn visible_functions(list: Rect) -> usize {
    (list.height / FUNCTION_ROWS).max(1) as usize
}

/// First function index shown for `selected` (page-aligned scrolling).
pub(crate) fn list_offset(selected: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    (selected / visible) * visible
}

/// Map a terminal row inside the sidebar list to a function index.
///
/// Rows are grouped in pairs (name + identity), and `offset` is the first
/// function index rendered at the top of `list`. Rows outside the list, empty
/// lists, and rows past the last function all return `None`.
pub(crate) fn function_row_at(list: Rect, offset: usize, count: usize, row: u16) -> Option<usize> {
    if count == 0 || row < list.y || row >= list.y.saturating_add(list.height) {
        return None;
    }
    let index = offset + usize::from((row - list.y) / FUNCTION_ROWS);
    (index < count).then_some(index)
}

pub fn render(frame: &mut Frame, app: &App, screen: &Screen) {
    render_area(frame, app, screen, frame.area());
}

/// Render the complete workbench UI inside `area` (a split pane, a test
/// backend, or the full terminal).
pub fn render_area(frame: &mut Frame, app: &App, screen: &Screen, area: Rect) {
    frame.render_widget(
        Block::default().style(Style::default().bg(AURA.canvas)),
        area,
    );
    let rects = layout(area);

    render_header(frame, app, rects.header);
    render_editor(frame, app, screen, rects.editor);
    render_sidebar(frame, app, rects.sidebar);
    render_footer(frame, app, rects.footer);
}

/// Compact tab bar: `[1 engine.cpp●] [2 util.c]`, active tab highlighted.
fn tab_spans(app: &App) -> Vec<Span<'static>> {
    let active = app.tabs.active_index();
    app.tabs
        .titles()
        .into_iter()
        .enumerate()
        .map(|(index, title)| {
            let style = if Some(index) == active {
                Style::default()
                    .fg(AURA.canvas)
                    .bg(AURA.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(AURA.muted)
            };
            Span::styled(format!(" [{} {}] ", index + 1, title), style)
        })
        .collect()
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view.as_ref();
    let mut path_spans = vec![
        Span::styled(
            " ccn workbench",
            Style::default()
                .fg(AURA.accent_alt)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
    ];
    if app.tabs.is_empty() {
        path_spans.push(Span::styled(
            app.display.as_str(),
            Style::default().fg(AURA.text).add_modifier(Modifier::BOLD),
        ));
    } else {
        path_spans.extend(tab_spans(app));
    }
    path_spans.push(Span::styled("   ", Style::default()));
    path_spans.push(Span::styled(
        app.rpc_label(),
        Style::default().fg(match app.rpc_status {
            RpcStatus::Live => AURA.ok,
            RpcStatus::Connecting => AURA.warn,
            RpcStatus::Off => AURA.muted,
        }),
    ));
    if app.unsaved() {
        path_spans.push(Span::styled(
            "  unsaved ",
            Style::default()
                .fg(AURA.canvas)
                .bg(AURA.warn)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.rpc_status == RpcStatus::Off && !app.rpc_disabled {
        path_spans.push(Span::styled(
            "  (mtime fallback)",
            Style::default().fg(AURA.muted),
        ));
    }
    let path = Line::from(path_spans);

    let summary = if let (true, Some(live)) = (app.live_mode(), app.live.as_ref()) {
        let loc = view.as_ref().map_or(0, |v| v.loc);
        let initial = app.initial.len();
        let count = if live.functions.len() == initial {
            format!("{} functions live", live.functions.len())
        } else {
            format!(
                "{} functions live (initial {initial})",
                live.functions.len()
            )
        };
        Line::from(format!(
            " {} · {} LOC · {} · CCN max {} · sum {} · tier {}",
            view.map_or(live.name.as_str(), |v| v.lang.as_str()),
            loc,
            count,
            live.max,
            live.sum,
            live.tier.as_str()
        ))
    } else {
        match view {
            Some(v) => match (v.tier, v.count) {
                (_, 0) if v.file_ccn.is_some() => Line::from(format!(
                    " {} · {} LOC (total {}) · file CCN {} (heuristic)",
                    v.lang,
                    v.loc,
                    v.loc_total,
                    v.file_ccn.unwrap_or_default()
                )),
                (Some(tier), count) => Line::from(format!(
                    " {} · {} LOC (total {}) · {} functions · CCN max {} · p90 {} · sum {} · tier {}",
                    v.lang,
                    v.loc,
                    v.loc_total,
                    count,
                    v.max,
                    v.p90,
                    v.sum,
                    tier.as_str()
                )),
                (None, _) => Line::from(format!(
                    " {} · {} LOC (total {}) · no function CCN ({})",
                    v.lang,
                    v.loc,
                    v.loc_total,
                    v.skip_label()
                )),
            },
            None => Line::from(Span::styled(
                " no scan result yet",
                Style::default().fg(AURA.muted),
            )),
        }
    };
    let summary = summary.style(Style::default().fg(AURA.muted));

    let (grade, grade_label) = app.grade();
    let mut status_spans = vec![Span::styled(
        format!(" grade {grade} "),
        Style::default()
            .fg(grade_color(grade))
            .add_modifier(Modifier::BOLD),
    )];
    status_spans.push(Span::styled(
        format!(
            "· {} · focus: {} · scan {:.1} ms",
            grade_label,
            app.focus.label(),
            app.scan_ms as f64
        ),
        Style::default().fg(AURA.muted),
    ));
    if let Some(delta) = app.delta_sum() {
        status_spans.push(Span::styled(
            format!("  Δ +{delta}"),
            Style::default()
                .fg(if delta > 0 { AURA.warn } else { AURA.ok })
                .add_modifier(Modifier::BOLD),
        ));
    }
    let breadcrumb = if app.live_mode() {
        app.live
            .as_ref()
            .and_then(|live| live.functions.get(app.selected))
            .map(|row| breadcrumb::line(&row.function))
    } else {
        app.view
            .as_ref()
            .and_then(|view| view.functions.get(app.selected))
            .map(breadcrumb::line)
    };
    if let Some(crumb) = breadcrumb {
        status_spans.push(Span::styled(
            format!("  {crumb}"),
            Style::default().fg(AURA.text),
        ));
    }
    if let Some(note) = &app.rpc_note {
        status_spans.push(Span::styled(
            format!("  rpc: {note}"),
            Style::default().fg(AURA.danger),
        ));
    }
    if let Some(note) = &app.exited {
        status_spans.push(Span::styled(
            format!("  [editor exited: {note}]"),
            Style::default().fg(AURA.danger),
        ));
    }
    if let Some(err) = &app.scan_error {
        status_spans.push(Span::styled(
            format!("  scan error: {err}"),
            Style::default().fg(AURA.danger),
        ));
    }

    let content = Paragraph::new(vec![path, summary, Line::from(status_spans)])
        .style(Style::default().bg(AURA.canvas));
    frame.render_widget(content, area);
}

fn render_editor(frame: &mut Frame, app: &App, screen: &Screen, area: Rect) {
    let focused = app.focus == Focus::Editor;
    let border = if focused {
        AURA.border_focus
    } else {
        AURA.border
    };
    let title = Line::from(vec![
        Span::styled(" editor ", Style::default().fg(AURA.accent)),
        Span::styled(
            format!("· {} ", app.editor_label),
            Style::default().fg(AURA.text),
        ),
    ]);
    let hint = if app.exited.is_some() {
        " [editor exited] "
    } else if focused {
        " typing goes to the editor "
    } else {
        " F9 to focus "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(Color::Black))
        .title(title)
        .title_bottom(Line::from(Span::styled(
            hint,
            Style::default().fg(if app.exited.is_some() {
                AURA.danger
            } else {
                AURA.muted
            }),
        )));

    let terminal = PseudoTerminal::new(screen).block(block);
    frame.render_widget(terminal, area);
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let border = if focused {
        AURA.border_focus
    } else {
        AURA.border
    };
    let live = app.live_mode();
    let title = if focused {
        Line::from(vec![
            Span::styled(" functions ", Style::default().fg(AURA.accent)),
            Span::styled(
                if live {
                    "· live · F9 back "
                } else {
                    "· F9 back "
                },
                Style::default().fg(AURA.muted),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" functions ", Style::default().fg(AURA.accent)),
            Span::styled(
                if live { "· live " } else { "" },
                Style::default().fg(AURA.ok),
            ),
        ])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(AURA.card))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let (summary_area, list_area) = sidebar_regions(area);

    let initial = app.initial.len();
    let summary = match (&app.live, &app.view) {
        (Some(live), view) => {
            let mut lines = vec![Line::from(Span::styled(
                format!(
                    " {} · {} LOC",
                    live.name,
                    view.as_ref().map_or(0, |v| v.loc)
                ),
                Style::default().fg(AURA.text).add_modifier(Modifier::BOLD),
            ))];
            let count = if live.functions.len() == initial {
                format!("{} functions live", live.functions.len())
            } else {
                format!(
                    "{} functions live (initial {initial})",
                    live.functions.len()
                )
            };
            lines.push(Line::from(Span::styled(
                format!(
                    " {count} · max {} · sum {} · {:.0} ms",
                    live.max, live.sum, live.read_ms
                ),
                Style::default().fg(AURA.muted),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" tier {} · grade {} · ", live.tier.as_str(), app.grade().0),
                    Style::default().fg(AURA.muted),
                ),
                Span::styled(
                    format!("Δ +{}", live.positive_delta()),
                    Style::default()
                        .fg(if live.positive_delta() > 0 {
                            AURA.warn
                        } else {
                            AURA.ok
                        })
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                if live.unsaved() {
                    " unsaved buffer · +new ~worse -better"
                } else {
                    " saved buffer · +new ~worse -better"
                },
                Style::default().fg(AURA.pill),
            )));
            Paragraph::new(lines).style(Style::default().bg(AURA.card))
        }
        (None, Some(v)) => {
            let mut lines = vec![Line::from(Span::styled(
                format!(" {} · {} LOC", v.name, v.loc),
                Style::default().fg(AURA.text).add_modifier(Modifier::BOLD),
            ))];
            lines.push(Line::from(Span::styled(
                if v.count == 0 {
                    match v.file_ccn {
                        Some(file_ccn) => format!(" file CCN {file_ccn} (heuristic)"),
                        None => " no function CCN".to_string(),
                    }
                } else {
                    format!(" {} functions · max {} · p90 {}", v.count, v.max, v.p90)
                },
                Style::default().fg(AURA.muted),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    " tier {} · grade {}",
                    v.tier.map_or("n/a", crate::ccn::types::CcnTier::as_str),
                    app.grade().0
                ),
                Style::default().fg(AURA.muted),
            )));
            lines.push(Line::from(Span::styled(
                match app.rpc_status {
                    RpcStatus::Connecting => " rpc connecting · initial scan",
                    RpcStatus::Off if app.rpc_disabled => " rpc off · mtime refresh",
                    RpcStatus::Off => " rpc off · mtime refresh",
                    RpcStatus::Live => " live snapshot pending",
                },
                Style::default().fg(AURA.pill),
            )));
            Paragraph::new(lines).style(Style::default().bg(AURA.card))
        }
        (None, None) => Paragraph::new(vec![Line::from(Span::styled(
            match &app.scan_error {
                Some(err) => format!(" scan failed: {err}"),
                None => " no file data".to_string(),
            },
            Style::default().fg(AURA.danger),
        ))])
        .style(Style::default().bg(AURA.card)),
    };
    frame.render_widget(summary, summary_area);

    let live_rows = app.live.as_ref().filter(|_| live);
    let scan_rows = app.view.as_ref().filter(|_| !live);
    let count =
        live_rows.map_or(0, |l| l.functions.len()) + scan_rows.map_or(0, |v| v.functions.len());
    if count == 0 {
        let reason = match (&app.live, &app.view) {
            (Some(_), _) if live => " no functions in the live buffer",
            (_, Some(view)) => view.skip_label(),
            _ => " no functions",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {reason}"),
                Style::default().fg(AURA.muted),
            )))
            .style(Style::default().bg(AURA.card)),
            list_area,
        );
        return;
    }

    let visible = visible_functions(list_area);
    let start = list_offset(app.selected, visible);
    let end = (start + visible).min(count);

    let mut lines: Vec<Line> = Vec::new();
    for index in start..end {
        let selected = index == app.selected;
        let bg = if selected { AURA.pill } else { AURA.card };
        let select = if selected { "▸" } else { " " };
        let (line_no, ccn, name, identity, delta) = if let Some(live) = live_rows {
            match live.functions.get(index) {
                Some(row) => (
                    row.function.line,
                    row.function.ccn,
                    row.function.name.clone(),
                    row.function.identity.clone(),
                    delta_text(row),
                ),
                None => continue,
            }
        } else if let Some(view) = scan_rows {
            match view.functions.get(index) {
                Some(function) => (
                    function.line,
                    function.ccn,
                    function.name.clone(),
                    function.identity.clone(),
                    String::new(),
                ),
                None => continue,
            }
        } else {
            continue;
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{select}{line_no:>4} "),
                Style::default().fg(AURA.muted).bg(bg),
            ),
            Span::styled(
                format!("{ccn:>3} "),
                Style::default()
                    .fg(ccn_color(ccn))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{delta:>4} "),
                Style::default()
                    .fg(delta_color(&delta))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                name,
                Style::default()
                    .fg(if selected { AURA.accent } else { AURA.text })
                    .bg(bg)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("      {identity} "),
            Style::default().fg(AURA.muted).bg(bg),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
        list_area,
    );
}

/// Compact delta column: `+n` new, `~n` worsened, `-n` improved.
fn delta_text(row: &LiveRow) -> String {
    if row.is_new {
        format!("+{}", row.delta.max(0))
    } else if row.delta > 0 {
        format!("~{}", row.delta)
    } else if row.delta < 0 {
        format!("-{}", row.delta.unsigned_abs())
    } else {
        String::new()
    }
}

fn delta_color(delta: &str) -> Color {
    match delta.chars().next() {
        Some('+') => AURA.danger,
        Some('~') => AURA.warn,
        Some('-') => AURA.ok,
        _ => AURA.muted,
    }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let keys = match app.focus {
        Focus::Editor => " F10 dashboard - Alt+W panes - Alt+O symbols - F5 commands - F6 review - F7 diagnostics ",
        Focus::Sidebar => " j/k move - Enter jump - F9 editor - F5 commands - F10 dashboard ",
    };
    let keys = if app.split {
        format!(" Alt+W panes |{keys}")
    } else {
        keys.to_string()
    };
    let status_style = if app.status_is_error() {
        Style::default().fg(AURA.danger)
    } else {
        Style::default().fg(AURA.ok)
    };
    let mut status_spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(app.status.clone(), status_style),
    ];
    if let Some(note) = &app.rpc_note {
        status_spans.push(Span::styled(
            format!("  · rpc: {note}"),
            Style::default().fg(AURA.danger),
        ));
    }
    let content = Paragraph::new(vec![
        Line::from(Span::styled(keys, Style::default().fg(AURA.muted))),
        Line::from(status_spans),
    ])
    .style(Style::default().bg(AURA.canvas));
    frame.render_widget(content, area);
}

fn ccn_color(ccn: u32) -> Color {
    match ccn {
        0..=5 => AURA.ok,
        6..=10 => Color::Rgb(137, 220, 235),
        11..=20 => AURA.warn,
        21..=50 => Color::Rgb(250, 179, 135),
        _ => AURA.danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn layout_renders_header_editor_sidebar_and_footer() {
        let dir = std::env::temp_dir().join(format!("ccn-workbench-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("sample.c");
        std::fs::write(
            &file,
            "int one(int x) { if (x) { return 1; } return 0; }\nint two(int y) {\n    if (y) { return 2; }\n    return 0;\n}\n",
        )
        .expect("write sample");
        let app = App::new(file.clone(), "sample.c".to_string(), "nvim".to_string());

        let mut parser = vt100::Parser::new(24, 60, 0);
        parser.process(b"NVIM-EDITOR-CONTENT");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &app, parser.screen()))
            .expect("draw");
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("ccn workbench"), "{rendered}");
        assert!(rendered.contains("[1 sample.c]"), "{rendered}");
        assert!(rendered.contains("NVIM-EDITOR-CONTENT"), "{rendered}");
        assert!(rendered.contains("functions"), "{rendered}");
        assert!(rendered.contains("F10 dashboard"), "{rendered}");
        assert!(rendered.contains("one(int)"), "{rendered}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn list() -> Rect {
        Rect::new(80, 10, 20, 8)
    }

    #[test]
    fn function_row_at_maps_both_rows_of_the_first_function() {
        assert_eq!(function_row_at(list(), 0, 5, 10), Some(0));
        assert_eq!(function_row_at(list(), 0, 5, 11), Some(0));
        assert_eq!(function_row_at(list(), 0, 5, 12), Some(1));
    }

    #[test]
    fn function_row_at_honors_the_scroll_offset() {
        assert_eq!(function_row_at(list(), 4, 10, 10), Some(4));
        assert_eq!(function_row_at(list(), 4, 10, 13), Some(5));
    }

    #[test]
    fn function_row_at_rejects_rows_past_the_last_function() {
        // Offset 8 with only 9 functions: local rows map to 8 and then 9+
        // (off the end).
        assert_eq!(function_row_at(list(), 8, 9, 12), None);
        assert_eq!(function_row_at(list(), 8, 9, 16), None);
    }

    #[test]
    fn function_row_at_rejects_empty_lists() {
        assert_eq!(function_row_at(list(), 0, 0, 10), None);
    }

    #[test]
    fn function_row_at_ignores_summary_border_and_below_list_clicks() {
        assert_eq!(function_row_at(list(), 0, 5, 9), None);
        assert_eq!(function_row_at(list(), 0, 5, 18), None);
        assert_eq!(function_row_at(Rect::new(80, 10, 20, 0), 0, 5, 10), None);
    }

    #[test]
    fn list_offset_pages_with_the_selection() {
        assert_eq!(list_offset(0, 4), 0);
        assert_eq!(list_offset(3, 4), 0);
        assert_eq!(list_offset(4, 4), 4);
        assert_eq!(list_offset(9, 4), 8);
        assert_eq!(list_offset(9, 0), 9);
    }

    #[test]
    fn visible_functions_uses_paired_rows() {
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 8)), 4);
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 1)), 1);
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 0)), 1);
    }
}
