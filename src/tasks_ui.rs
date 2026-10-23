//! Task overlay: one bordered card with a header and the captured output.
//!
//! The header is the only chrome: `Build / <command> / running 2.1s / Esc
//! close` while the child runs, and `Build / exit 0 / 1.8s / Esc close` once it
//! has finished. The body is plain output, with diagnostics in
//! the danger colour and the selected diagnostic underlined so the signal
//! survives `NO_COLOR`. Empty and freshly started runs get a single muted
//! placeholder instead of an empty box.
//!
//! The renderer never drives time or state: it only reads [`TaskState`] and
//! reports its body height back for paging. Areas too small for a usable card
//! are ignored rather than squeezed.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tasks::{TaskState, MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES};
use crate::theme::palette;

/// Draw the overlay over `area` when it is open.
pub fn render(frame: &mut Frame, area: Rect, state: &TaskState) {
    if !state.is_open() || area.width < 20 || area.height < 5 {
        return;
    }

    let width = area.width.saturating_sub(4).clamp(20, 100);
    let height = area.height.saturating_sub(2).clamp(5, 40);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette().border_focus));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let header_color = if state.is_running() {
        palette().accent
    } else if state.exit_code() == Some(0) {
        palette().ok
    } else {
        palette().danger
    };
    let header = truncate(&state.header(), width);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            header,
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let body_height = inner.height.saturating_sub(1) as usize;
    if body_height == 0 {
        return;
    }

    // The truncation note keeps its own last row so the user always knows the
    // output is incomplete, even while scrolled up.
    let note = state.truncated() && body_height > 1;
    let rows = if note { body_height - 1 } else { body_height };
    state.set_viewport(rows);

    let mut lines: Vec<Line> = Vec::with_capacity(body_height);
    let output = state.lines();
    if output.is_empty() {
        let placeholder = if state.is_running() {
            format!(
                "Running {} ...",
                state.task().map_or("task", |task| task.command.as_str())
            )
        } else {
            "(no output)".to_string()
        };
        lines.push(Line::from(Span::styled(
            truncate(&placeholder, width),
            Style::default().fg(palette().muted),
        )));
    } else {
        let start = state.scroll().min(state.max_scroll());
        let selected = state.selected_error_line();
        for (index, text) in output.iter().enumerate().skip(start).take(rows) {
            let is_error = state.error_lines().binary_search(&index).is_ok();
            let color = if is_error {
                palette().danger
            } else {
                palette().text
            };
            let mut style = Style::default().fg(color);
            if selected == Some(index) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            } else if is_error {
                style = style.add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(Span::styled(clean_line(text, width), style)));
        }
    }

    if note && lines.len() < body_height {
        lines.push(Line::from(Span::styled(
            truncate(&truncation_note(), width),
            Style::default().fg(palette().muted),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(inner.x, inner.y + 1, inner.width, body_height as u16),
    );
}

fn truncation_note() -> String {
    format!(
        "... output truncated (kept {MAX_OUTPUT_LINES} lines / {} KB)",
        MAX_OUTPUT_BYTES / 1024
    )
}

/// Expand tabs, drop carriage returns and clip to `width` characters. Output
/// comes from arbitrary tools, so this must never split a multi-byte char.
fn clean_line(text: &str, width: usize) -> String {
    let text = text.replace('\t', "    ");
    let text = text.trim_end_matches('\r');
    truncate(text, width)
}

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "~".to_string();
    }
    let mut clipped: String = text.chars().take(width - 1).collect();
    clipped.push('~');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    use crate::tasks::{Task, TaskKind};

    fn task(name: &str, command: &str, kind: TaskKind) -> Task {
        Task {
            name: name.to_string(),
            command: command.to_string(),
            kind,
        }
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_text(state: &TaskState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), state))
            .expect("draw tasks");
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn finished_run_shows_header_and_output() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "cargo build", TaskKind::Build),
            "src/main.rs:3:5: error: mismatched types\nnote: expected u32\n",
            Some(1),
            false,
        );

        let text = render_text(&state, 80, 20);

        assert!(text.contains("Build"), "{text}");
        assert!(text.contains("exit 1"), "{text}");
        assert!(text.contains("1.8s"), "{text}");
        assert!(text.contains("Esc close"), "{text}");
        assert!(text.contains("mismatched types"), "{text}");
        assert!(text.contains("note: expected u32"), "{text}");
    }

    #[test]
    fn running_empty_run_shows_placeholder_and_command() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "cargo build", TaskKind::Build),
            "",
            None,
            true,
        );

        let text = render_text(&state, 80, 20);

        assert!(text.contains("running"), "{text}");
        assert!(text.contains("cargo build"), "{text}");
        assert!(text.contains("Running cargo build"), "{text}");
    }

    #[test]
    fn empty_finished_run_says_no_output() {
        let mut state = TaskState::new();
        state.seed_for_test(task("test", "true", TaskKind::Test), "", Some(0), false);

        let text = render_text(&state, 60, 12);

        assert!(text.contains("Test"), "{text}");
        assert!(text.contains("(no output)"), "{text}");
    }

    #[test]
    fn truncation_note_is_visible() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "cargo build", TaskKind::Build),
            "line\n",
            Some(0),
            false,
        );
        state.mark_truncated_for_test();

        let text = render_text(&state, 80, 12);

        assert!(text.contains("output truncated"), "{text}");
    }

    #[test]
    fn closed_overlay_draws_nothing() {
        let state = TaskState::new();
        let text = render_text(&state, 80, 20);
        assert!(!text.contains("Esc close"), "{text}");
    }

    #[test]
    fn tiny_areas_are_ignored_without_panicking() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "cargo build", TaskKind::Build),
            "x\n",
            Some(0),
            false,
        );

        for (width, height) in [(1, 1), (19, 4), (20, 5), (21, 6)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test backend");
            terminal
                .draw(|frame| render(frame, frame.area(), &state))
                .expect("draw tiny tasks");
        }
    }
}
