//! Layout and rendering for the workbench spike.
//!
//! The layout is deliberately boring: header, editor | sidebar, footer. The
//! editor pane is a real vt100 screen rendered by `tui-term`; everything else
//! is plain ratatui.

use ccn::theme::{grade_color, AURA};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use vt100::Screen;

use crate::{App, Focus};

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

pub fn render(frame: &mut Frame, app: &App, screen: &Screen) {
    let area = frame.area();
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

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view.as_ref();
    let path = Line::from(vec![
        Span::styled(
            " ccn workbench",
            Style::default()
                .fg(AURA.accent_alt)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            app.display.as_str(),
            Style::default().fg(AURA.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            app.scan_label(),
            Style::default().fg(if app.scan_error.is_some() {
                AURA.danger
            } else {
                AURA.ok
            }),
        ),
    ]);

    let summary = match view {
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
    let title = if focused {
        Line::from(vec![
            Span::styled(" functions ", Style::default().fg(AURA.accent)),
            Span::styled("· F9 back ", Style::default().fg(AURA.muted)),
        ])
    } else {
        Line::from(Span::styled(
            " functions ",
            Style::default().fg(AURA.accent),
        ))
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

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).areas(inner);

    let summary = match &app.view {
        Some(v) => {
            let mut lines = vec![Line::from(Span::styled(
                format!(" {} · {} LOC", v.name, v.loc,),
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
                    v.tier.map_or("n/a", ccn::ccn::types::CcnTier::as_str),
                    app.grade().0
                ),
                Style::default().fg(AURA.muted),
            )));
            lines.push(Line::from(Span::styled(
                " sorted by CCN · identity below name",
                Style::default().fg(AURA.pill),
            )));
            Paragraph::new(lines).style(Style::default().bg(AURA.card))
        }
        None => Paragraph::new(vec![Line::from(Span::styled(
            match &app.scan_error {
                Some(err) => format!(" scan failed: {err}"),
                None => " no file data".to_string(),
            },
            Style::default().fg(AURA.danger),
        ))])
        .style(Style::default().bg(AURA.card)),
    };
    frame.render_widget(summary, summary_area);

    let functions = app
        .view
        .as_ref()
        .map_or(&[][..], |view| view.functions.as_slice());
    if functions.is_empty() {
        let reason = app
            .view
            .as_ref()
            .map_or(" no functions", |view| view.skip_label());
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

    let rows_per_function = 2u16;
    let visible = (list_area.height / rows_per_function).max(1) as usize;
    let start = (app.selected / visible) * visible;
    let end = (start + visible).min(functions.len());

    let mut lines: Vec<Line> = Vec::new();
    for (index, function) in functions.iter().enumerate().take(end).skip(start) {
        let selected = index == app.selected;
        let bg = if selected { AURA.pill } else { AURA.card };
        let marker = if selected { "▸" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{:>4} ", function.line),
                Style::default().fg(AURA.muted).bg(bg),
            ),
            Span::styled(
                format!("{:>3} ", function.ccn),
                Style::default()
                    .fg(ccn_color(function.ccn))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                function.name.clone(),
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
            format!("      {} ", function.identity),
            Style::default().fg(AURA.muted).bg(bg),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(AURA.card)),
        list_area,
    );
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let keys = match app.focus {
        Focus::Editor => {
            " F10 quit · F9 focus sidebar · typing goes to the editor · r rescans in the sidebar "
        }
        Focus::Sidebar => " F10 quit · F9 back to editor · j/k move · g/G top/bottom · r rescan ",
    };
    let status_style = if app.status_is_error() {
        Style::default().fg(AURA.danger)
    } else {
        Style::default().fg(AURA.ok)
    };
    let content = Paragraph::new(vec![
        Line::from(Span::styled(keys, Style::default().fg(AURA.muted))),
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(app.status.clone(), status_style),
        ]),
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
        let app = App::new(file.clone(), file.display().to_string(), "nvim".to_string());

        let mut parser = vt100::Parser::new(24, 60, 0);
        parser.process(b"NVIM-EDITOR-CONTENT");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &app, parser.screen()))
            .expect("draw");
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("ccn workbench"), "{rendered}");
        assert!(rendered.contains("NVIM-EDITOR-CONTENT"), "{rendered}");
        assert!(rendered.contains("functions"), "{rendered}");
        assert!(rendered.contains("F10 quit"), "{rendered}");
        assert!(rendered.contains("one(int)"), "{rendered}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
