//! Command palette (F5) for the unified TUI.
//!
//! The palette is a small, dependency-free list of app actions. Filtering
//! reuses [`crate::fuzzy::rank`] over the command labels, so typing `diag`
//! narrows the list to `Diagnostics` with the same scoring as file search.
//! The palette owns its query, selection and ranked match indices; rendering
//! is a single centered popup: one border, one query line and one shortcut
//! column, with a `▌` marker on the selected row.
//!
//! The empty-query catalog is deliberately short and named after the outcome
//! the user wants (`Open file`, `Show overview`, ...) rather than after the
//! widget that implements it. Tabs, pane focus and the legacy filter entry stay
//! reachable by typing, but do not crowd the first screen.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::fuzzy::rank;
use crate::theme::palette;

/// Maximum number of command rows drawn at once.
pub const MAX_ROWS: usize = 16;

/// Marker drawn on the selected row instead of a filled background pill.
const SELECTED_MARKER: char = '▌';

/// Action requested by a palette command.
///
/// The caller maps these onto its own key handling; the palette itself never
/// mutates application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    OpenFilePicker,
    SearchWorkspace,
    SearchSymbols,
    ExplainCcn,
    ToggleRail,
    FocusRail,
    FocusEditor,
    ToggleFullscreen,
    ShowOverview,
    ToggleReview,
    ToggleDiagnostics,
    RunBuild,
    RunTest,
    RunLastTask,
    ChooseTask,
    ShowHelp,
    NextTab,
    PrevTab,
    CloseTab,
    Rescan,
    Quit,
    /// Legacy action kept as a compatibility alias for existing key handlers.
    /// It stays out of the empty-query catalog and is only reachable by typing.
    FilterFiles,
    /// Legacy pane-focus action, likewise kept for compatibility.
    FocusFiles,
}

/// One entry in the command list.
#[derive(Debug, Clone)]
pub struct Command {
    /// Stable identifier, useful for tests and key bindings.
    pub id: &'static str,
    /// ASCII label shown in the popup and matched by the fuzzy filter.
    pub label: &'static str,
    /// ASCII shortcut hint shown on the right of the row. May be empty for
    /// actions whose only entry point is this palette.
    pub shortcut: &'static str,
    /// Action emitted when the row is accepted.
    pub action: PaletteAction,
    /// Rows kept out of the empty-query catalog: tab and pane commands that
    /// would crowd the first screen, plus legacy compatibility entries. They
    /// still participate in fuzzy filtering once the user types something.
    pub hidden: bool,
}

/// The command catalog in declaration order.
///
/// Declaration order is the order shown for an empty query, and the first
/// [`MAX_ROWS`] visible entries are the whole first screen. The index into
/// this slice is what [`Palette`] stores as a match.
const COMMANDS: &[Command] = &[
    Command {
        id: "open_file",
        label: "Open file",
        // F5 is the palette itself; `Ctrl+P` belongs to Neovim and must not be
        // advertised here.
        shortcut: "F5",
        action: PaletteAction::OpenFilePicker,
        hidden: false,
    },
    Command {
        id: "run_build",
        label: "Build",
        shortcut: "F5",
        action: PaletteAction::RunBuild,
        hidden: false,
    },
    Command {
        id: "run_test",
        label: "Test",
        shortcut: "F5",
        action: PaletteAction::RunTest,
        hidden: false,
    },
    Command {
        id: "run_last_task",
        label: "Run last task",
        shortcut: "",
        action: PaletteAction::RunLastTask,
        hidden: false,
    },
    Command {
        id: "choose_task",
        label: "Run task...",
        shortcut: "",
        action: PaletteAction::ChooseTask,
        hidden: false,
    },
    Command {
        id: "search_workspace",
        label: "Search workspace",
        shortcut: "Alt+F",
        action: PaletteAction::SearchWorkspace,
        hidden: false,
    },
    Command {
        id: "search_symbols",
        label: "Functions",
        shortcut: "Alt+O",
        action: PaletteAction::SearchSymbols,
        hidden: false,
    },
    Command {
        id: "explain_ccn",
        label: "Explain CCN",
        shortcut: "Alt+E",
        action: PaletteAction::ExplainCcn,
        hidden: false,
    },
    Command {
        id: "show_overview",
        label: "Show overview",
        shortcut: "F10",
        action: PaletteAction::ShowOverview,
        hidden: false,
    },
    Command {
        id: "toggle_rail",
        label: "Toggle rail",
        shortcut: "F9",
        action: PaletteAction::ToggleRail,
        hidden: false,
    },
    Command {
        id: "baseline_review",
        label: "Baseline review",
        shortcut: "F6",
        action: PaletteAction::ToggleReview,
        hidden: false,
    },
    Command {
        id: "diagnostics",
        label: "Diagnostics",
        shortcut: "F7",
        action: PaletteAction::ToggleDiagnostics,
        hidden: false,
    },
    Command {
        id: "toggle_fullscreen",
        label: "Toggle fullscreen",
        shortcut: "Alt+Z",
        action: PaletteAction::ToggleFullscreen,
        hidden: false,
    },
    Command {
        id: "help",
        label: "Help",
        shortcut: "?",
        action: PaletteAction::ShowHelp,
        hidden: false,
    },
    Command {
        id: "rescan",
        label: "Rescan",
        shortcut: "r",
        action: PaletteAction::Rescan,
        hidden: false,
    },
    Command {
        id: "quit",
        label: "Quit",
        shortcut: "q",
        action: PaletteAction::Quit,
        hidden: false,
    },
    // Below-the-fold entries: discoverable by typing, kept out of the first
    // screen so the catalog stays short.
    Command {
        id: "focus_rail",
        label: "Focus rail",
        shortcut: "F9",
        action: PaletteAction::FocusRail,
        hidden: true,
    },
    Command {
        id: "focus_editor",
        label: "Focus editor",
        shortcut: "F8",
        action: PaletteAction::FocusEditor,
        hidden: true,
    },
    Command {
        id: "next_tab",
        label: "Next tab",
        shortcut: "F3",
        action: PaletteAction::NextTab,
        hidden: true,
    },
    Command {
        id: "prev_tab",
        label: "Previous tab",
        shortcut: "F2",
        action: PaletteAction::PrevTab,
        hidden: true,
    },
    Command {
        id: "close_tab",
        label: "Close tab",
        shortcut: "F4",
        action: PaletteAction::CloseTab,
        hidden: true,
    },
    // Compatibility entries: preserved for callers, hidden from the catalog.
    Command {
        id: "focus_files",
        label: "Focus files",
        shortcut: "F8",
        action: PaletteAction::FocusFiles,
        hidden: true,
    },
    Command {
        id: "filter_files",
        label: "Filter files",
        shortcut: "/",
        action: PaletteAction::FilterFiles,
        hidden: true,
    },
];

/// Return a copy of the full command catalog in declaration order.
///
/// This includes hidden compatibility entries so callers and tests can still
/// resolve them; use [`catalog`] for the rows the palette advertises.
pub fn commands() -> Vec<Command> {
    COMMANDS.to_vec()
}

/// The rows shown for an empty query: the advertised catalog, in order.
pub fn catalog() -> Vec<Command> {
    COMMANDS
        .iter()
        .filter(|command| !command.hidden)
        .cloned()
        .collect()
}

/// Popup state: query, selected row and ranked command indices.
///
/// `selected` indexes `matches`, not [`COMMANDS`]; [`Palette::selected`]
/// resolves it to the actual command.
#[derive(Debug, Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    matches: Vec<usize>,
}

impl Palette {
    /// Create a closed palette with an empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the palette should capture input and be drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the palette, reset the query and selection, and show the catalog.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    /// Hide the palette and drop transient query and results.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
        self.matches.clear();
    }

    /// Current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Append a character and re-rank. Selection returns to the best match.
    pub fn type_char(&mut self, ch: char) {
        self.query.push(ch);
        self.refresh();
    }

    /// Remove the last character and re-rank. Selection returns to the best
    /// match.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    /// Move the selection by `delta` rows, clamped to the match list. The
    /// selection does not wrap.
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.matches.len() - 1;
        let current = self.selected.min(last) as isize;
        self.selected = (current + delta).clamp(0, last as isize) as usize;
    }

    /// The highlighted command, or `None` when nothing matches.
    pub fn selected(&self) -> Option<&Command> {
        self.matches
            .get(self.selected)
            .and_then(|&index| COMMANDS.get(index))
    }

    /// Ranked commands for the current query, best first.
    ///
    /// This returns references to the static catalog rather than indices so
    /// callers can read labels and actions directly. The order matches the
    /// popup rows.
    pub fn matches(&self) -> Vec<&Command> {
        self.matches
            .iter()
            .filter_map(|&index| COMMANDS.get(index))
            .collect()
    }

    /// Re-rank the visible catalog, keeping at most [`MAX_ROWS`] entries.
    ///
    /// Hidden compatibility entries only enter the ranking once the query is
    /// non-empty, so they never occupy the first screen but remain reachable
    /// by name.
    fn refresh(&mut self) {
        let include_hidden = !self.query.is_empty();
        let visible: Vec<(usize, &Command)> = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, command)| include_hidden || !command.hidden)
            .collect();
        let labels: Vec<String> = visible
            .iter()
            .map(|(_, command)| command.label.to_string())
            .collect();
        self.matches = rank(&labels, &self.query, MAX_ROWS)
            .into_iter()
            .filter_map(|position| visible.get(position).map(|(index, _)| *index))
            .collect();
        self.selected = 0;
    }
}

/// Draw a centered command popup over `area`.
///
/// The popup is only drawn while the palette is open. It is one bordered card
/// with a query line followed by up to [`MAX_ROWS`] commands, each with a
/// right-aligned shortcut hint and a `▌` marker on the selected row. Areas too
/// small for the popup are ignored rather than panicking.
pub fn render(frame: &mut Frame, area: Rect, state: &Palette) {
    if !state.is_open() || area.width < 12 || area.height < 5 {
        return;
    }

    let row_count = state.matches.len().max(1) as u16;
    let height = (row_count + 3).min(area.height);
    let width = area.width.saturating_sub(4).clamp(12, 76);
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
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    let prompt = format!("> {}", state.query);
    lines.push(Line::from(Span::styled(
        truncate(&prompt, width),
        Style::default().fg(palette().text),
    )));

    if state.matches.is_empty() {
        if lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                "  no matches",
                Style::default().fg(palette().muted),
            )));
        }
    } else {
        for (position, &index) in state.matches.iter().enumerate().take(MAX_ROWS) {
            if lines.len() >= inner.height as usize {
                break;
            }
            let Some(command) = COMMANDS.get(index) else {
                continue;
            };
            let selected = position == state.selected;
            let text = row_text(command.label, command.shortcut, width, selected);
            let style = if selected {
                Style::default()
                    .fg(palette().text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette().text)
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Compose one row: marker and label on the left, shortcut on the right.
fn row_text(label: &str, shortcut: &str, width: usize, selected: bool) -> String {
    let marker = if selected { SELECTED_MARKER } else { ' ' };
    let shortcut_width = shortcut.chars().count();
    let label_space = width.saturating_sub(shortcut_width + 3);
    let prefix = format!("{marker} ");
    let mut text = format!("{prefix}{label}");
    if text.chars().count() > label_space {
        text = truncate(&text, label_space);
    }
    let padding = width.saturating_sub(text.chars().count() + shortcut_width + 1);
    format!("{text}{}{}", " ".repeat(padding), shortcut)
}

/// Cut `text` to `width` characters without splitting a character.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

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

    fn filtered(query: &str) -> Palette {
        let mut palette = Palette::new();
        palette.open();
        for ch in query.chars() {
            palette.type_char(ch);
        }
        palette
    }

    #[test]
    fn open_and_close_reset_transient_state() {
        let mut palette = Palette::new();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());

        palette.open();
        assert!(palette.is_open());
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), catalog().len().min(MAX_ROWS));

        palette.type_char('q');
        assert_eq!(palette.query(), "q");
        palette.close();
        assert!(!palette.is_open());
        assert_eq!(palette.query(), "");
        assert!(palette.selected().is_none());
        assert!(palette.matches().is_empty());

        palette.open();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.selected().unwrap().id, "open_file");
    }

    #[test]
    fn filters_diagnostics_from_diag() {
        let palette = filtered("diag");
        assert_eq!(palette.query(), "diag");
        assert_eq!(palette.matches().len(), 1);
        assert_eq!(palette.selected().unwrap().id, "diagnostics");
        assert_eq!(palette.selected().unwrap().label, "Diagnostics");
    }

    #[test]
    fn every_command_has_a_label_and_shortcut() {
        let all = commands();
        assert!(all.len() >= 12);
        for command in &all {
            assert!(!command.id.is_empty());
            assert!(!command.label.is_empty());
            assert!(command.label.is_ascii());
            assert!(command.shortcut.is_ascii());
        }
        let advertised = catalog();
        let ids: Vec<&str> = advertised.iter().map(|command| command.id).collect();
        assert!(
            !ids.contains(&"filter_files"),
            "compat entry must be hidden"
        );
        assert!(!ids.contains(&"focus_files"), "compat entry must be hidden");
        let shortcuts: Vec<&str> = all.iter().map(|command| command.shortcut).collect();
        assert!(shortcuts.contains(&"F5"));
        assert!(shortcuts.contains(&"F6"));
        assert!(shortcuts.contains(&"F7"));
        assert!(shortcuts.contains(&"F10"));
        assert!(shortcuts.contains(&"?"));
        assert!(shortcuts.contains(&"r"));
        assert!(shortcuts.contains(&"q"));
    }

    #[test]
    fn ctrl_p_is_never_advertised() {
        for command in commands() {
            assert!(
                !command.shortcut.to_lowercase().contains("ctrl+p"),
                "{} advertises Ctrl+P",
                command.id
            );
        }
        let palette = filtered("");
        let text = render_text(&palette, 60, 22);
        assert!(!text.to_lowercase().contains("ctrl+p"), "{text}");
    }

    #[test]
    fn catalog_is_short_and_outcome_named() {
        let ids: Vec<&str> = catalog().iter().map(|command| command.id).collect();
        assert_eq!(
            ids,
            vec![
                "open_file",
                "run_build",
                "run_test",
                "run_last_task",
                "choose_task",
                "search_workspace",
                "search_symbols",
                "explain_ccn",
                "show_overview",
                "toggle_rail",
                "baseline_review",
                "diagnostics",
                "toggle_fullscreen",
                "help",
                "rescan",
                "quit",
            ]
        );
        assert!(catalog().len() >= MAX_ROWS);
    }

    #[test]
    fn catalog_contains_new_actions() {
        // Tab and pane actions sit below the fold but stay in the command list.
        let actions: Vec<PaletteAction> = commands().iter().map(|command| command.action).collect();
        for expected in [
            PaletteAction::OpenFilePicker,
            PaletteAction::RunBuild,
            PaletteAction::RunTest,
            PaletteAction::RunLastTask,
            PaletteAction::ChooseTask,
            PaletteAction::SearchWorkspace,
            PaletteAction::SearchSymbols,
            PaletteAction::ExplainCcn,
            PaletteAction::ToggleRail,
            PaletteAction::FocusRail,
            PaletteAction::FocusEditor,
            PaletteAction::ToggleFullscreen,
            PaletteAction::ShowOverview,
            PaletteAction::ToggleReview,
            PaletteAction::ToggleDiagnostics,
            PaletteAction::ShowHelp,
            PaletteAction::NextTab,
            PaletteAction::PrevTab,
            PaletteAction::CloseTab,
            PaletteAction::Rescan,
            PaletteAction::Quit,
        ] {
            assert!(
                actions.contains(&expected),
                "{expected:?} missing from the catalog"
            );
        }
    }

    #[test]
    fn filtering_finds_new_actions() {
        for (query, id) in [
            ("open", "open_file"),
            ("build", "run_build"),
            ("test", "run_test"),
            ("last", "run_last_task"),
            ("task", "choose_task"),
            ("work", "search_workspace"),
            ("fun", "search_symbols"),
            ("expl", "explain_ccn"),
            ("rail", "toggle_rail"),
            ("focus r", "focus_rail"),
            ("full", "toggle_fullscreen"),
            ("over", "show_overview"),
            ("base", "baseline_review"),
            ("diag", "diagnostics"),
            ("nex", "next_tab"),
            ("prev", "prev_tab"),
            ("clos", "close_tab"),
            ("resc", "rescan"),
            ("qui", "quit"),
        ] {
            let palette = filtered(query);
            let ids: Vec<&str> = palette.matches().iter().map(|command| command.id).collect();
            assert!(ids.contains(&id), "query {query:?} missed {id}: {ids:?}");
        }
    }

    #[test]
    fn hidden_compat_entry_appears_only_while_typing() {
        let empty = filtered("");
        assert!(!empty
            .matches()
            .iter()
            .any(|command| command.id == "filter_files"));

        let typed = filtered("filter");
        assert!(typed
            .matches()
            .iter()
            .any(|command| command.id == "filter_files"));
    }

    #[test]
    fn empty_query_keeps_declaration_order() {
        let palette = filtered("");
        let shown: Vec<&str> = palette.matches().iter().map(|command| command.id).collect();
        let expected: Vec<&str> = catalog()
            .iter()
            .take(MAX_ROWS)
            .map(|command| command.id)
            .collect();
        assert_eq!(shown, expected);
        assert_eq!(palette.selected().unwrap().id, "open_file");

        let mut palette = Palette::new();
        palette.open();
        assert_eq!(palette.matches().len(), expected.len());
    }

    #[test]
    fn selection_clamps_and_never_wraps() {
        let mut palette = filtered("");
        assert_eq!(palette.selected().unwrap().id, "open_file");

        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().id, "run_build");
        palette.move_selection(-99);
        assert_eq!(palette.selected().unwrap().id, "open_file");
        palette.move_selection(999);
        assert_eq!(palette.selected().unwrap().id, "quit");
        palette.move_selection(1);
        assert_eq!(palette.selected().unwrap().id, "quit");

        let mut empty = filtered("zzzz");
        assert!(empty.selected().is_none());
        empty.move_selection(5);
        assert!(empty.selected().is_none());
    }

    #[test]
    fn backspace_restores_broader_matches() {
        let mut palette = filtered("diag");
        assert_eq!(palette.matches().len(), 1);
        palette.backspace();
        palette.backspace();
        palette.backspace();
        palette.backspace();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.matches().len(), catalog().len().min(MAX_ROWS));
    }

    fn render_text(palette: &Palette, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), palette))
            .expect("draw palette");
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn renders_one_flattened_popup_with_shortcuts() {
        let palette = filtered("");
        let text = render_text(&palette, 50, 20);
        assert!(text.contains("Open file"), "{text}");
        assert!(text.contains("F5"), "{text}");
        assert!(text.contains("Diagnostics"), "{text}");
        assert!(text.contains("F7"), "{text}");
        assert!(text.contains("Alt+F"), "{text}");
        assert!(text.contains(SELECTED_MARKER), "{text}");
        assert!(!text.contains("COMMANDS"), "no uppercase eyebrow: {text}");
    }

    #[test]
    fn selected_row_is_marked_not_filled() {
        let palette = filtered("");
        let text = render_text(&palette, 50, 20);
        let selected_line = text
            .lines()
            .find(|line| line.contains("Open file"))
            .expect("open file row");
        assert!(
            selected_line.contains(&format!("{SELECTED_MARKER} Open file")),
            "{text}"
        );
    }

    #[test]
    fn tiny_area_is_ignored_without_panicking() {
        let palette = filtered("");
        for (width, height) in [(1, 1), (11, 4), (12, 5)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test backend");
            terminal
                .draw(|frame| render(frame, frame.area(), &palette))
                .expect("draw tiny palette");
        }
    }

    #[test]
    fn closed_palette_draws_nothing() {
        let palette = Palette::new();
        let text = render_text(&palette, 40, 12);
        assert!(!text.contains("Open file"), "{text}");
    }
}
