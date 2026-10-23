//! Interactive terminal dashboard.
//!
//! The dashboard is a pure view over an already-built [`ScanResult`] and
//! [`Report`]: it never scans, never writes and never reads the clock. Both
//! inputs are borrowed for the lifetime of [`App`], so the event loop only moves
//! an index around.
//!
//! Two things are deliberate:
//!
//! - The file list is built from `ScanResult.files`, **not** from
//!   `Report.top`. The report keeps 20 rows for its table; browsing beyond the
//!   top 20 is the whole point of the dashboard, so the list reuses the same
//!   measurement (function-level CCN, generated/vendored files excluded) and
//!   recomputes each file's risk with the same size-weighted penalty the report
//!   uses.
//! - Rendering is deterministic for a given [`App`] state. There are no
//!   timers, no colours used as the only signal, and no state hidden in the
//!   renderer other than the scroll offset, which is derived from the selection
//!   and the current height.

use std::io::{self, Stdout};
use std::panic::AssertUnwindSafe;
#[cfg(feature = "workbench")]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(feature = "workbench")]
use crate::diagnostics::{Area, Diagnostic, Diagnostics, Filter as DiagFilter, Severity};
#[cfg(feature = "workbench")]
use crate::diff::{diff_lines, hunks, next_hunk, prev_hunk, DiffLine, Hunk};
#[cfg(feature = "workbench")]
use crate::explain::Explanation;
#[cfg(feature = "workbench")]
use crate::fuzzy::FuzzyState;
#[cfg(feature = "workbench")]
use crate::palette::{Palette, PaletteAction};
#[cfg(feature = "workbench")]
use crate::review::ReviewQueue;
#[cfg(feature = "workbench")]
use crate::search::SearchState;
#[cfg(feature = "workbench")]
use crate::symbols::SymbolPalette;
#[cfg(feature = "workbench")]
use crate::tree::{Tree, TreeAction};
use crossterm::cursor::{Hide, Show};
#[cfg(feature = "workbench")]
use crossterm::event::EnableMouseCapture;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers,
};
#[cfg(feature = "workbench")]
use crossterm::event::{MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::tty::IsTty;
use ratatui::backend::CrosstermBackend;
#[cfg(feature = "workbench")]
use ratatui::layout::Alignment;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
#[cfg(feature = "workbench")]
use std::collections::BTreeSet;

use crate::ccn::types::{CcnSummary, CcnTier, FunctionCcn};
use crate::metrics::{penalty, Report};
use crate::model::ScanResult;
use crate::theme::{bin_color, grade_color, palette, risk_color};

/// Below this the dashboard shows a single "too small" line instead of trying
/// to lay out panels it cannot fit.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 8;

/// Below this the split view shows a message instead of squeezing two panes
/// (and the editor's PTY) into a space neither can use.
#[cfg(feature = "workbench")]
const SPLIT_MIN_WIDTH: u16 = 60;
#[cfg(feature = "workbench")]
const SPLIT_MIN_HEIGHT: u16 = 16;

/// Whether the terminal is too small to lay out the split view. Pure so the
/// guard can be tested without a terminal; `split_areas` itself already uses
/// saturating arithmetic and stays safe at any size.
#[cfg(feature = "workbench")]
fn split_too_small(area: Rect) -> bool {
    area.width < SPLIT_MIN_WIDTH || area.height < SPLIT_MIN_HEIGHT
}

/// What the user asked for when leaving the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    /// Close the dashboard and finish the scan pipeline.
    Quit,
    /// Open the workbench on this scan-relative file, then rerun the dashboard.
    Edit(PathBuf),
}

/// Run the dashboard. Returns the user's requested next action on quit.
///
/// Fails when stdout is not a TTY so the caller can fall back to the plain
/// terminal block instead of painting escape codes into a pipe.
pub fn run(result: &ScanResult, report: &Report) -> std::io::Result<TuiAction> {
    if !is_interactive() {
        return Err(io::Error::other(
            "ccn: the interactive dashboard needs a terminal (stdout is not a TTY)",
        ));
    }

    let mut app = App::new(result, report);
    // The guard restores the terminal on every path: normal return, I/O error
    // and panic. `drop(guard)` happens *before* the panic is resumed, so a
    // crashing dashboard never leaves the user's shell in raw mode.
    let outcome = {
        let mut guard = TerminalGuard::enter()?;
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            event_loop(&mut guard.terminal, &mut app)
        }));
        drop(guard);
        outcome
    };

    match outcome {
        Ok(result) => result.map(|()| {
            app.pending_edit
                .take()
                .map_or(TuiAction::Quit, TuiAction::Edit)
        }),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Dashboard navigation state that survives a workbench round trip and rescan.
#[derive(Clone)]
struct DashboardState {
    selected_path: Option<String>,
    sort: SortKey,
    sort_desc: bool,
    filter: Option<String>,
    left_tab: LeftTab,
}

#[cfg(feature = "workbench")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitFocus {
    Files,
    Workbench,
}

#[cfg(feature = "workbench")]
struct DiffPane {
    rows: Vec<DiffLine>,
    selected: usize,
    hunk: Option<Hunk>,
    side_by_side: bool,
    title: String,
}

/// Left dashboard table + right workbench pane. The left pane shrinks first
/// on narrow terminals, leaving at least ~40 columns for the editor.
#[cfg(feature = "workbench")]
fn split_areas(size: Rect, ratio: u16) -> (Rect, Rect, Rect) {
    let max_left = size.width.saturating_sub(31).max(20);
    let left_width =
        ((u32::from(size.width) * u32::from(ratio)) / 100).clamp(20, u32::from(max_left)) as u16;
    let left = Rect::new(size.x, size.y, left_width, size.height);
    let divider = Rect::new(size.x + left_width, size.y, 1, size.height);
    let right = Rect::new(
        size.x + left_width + 1,
        size.y,
        size.width.saturating_sub(left_width + 1),
        size.height,
    );
    (left, divider, right)
}

/// Pane focus toggle. Alt+W is the advertised key because many terminals
/// collapse Ctrl+Shift+<letter> into Ctrl+<letter>; the latter would steal a
/// Neovim binding. Ctrl+Shift+E and F8 stay as compatibility aliases.
#[cfg(feature = "workbench")]
fn is_pane_toggle(key: &KeyEvent) -> bool {
    let alt_w =
        key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char('w' | 'W'));
    let ctrl_shift_e = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char('e' | 'E'));
    alt_w || ctrl_shift_e
}
/// Convert a mouse x position into a clamped split percentage.
#[cfg(feature = "workbench")]
fn split_ratio_from_x(x: u16, width: u16) -> u16 {
    let width = u32::from(width.max(1));
    ((u32::from(x) * 100) / width).clamp(20, 70) as u16
}

/// Files shown in the split view's tree: every measured source file, not only
/// the ones that received a CCN pass.
#[cfg(feature = "workbench")]
fn tree_paths(result: &ScanResult) -> Vec<String> {
    result
        .files
        .iter()
        .filter(|file| !file.is_generated && !file.is_vendored)
        .map(|file| file.path.replace('\\', "/"))
        .collect()
}

/// Convert an absolute tab path back to the scan-relative, `/`-separated form
/// the session file stores. Paths outside the scan root are not persisted.
#[cfg(feature = "workbench")]
fn relative_to_root(root: &str, path: &Path) -> Option<String> {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let relative = path
        .strip_prefix(&canonical_root)
        .or_else(|_| path.strip_prefix(root))
        .ok()?;
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() || text == "." {
        None
    } else {
        Some(text)
    }
}

/// Persist split ratio, dashboard state and open buffers. Errors are non-fatal:
/// a read-only cache must never take the TUI down.
#[cfg(feature = "workbench")]
fn save_workspace(
    root: &str,
    split_ratio: u16,
    state: &Option<DashboardState>,
    session: Option<&crate::workbench::WorkbenchSession>,
    reviewed: &BTreeSet<String>,
    tree: Option<&Tree>,
) {
    let mut saved = crate::session::Session {
        split_ratio,
        ..crate::session::Session::default()
    };
    if let Some(state) = state {
        saved.selected_path = state.selected_path.clone();
        saved.sort = state.sort.persisted().to_string();
        saved.sort_desc = state.sort_desc;
        saved.filter = state.filter.clone();
        saved.left_tab = state.left_tab.persisted().to_string();
    }
    saved.reviewed = reviewed.iter().cloned().collect();
    if let Some(tree) = tree {
        saved.tree_expanded = tree.expanded_paths();
    }
    if let Some(session) = session {
        for path in session.tabs().paths() {
            if let Some(relative) = relative_to_root(root, &path) {
                saved.open_files.push(relative);
            }
        }
        saved.active_file = session
            .active_tab()
            .and_then(|tab| relative_to_root(root, &tab.path));
    }
    let _ = crate::session::save(root, &saved);
}

#[cfg(feature = "workbench")]
fn build_diagnostics(
    result: &ScanResult,
    report: &Report,
    session: &crate::workbench::WorkbenchSession,
) -> Diagnostics {
    let mut diagnostics = Diagnostics::from_scan(result, report.baseline.as_ref());
    let app = &session.app;
    if let Some(note) = &app.rpc_note {
        diagnostics.push(Diagnostic {
            area: Area::Editor,
            severity: Severity::Warn,
            path: relative_to_root(&result.root, &app.target),
            line: None,
            code: "rpc-note".to_string(),
            message: note.clone(),
        });
    }
    if let Some(error) = &app.scan_error {
        diagnostics.push(Diagnostic {
            area: Area::Editor,
            severity: Severity::Error,
            path: relative_to_root(&result.root, &app.target),
            line: None,
            code: "scan-error".to_string(),
            message: error.clone(),
        });
    }
    if let Some(note) = &app.exited {
        diagnostics.push(Diagnostic {
            area: Area::Editor,
            severity: Severity::Warn,
            path: relative_to_root(&result.root, &app.target),
            line: None,
            code: "editor-exited".to_string(),
            message: note.clone(),
        });
    }
    diagnostics
}

#[cfg(feature = "workbench")]
fn baseline_text(root: &str, rev: &str, path: &str) -> Result<String, String> {
    let root_path = Path::new(root);
    let backend =
        crate::vcs::detect(root_path).ok_or_else(|| "no git or Perforce workspace".to_string())?;
    match crate::vcs::file_at(root_path, backend, rev, path) {
        crate::vcs::FileAt::Found(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        crate::vcs::FileAt::Absent => Ok(String::new()),
        crate::vcs::FileAt::Unmapped => Err("path is outside the depot/client view".to_string()),
        crate::vcs::FileAt::Unknown => Err("client view is incomplete".to_string()),
        crate::vcs::FileAt::Unavailable => Err("baseline content is unavailable".to_string()),
    }
}

#[cfg(feature = "workbench")]
fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[cfg(feature = "workbench")]
fn build_diff_pane(
    root: &str,
    rev: &str,
    change: &crate::baseline::Change,
    session: &crate::workbench::WorkbenchSession,
) -> Result<DiffPane, String> {
    let old_text = baseline_text(root, rev, &change.path)?;
    let active = relative_to_root(root, &session.app.target);
    let current_text = if active.as_deref() == Some(change.path.as_str()) {
        session.app.live.as_ref().map(|live| live.lines.join("\n"))
    } else {
        None
    }
    .or_else(|| std::fs::read_to_string(Path::new(root).join(&change.path)).ok())
    .unwrap_or_default();
    let rows = diff_lines(&split_lines(&old_text), &split_lines(&current_text));
    Ok(DiffPane {
        rows,
        selected: 0,
        hunk: None,
        side_by_side: false,
        title: format!("{} - {} @ {}", change.name, change.path, rev),
    })
}

#[cfg(feature = "workbench")]
fn build_explanation(session: &crate::workbench::WorkbenchSession) -> Option<Explanation> {
    let (function, source) = if session.app.live_mode() {
        let live = session.app.live.as_ref()?;
        let function = live.functions.get(session.app.selected)?.function.clone();
        let source = live.lines.join("\n");
        (function, source)
    } else {
        let view = session.app.view.as_ref()?;
        let function = view.functions.get(session.app.selected)?.clone();
        let source = std::fs::read_to_string(&session.app.target).unwrap_or_default();
        (function, source)
    };
    Some(crate::explain::explain(
        &source,
        function.line,
        function.end_line,
        &function.name,
        function.ccn,
        function.max_nesting,
    ))
}

/// Run the dashboard and workbench in one terminal and one event loop.
///
/// The dashboard is rebuilt after a workbench session so the scan/report and
/// the final gates see files edited in the editor. The terminal guard lives for
/// the whole loop; `e` switches views without leaving the alternate screen.
pub fn run_unified(
    result: &mut ScanResult,
    report: &mut Report,
    options: &crate::loc::ScanOptions,
    hotspots: bool,
    history_limit: u32,
    editor: &str,
) -> io::Result<()> {
    if !is_interactive() {
        return Err(io::Error::other(
            "ccn: the interactive dashboard needs a terminal (stdout is not a TTY)",
        ));
    }
    #[cfg(not(feature = "workbench"))]
    let _ = (options, hotspots, history_limit, editor);

    let mut guard = TerminalGuard::enter()?;
    #[cfg(feature = "workbench")]
    let mut session: Option<crate::workbench::WorkbenchSession> = None;
    #[cfg(feature = "workbench")]
    let saved = crate::session::load(&result.root);
    #[cfg(feature = "workbench")]
    let mut split_ratio: u16 = saved.split_ratio;
    #[cfg(feature = "workbench")]
    let mut reviewed_keys: BTreeSet<String> = saved.reviewed.iter().cloned().collect();
    #[cfg(feature = "workbench")]
    let mut fuzzy = FuzzyState::new();
    #[cfg(feature = "workbench")]
    let mut palette = Palette::new();
    #[cfg(feature = "workbench")]
    let mut diagnostics: Option<(Diagnostics, usize)> = None;
    #[cfg(feature = "workbench")]
    let mut diag_filter = DiagFilter::default();
    #[cfg(feature = "workbench")]
    let mut diff: Option<DiffPane> = None;
    #[cfg(feature = "workbench")]
    let mut explanation: Option<Explanation> = None;
    #[cfg(feature = "workbench")]
    let mut review: Option<ReviewQueue> = None;
    #[cfg(feature = "workbench")]
    let mut symbols = SymbolPalette::new();
    #[cfg(feature = "workbench")]
    let mut search = SearchState::new();
    #[cfg(feature = "workbench")]
    let mut review_current_only = false;
    #[cfg(feature = "workbench")]
    let mut tree_state: Option<Tree> = None;
    #[cfg(feature = "workbench")]
    let mut dashboard_state: Option<DashboardState> = Some(DashboardState {
        selected_path: saved.selected_path.clone(),
        sort: SortKey::from_persisted(&saved.sort),
        sort_desc: saved.sort_desc,
        filter: saved.filter.clone(),
        left_tab: LeftTab::from_persisted(&saved.left_tab),
    });
    #[cfg(not(feature = "workbench"))]
    let mut dashboard_state: Option<DashboardState> = None;
    #[cfg(feature = "workbench")]
    let vcs_identity = crate::vcs::detect(Path::new(&result.root)).map(|backend| {
        match backend {
            crate::vcs::Backend::Git => crate::status::identity_line(&crate::vcs::identity(
                Path::new(&result.root),
                backend,
            )),
            // P4 identity can invoke two bounded network commands; keep the
            // dashboard startup path fast and show the backend only.
            crate::vcs::Backend::P4 => "p4".to_string(),
        }
    });
    // No session file yet means this is the first interactive run for this
    // root. Computed once: exiting saves a session, so later App instances in
    // the same process keep showing the hint until the run ends.
    let first_run = !crate::session::exists(&result.root);

    loop {
        let (action, state) = {
            let mut app = App::new(result, report);
            app.first_run = first_run;
            if let Some(state) = &dashboard_state {
                app.restore_state(state);
            }
            #[cfg(feature = "workbench")]
            app.set_vcs_identity(vcs_identity.clone());
            #[cfg(feature = "workbench")]
            let fuzzy_paths = tree_paths(result);
            loop {
                #[cfg(feature = "workbench")]
                if let Some(session) = session.as_mut() {
                    session.tick();
                }
                guard.terminal.draw(|frame| {
                    render(frame, &mut app);
                    #[cfg(feature = "workbench")]
                    if fuzzy.is_open() {
                        crate::fuzzy::render(frame, frame.area(), &fuzzy, &fuzzy_paths);
                    }
                })?;
                if app.quit {
                    let action = app
                        .pending_edit
                        .take()
                        .map_or(TuiAction::Quit, TuiAction::Edit);
                    break (action, app.capture_state());
                }
                if event::poll(Duration::from_millis(50))? {
                    let event = event::read()?;
                    if let Event::Paste(text) = &event {
                        #[cfg(feature = "workbench")]
                        if fuzzy.is_open() {
                            for ch in text.chars().filter(|ch| *ch != '\r' && *ch != '\n') {
                                fuzzy.type_char(ch, &fuzzy_paths);
                            }
                        } else {
                            app.paste_filter(text);
                        }
                        #[cfg(not(feature = "workbench"))]
                        app.paste_filter(text);
                        continue;
                    }
                    if let Event::Key(key) = event {
                        #[cfg(feature = "workbench")]
                        if fuzzy.is_open() {
                            match key.code {
                                KeyCode::Esc => fuzzy.close(),
                                KeyCode::Up => fuzzy.move_selection(-1),
                                KeyCode::Down => fuzzy.move_selection(1),
                                KeyCode::Enter => {
                                    if let Some(index) = fuzzy.selected() {
                                        if let Some(path) = fuzzy_paths.get(index) {
                                            app.pending_edit = Some(PathBuf::from(path));
                                            app.quit = true;
                                        }
                                    }
                                    fuzzy.close();
                                }
                                KeyCode::Backspace => fuzzy.backspace(&fuzzy_paths),
                                KeyCode::Char(ch)
                                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    fuzzy.type_char(ch, &fuzzy_paths)
                                }
                                _ => {}
                            }
                            continue;
                        }
                        #[cfg(feature = "workbench")]
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('p' | 'P'))
                        {
                            fuzzy.open(&fuzzy_paths);
                            continue;
                        }
                        #[cfg(feature = "workbench")]
                        {
                            let ctrl_c = key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL);
                            let destructive = key.code == KeyCode::Char('q')
                                || key.code == KeyCode::Esc
                                || ctrl_c;
                            let blocked_by_overlay =
                                app.overlay != Overlay::None || app.filter_input.is_some();
                            // Ctrl+C is always destructive; q/Esc only when no
                            // overlay or filter owns that key.
                            if session.is_some() && destructive && (ctrl_c || !blocked_by_overlay) {
                                if app.notice.is_some() && destructive {
                                    // Second destructive key: explicit discard.
                                } else if app.notice.is_none() {
                                    app.notice = Some(
                                        "live editor session: save unsaved buffers, then press q or Ctrl+C again to exit"
                                            .to_string(),
                                    );
                                    continue;
                                } else {
                                    continue;
                                }
                            }
                            if app.notice.is_some()
                                && key.code != KeyCode::Char('q')
                                && !destructive
                            {
                                app.notice = None;
                            }
                        }
                        app.handle_key(key);
                    }
                }
            }
        };
        dashboard_state = Some(state);

        match action {
            TuiAction::Quit => {
                #[cfg(feature = "workbench")]
                {
                    save_workspace(
                        &result.root,
                        split_ratio,
                        &dashboard_state,
                        session.as_ref(),
                        &reviewed_keys,
                        tree_state.as_ref(),
                    );
                    if let Some(session) = session.take() {
                        session.close();
                    }
                }
                return Ok(());
            }
            TuiAction::Edit(path) => {
                #[cfg(feature = "workbench")]
                {
                    let target = Path::new(&result.root).join(&path);
                    let target = std::fs::canonicalize(&target).unwrap_or(target);
                    let (restore_files, restore_active) = crate::session::restore_order(&saved);
                    let restore_active_path = restore_active.as_ref().map(|relative| {
                        let joined = Path::new(&result.root).join(relative);
                        std::fs::canonicalize(&joined).unwrap_or(joined)
                    });
                    let target = if session.is_none() {
                        restore_active_path.clone().unwrap_or(target)
                    } else {
                        target
                    };
                    let needs_restart = session
                        .as_ref()
                        .is_none_or(|session| session.app.exited.is_some());
                    let mut open_error = None;
                    if needs_restart {
                        if let Some(old) = session.take() {
                            old.close();
                        }
                    } else if session.as_ref().is_some_and(|s| s.app.target != target) {
                        if let Some(session) = session.as_mut() {
                            if let Err(e) = session.open_file(&target) {
                                open_error = Some(e);
                            }
                        }
                    }
                    if let Some(error) = &open_error {
                        eprintln!("ccn: {error}; keeping the current editor session");
                    }
                    if needs_restart {
                        if let Some(old) = session.take() {
                            old.close();
                        }
                        let size = guard.terminal.size()?;
                        let area = Rect::new(0, 0, size.width, size.height);
                        let (_, _, right) = split_areas(area, split_ratio);
                        match crate::workbench::WorkbenchSession::open(
                            &target,
                            editor,
                            false,
                            right.width,
                            right.height,
                        ) {
                            Ok(open) => {
                                session = Some(open);
                                if let Some(open) = session.as_mut() {
                                    let restored: Vec<(PathBuf, String)> = restore_files
                                        .iter()
                                        .map(|relative| {
                                            let joined = Path::new(&result.root).join(relative);
                                            let path =
                                                std::fs::canonicalize(&joined).unwrap_or(joined);
                                            (path, relative.clone())
                                        })
                                        .collect();
                                    if let Some(active) = restore_active_path.as_ref() {
                                        open.restore_ordered(&restored, active);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("ccn: {e}");
                                continue;
                            }
                        }
                    }

                    if let Some(session) = session.as_mut() {
                        let changes = crate::status::changes(Path::new(&result.root));
                        let fresh_tree = tree_state.is_none();
                        let mut tree = match tree_state.take() {
                            Some(mut tree) => {
                                tree.rebuild(tree_paths(result));
                                tree
                            }
                            None => Tree::new(tree_paths(result)),
                        };
                        if fresh_tree && !saved.tree_expanded.is_empty() {
                            tree.restore_expanded(&saved.tree_expanded);
                        }
                        tree.set_changes(&changes);
                        #[cfg(feature = "workbench")]
                        let fuzzy_paths = tree_paths(result);
                        let mut focus = SplitFocus::Workbench;
                        let mut dragging = false;
                        let mut last_right = (0u16, 0u16);
                        guard.set_mouse(true)?;
                        loop {
                            session.tick();
                            session.app.split = true;
                            let size = guard.terminal.size()?;
                            let area = Rect::new(0, 0, size.width, size.height);
                            if split_too_small(area) {
                                guard.terminal.draw(|frame| {
                                    frame.render_widget(Clear, area);
                                    frame.render_widget(
                                        Paragraph::new(vec![
                                            Line::from(Span::styled(
                                                "terminal too small for split (need at least 60x16)",
                                                Style::default().fg(crate::theme::palette().muted),
                                            )),
                                            Line::from(Span::styled(
                                                "F10 returns to dashboard",
                                                Style::default().fg(crate::theme::palette().muted),
                                            )),
                                        ])
                                        .alignment(Alignment::Center),
                                        area,
                                    );
                                })?;
                                // Keep the editor session alive: only the draw
                                // is skipped so growing the terminal resumes
                                // the split with its buffers and tabs intact.
                                if !event::poll(Duration::from_millis(16))? {
                                    continue;
                                }
                                match event::read()? {
                                    Event::Key(key)
                                        if key.kind != KeyEventKind::Release
                                            && key.code == KeyCode::F(10) =>
                                    {
                                        break;
                                    }
                                    Event::Resize(_, _) => {}
                                    _ => {}
                                }
                                continue;
                            }
                            let (left, divider, right) = split_areas(area, split_ratio);
                            if last_right != (right.width, right.height) {
                                session.resize(right.width, right.height);
                                last_right = (right.width, right.height);
                            }
                            guard.terminal.draw(|frame| {
                                tree.render(frame, left, focus == SplitFocus::Files);
                                session.draw_in(frame, right);
                                let mut handle = String::new();
                                for _ in 0..divider.height {
                                    handle.push('│');
                                    handle.push('\n');
                                }
                                frame.render_widget(
                                    Paragraph::new(handle).style(Style::default().fg(
                                        if dragging {
                                            crate::theme::palette().accent
                                        } else {
                                            crate::theme::palette().border
                                        },
                                    )),
                                    divider,
                                );
                                if fuzzy.is_open() {
                                    crate::fuzzy::render(frame, frame.area(), &fuzzy, &fuzzy_paths);
                                }
                                if let (Some(queue), Some(base)) =
                                    (&review, report.baseline.as_ref())
                                {
                                    frame.render_widget(Clear, right);
                                    let title = queue.path_filter().map_or_else(
                                        || format!("{} - all files", base.rev),
                                        |path| format!("{} - {path}", base.rev),
                                    );
                                    crate::workbench::review_ui::render_filtered_with_reviewed(
                                        frame,
                                        right,
                                        queue.changes(),
                                        queue.selected(),
                                        true,
                                        &title,
                                        base.threshold,
                                        queue.kind_filter(),
                                        queue.reviewed_keys(),
                                    );
                                }
                                if let Some((items, selected)) = &diagnostics {
                                    frame.render_widget(Clear, right);
                                    crate::workbench::diagnostics_ui::render_filtered(
                                        frame,
                                        right,
                                        items,
                                        *selected,
                                        true,
                                        diag_filter,
                                    );
                                }
                                if let Some(pane) = &diff {
                                    frame.render_widget(Clear, right);
                                    if pane.side_by_side {
                                        crate::workbench::diff_ui::render_side_by_side(
                                            frame,
                                            right,
                                            &pane.rows,
                                            pane.selected,
                                            true,
                                            &pane.title,
                                            pane.hunk,
                                        );
                                    } else {
                                        crate::workbench::diff_ui::render_with_hunks(
                                            frame,
                                            right,
                                            &pane.rows,
                                            pane.selected,
                                            true,
                                            &pane.title,
                                            pane.hunk,
                                        );
                                    }
                                }
                                if symbols.is_open() {
                                    crate::symbols::render(frame, frame.area(), &symbols);
                                }
                                if let Some(explanation) = &explanation {
                                    frame.render_widget(Clear, right);
                                    crate::workbench::explain_ui::render(
                                        frame,
                                        right,
                                        explanation,
                                        true,
                                    );
                                }
                                if search.is_open() {
                                    crate::search::render(frame, frame.area(), &search);
                                }
                                crate::palette::render(frame, frame.area(), &palette);
                            })?;
                            if !event::poll(Duration::from_millis(16))? {
                                continue;
                            }
                            match event::read()? {
                                Event::Paste(text) => {
                                    if fuzzy.is_open() {
                                        for ch in
                                            text.chars().filter(|ch| *ch != '\r' && *ch != '\n')
                                        {
                                            fuzzy.type_char(ch, &fuzzy_paths);
                                        }
                                    } else if focus == SplitFocus::Workbench {
                                        session.paste(&text);
                                    }
                                }
                                Event::Key(key) if key.kind != KeyEventKind::Release => {
                                    if palette.is_open() {
                                        match key.code {
                                            KeyCode::Esc | KeyCode::F(5) => palette.close(),
                                            KeyCode::Up => palette.move_selection(-1),
                                            KeyCode::Down => palette.move_selection(1),
                                            KeyCode::Backspace => palette.backspace(),
                                            KeyCode::Enter => {
                                                let action = palette.selected().map(|c| c.action);
                                                palette.close();
                                                if let Some(action) = action {
                                                    match action {
                                                        PaletteAction::OpenFilePicker => {
                                                            fuzzy.open(&fuzzy_paths)
                                                        }
                                                        PaletteAction::ToggleReview => {
                                                            if let Some(base) =
                                                                report.baseline.as_ref()
                                                            {
                                                                let active = relative_to_root(
                                                                    &result.root,
                                                                    &session.app.target,
                                                                );
                                                                let filter = if review_current_only {
                                                                    active.as_deref()
                                                                } else {
                                                                    None
                                                                };
                                                                let mut queue =
                                                                    ReviewQueue::new(
                                                                        &base.changes,
                                                                        filter,
                                                                    );
                                                                queue.apply_reviewed(
                                                                    reviewed_keys.iter().cloned(),
                                                                );
                                                                review = Some(queue);
                                                                diagnostics = None;
                                                                fuzzy.close();
                                                            } else {
                                                                session.note(
                                                                    "no baseline: run with --baseline <rev>",
                                                                );
                                                            }
                                                        }
                                                        PaletteAction::ToggleDiagnostics => {
                                                            diagnostics = Some((
                                                                build_diagnostics(
                                                                    result, report, session,
                                                                ),
                                                                0,
                                                            ));
                                                            diag_filter = DiagFilter::default();
                                                            review = None;
                                                            fuzzy.close();
                                                        }
                                                        PaletteAction::ShowHelp => session.note(
                                                            "F6 review - F7 diagnostics - Ctrl+Shift+P commands - F8 panes",
                                                        ),
                                                        PaletteAction::NextTab => {
                                                            if let Err(error) = session.next_tab() {
                                                                session.note(error);
                                                            }
                                                        }
                                                        PaletteAction::PrevTab => {
                                                            if let Err(error) = session.prev_tab() {
                                                                session.note(error);
                                                            }
                                                        }
                                                        PaletteAction::CloseTab => {
                                                            if let Err(error) = session.close_tab() {
                                                                session.note(error);
                                                            }
                                                        }
                                                        PaletteAction::FocusFiles => {
                                                            focus = SplitFocus::Files
                                                        }
                                                        PaletteAction::FocusEditor => {
                                                            focus = SplitFocus::Workbench
                                                        }
                                                        PaletteAction::FilterFiles => {
                                                            focus = SplitFocus::Files;
                                                            session.note(
                                                                "split filter is Ctrl+P; / filters the dashboard table",
                                                            );
                                                        }
                                                        PaletteAction::Rescan => session.rescan(),
                                                        PaletteAction::Quit => break,
                                                    }
                                                }
                                            }
                                            KeyCode::Char(ch)
                                                if !key
                                                    .modifiers
                                                    .contains(KeyModifiers::CONTROL) =>
                                            {
                                                palette.type_char(ch)
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if symbols.is_open() {
                                        match key.code {
                                            KeyCode::Esc => symbols.close(),
                                            KeyCode::Up => symbols.move_selection(-1),
                                            KeyCode::Down => symbols.move_selection(1),
                                            KeyCode::PageUp => symbols.move_selection(-10),
                                            KeyCode::PageDown => symbols.move_selection(10),
                                            KeyCode::Backspace => symbols.backspace(),
                                            KeyCode::Enter => {
                                                if let Some(function) = symbols.selected() {
                                                    let line = function.line;
                                                    symbols.close();
                                                    session.jump_line(line);
                                                }
                                            }
                                            KeyCode::Char(ch)
                                                if !key
                                                    .modifiers
                                                    .contains(KeyModifiers::CONTROL) =>
                                            {
                                                symbols.type_char(ch)
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if key.modifiers.contains(KeyModifiers::ALT)
                                        && matches!(key.code, KeyCode::Char('o' | 'O'))
                                    {
                                        symbols.open(&session.functions());
                                        review = None;
                                        diff = None;
                                        diagnostics = None;
                                        fuzzy.close();
                                        continue;
                                    }
                                    if key.code == KeyCode::F(5)
                                        || (key.modifiers.contains(KeyModifiers::CONTROL)
                                            && key.modifiers.contains(KeyModifiers::SHIFT)
                                            && matches!(key.code, KeyCode::Char('p' | 'P')))
                                    {
                                        palette.open();
                                        continue;
                                    }
                                    if explanation.is_some() {
                                        if key.code == KeyCode::Esc
                                            || (key.modifiers.contains(KeyModifiers::ALT)
                                                && matches!(key.code, KeyCode::Char('e' | 'E')))
                                        {
                                            explanation = None;
                                        }
                                        continue;
                                    }
                                    if key.modifiers.contains(KeyModifiers::ALT)
                                        && matches!(key.code, KeyCode::Char('e' | 'E'))
                                    {
                                        explanation = build_explanation(session);
                                        review = None;
                                        diff = None;
                                        diagnostics = None;
                                        fuzzy.close();
                                        continue;
                                    }
                                    if search.is_open() {
                                        match key.code {
                                            KeyCode::Esc => search.close(),
                                            KeyCode::Up => search.move_selection(-1),
                                            KeyCode::Down => search.move_selection(1),
                                            KeyCode::Backspace => {
                                                search.backspace(&result.root, &fuzzy_paths)
                                            }
                                            KeyCode::Enter => {
                                                if let Some(item) = search.selected().cloned() {
                                                    search.close();
                                                    let target =
                                                        Path::new(&result.root).join(&item.path);
                                                    if let Err(error) = session.open_file(&target) {
                                                        session.note(error);
                                                    } else {
                                                        session.jump_line(item.line);
                                                    }
                                                }
                                            }
                                            KeyCode::Char(ch)
                                                if !key
                                                    .modifiers
                                                    .contains(KeyModifiers::CONTROL) =>
                                            {
                                                search.type_char(ch, &result.root, &fuzzy_paths)
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if key.modifiers.contains(KeyModifiers::ALT)
                                        && matches!(key.code, KeyCode::Char('f' | 'F'))
                                    {
                                        search.open();
                                        explanation = None;
                                        review = None;
                                        diff = None;
                                        diagnostics = None;
                                        fuzzy.close();
                                        continue;
                                    }
                                    if let Some(pane) = diff.as_mut() {
                                        match key.code {
                                            KeyCode::Esc | KeyCode::Char('d') => diff = None,
                                            KeyCode::Char('j') | KeyCode::Down => {
                                                pane.selected = (pane.selected + 1)
                                                    .min(pane.rows.len().saturating_sub(1));
                                                pane.hunk = hunks(&pane.rows)
                                                    .into_iter()
                                                    .find(|hunk| hunk.contains(pane.selected));
                                            }
                                            KeyCode::Char('k') | KeyCode::Up => {
                                                pane.selected = pane.selected.saturating_sub(1);
                                                pane.hunk = hunks(&pane.rows)
                                                    .into_iter()
                                                    .find(|hunk| hunk.contains(pane.selected));
                                            }
                                            KeyCode::Char(']') => {
                                                if let Some(row) =
                                                    next_hunk(&pane.rows, pane.selected)
                                                {
                                                    pane.selected = row;
                                                }
                                                pane.hunk = hunks(&pane.rows)
                                                    .into_iter()
                                                    .find(|hunk| hunk.contains(pane.selected));
                                            }
                                            KeyCode::Char('[') => {
                                                if let Some(row) =
                                                    prev_hunk(&pane.rows, pane.selected)
                                                {
                                                    pane.selected = row;
                                                }
                                                pane.hunk = hunks(&pane.rows)
                                                    .into_iter()
                                                    .find(|hunk| hunk.contains(pane.selected));
                                            }
                                            KeyCode::PageDown => {
                                                pane.selected = (pane.selected + 10)
                                                    .min(pane.rows.len().saturating_sub(1))
                                            }
                                            KeyCode::PageUp => {
                                                pane.selected = pane.selected.saturating_sub(10)
                                            }
                                            KeyCode::Char('s') => {
                                                pane.side_by_side = !pane.side_by_side
                                            }
                                            KeyCode::Char('g') | KeyCode::Home => pane.selected = 0,
                                            KeyCode::Char('G') | KeyCode::End => {
                                                pane.selected = pane.rows.len().saturating_sub(1)
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if let Some((items, selected)) = diagnostics.as_mut() {
                                        let shown = items.filtered(diag_filter).len();
                                        match key.code {
                                            KeyCode::Esc | KeyCode::F(7) => diagnostics = None,
                                            KeyCode::Char('j')
                                            | KeyCode::Down
                                            | KeyCode::Char('n') => {
                                                *selected =
                                                    (*selected + 1).min(shown.saturating_sub(1))
                                            }
                                            KeyCode::Char('k')
                                            | KeyCode::Up
                                            | KeyCode::Char('p') => {
                                                *selected = selected.saturating_sub(1)
                                            }
                                            KeyCode::Char('e') => {
                                                diag_filter.severity = Some(Severity::Error);
                                                *selected = 0;
                                            }
                                            KeyCode::Char('w') => {
                                                diag_filter.severity = Some(Severity::Warn);
                                                *selected = 0;
                                            }
                                            KeyCode::Char('i') => {
                                                diag_filter.severity = Some(Severity::Info);
                                                *selected = 0;
                                            }
                                            KeyCode::Char('u') => {
                                                diag_filter = DiagFilter::default();
                                                *selected = 0;
                                            }
                                            KeyCode::Char('g') | KeyCode::Home => *selected = 0,
                                            KeyCode::Char('G') | KeyCode::End => {
                                                *selected = shown.saturating_sub(1)
                                            }
                                            KeyCode::Enter => {
                                                let item = items
                                                    .filtered(diag_filter)
                                                    .get(*selected)
                                                    .map(|item| (*item).clone());
                                                diagnostics = None;
                                                if let Some(item) = item {
                                                    if let Some(path) = item.path {
                                                        let target =
                                                            Path::new(&result.root).join(&path);
                                                        let active = relative_to_root(
                                                            &result.root,
                                                            &session.app.target,
                                                        );
                                                        if active.as_deref() != Some(path.as_str())
                                                        {
                                                            if let Err(error) =
                                                                session.open_file(&target)
                                                            {
                                                                session.note(error);
                                                            }
                                                        }
                                                        if let Some(line) = item.line {
                                                            session.jump_line(line);
                                                        }
                                                    } else {
                                                        session.note(item.message);
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if key.code == KeyCode::F(7) {
                                        diagnostics =
                                            Some((build_diagnostics(result, report, session), 0));
                                        diag_filter = DiagFilter::default();
                                        review = None;
                                        fuzzy.close();
                                        continue;
                                    }
                                    if let Some(queue) = review.as_mut() {
                                        match key.code {
                                            KeyCode::Esc | KeyCode::F(6) => review = None,
                                            KeyCode::Char('j')
                                            | KeyCode::Down
                                            | KeyCode::Char('n') => queue.next(),
                                            KeyCode::Char('k')
                                            | KeyCode::Up
                                            | KeyCode::Char('p') => queue.prev(),
                                            KeyCode::Char('g') | KeyCode::Home => queue.select(0),
                                            KeyCode::Char('G') | KeyCode::End => {
                                                queue.select(queue.len().saturating_sub(1))
                                            }
                                            KeyCode::Char('}') => queue.next_file(),
                                            KeyCode::Char('{') => queue.prev_file(),
                                            KeyCode::Char('a') => queue.set_kind_filter(Some(
                                                crate::baseline::Kind::Added,
                                            )),
                                            KeyCode::Char('w') => queue.set_kind_filter(Some(
                                                crate::baseline::Kind::Worsened,
                                            )),
                                            KeyCode::Char('u') => queue.set_kind_filter(None),
                                            KeyCode::Char('x') | KeyCode::Char(' ') => {
                                                queue.toggle_selected_reviewed();
                                                reviewed_keys = queue.reviewed_keys().clone();
                                            }
                                            KeyCode::Char('f') => {
                                                if let Some(base) = report.baseline.as_ref() {
                                                    review_current_only = !review_current_only;
                                                    let active = relative_to_root(
                                                        &result.root,
                                                        &session.app.target,
                                                    );
                                                    let filter = if review_current_only {
                                                        active.as_deref()
                                                    } else {
                                                        None
                                                    };
                                                    *queue =
                                                        ReviewQueue::new(&base.changes, filter);
                                                    queue.apply_reviewed(
                                                        reviewed_keys.iter().cloned(),
                                                    );
                                                }
                                            }
                                            KeyCode::Char('d') => {
                                                if let Some(base) = report.baseline.as_ref() {
                                                    if let Some(change) =
                                                        queue.selected_change().cloned()
                                                    {
                                                        match build_diff_pane(
                                                            &result.root,
                                                            &base.rev,
                                                            &change,
                                                            session,
                                                        ) {
                                                            Ok(pane) => diff = Some(pane),
                                                            Err(error) => session.note(error),
                                                        }
                                                    }
                                                }
                                            }
                                            KeyCode::Enter => {
                                                if let Some(change) =
                                                    queue.selected_change().cloned()
                                                {
                                                    let target =
                                                        Path::new(&result.root).join(&change.path);
                                                    let active = relative_to_root(
                                                        &result.root,
                                                        &session.app.target,
                                                    );
                                                    if active.as_deref()
                                                        != Some(change.path.as_str())
                                                    {
                                                        if let Err(error) =
                                                            session.open_file(&target)
                                                        {
                                                            eprintln!("ccn: {error}");
                                                        }
                                                    }
                                                    session.jump_line(change.line);
                                                    review = None;
                                                }
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if key.code == KeyCode::F(6) {
                                        if let Some(base) = report.baseline.as_ref() {
                                            let active =
                                                relative_to_root(&result.root, &session.app.target);
                                            let filter = if review_current_only {
                                                active.as_deref()
                                            } else {
                                                None
                                            };
                                            let mut queue = ReviewQueue::new(&base.changes, filter);
                                            queue.apply_reviewed(reviewed_keys.iter().cloned());
                                            review = Some(queue);
                                        }
                                        continue;
                                    }
                                    if fuzzy.is_open() {
                                        match key.code {
                                            KeyCode::Esc => fuzzy.close(),
                                            KeyCode::Up => fuzzy.move_selection(-1),
                                            KeyCode::Down => fuzzy.move_selection(1),
                                            KeyCode::Enter => {
                                                let selected = fuzzy
                                                    .selected()
                                                    .and_then(|index| fuzzy_paths.get(index))
                                                    .cloned();
                                                fuzzy.close();
                                                if let Some(path) = selected {
                                                    let target =
                                                        Path::new(&result.root).join(&path);
                                                    if let Err(e) = session.open_file(&target) {
                                                        eprintln!("ccn: {e}");
                                                    } else {
                                                        focus = SplitFocus::Workbench;
                                                    }
                                                }
                                            }
                                            KeyCode::Backspace => fuzzy.backspace(&fuzzy_paths),
                                            KeyCode::Char(ch)
                                                if !key
                                                    .modifiers
                                                    .contains(KeyModifiers::CONTROL) =>
                                            {
                                                fuzzy.type_char(ch, &fuzzy_paths)
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    if key.modifiers.contains(KeyModifiers::CONTROL)
                                        && matches!(key.code, KeyCode::Char('p' | 'P'))
                                    {
                                        fuzzy.open(&fuzzy_paths);
                                        continue;
                                    }
                                    if is_pane_toggle(&key) {
                                        focus = match focus {
                                            SplitFocus::Files => SplitFocus::Workbench,
                                            SplitFocus::Workbench => SplitFocus::Files,
                                        };
                                        continue;
                                    }
                                    match focus {
                                        SplitFocus::Files => match key.code {
                                            KeyCode::Char('q') => break,
                                            KeyCode::Char('c')
                                                if key
                                                    .modifiers
                                                    .contains(KeyModifiers::CONTROL) =>
                                            {
                                                break
                                            }
                                            KeyCode::Char('c') => {
                                                tree.changed_only(!tree.is_changed_only())
                                            }
                                            KeyCode::Char('[') => {
                                                split_ratio = split_ratio.saturating_sub(2).max(20)
                                            }
                                            KeyCode::Char(']') => {
                                                split_ratio = (split_ratio + 2).min(70)
                                            }
                                            KeyCode::F(8) => focus = SplitFocus::Workbench,
                                            KeyCode::F(10) => break,
                                            _ => {
                                                if let TreeAction::Open(path) = tree.handle_key(key)
                                                {
                                                    let target = Path::new(&result.root).join(path);
                                                    if let Err(e) = session.open_file(&target) {
                                                        eprintln!("ccn: {e}");
                                                    }
                                                    focus = SplitFocus::Workbench;
                                                }
                                            }
                                        },
                                        SplitFocus::Workbench => {
                                            if key.code == KeyCode::F(8) {
                                                focus = SplitFocus::Files;
                                                continue;
                                            }
                                            if session.handle_key(key) {
                                                break;
                                            }
                                        }
                                    }
                                }
                                Event::Mouse(mouse) => match mouse.kind {
                                    MouseEventKind::Down(MouseButton::Left)
                                        if mouse.column >= divider.x
                                            && mouse.column < divider.x + divider.width =>
                                    {
                                        dragging = true;
                                    }
                                    MouseEventKind::Down(MouseButton::Left)
                                        if review.is_none()
                                            && diff.is_none()
                                            && diagnostics.is_none()
                                            && !fuzzy.is_open()
                                            && !palette.is_open()
                                            && mouse.column < left.width =>
                                    {
                                        focus = SplitFocus::Files;
                                        if let TreeAction::Open(path) =
                                            tree.click(mouse.column, mouse.row, left)
                                        {
                                            let target = Path::new(&result.root).join(path);
                                            if let Err(error) = session.open_file(&target) {
                                                eprintln!("ccn: {error}");
                                            } else {
                                                focus = SplitFocus::Workbench;
                                            }
                                        }
                                    }
                                    MouseEventKind::Down(MouseButton::Left)
                                        if review.is_none()
                                            && diff.is_none()
                                            && diagnostics.is_none()
                                            && !fuzzy.is_open()
                                            && !palette.is_open()
                                            && mouse.column >= right.x =>
                                    {
                                        session.click_workbench(mouse.column, mouse.row, right);
                                        focus = SplitFocus::Workbench;
                                    }
                                    MouseEventKind::Drag(MouseButton::Left) if dragging => {
                                        split_ratio = split_ratio_from_x(mouse.column, area.width);
                                    }
                                    MouseEventKind::Up(MouseButton::Left) => dragging = false,
                                    _ => {}
                                },
                                Event::Resize(_, _) => {}
                                _ => {}
                            }
                        }
                        guard.set_mouse(false)?;
                        tree_state = Some(tree);
                    }

                    save_workspace(
                        &result.root,
                        split_ratio,
                        &dashboard_state,
                        session.as_ref(),
                        &reviewed_keys,
                        tree_state.as_ref(),
                    );
                    if session.as_ref().is_some_and(|s| s.tabs().is_empty()) {
                        if let Some(empty) = session.take() {
                            empty.close();
                        }
                    }

                    match crate::loc::scan(options) {
                        Ok(updated) => {
                            *result = updated;
                            *report = crate::metrics::build_with(result, hotspots, history_limit);
                        }
                        Err(e) => eprintln!("ccn: {e}"),
                    }
                }

                #[cfg(not(feature = "workbench"))]
                {
                    let _ = path;
                    eprintln!("ccn: this build has no workbench support");
                }
            }
        }
    }
}
/// Whether a dashboard can be shown on this terminal.
pub fn is_interactive() -> bool {
    std::io::stdout().is_tty()
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App<'_>,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if app.quit {
            return Ok(());
        }
        if let Event::Key(key) = event::read()? {
            app.handle_key(key);
        }
    }
}

/// Owns raw mode and the alternate screen.
///
/// `Drop` is the single restoration point, which is why `run` only has to make
/// sure the guard is dropped before propagating anything.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    DisableBracketedPaste,
                    DisableMouseCapture,
                    Show
                );
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    /// Mouse reporting is only needed for the split divider. Leaving it off on
    /// the dashboard keeps normal terminal drag-select/copy (Ctrl+C) working.
    #[cfg(feature = "workbench")]
    fn set_mouse(&mut self, enabled: bool) -> io::Result<()> {
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best effort: each step is independent so one failure cannot skip the
        // rest of the restoration.
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            Show
        );
        let _ = self.terminal.show_cursor();
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which chart the left panel shows. `Tab` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftTab {
    Ccn,
    Loc,
}

impl LeftTab {
    #[cfg(feature = "workbench")]
    fn from_persisted(value: &str) -> Self {
        match value {
            "loc" => LeftTab::Loc,
            _ => LeftTab::Ccn,
        }
    }

    #[cfg(feature = "workbench")]
    fn persisted(self) -> &'static str {
        match self {
            LeftTab::Ccn => "ccn",
            LeftTab::Loc => "loc",
        }
    }

    fn next(self) -> Self {
        match self {
            LeftTab::Ccn => LeftTab::Loc,
            LeftTab::Loc => LeftTab::Ccn,
        }
    }

    fn title(self) -> &'static str {
        match self {
            LeftTab::Ccn => "CCN per function",
            LeftTab::Loc => "LOC per file",
        }
    }

    fn number(self) -> usize {
        match self {
            LeftTab::Ccn => 1,
            LeftTab::Loc => 2,
        }
    }
}

/// Ordering keys for the file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Risk,
    Loc,
    MaxCcn,
    Over10,
    Path,
}

impl SortKey {
    #[cfg(feature = "workbench")]
    fn from_persisted(value: &str) -> Self {
        match value {
            "loc" => SortKey::Loc,
            "max-ccn" => SortKey::MaxCcn,
            "over-10" => SortKey::Over10,
            "path" => SortKey::Path,
            _ => SortKey::Risk,
        }
    }

    #[cfg(feature = "workbench")]
    fn persisted(self) -> &'static str {
        match self {
            SortKey::Risk => "risk",
            SortKey::Loc => "loc",
            SortKey::MaxCcn => "max-ccn",
            SortKey::Over10 => "over-10",
            SortKey::Path => "path",
        }
    }

    fn next(self) -> Self {
        match self {
            SortKey::Risk => SortKey::Loc,
            SortKey::Loc => SortKey::MaxCcn,
            SortKey::MaxCcn => SortKey::Over10,
            SortKey::Over10 => SortKey::Path,
            SortKey::Path => SortKey::Risk,
        }
    }

    /// Numeric columns are most useful worst-first; paths read A-Z.
    fn default_desc(self) -> bool {
        self != SortKey::Path
    }

    fn label(self) -> &'static str {
        match self {
            SortKey::Risk => "risk",
            SortKey::Loc => "loc",
            SortKey::MaxCcn => "max ccn",
            SortKey::Over10 => "over 10",
            SortKey::Path => "path",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Detail,
    Help,
}

/// One measured file. `index` points back into `ScanResult.files` so the detail
/// overlay can read the function list without cloning it.
#[derive(Debug, Clone)]
struct FileRow {
    index: usize,
    path: String,
    lang: String,
    loc: u32,
    loc_total: u32,
    functions: usize,
    ccn_sum: u32,
    ccn_max: u32,
    p90: u32,
    over_10: usize,
    over_20: usize,
    over_50: usize,
    risk: f64,
    tier: CcnTier,
}

/// Dashboard state. Every visual transition goes through this struct, which is
/// what lets the tests drive keys without a real terminal.
pub struct App<'a> {
    result: &'a ScanResult,
    report: &'a Report,
    /// All files with function-level CCN, before filtering and sorting.
    files: Vec<FileRow>,
    /// Indices into `files`, filtered and ordered for display.
    view: Vec<usize>,
    selected: usize,
    /// Scroll offset, clamped by the renderer against the visible height.
    offset: usize,
    sort: SortKey,
    sort_desc: bool,
    filter: Option<String>,
    /// `Some` while the user is typing a filter; the text is live-applied.
    filter_input: Option<String>,
    left_tab: LeftTab,
    overlay: Overlay,
    /// Set by `e`; consumed by `run` after the terminal is restored.
    pending_edit: Option<PathBuf>,
    /// Transient dashboard warning (e.g. unsaved buffers blocking quit).
    notice: Option<String>,
    /// VCS label (`git main`, `p4 client`) shown in the header when known.
    vcs_identity: Option<String>,
    /// Presentation-only: no session file existed when this run started. Set by
    /// `run_unified`; never persisted, so the hint disappears after the first
    /// run saves a session.
    first_run: bool,
    quit: bool,
}

impl<'a> App<'a> {
    pub fn new(result: &'a ScanResult, report: &'a Report) -> Self {
        let files = result
            .files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                let summary = file.ccn.as_ref()?;
                if summary.tier == CcnTier::Heuristic || file.is_generated || file.is_vendored {
                    return None;
                }
                Some(FileRow {
                    index,
                    path: file.path.clone(),
                    lang: file.lang.clone(),
                    loc: file.loc as u32,
                    loc_total: file.loc_total as u32,
                    functions: summary.functions,
                    ccn_sum: summary.sum,
                    ccn_max: summary.max,
                    p90: summary.p90,
                    over_10: summary.over_10,
                    over_20: summary.over_20,
                    over_50: summary.over_50,
                    risk: file_risk(summary),
                    tier: summary.tier,
                })
            })
            .collect();

        let mut app = Self {
            result,
            report,
            files,
            view: Vec::new(),
            selected: 0,
            offset: 0,
            sort: SortKey::Risk,
            sort_desc: true,
            filter: None,
            filter_input: None,
            left_tab: LeftTab::Ccn,
            overlay: Overlay::None,
            pending_edit: None,
            notice: None,
            vcs_identity: None,
            first_run: false,
            quit: false,
        };
        app.rebuild();
        app
    }

    /// Set the VCS label once per scan/identity refresh.
    pub fn set_vcs_identity(&mut self, value: Option<String>) {
        self.vcs_identity = value;
    }

    /// Filter that is currently narrowing the list, if any.
    fn active_filter(&self) -> Option<&str> {
        match &self.filter_input {
            Some(text) if !text.is_empty() => Some(text.as_str()),
            Some(_) => None,
            None => self.filter.as_deref(),
        }
    }

    /// Append terminal bracketed-paste text to the live filter. Newlines are
    /// stripped because a pasted multi-line path must not submit/move the list.
    fn paste_filter(&mut self, text: &str) {
        if let Some(buffer) = &mut self.filter_input {
            for ch in text.chars().filter(|ch| *ch != '\r' && *ch != '\n') {
                buffer.push(ch);
            }
            self.selected = 0;
            self.rebuild();
        }
    }

    fn selected_row(&self) -> Option<&FileRow> {
        self.view
            .get(self.selected)
            .map(|&index| &self.files[index])
    }

    /// Snapshot the navigation state before leaving the dashboard for the
    /// workbench; the scan may change while we are away.
    fn capture_state(&self) -> DashboardState {
        DashboardState {
            selected_path: self.selected_row().map(|row| row.path.clone()),
            sort: self.sort,
            sort_desc: self.sort_desc,
            filter: self.filter.clone(),
            left_tab: self.left_tab,
        }
    }

    /// Restore sort/filter/tab and re-select the same path after a rescan.
    fn restore_state(&mut self, state: &DashboardState) {
        self.sort = state.sort;
        self.sort_desc = state.sort_desc;
        self.filter = state.filter.clone();
        self.left_tab = state.left_tab;
        self.rebuild();
        if let Some(path) = &state.selected_path {
            if let Some(position) = self
                .view
                .iter()
                .position(|&index| &self.files[index].path == path)
            {
                self.selected = position;
            }
        }
        self.selected = self.selected.min(self.view.len().saturating_sub(1));
    }

    /// Recompute the visible order. Cheap enough to run on every keystroke:
    /// a repository is a few thousand files, and this is a single sort.
    fn rebuild(&mut self) {
        let needle = self.active_filter().map(str::to_lowercase);
        let mut view: Vec<usize> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, row)| match &needle {
                Some(needle) => row.path.to_lowercase().contains(needle),
                None => true,
            })
            .map(|(index, _)| index)
            .collect();

        let key = self.sort;
        let desc = self.sort_desc;
        view.sort_by(|&a, &b| {
            let ordering = compare_rows(&self.files[a], &self.files[b], key);
            if desc {
                ordering.reverse()
            } else {
                ordering
            }
        });

        self.view = view;
        self.offset = 0;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.view.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.view.len() {
            self.selected = self.view.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.view.len() - 1;
        let next = self.selected.saturating_add_signed(delta).min(last);
        self.selected = next;
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.view.len().saturating_sub(1);
    }

    /// `s`: advance the sort key. Repeating `s` on the last key (`path`) toggles
    /// its direction before wrapping around.
    fn cycle_sort(&mut self) {
        if self.sort == SortKey::Path && !self.sort_desc {
            self.sort_desc = true;
        } else {
            self.sort = self.sort.next();
            self.sort_desc = self.sort.default_desc();
        }
        self.rebuild();
    }

    fn toggle_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        self.rebuild();
    }

    fn begin_filter(&mut self) {
        self.filter_input = Some(self.filter.clone().unwrap_or_default());
    }

    fn apply_filter(&mut self) {
        let typed = self.filter_input.take().unwrap_or_default();
        let typed = typed.trim().to_string();
        self.filter = if typed.is_empty() { None } else { Some(typed) };
        self.selected = 0;
        self.rebuild();
    }

    fn cancel_filter(&mut self) {
        self.filter_input = None;
        self.selected = 0;
        self.rebuild();
    }

    /// Apply one key press. Test entry point; the event loop calls exactly this.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }

        match self.overlay {
            Overlay::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.overlay = Overlay::None;
                }
                return;
            }
            Overlay::Detail => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('d') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                    KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                    _ => {}
                }
                return;
            }
            Overlay::None => {}
        }

        if self.filter_input.is_some() {
            match key.code {
                KeyCode::Esc => self.cancel_filter(),
                KeyCode::Enter => self.apply_filter(),
                KeyCode::Backspace => {
                    if let Some(buffer) = &mut self.filter_input {
                        buffer.pop();
                    }
                    self.selected = 0;
                    self.rebuild();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(buffer) = &mut self.filter_input {
                        buffer.push(ch);
                    }
                    self.selected = 0;
                    self.rebuild();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::Tab => self.left_tab = self.left_tab.next(),
            KeyCode::Char('1') => self.left_tab = LeftTab::Ccn,
            KeyCode::Char('2') => self.left_tab = LeftTab::Loc,
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('S') => self.toggle_direction(),
            KeyCode::Char('/') => self.begin_filter(),
            KeyCode::Char('e') => {
                if let Some(path) = self.selected_row().map(|row| PathBuf::from(&row.path)) {
                    self.pending_edit = Some(path);
                    self.quit = true;
                }
            }
            KeyCode::Enter | KeyCode::Char('d') => {
                if self.selected_row().is_some() {
                    self.overlay = Overlay::Detail;
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }
}

/// Same size-weighted penalty as `metrics::file_risk`, computed here because the
/// report only exposes it for its top 20 rows.
fn file_risk(summary: &CcnSummary) -> f64 {
    let mut mass = 0.0;
    let mut lines = 0u64;
    for function in &summary.detail {
        let weight = f64::from(function.nloc.max(1) as u32);
        mass += weight * penalty(function.ccn);
        lines += function.nloc.max(1) as u64;
    }
    if lines == 0 {
        0.0
    } else {
        mass / lines as f64
    }
}

fn compare_rows(a: &FileRow, b: &FileRow, key: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let primary = match key {
        SortKey::Risk => a
            .risk
            .partial_cmp(&b.risk)
            .unwrap_or(Ordering::Equal)
            // A file with no functions has risk 0 by construction; it must not
            // outrank a real but simple file just because of its path.
            .then_with(|| (a.functions > 0).cmp(&(b.functions > 0)))
            .then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::Loc => a.loc.cmp(&b.loc).then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::MaxCcn => a
            .ccn_max
            .cmp(&b.ccn_max)
            .then_with(|| a.ccn_sum.cmp(&b.ccn_sum)),
        SortKey::Over10 => a
            .over_10
            .cmp(&b.over_10)
            .then_with(|| a.ccn_max.cmp(&b.ccn_max)),
        SortKey::Path => Ordering::Equal,
    };
    primary.then_with(|| a.path.cmp(&b.path))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the whole dashboard. Kept free of terminal setup so tests can point a
/// `TestBackend` at it.
pub fn render(frame: &mut Frame, app: &mut App<'_>) {
    let area = frame.area();
    // The canvas goes down first: every later widget paints a card on top of
    // it, so a dark background is guaranteed even where nothing is drawn.
    frame.render_widget(
        Block::default().style(Style::default().bg(palette().canvas)),
        area,
    );

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "terminal too small (need at least 40x8)",
                Style::default().fg(palette().muted),
            )),
            area,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, app, rows[0]);

    let body =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(rows[1]);
    render_left(frame, app, body[0]);
    render_right(frame, app, body[1], true);
    render_footer(frame, app, rows[2]);

    match app.overlay {
        Overlay::Detail => render_detail(frame, app, area),
        Overlay::Help => render_help(frame, area),
        Overlay::None => {}
    }
}

/// One line of key hints. Navigation targets the list, so the list is the
/// focused panel; the chart panel keeps the quiet border.
fn render_footer(frame: &mut Frame, app: &App<'_>, area: Rect) {
    if let Some(notice) = &app.notice {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                notice.as_str(),
                Style::default()
                    .fg(palette().danger)
                    .add_modifier(Modifier::BOLD),
            ))),
            area,
        );
        return;
    }
    let key = Style::default().fg(palette().accent);
    let chrome = Style::default().fg(palette().muted);
    if app.first_run {
        let mut spans = vec![
            Span::styled("first run · ", chrome),
            Span::styled("?", key),
            Span::styled(" help · ", chrome),
            Span::styled("F5", key),
            Span::styled(" palette · ", chrome),
            Span::styled("e", key),
            Span::styled(" opens the workbench", chrome),
        ];
        if let Some(filter) = app.active_filter() {
            spans.push(Span::styled("  ", chrome));
            spans.push(Span::styled(
                format!(" /{filter} "),
                Style::default().fg(palette().text).bg(palette().pill),
            ));
        }
        let spans = truncate_spans(&spans, area.width as usize);
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let mut spans = vec![
        Span::styled("j/k", key),
        Span::styled(" move - ", chrome),
        Span::styled("/", key),
        Span::styled(" filter - ", chrome),
        Span::styled("e", key),
        Span::styled(" edit - ", chrome),
        Span::styled("Ctrl+P", key),
        Span::styled(" open - ", chrome),
        Span::styled("?", key),
        Span::styled(" help - ", chrome),
        Span::styled("q", key),
        Span::styled(" quit", chrome),
    ];
    if let Some(filter) = app.active_filter() {
        spans.push(Span::styled("  ", chrome));
        spans.push(Span::styled(
            format!(" /{filter} "),
            Style::default().fg(palette().text).bg(palette().pill),
        ));
    }
    let spans = truncate_spans(&spans, area.width as usize);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A bordered card. `focused` swaps the border and puts the accent marker on
/// the title; the title text itself stays muted so it never competes with data.
fn panel_block<'a>(focused: bool, title: Line<'a>) -> Block<'a> {
    let border = if focused {
        palette().border_focus
    } else {
        palette().border
    };
    Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(palette().card))
        .border_style(Style::default().fg(border))
        .title(title)
}

fn render_header(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(palette().card))
        .border_style(Style::default().fg(palette().border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let text = Style::default().fg(palette().text);
    let muted = Style::default().fg(palette().muted);

    let mut left = vec![
        Span::styled("ccn", muted),
        Span::styled(" · ", muted),
        Span::styled(app.report.root.replace('\\', "/"), text),
    ];
    if let Some(identity) = &app.vcs_identity {
        left.push(Span::styled("  ", muted));
        left.push(Span::styled(
            identity.as_str(),
            Style::default().fg(palette().accent_alt),
        ));
    }
    if let Some(filter) = app.active_filter() {
        left.push(Span::styled("  [filter: ", muted));
        left.push(Span::styled(filter.to_string(), text));
        left.push(Span::styled("]", muted));
    }

    let mut right = match &app.report.health {
        Some(health) => vec![
            Span::styled("Health ", muted),
            Span::styled(
                format!("{:.1}", health.score),
                Style::default()
                    .fg(palette().text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / 100  grade ", muted),
            Span::styled(
                health.grade.to_string(),
                Style::default()
                    .fg(grade_color(health.grade))
                    .add_modifier(Modifier::BOLD),
            ),
        ],
        None => vec![Span::styled("Health n/a", muted)],
    };
    let counts = format!(
        " · {} files · {} fns",
        thousands(app.report.files_measured),
        thousands(app.report.stats.functions_total)
    );
    if spans_width(&left) + spans_width(&right) + counts.chars().count() + 4 <= width {
        right.push(Span::styled(counts, muted));
    }

    let right_width = spans_width(&right);
    let line = if spans_width(&left) + right_width + 2 <= width {
        let padding = width - spans_width(&left) - right_width;
        let mut spans = left;
        spans.push(Span::styled(" ".repeat(padding), muted));
        spans.extend(right);
        Line::from(spans)
    } else if right_width + 1 < width {
        let mut spans = truncate_spans(&left, width - right_width - 1);
        spans.push(Span::styled(" ", muted));
        spans.extend(right);
        Line::from(spans)
    } else {
        Line::from(truncate_spans(&right, width))
    };

    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(palette().card)),
        inner,
    );
}

fn render_left(frame: &mut Frame, app: &App<'_>, area: Rect) {
    // Keep the original single chart on short terminals. On a normal dashboard
    // the left column becomes contextual: histogram, selected file, repo pulse.
    if area.height < 22 {
        render_histogram(frame, app, area);
        return;
    }
    let hist_height = if app.left_tab == LeftTab::Ccn { 10 } else { 9 };
    let chunks = Layout::vertical([
        Constraint::Length(hist_height),
        Constraint::Min(5),
        Constraint::Length(7),
    ])
    .split(area);
    render_histogram(frame, app, chunks[0]);
    render_selected_file(frame, app, chunks[1]);
    render_repo_pulse(frame, app, chunks[2]);
}

fn render_selected_file(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let title = Line::from(Span::styled(
        " selected file ",
        Style::default().fg(palette().muted),
    ));
    let block = panel_block(false, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(row) = app.selected_row() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no file selected",
                Style::default().fg(palette().muted),
            ))
            .style(Style::default().bg(palette().card)),
            inner,
        );
        return;
    };

    let tier = match row.tier {
        CcnTier::Exact => "exact",
        CcnTier::BestEffort => "best-effort",
        CcnTier::Heuristic => "heuristic",
    };
    let mut lines = vec![
        Line::from(Span::styled(
            row.path.as_str(),
            Style::default()
                .fg(palette().text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "{} · {} LOC · {} fn · CCN max {} · sum {} · p90 {}",
                row.lang, row.loc, row.functions, row.ccn_max, row.ccn_sum, row.p90
            ),
            Style::default().fg(palette().muted),
        )),
        Line::from(Span::styled(
            format!("tier {tier} · risk {:.1} · >10 {}", row.risk, row.over_10),
            Style::default().fg(palette().muted),
        )),
    ];

    let functions = app
        .result
        .files
        .get(row.index)
        .and_then(|file| file.ccn.as_ref())
        .map(|summary| summary.detail.as_slice())
        .unwrap_or(&[]);
    let mut top: Vec<&crate::ccn::types::FunctionCcn> = functions.iter().collect();
    top.sort_by(|a, b| b.ccn.cmp(&a.ccn).then(a.line.cmp(&b.line)));
    if top.is_empty() {
        lines.push(Line::from(Span::styled(
            " no function detail",
            Style::default().fg(palette().muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " top functions",
            Style::default().fg(palette().accent_alt),
        )));
        for function in top.into_iter().take(3) {
            if lines.len() >= inner.height as usize {
                break;
            }
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:>3} ", function.ccn),
                    Style::default().fg(risk_color(f64::from(function.ccn))),
                ),
                Span::styled(
                    truncate_right(&function.name, inner.width as usize - 12),
                    Style::default().fg(palette().text),
                ),
                Span::styled(
                    format!(" L{}", function.line),
                    Style::default().fg(palette().muted),
                ),
            ]));
        }
    }
    if let Some(baseline) = app.report.baseline.as_ref() {
        let changes = baseline
            .changes
            .iter()
            .filter(|change| change.path == row.path)
            .count();
        if lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                format!("baseline {}: {} changes", baseline.rev, changes),
                Style::default().fg(if changes > 0 {
                    palette().warn
                } else {
                    palette().muted
                }),
            )));
        }
    }
    lines.truncate(inner.height as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

fn render_repo_pulse(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let title = Line::from(Span::styled(
        " repo pulse ",
        Style::default().fg(palette().muted),
    ));
    let block = panel_block(false, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let stats = &app.report.stats;
    let ccn = app.result.ccn.as_ref();
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "{} exact · {} best · {} heuristic",
                stats.files_exact, stats.files_best_effort, stats.files_heuristic
            ),
            Style::default().fg(palette().muted),
        )),
        Line::from(Span::styled(
            match ccn {
                Some(ccn) => format!(
                    ">10 {} · >20 {} · >50 {}",
                    ccn.over_10, ccn.over_20, ccn.over_50
                ),
                None => "no CCN pass".to_string(),
            },
            Style::default().fg(palette().muted),
        )),
        Line::from(Span::styled(
            format!(
                "excluded: {} generated · {} vendored",
                stats.files_excluded_generated, stats.files_excluded_vendored
            ),
            Style::default().fg(palette().muted),
        )),
    ];

    let mut read_errors = 0;
    let mut oversize = 0;
    let mut no_grammar = 0;
    for file in &app.result.files {
        match file.ccn_skip {
            Some(crate::model::CcnSkip::ReadError) => read_errors += 1,
            Some(crate::model::CcnSkip::Oversize) => oversize += 1,
            Some(crate::model::CcnSkip::NoGrammar) => no_grammar += 1,
            None => {}
        }
    }
    lines.push(Line::from(Span::styled(
        format!("read errors {read_errors} · oversize {oversize} · no grammar {no_grammar}"),
        Style::default().fg(if read_errors > 0 {
            palette().danger
        } else {
            palette().muted
        }),
    )));
    if let Some(baseline) = app.report.baseline.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(
                "baseline {}: {} changes{}",
                baseline.rev,
                baseline.changes.len(),
                if baseline.complete {
                    ""
                } else {
                    " · incomplete"
                }
            ),
            Style::default().fg(if baseline.complete {
                palette().muted
            } else {
                palette().warn
            }),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "baseline: none (--baseline <rev>)",
            Style::default().fg(palette().muted),
        )));
    }
    if let Some(health) = app.report.health.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(
                "health {:.1} · grade {} · debt {:.2} of lines",
                health.score, health.grade, health.debt
            ),
            Style::default().fg(grade_color(health.grade)),
        )));
    }
    lines.truncate(inner.height as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

fn render_histogram(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let title = Line::from(vec![Span::styled(
        format!(" {} [{}/2] ", app.left_tab.title(), app.left_tab.number()),
        Style::default().fg(palette().muted),
    )]);
    let block = panel_block(false, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let histogram = match app.left_tab {
        LeftTab::Ccn => &app.report.ccn,
        LeftTab::Loc => &app.report.loc,
    };
    if histogram.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no data",
                Style::default().fg(palette().muted),
            ))
            .style(Style::default().bg(palette().card)),
            inner,
        );
        return;
    }

    let muted = Style::default().fg(palette().muted);
    let width = inner.width as usize;
    let label_width = if app.left_tab == LeftTab::Loc { 11 } else { 6 };
    let max_share = histogram
        .bins
        .iter()
        .map(|bin| bin.share_count)
        .fold(0.0_f64, f64::max);
    let max_weight = histogram
        .bins
        .iter()
        .map(|bin| bin.share_weight)
        .fold(0.0_f64, f64::max);
    let total_bins = histogram.bins.len();
    // The CCN chart spends its last row on the McCabe boundary annotation.
    let budget = if app.left_tab == LeftTab::Ccn {
        (inner.height as usize).saturating_sub(1)
    } else {
        inner.height as usize
    };

    let mut lines: Vec<Line> = Vec::new();
    for (index, bin) in histogram.bins.iter().enumerate().take(budget) {
        let mut spans = vec![
            Span::styled(pad(&bin.label, label_width), muted),
            Span::styled(format!("{:>7}", thousands(bin.count)), muted),
            Span::styled(format!("{:>5.1}%", bin.share_count), muted),
        ];

        let used = label_width + 7 + 7;
        if width > used + 1 {
            let bar_width = width - used - 1;
            spans.push(Span::styled(" ", muted));
            if app.left_tab == LeftTab::Loc {
                // Dual encoding: file count on the left track, lines on the
                // right one, each scaled to its own maximum.
                let files_width = bar_width.saturating_sub(1) / 2;
                let lines_width = bar_width.saturating_sub(1).saturating_sub(files_width);
                let files = bar_cells(bin.share_count, max_share, files_width);
                let line_cells = bar_cells(bin.share_weight, max_weight, lines_width);
                spans.push(Span::styled(
                    "█".repeat(files),
                    Style::default().fg(palette().ramp[0]),
                ));
                spans.push(Span::styled("│", muted));
                spans.push(Span::styled(
                    "█".repeat(line_cells),
                    Style::default().fg(palette().ramp[2]),
                ));
            } else {
                let cells = bar_cells(bin.share_count, max_share, bar_width);
                spans.push(Span::styled(
                    "█".repeat(cells),
                    Style::default().fg(bin_color(index, total_bins)),
                ));
            }
        }
        lines.push(Line::from(spans));
    }

    if app.left_tab == LeftTab::Ccn && lines.len() < inner.height as usize {
        lines.push(Line::from(vec![
            Span::styled(pad("", label_width), muted),
            Span::styled(
                truncate_right(
                    "└ McCabe limit 10 · 11–20 and up are debt",
                    width.saturating_sub(label_width),
                ),
                Style::default().fg(palette().warn),
            ),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

/// Bar length for `share` against `max`, at least one cell for a non-zero bin.
fn bar_cells(share: f64, max: f64, width: usize) -> usize {
    if width == 0 || max <= 0.0 {
        return 0;
    }
    let cells = (share / max * width as f64).round() as usize;
    let cells = if share > 0.0 { cells.max(1) } else { cells };
    cells.min(width)
}

fn render_right(frame: &mut Frame, app: &mut App<'_>, area: Rect, focused: bool) {
    let arrow = if app.sort_desc { '↓' } else { '↑' };
    let title = Line::from(vec![
        Span::styled("▌", Style::default().fg(palette().accent)),
        Span::styled(
            format!(
                " Files ({})   sort: {} {} ",
                app.view.len(),
                app.sort.label(),
                arrow
            ),
            Style::default().fg(palette().muted),
        ),
    ]);
    let block = panel_block(focused, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.view.is_empty() {
        let message = match app.active_filter() {
            Some(filter) => format!("no file matches \"{filter}\""),
            None => "no measured files with function-level CCN".to_string(),
        };
        frame.render_widget(
            Paragraph::new(Span::styled(message, Style::default().fg(palette().muted)))
                .style(Style::default().bg(palette().card)),
            inner,
        );
        return;
    }

    let width = inner.width as usize;
    let with_stats = width >= 40;
    let path_width = if with_stats {
        width.saturating_sub(31)
    } else {
        width.saturating_sub(4)
    };

    let header_height = 1;
    let rows_height = (inner.height as usize).saturating_sub(header_height);
    if rows_height == 0 {
        return;
    }

    if app.selected < app.offset {
        app.offset = app.selected;
    }
    if app.selected >= app.offset + rows_height {
        app.offset = app.selected + 1 - rows_height;
    }
    let max_offset = app.view.len().saturating_sub(rows_height);
    if app.offset > max_offset {
        app.offset = max_offset;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(rows_height + 1);
    let header = if with_stats {
        format!(
            "    {:<path_width$}{:>7}{:>8}{:>6}{:>6}",
            "path",
            "loc",
            "max",
            ">10",
            "risk",
            path_width = path_width
        )
    } else {
        format!("    {:<path_width$}", "path", path_width = path_width)
    };
    lines.push(Line::from(Span::styled(
        truncate_right(&header, width),
        Style::default()
            .fg(palette().muted)
            .add_modifier(Modifier::DIM),
    )));

    let end = (app.offset + rows_height).min(app.view.len());
    for position in app.offset..end {
        let Some(&index) = app.view.get(position) else {
            continue;
        };
        let row = &app.files[index];
        let selected = position == app.selected;
        let base = if selected {
            Style::default().fg(palette().text).bg(palette().pill)
        } else {
            Style::default().fg(palette().text)
        };

        let marker = if selected { "▌ " } else { "  " };
        let tag = tier_tag(row.tier);
        let path = pad(&truncate_left(&row.path, path_width), path_width);
        let tag_color = if row.tier == CcnTier::Exact {
            palette().muted
        } else {
            palette().warn
        };

        let mut spans = vec![
            Span::styled(
                marker,
                if selected {
                    base.fg(palette().accent)
                } else {
                    base.fg(palette().muted)
                },
            ),
            Span::styled(format!("{tag} "), base.fg(tag_color)),
            Span::styled(path, base.fg(palette().text)),
        ];
        if with_stats {
            spans.push(Span::styled(
                format!("{:>7}", thousands(row.loc as usize)),
                base.fg(palette().muted),
            ));
            spans.push(Span::styled(
                format!("{:>8}", row.ccn_max),
                base.fg(risk_color(row.risk)),
            ));
            spans.push(Span::styled(
                format!("{:>6}", row.over_10),
                base.fg(palette().muted),
            ));
            spans.push(Span::styled(
                format!("{:>6}", (row.risk * 100.0).round() as u32),
                base.fg(palette().muted),
            ));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

fn render_detail(frame: &mut Frame, app: &App<'_>, area: Rect) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let area = centered(area, 92, 82);
    if area.width < 4 || area.height < 4 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(palette().card))
        .border_style(Style::default().fg(palette().border_focus))
        .title(Line::from(Span::styled(
            " FILE DETAIL · Esc closes ",
            Style::default().fg(palette().accent),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let text = Style::default().fg(palette().text);
    let muted = Style::default().fg(palette().muted);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        truncate_left(&row.path, width),
        text.add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "{} · {} loc ({} total) · {} functions · tier {}",
            row.lang,
            thousands(row.loc as usize),
            thousands(row.loc_total as usize),
            thousands(row.functions),
            row.tier.as_str()
        ),
        muted,
    )));
    lines.push(Line::from(vec![
        Span::styled("ccn sum ", muted),
        Span::styled(row.ccn_sum.to_string(), text),
        Span::styled(" · max ", muted),
        Span::styled(
            row.ccn_max.to_string(),
            Style::default().fg(risk_color(penalty(row.ccn_max))),
        ),
        Span::styled(" · p90 ", muted),
        Span::styled(row.p90.to_string(), text),
    ]));
    lines.push(Line::from(vec![
        Span::styled("over 10: ", muted),
        Span::styled(row.over_10.to_string(), text),
        Span::styled("   over 20: ", muted),
        Span::styled(row.over_20.to_string(), text),
        Span::styled("   over 50: ", muted),
        Span::styled(row.over_50.to_string(), text),
        Span::styled("   risk ", muted),
        Span::styled(
            ((row.risk * 100.0).round() as u32).to_string(),
            Style::default().fg(risk_color(row.risk)),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "top functions by ccn",
        text.add_modifier(Modifier::BOLD),
    )));

    let mut functions: Vec<&FunctionCcn> = app
        .result
        .files
        .get(row.index)
        .and_then(|file| file.ccn.as_ref())
        .map(|summary| summary.detail.iter().collect())
        .unwrap_or_default();
    functions.sort_by(|a, b| {
        b.ccn
            .cmp(&a.ccn)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });

    if functions.is_empty() {
        lines.push(Line::from(Span::styled(
            "no functions measured",
            Style::default().fg(palette().muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("{:>4}  {:<30}{:>6}  {:>5}", "ccn", "name", "line", "nloc"),
            Style::default()
                .fg(palette().muted)
                .add_modifier(Modifier::DIM),
        )));
        for function in functions.iter().take(10) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>4}", function.ccn),
                    Style::default().fg(risk_color(penalty(function.ccn))),
                ),
                Span::styled(
                    format!(
                        "  {:<30}{:>6}  {:>5}",
                        truncate_right(&function.name, 30),
                        function.line,
                        function.nloc
                    ),
                    muted,
                ),
            ]));
        }
    }

    lines.truncate(inner.height as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

/// Advanced keys remain available but stay out of the compact help unless the
/// user explicitly asks for them. This keeps the default key map teachable.
fn advanced_keys() -> bool {
    std::env::var("CCN_KEYMAP")
        .map(|value| value.eq_ignore_ascii_case("advanced"))
        .unwrap_or(false)
}
fn render_help(frame: &mut Frame, area: Rect) {
    let area = centered(area, 76, 76);
    if area.width < 4 || area.height < 4 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(palette().card))
        .border_style(Style::default().fg(palette().border_focus))
        .title(Line::from(Span::styled(
            " keys · Esc closes ",
            Style::default().fg(palette().accent),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "NAVIGATE",
            &[
                ("j / k", "move selection"),
                ("/", "filter paths (Enter applies, Esc cancels)"),
                ("Enter / d", "file detail"),
                ("e", "open the workbench"),
                ("Ctrl+P", "open a file by name"),
                ("?", "this help"),
                ("q / Esc", "quit"),
            ],
        ),
        (
            "WORKBENCH",
            &[
                ("F5", "command palette"),
                ("F6", "baseline review (then d for diff)"),
                ("F7", "diagnostics"),
                ("Alt+W", "files / editor panes"),
                ("Alt+O", "search functions in this file"),
                ("Alt+F", "search workspace text"),
                ("F10", "back to the dashboard"),
            ],
        ),
        (
            "ADVANCED (still works)",
            &[
                ("F8 / Ctrl+Shift+E", "pane switch (compatibility)"),
                ("F9", "editor / function sidebar"),
                ("g / G", "first / last"),
                ("Tab", "chart: CCN per function / LOC per file"),
                ("s / S", "sort key / reverse direction"),
                ("1 / 2", "chart tab"),
                ("[ / ]", "resize the split from the file pane"),
                ("F2 / F3 / F4", "previous / next / close tab"),
                ("Ctrl+C", "quit confirmation / interrupt in the editor"),
            ],
        ),
    ];
    let mut lines: Vec<Line> = Vec::new();
    let visible = if advanced_keys() {
        sections.len()
    } else {
        sections.len() - 1
    };
    for (index, (title, entries)) in sections.iter().take(visible).enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            Style::default()
                .fg(palette().accent_alt)
                .add_modifier(Modifier::BOLD),
        )));
        for (keys, description) in entries.iter() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {:<14}", keys),
                    Style::default().fg(palette().accent),
                ),
                Span::styled(*description, Style::default().fg(palette().text)),
            ]));
        }
    }
    lines.truncate(inner.height as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().card)),
        inner,
    );
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Tier tag: only non-`Exact` files get a letter, so the common case is blank.
fn tier_tag(tier: CcnTier) -> char {
    match tier {
        CcnTier::Exact => ' ',
        CcnTier::BestEffort => 'B',
        CcnTier::Heuristic => '?',
    }
}

/// Thousands separator; a local copy keeps the dashboard independent of the
/// plain renderer's formatting choices.
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Display width of a styled line, in cells.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

/// Cut a styled line to `max` cells, keeping each span's colour and adding an
/// ellipsis when the cut lands mid-span.
fn truncate_spans(spans: &[Span<'_>], max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut left = max;
    for span in spans {
        if left == 0 {
            break;
        }
        let length = span.content.chars().count();
        if length <= left {
            out.push(Span::styled(span.content.to_string(), span.style));
            left -= length;
        } else {
            let text = if left == 1 {
                "…".to_string()
            } else {
                let head: String = span.content.chars().take(left - 1).collect();
                format!("{head}…")
            };
            out.push(Span::styled(text, span.style));
            left = 0;
        }
    }
    out
}

/// Keep the tail of a string, which is the informative half of a path.
fn truncate_left(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let skip = length - (max - 1);
    let tail: String = text.chars().skip(skip).collect();
    format!("…{tail}")
}

/// Keep the head of a string.
fn truncate_right(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}

fn pad(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - length))
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1]);
    horizontal[1]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{CcnSummary, FileCcn};
    use crate::metrics;
    use crate::model::{ExcludedFiles, FileLoc};
    use ratatui::backend::TestBackend;

    fn function(name: &str, line: usize, ccn: u32, nloc: usize) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            identity: format!("{name}()"),
            line,
            end_line: line + nloc.saturating_sub(1),
            nloc,
            ccn,
            params: 0,
            max_nesting: 0,
        }
    }

    fn file(path: &str, loc: usize, lang: &str, functions: Vec<FunctionCcn>) -> FileLoc {
        FileLoc {
            path: path.to_string(),
            lang: lang.to_string(),
            loc_total: loc + 10,
            loc,
            comments: 0,
            blanks: 10,
            ccn: Some(CcnSummary::from_file(FileCcn {
                tier: CcnTier::Exact,
                functions,
            })),
            ccn_skip: None,
            is_generated: false,
            is_vendored: false,
        }
    }

    /// Four files chosen so every sort key produces a distinct first row:
    ///
    /// | path              | loc | max ccn | risk |
    /// |-------------------|-----|---------|------|
    /// | src/beta.rs       | 100 | 137     | 1.00 |
    /// | src/tui_app.py    | 300 | 20      | 0.30 |
    /// | src/alpha.rs      | 500 | 1       | 0.00 |
    /// | src/zeta.rs       | 900 | 2       | 0.00 |
    fn fixture() -> (ScanResult, Report) {
        let files = vec![
            file(
                "src/alpha.rs",
                500,
                "Rust",
                vec![function("alpha_main", 10, 1, 400)],
            ),
            file(
                "src/beta.rs",
                100,
                "Rust",
                vec![function("monster", 7, 137, 90)],
            ),
            file(
                "src/tui_app.py",
                300,
                "Python",
                vec![
                    function("draw", 3, 20, 80),
                    function("event_loop", 90, 3, 50),
                ],
            ),
            file(
                "src/zeta.rs",
                900,
                "Rust",
                vec![
                    function("helper_one", 1, 2, 300),
                    function("helper_two", 400, 1, 300),
                ],
            ),
        ];

        let loc: usize = files.iter().map(|file| file.loc).sum();
        let loc_total: usize = files.iter().map(|file| file.loc_total).sum();
        let functions_total: usize = files
            .iter()
            .filter_map(|file| file.ccn.as_ref())
            .map(|summary| summary.functions)
            .sum();
        let result = ScanResult {
            root: "C:/work/repo".to_string(),
            files_analyzed: files.len(),
            loc_total,
            loc,
            comments: 0,
            blanks: loc_total - loc,
            files_excluded: ExcludedFiles::default(),
            functions_total,
            ccn: None,
            languages: Vec::new(),
            files,
        };
        let report = metrics::build(&result);
        (result, report)
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn draw(app: &mut App<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, app))
            .expect("draw dashboard");
        buffer_text(terminal.backend().buffer())
    }

    fn press(app: &mut App<'_>, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App<'_>, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    fn first_path(app: &App<'_>) -> String {
        app.view
            .first()
            .map(|&index| app.files[index].path.clone())
            .unwrap_or_default()
    }

    #[test]
    fn renders_header_and_panels() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let text = draw(&mut app, 100, 30);

        assert!(text.contains("repo"), "repo root missing:\n{text}");
        assert!(text.contains("Health"), "health missing:\n{text}");
        assert!(text.contains("sort:"), "sort label missing:\n{text}");
        assert!(text.contains("risk"), "sort key missing:\n{text}");
        assert!(text.contains("beta.rs"), "first file missing:\n{text}");
        assert!(text.contains("Files (4)"), "file count missing:\n{text}");
        assert!(text.contains("CCN per function"), "chart missing:\n{text}");
    }

    #[test]
    fn selection_moves_and_clamps() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let last = app.view.len() - 1;

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0, "k at the top stays put");

        for _ in 0..20 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.selected, last, "j clamps at the bottom");
        assert!(app.selected < app.view.len());

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, last - 1);

        for _ in 0..20 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.selected, last, "arrows behave like j/k");

        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected, last);

        // An empty list must not be able to produce an out-of-range index.
        app.view.clear();
        app.clamp_selection();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn sort_cycles() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        assert_eq!(app.sort, SortKey::Risk);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/beta.rs");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Loc);
        assert_eq!(first_path(&app), "src/zeta.rs", "longest file first");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::MaxCcn);
        assert_eq!(first_path(&app), "src/beta.rs");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Over10);
        assert_eq!(app.sort.label(), "over 10");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Path);
        assert!(!app.sort_desc, "path sorts ascending");
        assert_eq!(
            first_path(&app),
            "src/alpha.rs",
            "path sort is lexicographic"
        );

        // Repeating `s` on the last key flips the direction ...
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Path);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/zeta.rs");

        // ... and the next press wraps back to the default.
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Risk);
        assert!(app.sort_desc);
        assert_eq!(first_path(&app), "src/beta.rs");

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("sort: risk"), "{text}");
    }

    #[test]
    fn filter_narrows_the_list() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let before = app.view.len();
        assert_eq!(before, 4);

        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "tui");
        assert!(app.view.len() < before, "filter must narrow the list");
        assert_eq!(app.view.len(), 1);

        let text = draw(&mut app, 100, 30);
        assert!(
            text.contains("[filter: tui]"),
            "filter chip missing:\n{text}"
        );
        assert!(text.contains("tui_app.py"), "{text}");
        assert!(
            !text.contains("beta.rs"),
            "filtered row still visible:\n{text}"
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.filter.as_deref(), Some("tui"));
        assert!(app.filter_input.is_none());

        // Esc while typing discards the edit but keeps the applied filter.
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "zzz");
        assert!(app.view.is_empty());
        let empty = draw(&mut app, 100, 30);
        assert!(
            empty.contains("no file matches"),
            "empty-state line missing:\n{empty}"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter.as_deref(), Some("tui"));
        assert_eq!(app.view.len(), 1);

        // An applied filter can be cleared by submitting an empty box.
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        assert!(app.filter.is_none());
        assert_eq!(app.view.len(), before);
    }

    #[test]
    fn detail_overlay_shows_functions() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        // Default sort puts the 137-CCN file first.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.overlay, Overlay::Detail);

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("beta.rs"), "detail path missing:\n{text}");
        assert!(text.contains("monster"), "function name missing:\n{text}");
        assert!(text.contains("137"), "function ccn missing:\n{text}");
        assert!(
            text.contains("top functions"),
            "detail header missing:\n{text}"
        );
        assert!(text.contains("ccn sum"), "aggregates missing:\n{text}");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn renders_at_the_split_minimum_without_panicking() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let text = draw(&mut app, 60, 20);
        assert!(!text.trim().is_empty(), "60x20 produced no output");
        assert!(
            text.contains("repo"),
            "normal layout should render at 60x20:\n{text}"
        );
    }

    #[test]
    #[cfg(feature = "workbench")]
    fn split_guard_rejects_small_terminals() {
        assert!(split_too_small(Rect::new(0, 0, 1, 1)));
        assert!(split_too_small(Rect::new(
            0,
            0,
            SPLIT_MIN_WIDTH - 1,
            SPLIT_MIN_HEIGHT
        )));
        assert!(split_too_small(Rect::new(
            0,
            0,
            SPLIT_MIN_WIDTH,
            SPLIT_MIN_HEIGHT - 1
        )));
        assert!(!split_too_small(Rect::new(
            0,
            0,
            SPLIT_MIN_WIDTH,
            SPLIT_MIN_HEIGHT
        )));
        assert!(!split_too_small(Rect::new(0, 0, 200, 50)));
    }

    #[test]
    #[cfg(feature = "workbench")]
    fn split_areas_is_safe_at_tiny_sizes() {
        for (width, height) in [(0u16, 0u16), (1, 1), (2, 5), (59, 15)] {
            let size = Rect::new(0, 0, width, height);
            let (left, divider, right) = split_areas(size, 38);
            assert_eq!(divider.width, 1);
            assert!(left.width >= 20, "left pane keeps its floor");
            // The 20-column floor can exceed a sub-21-column terminal; ratatui
            // clips those rects. Once the floor fits, the widths tile exactly.
            if width >= 21 {
                assert_eq!(
                    left.width + divider.width + right.width,
                    width,
                    "tiny split lost columns at {width}x{height}"
                );
            }
        }
    }

    #[test]
    fn first_run_footer_shows_the_onboarding_hint() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        assert!(!app.first_run, "the hint is opt-in");

        app.first_run = true;
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("first run"), "hint missing:\n{text}");
        assert!(text.contains("F5"), "palette hint missing:\n{text}");
        assert!(
            text.contains("opens the workbench"),
            "workbench hint missing:\n{text}"
        );
        assert!(
            !text.contains("j/k move"),
            "the normal footer should be replaced:\n{text}"
        );

        // A notice still wins so a warning is never hidden by onboarding.
        app.notice = Some("unsaved buffers".to_string());
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("unsaved buffers"), "{text}");
        assert!(!text.contains("first run"), "{text}");
    }

    #[test]
    fn first_run_hint_truncates_safely_on_a_narrow_terminal() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        app.first_run = true;
        let text = draw(&mut app, MIN_WIDTH, MIN_HEIGHT);
        assert!(!text.trim().is_empty(), "narrow hint produced no output");
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let text = draw(&mut app, 20, 6);
        assert!(text.contains("too small"), "fallback line missing:\n{text}");

        // Also safe below the layout minimums in each dimension separately.
        let _ = draw(&mut app, 100, 4);
        let _ = draw(&mut app, 10, 40);
    }

    #[test]
    #[cfg(feature = "workbench")]
    fn split_ratio_moves_the_divider_and_keeps_both_panes_usable() {
        let size = Rect::new(0, 0, 140, 40);
        let (left, divider, right) = split_areas(size, 38);
        assert_eq!(left.width + divider.width + right.width, size.width);
        assert_eq!(divider.width, 1);
        assert!(left.width >= 20, "left pane too small");
        assert!(right.width >= 30, "right pane too small");

        let (wider, _, narrower) = split_areas(size, 60);
        assert!(wider.width > left.width, "dragging right widens the table");
        assert!(narrower.width < right.width, "and narrows the editor");

        assert_eq!(split_ratio_from_x(0, 140), 20);
        assert_eq!(split_ratio_from_x(140, 140), 70);
    }

    #[test]
    fn e_requests_the_selected_file_for_the_workbench() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        let selected = app.selected_row().expect("a selected row").path.clone();

        press(&mut app, KeyCode::Char('e'));

        assert!(app.quit, "e leaves the dashboard");
        assert_eq!(
            app.pending_edit.as_deref(),
            Some(std::path::Path::new(&selected))
        );
    }

    #[test]
    fn pasted_filter_text_strips_newlines() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        press(&mut app, KeyCode::Char('/'));
        app.paste_filter("alpha\r\n.rs");
        assert_eq!(app.filter_input.as_deref(), Some("alpha.rs"));
        assert_eq!(app.view.len(), 1);
    }

    #[test]
    #[cfg(feature = "workbench")]
    fn pane_toggle_prefers_alt_w_without_stealing_ctrl_e() {
        assert!(is_pane_toggle(&KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::ALT
        )));
        assert!(is_pane_toggle(&KeyEvent::new(
            KeyCode::Char('E'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!is_pane_toggle(&KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn dashboard_state_survives_a_workbench_round_trip() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);
        app.sort = SortKey::Loc;
        app.sort_desc = false;
        app.filter = Some("src".to_string());
        app.left_tab = LeftTab::Loc;
        app.rebuild();
        let selected = app.selected_row().expect("a selected row").path.clone();
        let state = app.capture_state();

        let mut fresh = App::new(&result, &report);
        fresh.restore_state(&state);

        assert_eq!(fresh.sort, SortKey::Loc);
        assert!(!fresh.sort_desc);
        assert_eq!(fresh.filter.as_deref(), Some("src"));
        assert_eq!(fresh.left_tab, LeftTab::Loc);
        assert_eq!(
            fresh.selected_row().map(|row| row.path.as_str()),
            Some(selected.as_str())
        );
    }

    #[test]
    fn help_overlay_lists_keys() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Overlay::Help);

        let text = draw(&mut app, 100, 30);
        assert!(text.contains("keys"), "help title missing:\n{text}");
        assert!(text.contains("move selection"), "{text}");
        assert!(text.contains("filter paths"), "{text}");
        assert!(text.contains("file detail"), "{text}");
        assert!(text.contains("quit"), "{text}");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn selected_row_uses_the_pill_background() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("draw dashboard");
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width;

        // Default sort puts src/beta.rs first, so it is the selected row.
        let mut selected_y = None;
        for y in 0..buffer.area.height {
            let mut text = String::new();
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            if text.contains("beta.rs") {
                selected_y = Some(y);
                break;
            }
        }
        let y = selected_y.expect("selected row rendered");

        let path = "src/beta.rs";
        let mut path_x = None;
        for x in 0..width.saturating_sub(path.len() as u16) {
            let mut text = String::new();
            for offset in 0..path.len() {
                text.push_str(buffer[(x + offset as u16, y)].symbol());
            }
            if text == path {
                path_x = Some(x);
                break;
            }
        }
        let path_x = path_x.expect("selected path start");
        // Row prefix is the accent marker + space, then the tier tag + space.
        let marker_x = path_x - 4;
        assert_eq!(buffer[(marker_x, y)].symbol(), "▌");

        for x in marker_x..width - 1 {
            assert_eq!(
                buffer[(x, y)].bg,
                palette().pill,
                "cell ({x}, {y}) must use the pill background"
            );
        }
    }

    #[test]
    #[cfg(feature = "workbench")]
    fn tree_paths_include_every_measured_source_file() {
        let (mut result, _) = fixture();
        // A file without a CCN summary is still a measured source file.
        result.files[0].ccn = None;
        result.files[1].is_generated = true;
        result.files[2].is_vendored = true;

        let paths = tree_paths(&result);
        assert!(paths.contains(&"src/alpha.rs".to_string()), "{paths:?}");
        assert!(paths.contains(&"src/zeta.rs".to_string()), "{paths:?}");
        assert!(!paths.contains(&"src/beta.rs".to_string()), "{paths:?}");
        assert!(!paths.contains(&"src/tui_app.py".to_string()), "{paths:?}");
    }

    #[test]
    fn grade_badge_uses_the_grade_colour() {
        let (result, mut report) = fixture();
        report.health = Some(metrics::Health {
            score: 90.0,
            grade: 'C',
            debt: 0.10,
            functions: 10,
            lines: 100,
            over_10: 1,
            best_effort_functions: 0,
            over_20: 0,
            over_50: 0,
            pct_over_10: 10.0,
        });
        let mut app = App::new(&result, &report);

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("draw dashboard");
        let buffer = terminal.backend().buffer();

        let expected = grade_color('C');
        let mut found = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].fg == expected {
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "no header cell uses grade_color('C')");
    }

    #[test]
    fn q_quits_but_esc_only_closes_overlays() {
        let (result, report) = fixture();
        let mut app = App::new(&result, &report);

        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert_eq!(app.overlay, Overlay::None);

        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }
}
