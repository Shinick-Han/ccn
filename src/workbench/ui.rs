//! Layout and rendering for the workbench shell.
//!
//! The shell is deliberately *calm*: exactly one header line, one footer line,
//! and an editor with no outer border (the PTY owns every editor cell). The
//! function list is a dense, reusable rail so the unified `tui` can render the
//! same widget inside its own single left rail.

use crate::breadcrumb;
use crate::ccn::types::FunctionCcn;
use crate::theme::{grade_color, is_monochrome, palette};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use vt100::Screen;

use super::{App, Focus, LiveRow, RpcStatus};

/// Rows reserved for the shell chrome. Both are single lines by design.
const HEADER_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;

/// Named regions of the workbench, recomputed on every resize.
pub struct LayoutRects {
    pub header: Rect,
    pub editor: Rect,
    pub sidebar: Rect,
    pub footer: Rect,
}

pub fn layout(area: Rect) -> LayoutRects {
    layout_with_sidebar(area, true)
}

/// Layout the workbench with an optional function sidebar. When the sidebar is
/// hidden the editor gets the full body width, which is the Alt+Z view.
pub fn layout_with_sidebar(area: Rect, show_sidebar: bool) -> LayoutRects {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Min(1),
        Constraint::Length(FOOTER_ROWS),
    ])
    .areas(area);
    if !show_sidebar || body.width < 24 {
        return LayoutRects {
            header,
            editor: body,
            sidebar: Rect::new(0, 0, 0, 0),
            footer,
        };
    }
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

/// The area available to the child process. The editor has no border, so this
/// is the editor rect itself: the PTY gets all of its rows and columns.
pub fn editor_inner(editor: Rect) -> Rect {
    editor
}

/// The rail inside the standalone sidebar: everything right of the one-column
/// divider.
pub(crate) fn sidebar_inner(sidebar: Rect) -> Rect {
    if sidebar.width == 0 {
        return Rect::new(sidebar.x, sidebar.y, 0, 0);
    }
    Rect::new(
        sidebar.x.saturating_add(1),
        sidebar.y,
        sidebar.width.saturating_sub(1),
        sidebar.height,
    )
}

/// Split the standalone sidebar into its rail header row and its list rows.
pub(crate) fn sidebar_regions(sidebar: Rect) -> (Rect, Rect) {
    let inner = sidebar_inner(sidebar);
    if inner.height == 0 {
        return (inner, inner);
    }
    let header = Rect::new(inner.x, inner.y, inner.width, 1);
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    (header, list)
}

/// Height in terminal rows of one rail entry. The rail is dense: one line per
/// function.
pub(crate) const FUNCTION_ROWS: u16 = 1;

/// Number of function entries that fit in the rail list area.
pub(crate) fn visible_functions(list: Rect) -> usize {
    usize::from(list.height / FUNCTION_ROWS).max(1)
}

/// First function index shown for `selected` (page-aligned scrolling).
pub(crate) fn list_offset(selected: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    (selected / visible) * visible
}

/// Map a terminal row inside a rail *list* area to a function index.
///
/// `offset` is the first function index rendered at the top of `list`. Rows
/// outside the list, empty lists, and rows past the last function return `None`.
pub(crate) fn function_row_at(list: Rect, offset: usize, count: usize, row: u16) -> Option<usize> {
    if count == 0 || row < list.y || row >= list.y.saturating_add(list.height) {
        return None;
    }
    let index = offset + usize::from(row - list.y);
    (index < count).then_some(index)
}

/// Map a terminal row inside a whole rail *area* (including its one-line
/// header) to a function index.
///
/// This is the mapping the unified `tui` needs: it passes the rail rect it
/// rendered, and the function sees the same scroll offset as the renderer.
#[allow(dead_code)] // Wired by the unified `tui` single rail.
pub(crate) fn function_rail_row_at(
    area: Rect,
    offset: usize,
    count: usize,
    row: u16,
) -> Option<usize> {
    if count == 0 || area.height <= 1 || area.width == 0 {
        return None;
    }
    let list = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    function_row_at(list, offset, count, row)
}

pub fn render(frame: &mut Frame, app: &App, screen: &Screen) {
    render_area(frame, app, screen, frame.area());
}

/// Render the complete workbench UI inside `area` (a split pane, a test
/// backend, or the full terminal).
pub fn render_area(frame: &mut Frame, app: &App, screen: &Screen, area: Rect) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(palette().canvas)),
        area,
    );
    let rects = layout_with_sidebar(area, app.sidebar);

    render_header(frame, app, rects.header);
    render_editor(frame, screen, rects.editor);
    if app.sidebar && rects.sidebar.width > 0 {
        render_sidebar(frame, app, rects.sidebar);
    }
    render_footer(frame, app, rects.footer);
}

/// The focus marker is a glyph, not a colour, so `NO_COLOR` keeps it visible.
fn focus_marker(focus: Focus) -> &'static str {
    if focus == Focus::Editor {
        "▌"
    } else {
        " "
    }
}

fn selected_function(app: &App) -> Option<&FunctionCcn> {
    if app.live_mode() {
        app.live
            .as_ref()?
            .functions
            .get(app.selected)
            .map(|row| &row.function)
    } else {
        app.view.as_ref()?.functions.get(app.selected)
    }
}

/// Compact tab strip for the one-line header: `[1 a.c] [2 b.c]`, active accent.
fn tab_spans(app: &App) -> Vec<Span<'static>> {
    let active = app.tabs.active_index();
    let mut spans = Vec::new();
    for (index, title) in app.tabs.titles().into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", Style::default().fg(palette().muted)));
        }
        let style = if Some(index) == active {
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().muted)
        };
        spans.push(Span::styled(format!("[{} {}]", index + 1, title), style));
    }
    spans
}

fn header_left(app: &App) -> Line<'static> {
    let marker = if app.focus == Focus::Editor {
        Style::default()
            .fg(palette().accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().muted)
    };
    let mut spans = vec![
        Span::styled(focus_marker(app.focus), marker),
        Span::raw(" "),
    ];

    if app.tabs.len() > 1 {
        spans.extend(tab_spans(app));
    } else {
        spans.push(Span::styled(
            app.display.clone(),
            Style::default()
                .fg(palette().text)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(function) = selected_function(app) {
            spans.push(Span::styled(" › ", Style::default().fg(palette().muted)));
            spans.push(Span::styled(
                breadcrumb::for_function(function).compact(),
                Style::default().fg(palette().accent),
            ));
        }
    }
    Line::from(spans)
}

/// File CCN and grade for the header's right edge.
fn header_ccn(app: &App) -> (String, char) {
    let grade = app.grade().0;
    let value = if app.live_mode() {
        app.live.as_ref().map(|live| live.max)
    } else {
        app.view.as_ref().and_then(|view| {
            if view.count == 0 {
                view.file_ccn
            } else {
                Some(view.max)
            }
        })
    };
    (
        value.map_or_else(|| "-".to_string(), |value| value.to_string()),
        grade,
    )
}

/// New/worsened delta badge: `Δ+4` for new functions, `Δ~4` for worsened ones.
fn delta_badge(app: &App) -> Option<(String, Color)> {
    if !app.live_mode() {
        return None;
    }
    let live = app.live.as_ref()?;
    let mut new = 0i64;
    let mut worse = 0i64;
    for row in &live.functions {
        if row.is_new {
            new += row.delta.max(0);
        } else if row.delta > 0 {
            worse += row.delta;
        }
    }
    match (new, worse) {
        (0, 0) => None,
        (plus, 0) => Some((format!("Δ+{plus}"), palette().danger)),
        (0, tilde) => Some((format!("Δ~{tilde}"), palette().warn)),
        (plus, tilde) => Some((format!("Δ+{plus}~{tilde}"), palette().warn)),
    }
}

/// Degraded RPC/scan state. Returns the text and colour; the text carries a
/// word or `!` so it is never colour-only.
fn degraded_state(app: &App) -> Option<(String, Color)> {
    if app.scan_error.is_some() {
        return Some(("! scan".to_string(), palette().danger));
    }
    if app.exited.is_some() {
        return Some(("! exited".to_string(), palette().danger));
    }
    if app.rpc_note.is_some() && app.rpc_status != RpcStatus::Live {
        return Some(("! rpc".to_string(), palette().danger));
    }
    match app.rpc_status {
        RpcStatus::Connecting => Some(("rpc connecting".to_string(), palette().warn)),
        RpcStatus::Off if app.rpc_disabled => Some(("rpc off".to_string(), palette().muted)),
        RpcStatus::Off => Some(("rpc off".to_string(), palette().danger)),
        RpcStatus::Live => None,
    }
}

fn header_right(app: &App) -> Line<'static> {
    let mut spans = Vec::new();
    let (ccn, grade) = header_ccn(app);
    spans.push(Span::styled(
        format!("CCN {ccn} {grade}"),
        Style::default()
            .fg(grade_color(grade))
            .add_modifier(Modifier::BOLD),
    ));
    if let Some((text, color)) = delta_badge(app) {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    if app.unsaved() {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            "unsaved",
            Style::default()
                .fg(palette().warn)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some((text, color)) = degraded_state(app) {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    Line::from(spans)
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let left = header_left(app).style(Style::default().bg(palette().canvas));
    let right = header_right(app).style(Style::default().bg(palette().canvas));
    render_left_right(frame, area, left, right);
}

fn render_editor(frame: &mut Frame, screen: &Screen, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = Style::default().bg(if is_monochrome(&palette()) {
        Color::Reset
    } else {
        // Keep the editor's true-black background in colour mode; the terminal
        // default is the only usable surface in monochrome.
        Color::Black
    });
    let terminal = PseudoTerminal::new(screen).style(style);
    frame.render_widget(terminal, area);
}

fn render_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let focused = app.focus == Focus::Sidebar;
    let divider = Span::styled(
        "│",
        Style::default().fg(if focused {
            palette().border_focus
        } else {
            palette().border
        }),
    );
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::from(vec![divider.clone()]))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().canvas)),
        Rect::new(area.x, area.y, 1, area.height),
    );
    render_functions_rail(frame, app, sidebar_inner(area), focused);
}

/// Render the dense function rail used both by the standalone sidebar and by
/// the unified `tui` single left rail.
///
/// One line per function: `▌ name  CCN  delta`. The selected row is marked with
/// `▌` plus accent/bold text instead of a filled pill. The header line is
/// `Functions (N)` with the selected signature when it fits; there are no
/// borders and no card fill.
pub(crate) fn render_functions_rail(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let count = app.function_count();
    let mut header = vec![Span::styled(
        format!("Functions ({count})"),
        Style::default()
            .fg(if focused {
                palette().accent
            } else {
                palette().text
            })
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(function) = selected_function(app) {
        header.push(Span::styled(" · ", Style::default().fg(palette().muted)));
        header.push(Span::styled(
            breadcrumb::for_function(function).display(),
            Style::default().fg(palette().muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(header)).style(Style::default().bg(palette().canvas)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    if area.height <= 1 {
        return;
    }
    let list = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    if count == 0 {
        let reason = match (&app.live, &app.view) {
            (Some(_), _) if app.live_mode() => " no functions in the live buffer",
            (_, Some(view)) => view.skip_label(),
            _ => " no functions",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                reason,
                Style::default().fg(palette().muted),
            )))
            .style(Style::default().bg(palette().canvas)),
            list,
        );
        return;
    }

    let visible = visible_functions(list);
    let start = list_offset(app.selected, visible);
    let end = (start + visible).min(count);
    let live = app.live_mode();
    let mut lines: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));

    for index in start..end {
        let (function, delta) = if live {
            let Some(row) = app.live.as_ref().and_then(|live| live.functions.get(index)) else {
                continue;
            };
            (&row.function, delta_text(row))
        } else {
            let Some(function) = app.view.as_ref().and_then(|view| view.functions.get(index))
            else {
                continue;
            };
            (function, String::new())
        };
        let selected = index == app.selected;
        let marker_style = if selected {
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().muted)
        };
        let name_style = if selected {
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().text)
        };
        let name = if function.name.is_empty() {
            function.identity.clone()
        } else {
            function.name.clone()
        };
        let mut spans = vec![
            Span::styled(if selected { "▌ " } else { "  " }, marker_style),
            Span::styled(name, name_style),
            Span::styled(
                format!("  {}", function.ccn),
                Style::default()
                    .fg(ccn_color(function.ccn))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if !delta.is_empty() {
            spans.push(Span::styled(
                format!("  {delta}"),
                Style::default()
                    .fg(delta_color(&delta))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().canvas)),
        list,
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
        Some('+') => palette().danger,
        Some('~') => palette().warn,
        Some('-') => palette().ok,
        _ => palette().muted,
    }
}

fn footer_keys(app: &App) -> &'static str {
    match app.focus {
        Focus::Editor => "F5 commands · F10 overview · ? help",
        Focus::Sidebar => "j/k move · Enter jump · F5 · ?",
    }
}

fn footer_left(app: &App) -> Line<'static> {
    if app.status.is_empty() {
        let hint = match app.focus {
            Focus::Editor => Focus::Editor.label(),
            Focus::Sidebar => "functions",
        };
        return Line::from(Span::styled(hint, Style::default().fg(palette().muted)));
    }
    if app.status_is_error() {
        Line::from(vec![
            Span::styled(
                "! ",
                Style::default()
                    .fg(palette().danger)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.status.clone(), Style::default().fg(palette().danger)),
        ])
    } else {
        Line::from(Span::styled(
            app.status.clone(),
            Style::default().fg(palette().muted),
        ))
    }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let left = footer_left(app).style(Style::default().bg(palette().canvas));
    let keys = Line::from(Span::styled(
        footer_keys(app),
        Style::default().fg(palette().muted),
    ))
    .style(Style::default().bg(palette().canvas));

    // Keep the quiet left side readable at tiny widths: drop the key hints
    // rather than letting them eat the status line.
    if usize::from(area.width) < keys.width().saturating_add(10) {
        frame.render_widget(Paragraph::new(left), area);
        return;
    }
    render_left_right(frame, area, left, keys);
}

/// Render a left-aligned line and a right-aligned line on one row. The right
/// side wins when space is tight, so CCN/unsaved signals survive truncation of
/// a long path.
fn render_left_right(frame: &mut Frame, area: Rect, left: Line<'static>, right: Line<'static>) {
    let right_width = (right.width() as u16).min(area.width);
    let mut left_width = area.width.saturating_sub(right_width).saturating_sub(1);
    if left_width < 3 {
        left_width = 0;
    }
    if left_width == 0 {
        frame.render_widget(Paragraph::new(right), area);
        return;
    }
    let right_area = Rect::new(
        area.x + area.width - right_width,
        area.y,
        right_width,
        area.height,
    );
    frame.render_widget(
        Paragraph::new(left),
        Rect::new(area.x, area.y, left_width, area.height),
    );
    frame.render_widget(Paragraph::new(right), right_area);
}

fn ccn_color(ccn: u32) -> Color {
    let palette = palette();
    if is_monochrome(&palette) {
        return Color::Reset;
    }
    match ccn {
        0..=5 => palette.ok,
        6..=10 => Color::Rgb(137, 220, 235),
        11..=20 => palette.warn,
        21..=50 => Color::Rgb(250, 179, 135),
        _ => palette.danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sample_app(tag: &str) -> (Fixture, App) {
        let dir =
            std::env::temp_dir().join(format!("ccn-workbench-ui-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("sample.c");
        std::fs::write(
            &file,
            "int one(int x) { if (x) { return 1; } return 0; }\nint two(int y) {\n    if (y) { return 2; }\n    return 0;\n}\n",
        )
        .expect("write sample");
        let app = App::new(file, "sample.c".to_string(), "nvim".to_string());
        (Fixture { dir }, app)
    }

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

    fn row_text(buffer: &Buffer, row: u16) -> String {
        let area = buffer.area;
        let mut text = String::new();
        for x in 0..area.width {
            text.push_str(buffer[(x, row)].symbol());
        }
        text
    }

    fn draw(app: &App, width: u16, height: u16) -> Buffer {
        let mut parser = vt100::Parser::new(height, width, 0);
        parser.process(b"NVIM-EDITOR-CONTENT");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app, parser.screen()))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn layout_keeps_header_and_footer_to_one_line() {
        let area = Rect::new(0, 0, 80, 24);
        let rects = layout_with_sidebar(area, true);
        assert_eq!(rects.header, Rect::new(0, 0, 80, 1));
        assert_eq!(rects.footer, Rect::new(0, 23, 80, 1));
        assert_eq!(rects.header.height, 1);
        assert_eq!(rects.footer.height, 1);
        assert_eq!(rects.editor.y, 1);
        assert_eq!(rects.editor.height, 22);
        assert_eq!(rects.editor.width + rects.sidebar.width, 80);
    }

    #[test]
    fn editor_inner_equals_the_body_at_80x24() {
        let area = Rect::new(0, 0, 80, 24);
        let rects = layout_with_sidebar(area, true);
        assert_eq!(editor_inner(rects.editor), rects.editor);
        assert_eq!(rects.editor.height, 22);

        let full = layout_with_sidebar(area, false);
        assert_eq!(editor_inner(full.editor), full.editor);
        assert_eq!(full.editor.width, area.width);
        assert_eq!(full.editor.height, 22);
        assert_eq!(full.sidebar.width, 0);
    }

    #[test]
    fn editor_has_no_outer_border() {
        let (_fixture, app) = sample_app("no-border");
        let buffer = draw(&app, 80, 24);
        let rects = layout_with_sidebar(Rect::new(0, 0, 80, 24), true);
        for (x, y) in [
            (rects.editor.x, rects.editor.y),
            (rects.editor.right().saturating_sub(1), rects.editor.y),
            (rects.editor.x, rects.editor.bottom().saturating_sub(1)),
            (
                rects.editor.right().saturating_sub(1),
                rects.editor.bottom().saturating_sub(1),
            ),
        ] {
            let symbol = buffer[(x, y)].symbol();
            assert!(
                !matches!(symbol, "┌" | "┐" | "└" | "┘" | "─" | "│"),
                "editor corner ({x},{y}) is a border glyph: {symbol:?}"
            );
        }
        let rendered = buffer_text(&buffer);
        assert!(rendered.contains("NVIM-EDITOR-CONTENT"), "{rendered}");
    }

    #[test]
    fn header_is_one_line_and_calm() {
        let (_fixture, app) = sample_app("header");
        let buffer = draw(&app, 100, 30);
        let header = row_text(&buffer, 0);
        assert!(header.contains("sample.c"), "{header}");
        assert!(header.contains("› one"), "{header}");
        assert!(header.contains("CCN"), "{header}");
        assert!(header.starts_with('▌'), "{header:?}");
        for removed in [
            "ccn workbench",
            "LOC",
            "functions live",
            "tier",
            "p90",
            "sum",
        ] {
            assert!(
                !header.contains(removed),
                "header leaked {removed:?}: {header}"
            );
        }
        // Exactly one line: the editor starts on the next row.
        assert!(!row_text(&buffer, 1).contains("CCN"));
    }

    #[test]
    fn footer_is_one_line_with_contextual_keys() {
        let (_fixture, mut app) = sample_app("footer");
        app.status.clear();
        let buffer = draw(&app, 100, 30);
        let footer = row_text(&buffer, 29);
        assert!(
            footer.contains("F5 commands · F10 overview · ? help"),
            "{footer}"
        );
        assert!(!row_text(&buffer, 28).contains("F5 commands"));

        app.focus = Focus::Sidebar;
        let buffer = draw(&app, 100, 30);
        let footer = row_text(&buffer, 29);
        assert!(
            footer.contains("j/k move · Enter jump · F5 · ?"),
            "{footer}"
        );
        assert!(!footer.contains("F10 overview"), "{footer}");
    }

    #[test]
    fn header_shows_compact_tabs_for_multiple_buffers() {
        let (fixture, mut app) = sample_app("tabs");
        let second = fixture.dir.join("other.c");
        std::fs::write(&second, "int three(void) { return 3; }\n").expect("write second");
        app.tabs
            .open_or_select_named(&second, "other.c".to_string());
        let buffer = draw(&app, 100, 30);
        let header = row_text(&buffer, 0);
        assert!(header.contains("[1 sample.c]"), "{header}");
        assert!(header.contains("[2 other.c]"), "{header}");
        assert!(!row_text(&buffer, 1).contains("[1 sample.c]"));
    }

    #[test]
    fn functions_rail_renders_and_maps_rows() {
        let (_fixture, app) = sample_app("rail");
        let area = Rect::new(0, 0, 40, 8);
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("test terminal");
        terminal
            .draw(|frame| render_functions_rail(frame, &app, area, true))
            .expect("draw");
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Functions (2)"), "{rendered}");
        assert!(rendered.contains("one"), "{rendered}");
        assert!(rendered.contains('▌'), "{rendered}");

        assert_eq!(function_rail_row_at(area, 0, 2, 0), None);
        assert_eq!(function_rail_row_at(area, 0, 2, 1), Some(0));
        assert_eq!(function_rail_row_at(area, 0, 2, 2), Some(1));
        assert_eq!(function_rail_row_at(area, 0, 2, 3), None);
        assert_eq!(function_rail_row_at(area, 1, 2, 1), Some(1));
        assert_eq!(function_rail_row_at(area, 1, 2, 2), None);
        assert_eq!(function_rail_row_at(area, 0, 0, 1), None);
    }

    #[test]
    fn tiny_10x4_renders_without_panicking() {
        let (_fixture, app) = sample_app("tiny");
        let area = Rect::new(0, 0, 10, 4);
        let rects = layout_with_sidebar(area, true);
        assert_eq!(rects.header.height, 1);
        assert_eq!(rects.footer.height, 1);
        assert_eq!(rects.editor.height, 2);

        let buffer = draw(&app, 10, 4);
        assert!(
            row_text(&buffer, 0).contains("CCN"),
            "{}",
            row_text(&buffer, 0)
        );
        assert_eq!(buffer.area.height, 4);
    }

    #[test]
    fn focus_marker_is_visible_without_color() {
        let (_fixture, mut app) = sample_app("focus-marker");
        let buffer = draw(&app, 80, 24);
        let header = row_text(&buffer, 0);
        assert!(header.starts_with('▌'), "{header:?}");

        app.focus = Focus::Sidebar;
        let buffer = draw(&app, 80, 24);
        let header = row_text(&buffer, 0);
        assert!(header.starts_with(' '), "{header:?}");
        assert!(!header.contains('▌'), "{header:?}");
    }

    #[test]
    fn sidebar_can_be_hidden_for_the_editor_only_view() {
        let area = Rect::new(0, 0, 120, 30);
        let full = layout_with_sidebar(area, false);
        assert_eq!(full.editor.width, area.width);
        assert_eq!(full.sidebar.width, 0);
        assert_eq!(full.editor.height, full.footer.y - full.editor.y);

        let split = layout_with_sidebar(area, true);
        assert!(split.sidebar.width > 0);
        assert!(split.editor.width < area.width);
    }

    fn list() -> Rect {
        Rect::new(80, 10, 20, 8)
    }

    #[test]
    fn function_row_at_maps_one_row_per_function() {
        assert_eq!(function_row_at(list(), 0, 5, 10), Some(0));
        assert_eq!(function_row_at(list(), 0, 5, 11), Some(1));
        assert_eq!(function_row_at(list(), 0, 5, 12), Some(2));
    }

    #[test]
    fn function_row_at_honors_the_scroll_offset() {
        assert_eq!(function_row_at(list(), 4, 10, 10), Some(4));
        assert_eq!(function_row_at(list(), 4, 10, 13), Some(7));
    }

    #[test]
    fn function_row_at_rejects_rows_past_the_last_function() {
        assert_eq!(function_row_at(list(), 8, 9, 10), Some(8));
        assert_eq!(function_row_at(list(), 8, 9, 11), None);
        assert_eq!(function_row_at(list(), 8, 9, 16), None);
    }

    #[test]
    fn function_row_at_rejects_empty_lists() {
        assert_eq!(function_row_at(list(), 0, 0, 10), None);
    }

    #[test]
    fn function_row_at_ignores_header_and_below_list_clicks() {
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
    fn visible_functions_is_one_row_per_entry() {
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 8)), 8);
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 1)), 1);
        assert_eq!(visible_functions(Rect::new(0, 0, 20, 0)), 1);
    }
}
