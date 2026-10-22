//! `ccn-workbench` spike: a real Neovim TUI in a ratatui pane, next to the
//! current file's complexity breakdown.
//!
//! The workbench itself owns no editing logic: Neovim runs in a PTY, its output
//! is rendered through `vt100` + `tui-term`, and keys are forwarded as bytes.
//! See `docs/workbench-spike.md`.

mod pty;
pub(crate) mod review_ui;
mod rpc;
mod tabs;
mod ui;

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Frame;
use ratatui::Terminal;

use crate::ccn::types::{CcnTier, FunctionCcn};
use crate::loc::{scan, ScanOptions};
use crate::model::{CcnSkip, FileLoc};

use pty::Pty;
use rpc::{LiveSnapshot, RpcEvent, RpcHandle, RpcStatus};
use tabs::{path_key, Tab, Tabs};

/// Re-scan the open file at most this often.
const MTIME_INTERVAL: Duration = Duration::from_millis(500);

/// Re-query dirty state for every tab at most this often.
const DIRTY_INTERVAL: Duration = Duration::from_millis(1000);

const SMOKE_SOURCE: &str = r#"/* ccn workbench smoke file */
int smoke_choice(int x) {
    if (x > 10) {
        return x * 2;
    }
    for (int i = 0; i < x; i++) {
        x += i;
    }
    return x;
}
"#;

#[derive(Parser, Debug)]
#[command(
    name = "ccn workbench",
    version,
    about = "Neovim in a ratatui pane, with ccn complexity side panels (ccn workbench)"
)]
struct Cli {
    /// File to open. A directory is rejected in this spike.
    target: Option<PathBuf>,

    /// Editor command (may include arguments in double quotes).
    #[arg(long, default_value = "nvim")]
    editor: String,

    /// Non-interactive PTY self-test: print CCN_SMOKE, assert the vt100 screen.
    #[arg(long)]
    smoke: bool,

    /// Non-interactive Neovim render test on a temp file (skips if missing).
    #[arg(long)]
    smoke_nvim: bool,

    /// Type Korean through the PTY into Neovim, save, and assert the bytes.
    #[arg(long)]
    smoke_utf8: bool,

    /// Prove the RPC path: connect, snapshot, annotate, dump extmarks.
    #[arg(long)]
    smoke_rpc: bool,

    /// Edit through the PTY without saving and assert the live CCN changed.
    #[arg(long)]
    smoke_rpc_edit: bool,

    /// Disable Neovim RPC; use mtime polling only.
    #[arg(long)]
    no_rpc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Editor,
    Sidebar,
}

#[derive(Debug, Clone, Copy)]
enum TabAction {
    Prev,
    Next,
    Close,
}

impl Focus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Focus::Editor => "editor",
            Focus::Sidebar => "sidebar",
        }
    }
}

/// The part of a scan the sidebar needs, owned so the event loop never holds a
/// `ScanResult` alive.
pub(crate) struct FileView {
    pub name: String,
    pub lang: String,
    pub loc: usize,
    pub loc_total: usize,
    pub count: usize,
    pub max: u32,
    pub p90: u32,
    pub sum: u32,
    pub tier: Option<CcnTier>,
    pub file_ccn: Option<u32>,
    pub skip: Option<CcnSkip>,
    pub functions: Vec<FunctionCcn>,
}

impl FileView {
    fn from_file(file: FileLoc) -> Self {
        let name = Path::new(&file.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.path.clone());
        let ccn = file.ccn.as_ref();
        let mut functions = ccn.map(|c| c.detail.clone()).unwrap_or_default();
        // Sidebar order: worst first, ties by source order.
        functions.sort_by(|a, b| b.ccn.cmp(&a.ccn).then_with(|| a.line.cmp(&b.line)));
        Self {
            name,
            lang: file.lang,
            loc: file.loc,
            loc_total: file.loc_total,
            count: ccn.map_or(0, |c| c.functions),
            max: ccn.map_or(0, |c| c.max),
            p90: ccn.map_or(0, |c| c.p90),
            sum: ccn.map_or(0, |c| c.sum),
            tier: ccn.map(|c| c.tier),
            file_ccn: ccn.and_then(|c| c.file_ccn),
            skip: file.ccn_skip,
            functions,
        }
    }

    pub(crate) fn skip_label(&self) -> &'static str {
        match self.skip {
            Some(CcnSkip::Oversize) => "file over the CCN size guard",
            Some(CcnSkip::ReadError) => "file could not be read",
            Some(CcnSkip::NoGrammar) => "no CCN grammar for this language",
            None if self.tier.is_none() => "CCN pass not run",
            None if self.tier == Some(CcnTier::Heuristic) => "file-level heuristic CCN",
            None if self.count == 0 => "no functions found",
            None => "function detail not included",
        }
    }
}

/// One live-buffer function plus its delta against the initial scan.
pub(crate) struct LiveRow {
    pub function: FunctionCcn,
    /// `+n` new (delta starts at 0), `+n` worsened, `-n` improved, `0` same.
    pub delta: i64,
    pub is_new: bool,
}

/// Live RPC snapshot prepared for rendering.
pub(crate) struct LiveView {
    pub name: String,
    pub tier: CcnTier,
    pub functions: Vec<LiveRow>,
    pub sum: u32,
    pub max: u32,
    pub changedtick: i64,
    pub base_tick: i64,
    pub read_ms: f64,
}

impl LiveView {
    pub(crate) fn unsaved(&self) -> bool {
        self.changedtick != self.base_tick
    }

    /// Sum of all positive changes, counting new functions from zero.
    pub(crate) fn positive_delta(&self) -> u32 {
        self.functions
            .iter()
            .map(|row| row.delta.max(0) as u32)
            .sum()
    }
}

/// All mutable workbench state. Rendering gets an immutable borrow, so input
/// handling is the only place that changes it.
pub(crate) struct App {
    pub target: PathBuf,
    pub display: String,
    pub editor_label: String,
    pub view: Option<FileView>,
    pub scan_error: Option<String>,
    pub scan_ms: u128,
    pub focus: Focus,
    pub selected: usize,
    pub status: String,
    status_error: bool,
    pub exited: Option<String>,
    mtime: Option<SystemTime>,
    /// Initial file scan, frozen for the session; deltas compare against this.
    pub initial: Vec<FunctionCcn>,
    pub live: Option<LiveView>,
    pub rpc_status: RpcStatus,
    pub rpc_read_ms: Option<f64>,
    pub rpc_note: Option<String>,
    rpc: Option<RpcHandle>,
    rpc_rx: Option<Receiver<RpcEvent>>,
    pub child_alive: Arc<AtomicBool>,
    pub rpc_disabled: bool,
    /// True when rendered as the right pane of the dashboard split.
    pub split: bool,
    /// Ordered open files; the active tab mirrors `target`/`display`.
    pub tabs: Tabs,
    /// Set while an async `:bdelete` request is in flight.
    pending_close: Option<PathBuf>,
    /// Async `:edit` rollback: (path, previous active index, tab was new).
    pending_switch: Option<(PathBuf, Option<usize>, bool)>,
    last_dirty_check: Instant,
}

impl App {
    pub(crate) fn new(target: PathBuf, display: String, editor_label: String) -> Self {
        let mut app = Self {
            target,
            display,
            editor_label,
            view: None,
            scan_error: None,
            scan_ms: 0,
            focus: Focus::Editor,
            selected: 0,
            status: String::new(),
            status_error: false,
            exited: None,
            mtime: None,
            initial: Vec::new(),
            live: None,
            rpc_status: RpcStatus::Off,
            rpc_read_ms: None,
            rpc_note: None,
            rpc: None,
            rpc_rx: None,
            child_alive: Arc::new(AtomicBool::new(true)),
            rpc_disabled: false,
            split: false,
            tabs: Tabs::new(),
            pending_close: None,
            pending_switch: None,
            last_dirty_check: Instant::now(),
        };
        app.tabs
            .open_or_select_named(&app.target, app.display.clone());
        app.rescan("scan");
        app.initial = app
            .view
            .as_ref()
            .map_or_else(Vec::new, |view| view.functions.clone());
        app
    }

    fn function_count(&self) -> usize {
        if self.live_mode() {
            self.live.as_ref().map_or(0, |live| live.functions.len())
        } else {
            self.view.as_ref().map_or(0, |view| view.functions.len())
        }
    }

    pub(crate) fn live_mode(&self) -> bool {
        self.rpc_status == RpcStatus::Live && self.live.is_some()
    }

    pub(crate) fn rpc_label(&self) -> String {
        match self.rpc_status {
            RpcStatus::Connecting => "rpc connecting".to_string(),
            RpcStatus::Live => match self.rpc_read_ms {
                Some(ms) => format!("rpc live ({ms:.0} ms)"),
                None => "rpc live".to_string(),
            },
            RpcStatus::Off if self.rpc_disabled => "rpc off (disabled)".to_string(),
            RpcStatus::Off => "rpc off".to_string(),
        }
    }

    pub(crate) fn unsaved(&self) -> bool {
        self.live_mode() && self.live.as_ref().is_some_and(LiveView::unsaved)
    }

    pub(crate) fn delta_sum(&self) -> Option<u32> {
        self.live.as_ref().map(LiveView::positive_delta)
    }

    /// Start the RPC worker after the editor is spawned. The baseline is the
    /// initial file scan captured in `App::new`.
    pub(crate) fn start_rpc(&mut self, address: String) {
        let baseline = self.initial.clone();
        let alive = Arc::clone(&self.child_alive);
        match rpc::spawn(address, self.target.clone(), baseline, alive) {
            Ok((handle, rx)) => {
                self.rpc = Some(handle);
                self.rpc_rx = Some(rx);
                self.rpc_status = RpcStatus::Connecting;
            }
            Err(e) => {
                self.rpc_status = RpcStatus::Off;
                self.rpc_note = Some(e);
            }
        }
    }

    pub(crate) fn shutdown_rpc(&mut self) {
        self.child_alive.store(false, Ordering::Relaxed);
        if let Some(handle) = self.rpc.take() {
            handle.shutdown();
        }
        self.rpc_rx = None;
    }

    /// Drain worker events without blocking the crossterm loop.
    pub(crate) fn poll_rpc(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.rpc_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for event in events {
            match event {
                RpcEvent::Status(status) => {
                    let was_live = self.rpc_status == RpcStatus::Live;
                    self.rpc_status = status;
                    if status == RpcStatus::Live {
                        self.rpc_note = None;
                    }
                    if was_live && status != RpcStatus::Live {
                        self.live = None;
                        self.rpc_read_ms = None;
                        self.rescan("rpc off → rescan");
                    }
                }
                RpcEvent::Snapshot(snapshot) => self.apply_snapshot(snapshot),
                RpcEvent::Note(note) => self.rpc_note = Some(note),
                RpcEvent::Modified { path, modified } => {
                    self.tabs.set_modified_loose(&path, modified);
                }
                RpcEvent::Closed {
                    path,
                    ok,
                    error,
                    current,
                } => self.apply_closed(path, ok, error, current),
                RpcEvent::Edited { path, ok, error } => self.apply_edited(path, ok, error),
            }
        }
        if disconnected {
            let was_live = self.rpc_status == RpcStatus::Live;
            self.rpc_status = RpcStatus::Off;
            self.rpc_rx = None;
            self.live = None;
            self.rpc_read_ms = None;
            if was_live {
                self.rescan("rpc closed → rescan");
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: LiveSnapshot) {
        if let Some(error) = snapshot.error {
            // Keep the last good sidebar; surface the failure instead.
            self.rpc_note = Some(error);
            return;
        }
        let Some(tier) = snapshot.tier else {
            return;
        };
        let baseline = rpc::baseline_map(&self.initial);
        let mut rows: Vec<LiveRow> = snapshot
            .functions
            .iter()
            .map(|function| {
                let key = rpc::function_key(function);
                let delta = baseline
                    .get(&key)
                    .map_or(i64::from(function.ccn), |before| {
                        i64::from(function.ccn) - i64::from(*before)
                    });
                LiveRow {
                    function: function.clone(),
                    delta,
                    is_new: !baseline.contains_key(&key),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.function
                .ccn
                .cmp(&a.function.ccn)
                .then_with(|| a.function.line.cmp(&b.function.line))
        });

        let previously_selected = self.selected_key();
        if let Some(key) = previously_selected {
            if let Some(index) = rows
                .iter()
                .position(|row| rpc::function_key(&row.function) == key)
            {
                self.selected = index;
            }
        }
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.rpc_read_ms = Some(snapshot.read_ms);
        let display_name = Path::new(&snapshot.name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| snapshot.name.clone());
        self.live = Some(LiveView {
            name: display_name,
            tier,
            functions: rows,
            sum: snapshot.sum,
            max: snapshot.max,
            changedtick: snapshot.changedtick,
            base_tick: snapshot.base_tick,
            read_ms: snapshot.read_ms,
        });
    }

    fn selected_key(&self) -> Option<String> {
        if self.live_mode() {
            self.live
                .as_ref()?
                .functions
                .get(self.selected)
                .map(|row| rpc::function_key(&row.function))
        } else {
            let view = self.view.as_ref()?;
            view.functions.get(self.selected).map(rpc::function_key)
        }
    }

    fn selected_line(&self) -> Option<usize> {
        if self.live_mode() {
            self.live
                .as_ref()?
                .functions
                .get(self.selected)
                .map(|row| row.function.line)
        } else {
            self.view
                .as_ref()?
                .functions
                .get(self.selected)
                .map(|function| function.line)
        }
    }

    /// Sidebar Enter/o: ask the worker to move Neovim's cursor, then focus the
    /// editor. Without a live connection this is a status message only.
    fn jump_selected(&mut self) {
        if self.rpc_status != RpcStatus::Live {
            self.set_status(
                "jump needs a live RPC connection (rpc off/connecting)".to_string(),
                true,
            );
            return;
        }
        let Some(line) = self.selected_line() else {
            return;
        };
        let Some(handle) = &self.rpc else {
            self.set_status("jump needs a live RPC connection".to_string(), true);
            return;
        };
        handle.jump(line);
        self.focus = Focus::Editor;
        self.set_status(format!("jump → line {line}"), false);
    }

    /// Open a file in the existing Neovim process, or select it when already
    /// open. Requires a live RPC connection: the PTY stream cannot reliably
    /// tell us which buffer is current.
    #[allow(dead_code)] // Wired by the split UI; the standalone session calls it through `open_file`.
    pub(crate) fn open_file(&mut self, path: &Path) -> Result<(), String> {
        let target =
            fs::canonicalize(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if target.is_dir() {
            return Err(format!(
                "{} is a directory; the workbench opens source files",
                target.display()
            ));
        }
        if !target.is_file() {
            return Err(format!("{} is not a regular file", target.display()));
        }

        if self.tabs.active_path() == Some(target.as_path()) {
            self.set_status(format!("tab: {} is already active", self.display), false);
            return Ok(());
        }
        if self.rpc_status != RpcStatus::Live || self.rpc.is_none() {
            return Err("tab switching needs a live Neovim RPC connection".to_string());
        }

        let display = display_path(&target);
        let was_new = !self.tabs.contains(&target);
        let previous = self.tabs.active_index();
        let index = self.tabs.open_or_select_named(&target, display.clone());
        match self.switch_to_index(index) {
            Ok(()) => {
                self.pending_switch = Some((target, previous, was_new));
                Ok(())
            }
            Err(error) => {
                if was_new {
                    self.tabs.remove_path_loose(&target);
                }
                if let Some(previous) = previous {
                    let _ = self.tabs.select(previous);
                }
                Err(error)
            }
        }
    }

    /// F2: previous tab (wraps).
    pub(crate) fn prev_tab(&mut self) -> Result<(), String> {
        if self.tabs.len() < 2 {
            self.set_status("only one tab is open".to_string(), false);
            return Ok(());
        }
        match self.tabs.prev_index() {
            Some(index) => self.switch_to_index(index),
            None => Err("no tabs are open".to_string()),
        }
    }

    /// F3: next tab (wraps).
    pub(crate) fn next_tab(&mut self) -> Result<(), String> {
        if self.tabs.len() < 2 {
            self.set_status("only one tab is open".to_string(), false);
            return Ok(());
        }
        match self.tabs.next_index() {
            Some(index) => self.switch_to_index(index),
            None => Err("no tabs are open".to_string()),
        }
    }

    /// F4: close the active tab.
    ///
    /// Dirty buffers are never discarded implicitly: a modified tab is left
    /// open and the footer tells the user to save first. Clean buffers get an
    /// async `:bdelete`; the tab is removed when Neovim confirms it.
    pub(crate) fn close_tab(&mut self) -> Result<(), String> {
        let Some(tab) = self.tabs.active().cloned() else {
            return Err("no tabs are open".to_string());
        };
        if tab.modified {
            self.set_status(
                format!(
                    "{} has unsaved changes — save it before closing (F4 does not force)",
                    tab.display
                ),
                true,
            );
            return Ok(());
        }
        if self.pending_close.is_some() {
            return Err("a tab close is already in progress".to_string());
        }
        if self.pending_switch.is_some() {
            return Err("a tab switch is in progress; close after it completes".to_string());
        }
        if self.rpc_status != RpcStatus::Live || self.rpc.is_none() {
            return Err("closing a buffer needs a live Neovim RPC connection".to_string());
        }
        if let Some(handle) = &self.rpc {
            handle.delete(tab.path.clone());
        }
        self.pending_close = Some(tab.path.clone());
        self.set_status(format!("closing {} …", tab.display), false);
        Ok(())
    }

    fn switch_to_index(&mut self, index: usize) -> Result<(), String> {
        if self.pending_switch.is_some() {
            return Err("a tab switch is already in progress".to_string());
        }
        if self.pending_close.is_some() {
            return Err("a tab close is in progress".to_string());
        }
        let Some(tab) = self.tabs.tab(index).cloned() else {
            return Err(format!("tab {} does not exist", index + 1));
        };
        if self.rpc_status != RpcStatus::Live || self.rpc.is_none() {
            return Err("tab switching needs a live Neovim RPC connection".to_string());
        }
        let previous = self.tabs.active_index();
        if let Some(handle) = &self.rpc {
            handle.edit(tab.path.clone());
        }
        self.tabs.select(index);
        self.pending_switch = Some((tab.path.clone(), previous, false));
        self.sync_active_tab();
        self.set_status(
            format!("tab {}/{} · {}", index + 1, self.tabs.len(), self.display),
            false,
        );
        Ok(())
    }

    /// Point the scan/sidebar at the selected tab and reset the live baseline
    /// so deltas always compare against the file being shown.
    fn sync_active_tab(&mut self) {
        let Some(tab) = self.tabs.active().cloned() else {
            self.set_status(
                "all tabs closed — F10 returns to the dashboard".to_string(),
                false,
            );
            return;
        };
        self.target = tab.path;
        self.display = tab.display;
        self.live = None;
        self.rpc_read_ms = None;
        self.selected = 0;
        self.rescan("tab");
        self.initial = self
            .view
            .as_ref()
            .map_or_else(Vec::new, |view| view.functions.clone());
        self.mtime = file_mtime(&self.target);
    }

    fn apply_closed(
        &mut self,
        path: PathBuf,
        ok: bool,
        error: Option<String>,
        current: Option<PathBuf>,
    ) {
        let pending = self
            .pending_close
            .as_deref()
            .is_some_and(|pending| path_key(pending) == path_key(&path));
        if !ok {
            if pending {
                self.pending_close = None;
            }
            let message = error.unwrap_or_else(|| "unknown error".to_string());
            // Neovim's E89 means the buffer is modified; keep the dirty marker
            // in sync so the next F4 does not retry blindly.
            if message.contains("E89") || message.to_ascii_lowercase().contains("modified") {
                self.tabs.set_modified_loose(&path, true);
            }
            self.set_status(format!("close failed: {message}"), true);
            return;
        }
        if !pending {
            return;
        }
        self.pending_close = None;
        if self.tabs.remove_path_loose(&path).is_none() {
            return;
        }
        if self.tabs.is_empty() {
            self.set_status(
                "all tabs closed — F10 returns to the dashboard".to_string(),
                false,
            );
            return;
        }
        // `:bdelete` chooses the next buffer itself; follow that exact buffer
        // instead of guessing a neighbour. Otherwise an autocmd that blocks an
        // explicit `:edit` leaves the tab bar and Neovim on different files.
        if let Some(current) = current {
            if let Some(index) = self.tabs.index_of_loose(&current) {
                self.tabs.select(index);
                self.sync_active_tab();
                self.set_status(
                    format!("closed {path_display}", path_display = path.display()),
                    false,
                );
                return;
            }
        }
        // No usable current-buffer name (non-file buffer or RPC hiccup): fall
        // back to a tracked switch so an async failure can still roll back.
        if let Some(index) = self.tabs.active_index() {
            if let Err(error) = self.switch_to_index(index) {
                self.set_status(format!("tab switch failed: {error}"), true);
                return;
            }
        }
        self.set_status(
            format!("closed {path_display}", path_display = path.display()),
            false,
        );
    }

    fn apply_edited(&mut self, path: PathBuf, ok: bool, error: Option<String>) {
        let matches = self
            .pending_switch
            .as_ref()
            .is_some_and(|(pending, _, _)| path_key(pending) == path_key(&path));
        if !matches {
            return;
        }
        let (_, previous, was_new) = self.pending_switch.take().expect("checked above");
        if ok {
            return;
        }

        if was_new {
            self.tabs.remove_path_loose(&path);
        }
        if let Some(index) = previous {
            let _ = self.tabs.select(index);
        }
        if self.tabs.active_index().is_some() {
            self.sync_active_tab();
        }
        let message = error.unwrap_or_else(|| "unknown error".to_string());
        self.set_status(format!("tab switch failed: {message}"), true);
    }

    /// Periodically ask Neovim which listed buffers are modified.
    pub(crate) fn maybe_query_modified(&mut self) {
        if self.rpc_status != RpcStatus::Live || self.last_dirty_check.elapsed() < DIRTY_INTERVAL {
            return;
        }
        self.last_dirty_check = Instant::now();
        let paths = self.tabs.paths();
        if paths.is_empty() {
            return;
        }
        if let Some(handle) = &self.rpc {
            handle.query_modified(paths);
        }
    }

    pub(crate) fn grade(&self) -> (char, &'static str) {
        let Some(view) = &self.view else {
            return ('-', "no data");
        };
        let max = if view.count == 0 {
            view.file_ccn.unwrap_or(0)
        } else {
            view.max
        };
        match max {
            0..=5 => ('A', "low complexity"),
            6..=10 => ('B', "moderate"),
            11..=20 => ('C', "elevated"),
            21..=50 => ('D', "high"),
            _ => ('E', "very high"),
        }
    }

    pub(crate) fn status_is_error(&self) -> bool {
        self.status_error
    }

    fn set_status(&mut self, message: String, error: bool) {
        self.status = message;
        self.status_error = error;
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Editor => Focus::Sidebar,
            Focus::Sidebar => Focus::Editor,
        };
        match self.focus {
            Focus::Sidebar => self.set_status(
                "sidebar focused · j/k move · g/G top/bottom · r rescan · F9 back".to_string(),
                false,
            ),
            Focus::Editor => {
                self.set_status("editor focused · keys go to the editor".to_string(), false)
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let count = self.function_count();
        if count == 0 {
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, count as i32 - 1) as usize;
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.function_count().saturating_sub(1);
    }

    /// Hard re-scan of the open file. `prefix` describes why (startup, save,
    /// manual) and becomes the footer status.
    fn rescan(&mut self, prefix: &str) {
        let started = Instant::now();
        let options = ScanOptions {
            root: self.target.clone(),
            verify_cache: false,
            ..Default::default()
        };
        let result = scan(&options);
        self.scan_ms = started.elapsed().as_millis();
        match result {
            Ok(scan_result) => match scan_result.files.into_iter().next() {
                Some(file) => {
                    let view = FileView::from_file(file);
                    let message = format!(
                        "{prefix}: {} functions · {} LOC · {} ms",
                        view.functions.len(),
                        view.loc,
                        self.scan_ms
                    );
                    self.view = Some(view);
                    self.scan_error = None;
                    self.selected = self.selected.min(self.function_count().saturating_sub(1));
                    self.set_status(message, false);
                }
                None => {
                    let message = "scan returned no file record".to_string();
                    self.scan_error = Some(message.clone());
                    self.set_status(format!("scan error: {message}"), true);
                }
            },
            Err(error) => {
                let message = error.to_string();
                self.scan_error = Some(message.clone());
                self.set_status(format!("scan error: {message}"), true);
            }
        }
        self.mtime = file_mtime(&self.target);
    }

    /// Save detection: mtime changed since the last scan.
    fn check_mtime(&mut self) {
        let now = file_mtime(&self.target);
        let changed = match (self.mtime, now) {
            (Some(before), Some(after)) => before != after,
            (None, Some(_)) => true,
            _ => false,
        };
        if changed {
            self.rescan("saved → rescanned");
        }
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn display_path(path: &Path) -> String {
    // `fs::canonicalize` returns a verbatim path on Windows (`\\?\C:\...`);
    // strip the prefix so the header shows the path the user typed.
    let cleaned = native_path(path);
    let cleaned = Path::new(&cleaned);
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_text = cwd.display().to_string();
        let cwd_clean = cwd_text.strip_prefix(r"\\?\").unwrap_or(&cwd_text);
        if let Ok(relative) = cleaned.strip_prefix(cwd_clean) {
            return relative.display().to_string();
        }
    }
    cleaned.display().to_string()
}

/// Path string without the Windows verbatim prefix, for the child and UI.
fn native_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

pub fn run(args: Vec<OsString>) -> i32 {
    let cli = Cli::parse_from(args);
    if cli.smoke {
        return report_smoke(run_smoke(&cli, false));
    }
    if cli.smoke_nvim {
        return report_smoke(run_smoke(&cli, true));
    }
    if cli.smoke_utf8 {
        return report_smoke(run_smoke_utf8(&cli));
    }
    if cli.smoke_rpc {
        return report_smoke(run_smoke_rpc(&cli, false));
    }
    if cli.smoke_rpc_edit {
        return report_smoke(run_smoke_rpc(&cli, true));
    }
    match run_workbench(&cli) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("ccn workbench: {message}");
            1
        }
    }
}

/// Launch the workbench for a single file, used by the dashboard `e` key.
pub fn run_path(path: &Path, editor: &str) -> i32 {
    run(vec![
        OsString::from("ccn workbench"),
        path.as_os_str().to_os_string(),
        OsString::from("--editor"),
        OsString::from(editor),
    ])
}

fn report_smoke(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("smoke failed: {message}");
            1
        }
    }
}

/// A workbench view that does not own the terminal.
///
/// The standalone `ccn workbench` command wraps this in a terminal guard; the
/// unified dashboard embeds it as a tab and keeps one terminal/event loop.
pub(crate) struct WorkbenchSession {
    pub(crate) app: App,
    pub(crate) child: Pty,
    last_mtime_check: Instant,
}

impl WorkbenchSession {
    pub(crate) fn open(
        target: &Path,
        editor_cmd: &str,
        no_rpc: bool,
        width: u16,
        height: u16,
    ) -> Result<Self, String> {
        let target = fs::canonicalize(target)
            .map_err(|e| format!("cannot open {}: {e}", target.display()))?;
        if target.is_dir() {
            return Err(format!(
                "{} is a directory; the workbench opens one source file",
                target.display()
            ));
        }
        if !target.is_file() {
            return Err(format!("{} is not a regular file", target.display()));
        }

        let editor = pty::parse_command(editor_cmd)?;
        let program = editor
            .first()
            .cloned()
            .ok_or_else(|| "empty --editor command".to_string())?;
        let label = Path::new(&program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.clone());

        let rpc_address = if rpc::is_nvim_program(&program) && !no_rpc {
            rpc::free_listen_address().ok()
        } else {
            None
        };
        let mut app = App::new(target.clone(), display_path(&target), label);
        app.rpc_disabled = rpc_address.is_none();

        let editor_rect = ui::editor_inner(ui::layout(Rect::new(0, 0, width, height)).editor);
        let cwd = target.parent().unwrap_or_else(|| Path::new("."));
        let mut editor_args = editor[1..].to_vec();
        if let Some(address) = &rpc_address {
            editor_args.push("--listen".to_string());
            editor_args.push(address.clone());
        }
        editor_args.push(native_path(&target));
        let child = Pty::spawn(
            &program,
            &editor_args,
            cwd,
            editor_rect.height,
            editor_rect.width,
        )
        .map_err(|e| {
            format!(
                "{e}\n  editor: {editor_cmd}\n  hint: put Neovim on PATH (`nvim --version` should work), or pass\n        --editor \"<path-to-nvim>\" (Windows portable:\n        --editor \"C:\\Tools\\nvim\\bin\\nvim.exe\").\n        AlmaLinux/RHEL 8: sudo dnf install neovim (EPEL)."
            )
        })?;
        if let Some(address) = rpc_address {
            app.start_rpc(address);
        }
        Ok(Self {
            app,
            child,
            last_mtime_check: Instant::now(),
        })
    }

    pub(crate) fn tick(&mut self) {
        self.app.poll_rpc();
        self.app.maybe_query_modified();
        if self.app.exited.is_none() {
            if let Some(note) = self.child.exit_status() {
                let empty = self.child.screen_text().trim().is_empty();
                let hint = if empty {
                    " (empty screen ? check --editor)"
                } else {
                    ""
                };
                self.app.exited = Some(note.clone());
                self.app.child_alive.store(false, Ordering::Relaxed);
                self.app.shutdown_rpc();
                self.app.set_status(
                    format!("[editor exited: {note}]{hint} ? F10 returns to the dashboard"),
                    note != "exit 0" || empty,
                );
            }
        }
        if !self.app.live_mode() && self.last_mtime_check.elapsed() >= MTIME_INTERVAL {
            self.last_mtime_check = Instant::now();
            self.app.check_mtime();
        }
    }

    pub(crate) fn draw_in(&self, frame: &mut Frame, area: Rect) {
        let parser = self.child.parser();
        let screen = match parser.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        ui::render_area(frame, &self.app, screen.screen(), area);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        handle_key(&mut self.app, &mut self.child, key)
    }

    /// Tabs currently open, in order.
    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn tabs(&self) -> &Tabs {
        &self.app.tabs
    }

    /// The active tab, or `None` after the last tab was closed.
    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn active_tab(&self) -> Option<&Tab> {
        self.app.tabs.active()
    }

    /// Open another file in the same Neovim instance.
    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn open_file(&mut self, path: &Path) -> Result<(), String> {
        self.app.open_file(path)
    }

    /// Restore the ordered buffer list from a saved session without opening
    /// each file in Neovim yet. The first switch to a restored tab issues the
    /// normal RPC `:edit`, so startup stays cheap.
    pub(crate) fn adopt_tabs(&mut self, paths: &[(PathBuf, String)]) {
        for (path, display) in paths {
            if path.is_file() {
                self.app.tabs.add_inactive(path, display.clone());
            }
        }
    }

    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn next_tab(&mut self) -> Result<(), String> {
        self.app.next_tab()
    }

    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn prev_tab(&mut self) -> Result<(), String> {
        self.app.prev_tab()
    }

    #[allow(dead_code)] // Public surface for the split UI / orchestrator.
    pub(crate) fn close_tab(&mut self) -> Result<(), String> {
        self.app.close_tab()
    }

    /// Forward a terminal bracketed paste to the embedded editor. Without the
    /// bracketed markers Neovim treats a multi-line paste as autoindented key
    /// input; with them it behaves like a real paste.
    pub(crate) fn paste(&mut self, text: &str) {
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        if let Err(error) = self.child.send(&bytes) {
            self.app
                .set_status(format!("paste to editor failed: {error}"), true);
        }
    }

    /// Jump the embedded editor to an absolute line and focus it. Used by the
    /// baseline review panel.
    pub(crate) fn jump_line(&mut self, line: usize) {
        if self.app.rpc_status != RpcStatus::Live {
            self.app
                .set_status("jump needs a live RPC connection".to_string(), true);
            return;
        }
        if let Some(handle) = &self.app.rpc {
            handle.jump(line);
            self.app.focus = Focus::Editor;
            self.app
                .set_status(format!("review jump -> line {line}"), false);
        }
    }

    pub(crate) fn resize(&mut self, width: u16, height: u16) {
        let editor = ui::editor_inner(ui::layout(Rect::new(0, 0, width, height)).editor);
        self.child.resize(editor.height, editor.width);
    }

    pub(crate) fn close(mut self) {
        self.app.shutdown_rpc();
        self.child.kill();
    }
}

fn run_workbench(cli: &Cli) -> Result<(), String> {
    let requested = cli.target.as_deref().ok_or_else(|| {
        "missing <path-or-file> (usage: ccn workbench [--editor <cmd>] <file>)".to_string()
    })?;
    let target = fs::canonicalize(requested)
        .map_err(|e| format!("cannot open {}: {e}", requested.display()))?;
    if target.is_dir() {
        return Err(format!(
            "{} is a directory; this spike opens one file (pass a C/C++ source file)",
            target.display()
        ));
    }
    if !target.is_file() {
        return Err(format!("{} is not a regular file", target.display()));
    }

    let editor = pty::parse_command(&cli.editor)?;
    let program = editor
        .first()
        .cloned()
        .ok_or_else(|| "empty --editor command".to_string())?;
    let label = Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.clone());

    let rpc_address = if rpc::is_nvim_program(&program) && !cli.no_rpc {
        match rpc::free_listen_address() {
            Ok(address) => Some(address),
            Err(e) => {
                eprintln!("ccn-workbench: RPC disabled: {e}");
                None
            }
        }
    } else {
        None
    };
    let mut app = App::new(target.clone(), display_path(&target), label);
    app.rpc_disabled = rpc_address.is_none();

    terminal::enable_raw_mode().map_err(|e| format!("cannot enable raw mode: {e}"))?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        cursor::Hide
    )
    .map_err(|e| format!("cannot enter the alternate screen: {e}"))?;
    install_panic_hook();

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(e) => {
            let _ = restore_terminal();
            return Err(format!("cannot initialise the terminal: {e}"));
        }
    };
    let size = match terminal.size() {
        Ok(size) => size,
        Err(e) => {
            let _ = restore_terminal();
            return Err(format!("cannot query the terminal size: {e}"));
        }
    };
    let editor_rect = ui::editor_inner(ui::layout(Rect::new(0, 0, size.width, size.height)).editor);
    let cwd = target.parent().unwrap_or_else(|| Path::new("."));
    let mut editor_args = editor[1..].to_vec();
    if let Some(address) = &rpc_address {
        editor_args.push("--listen".to_string());
        editor_args.push(address.clone());
    }
    editor_args.push(native_path(&target));
    let mut child = match Pty::spawn(
        &program,
        &editor_args,
        cwd,
        editor_rect.height,
        editor_rect.width,
    ) {
        Ok(child) => child,
        Err(e) => {
            let _ = restore_terminal();
            return Err(format!(
                "{e}\n  editor: {}\n  hint: put Neovim on PATH (`nvim --version` should work), or pass\n        --editor \"<path-to-nvim>\" (Windows portable:\n        --editor \"C:\\Tools\\nvim\\bin\\nvim.exe\").\n        AlmaLinux/RHEL 8: sudo dnf install neovim (EPEL).",
                cli.editor
            ));
        }
    };
    if let Some(address) = rpc_address {
        app.start_rpc(address);
    }

    let loop_result = event_loop(&mut terminal, &mut app, &mut child);

    app.shutdown_rpc();
    let exit_note = app.exited.clone();
    let screen_text = child.screen_text();
    child.kill();
    let restore_result = restore_terminal();
    if let Some(note) = exit_note {
        let last_screen = screen_tail(&screen_text, 8);
        if screen_text.trim().is_empty() {
            eprintln!("editor exited immediately ({note}) and produced no output.");
            eprintln!(
                "  hint: check --editor (currently {}) or that the editor starts in a terminal.",
                cli.editor
            );
        } else if note != "exit 0" {
            eprintln!("editor {note}; last screen:");
            for line in last_screen {
                eprintln!("  {line}");
            }
        }
    }
    restore_result.map_err(|e| format!("cannot restore the terminal: {e}"))?;
    loop_result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    child: &mut Pty,
) -> Result<(), String> {
    let mut last_mtime_check = Instant::now();
    loop {
        app.poll_rpc();
        app.maybe_query_modified();
        if event::poll(Duration::from_millis(16)).map_err(|e| format!("poll failed: {e}"))? {
            match event::read().map_err(|e| format!("read event failed: {e}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(app, child, key) {
                        app.shutdown_rpc();
                        return Ok(());
                    }
                }
                Event::Paste(text) => {
                    if app.focus == Focus::Editor {
                        let mut bytes = Vec::with_capacity(text.len() + 12);
                        bytes.extend_from_slice(b"\x1b[200~");
                        bytes.extend_from_slice(text.as_bytes());
                        bytes.extend_from_slice(b"\x1b[201~");
                        if let Err(error) = child.send(&bytes) {
                            app.set_status(format!("paste to editor failed: {error}"), true);
                        }
                    }
                }
                Event::Resize(width, height) => {
                    let editor =
                        ui::editor_inner(ui::layout(Rect::new(0, 0, width, height)).editor);
                    child.resize(editor.height, editor.width);
                }
                _ => {}
            }
        }

        if app.exited.is_none() {
            if let Some(note) = child.exit_status() {
                let empty = child.screen_text().trim().is_empty();
                let hint = if empty {
                    " (empty screen — check --editor)"
                } else {
                    ""
                };
                app.exited = Some(note.clone());
                app.child_alive.store(false, Ordering::Relaxed);
                app.shutdown_rpc();
                app.set_status(
                    format!("[editor exited: {note}]{hint} — F10 quits"),
                    note != "exit 0" || empty,
                );
            }
        }

        // RPC live is the source of truth; mtime polling is only the fallback.
        if !app.live_mode() && last_mtime_check.elapsed() >= MTIME_INTERVAL {
            last_mtime_check = Instant::now();
            app.check_mtime();
        }

        draw(terminal, app, child)?;
    }
}

fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &App,
    child: &Pty,
) -> Result<(), String> {
    let parser = child.parser();
    let screen = match parser.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    terminal
        .draw(|frame| ui::render(frame, app, screen.screen()))
        .map_err(|e| format!("draw failed: {e}"))?;
    Ok(())
}

/// Returns `true` when the workbench should quit.
fn handle_key(app: &mut App, child: &mut Pty, key: KeyEvent) -> bool {
    if key.code == KeyCode::F(10) {
        return key.kind == KeyEventKind::Press;
    }
    if key.code == KeyCode::F(9) {
        if key.kind == KeyEventKind::Press {
            app.toggle_focus();
        }
        return false;
    }
    // Tab keys are owned by the workbench and are never forwarded to Neovim.
    let tab_action = match key.code {
        KeyCode::F(2) => Some(TabAction::Prev),
        KeyCode::F(3) => Some(TabAction::Next),
        KeyCode::F(4) => Some(TabAction::Close),
        _ => None,
    };
    if let Some(action) = tab_action {
        if key.kind == KeyEventKind::Press {
            let result = match action {
                TabAction::Prev => app.prev_tab(),
                TabAction::Next => app.next_tab(),
                TabAction::Close => app.close_tab(),
            };
            if let Err(e) = result {
                app.set_status(e, true);
            }
        }
        return false;
    }

    match app.focus {
        Focus::Editor => {
            if let Some(bytes) = encode_key(&key) {
                if let Err(e) = child.send(&bytes) {
                    app.set_status(format!("send to editor failed: {e}"), true);
                }
            }
        }
        Focus::Sidebar => {
            if key.kind != KeyEventKind::Press {
                return false;
            }
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            if !plain {
                return false;
            }
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
                KeyCode::Char('g') | KeyCode::Home => app.select_first(),
                KeyCode::Char('G') | KeyCode::End => app.select_last(),
                KeyCode::Char('r') => app.rescan("manual rescan"),
                KeyCode::Enter | KeyCode::Char('o') => app.jump_selected(),
                KeyCode::Esc => app.toggle_focus(),
                _ => {}
            }
        }
    }
    false
}

/// Translate a crossterm key into the bytes a terminal would send.
fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let base = match key.code {
        KeyCode::Char(ch) => {
            if ctrl {
                let code = ch as u32;
                if code < 0x20 {
                    vec![code as u8]
                } else if code < 0x80 {
                    vec![(code as u8) & 0x1f]
                } else {
                    return None;
                }
            } else {
                let mut buffer = [0u8; 4];
                ch.encode_utf8(&mut buffer).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(number) => function_key(number)?,
        _ => return None,
    };

    if alt && key.code != KeyCode::Esc {
        let mut buffer = Vec::with_capacity(base.len() + 1);
        buffer.push(0x1b);
        buffer.extend_from_slice(&base);
        Some(buffer)
    } else {
        Some(base)
    }
}

fn function_key(number: u8) -> Option<Vec<u8>> {
    let sequence = match number {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    Some(sequence.as_bytes().to_vec())
}

/// `--smoke` / `--smoke-nvim`: run the real PTY + vt100 path without the
/// interactive terminal, then assert on the parsed screen.
fn run_smoke(cli: &Cli, nvim: bool) -> Result<(), String> {
    let temp = TempDir::new()?;
    let file = temp.path.join("ccn_workbench_smoke.c");
    fs::write(&file, SMOKE_SOURCE).map_err(|e| format!("cannot write the smoke file: {e}"))?;

    let (program, mut args, label) = if nvim {
        let parts = pty::parse_command(&cli.editor)?;
        let program = parts[0].clone();
        if !pty::program_available(&program) {
            println!(
                "skip: editor not found: {program} (install Neovim or pass --editor \"<cmd>\")"
            );
            return Ok(());
        }
        (program, parts[1..].to_vec(), "nvim")
    } else if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/c".to_string(), "echo CCN_SMOKE".to_string()],
            "cmd /c echo",
        )
    } else {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "echo CCN_SMOKE".to_string()],
            "sh -c echo",
        )
    };

    if nvim {
        args.push(file.display().to_string());
    }
    let app = App::new(file.clone(), file.display().to_string(), label.to_string());
    let mut child = Pty::spawn(&program, &args, &temp.path, 24, 80)?;

    let visual = io::stdout().is_terminal();
    let mut terminal = if visual {
        let _ = execute!(io::stdout(), EnterAlternateScreen, cursor::Hide);
        match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => Some(terminal),
            Err(e) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
                eprintln!("smoke: no drawable terminal ({e}); asserting headless");
                None
            }
        }
    } else {
        None
    };

    let duration = if nvim {
        Duration::from_millis(2500)
    } else {
        Duration::from_millis(1000)
    };
    let started = Instant::now();
    while started.elapsed() < duration {
        if let Some(terminal) = terminal.as_mut() {
            draw(terminal, &app, &child).ok();
        }
        let _ = child.exit_status();
        std::thread::sleep(Duration::from_millis(33));
    }
    if terminal.is_some() {
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
    drop(terminal);

    let text = child.screen_text();
    child.kill();

    let result = if nvim {
        let needle = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("--- nvim screen tail ---");
        for line in screen_tail(&text, 12) {
            println!("{line}");
        }
        let lower = text.to_ascii_lowercase();
        if text.contains(&needle) || lower.contains("file") {
            println!("smoke-nvim ok");
            Ok(())
        } else {
            Err(format!(
                "nvim screen did not show {needle}; captured {} bytes",
                text.len()
            ))
        }
    } else if text.contains("CCN_SMOKE") {
        println!("smoke ok");
        Ok(())
    } else {
        for line in screen_tail(&text, 8) {
            eprintln!("  {line}");
        }
        Err("the vt100 screen never contained CCN_SMOKE".to_string())
    };
    result
}

/// `--smoke-utf8`: prove the UTF-8 byte path through the real PTY.
///
/// This types `한글` with the same `encode_key` the UI uses, saves with `:wq`,
/// and asserts the file contains the bytes. It does not test OS IME
/// composition: the characters enter as already-composed UTF-8.
fn run_smoke_utf8(cli: &Cli) -> Result<(), String> {
    let temp = TempDir::new()?;
    let file = temp.path.join("ccn_workbench_utf8.c");
    fs::write(&file, "int utf8_probe(void) { return 0; }\n")
        .map_err(|e| format!("cannot write the UTF-8 smoke file: {e}"))?;

    let parts = pty::parse_command(&cli.editor)?;
    let program = parts[0].clone();
    if !pty::program_available(&program) {
        println!("skip: editor not found: {program} (install Neovim or pass --editor \"<cmd>\")");
        return Ok(());
    }
    let mut args = parts[1..].to_vec();
    args.push(native_path(&file));
    let mut child = Pty::spawn(&program, &args, &temp.path, 24, 80)?;

    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Give Neovim its normal startup window, then wait (briefly) for the file
    // to be visible before typing so a cold start cannot eat the keystrokes.
    std::thread::sleep(Duration::from_millis(1200));
    let ready = Instant::now() + Duration::from_millis(2000);
    while Instant::now() < ready && !child.screen_text().contains(&name) {
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut keys = vec![KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)];
    keys.extend(
        "한글"
            .chars()
            .map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
    );
    keys.push(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    keys.extend(
        ":wq"
            .chars()
            .map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
    );
    keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    for key in keys {
        let bytes = encode_key(&key)
            .ok_or_else(|| format!("cannot encode key {:?} for the editor", key.code))?;
        child
            .send(&bytes)
            .map_err(|e| format!("cannot send key {:?}: {e}", key.code))?;
        std::thread::sleep(Duration::from_millis(40));
    }

    let deadline = Instant::now() + Duration::from_millis(5000);
    let mut exited = false;
    while Instant::now() < deadline {
        if child.exit_status().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let screen = child.screen_text();
    child.kill();

    let content = fs::read_to_string(&file)
        .unwrap_or_else(|e| format!("<cannot read {}: {e}>", file.display()));
    if exited && content.contains("한글") {
        println!("smoke-utf8 ok");
        return Ok(());
    }

    println!("--- screen tail ---");
    for line in screen_tail(&screen, 12) {
        println!("{line}");
    }
    println!("--- file content ---");
    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    if !exited {
        Err("the editor did not exit after :wq".to_string())
    } else {
        Err("the saved file does not contain the Korean text".to_string())
    }
}

/// `--smoke-rpc` / `--smoke-rpc-edit`: prove the msgpack-RPC path without the
/// workbench UI. Pure RPC calls only, so the same binary works on 0.8 and 0.12.
fn run_smoke_rpc(cli: &Cli, edit: bool) -> Result<(), String> {
    let temp = TempDir::new()?;
    let file = temp.path.join("ccn_workbench_rpc.c");
    let source = "int simple(int x) {\n    return x;\n}\n\nint branchy(int x) {\n    if (x > 0) {\n        return 1;\n    }\n    return 0;\n}\n";
    fs::write(&file, source).map_err(|e| format!("cannot write the RPC smoke file: {e}"))?;
    let other = temp.path.join("ccn_workbench_rpc_other.c");
    fs::write(&other, "int other(int x) {\n    return x;\n}\n")
        .map_err(|e| format!("cannot write the second RPC smoke file: {e}"))?;

    let parts = pty::parse_command(&cli.editor)?;
    let program = parts[0].clone();
    if !pty::program_available(&program) {
        println!("skip: editor not found: {program} (install Neovim or pass --editor \"<cmd>\")");
        return Ok(());
    }
    if !rpc::is_nvim_program(&program) {
        return Err(format!("--smoke-rpc requires Neovim, got {program}"));
    }
    let address = rpc::free_listen_address()?;
    let mut args = parts[1..].to_vec();
    args.push("--listen".to_string());
    args.push(address.clone());
    args.push(native_path(&file));
    let mut child = Pty::spawn(&program, &args, &temp.path, 30, 100)?;

    let result = smoke_rpc_session(&address, &file, &other, &mut child, edit);
    child.kill();
    result
}

fn smoke_rpc_session(
    address: &str,
    file: &Path,
    other: &Path,
    child: &mut Pty,
    edit: bool,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the smoke runtime: {e}"))?;
    let (nvim, io) = smoke_connect(&runtime, address)?;

    let started = Instant::now();
    let snapshot = smoke_snapshot(&runtime, &nvim, file)?;
    println!("buffer: {}", snapshot.name);
    println!("namespace: {}", rpc::NAMESPACE);
    println!(
        "tier: {}",
        snapshot.tier.map_or("n/a", crate::ccn::CcnTier::as_str)
    );
    println!("functions: {}", snapshot.functions.len());
    println!("sum: {}  max: {}", snapshot.sum, snapshot.max);
    println!("read_ms: {:.2}", snapshot.read_ms);
    println!("changedtick: {}", snapshot.changedtick);
    if snapshot.functions.len() != 2 {
        io.abort();
        return Err(format!(
            "expected 2 functions, got {}",
            snapshot.functions.len()
        ));
    }

    let mut live = snapshot.clone();
    if edit {
        let disk_before =
            fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        smoke_send_text(child, "ji")?;
        smoke_send_text(child, "if (x) x++;")?;
        child
            .send(b"\x1b")
            .map_err(|e| format!("cannot send Esc: {e}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut changed = None;
        while Instant::now() < deadline {
            let next = runtime.block_on(rpc::read_snapshot(&nvim, file, snapshot.changedtick));
            if next.error.is_none()
                && next.changedtick != snapshot.changedtick
                && next.sum > snapshot.sum
            {
                changed = Some(next);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(next) = changed else {
            io.abort();
            return Err(format!(
                "live CCN did not increase after typing (tick {} -> ?, sum {}); screen tail: {:?}",
                snapshot.changedtick,
                snapshot.sum,
                screen_tail(&child.screen_text(), 6)
            ));
        };
        let disk_after =
            fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        if disk_before != disk_after {
            io.abort();
            return Err(
                "the PTY edit reached the file on disk; :w should not have run".to_string(),
            );
        }
        println!(
            "edit: changedtick {} -> {}  sum {} -> {} (disk unchanged)",
            snapshot.changedtick, next.changedtick, snapshot.sum, next.sum
        );
        live = next;
    }

    let baseline = rpc::baseline_map(&snapshot.functions);
    let placed = runtime.block_on(rpc::annotate(&nvim, &live.functions, &baseline))?;
    let marks = runtime.block_on(rpc::dump_marks(&nvim))?;
    println!("extmarks placed: {placed}  dumped: {}", marks.len());
    for mark in &marks {
        println!("  row={} hl={} text={:?}", mark.row, mark.hl, mark.text);
    }
    let expected = live.functions.len();
    if placed != expected || marks.len() != expected {
        io.abort();
        return Err(format!(
            "expected {expected} extmarks, placed {placed}, dumped {}",
            marks.len()
        ));
    }
    if marks.iter().any(|mark| !mark.text.contains("CCN")) {
        io.abort();
        return Err("an extmark is missing its CCN virtual text".to_string());
    }

    // Multi-buffer: switch to a second file in the same Neovim process and
    // verify the active buffer name follows `:edit`.
    runtime.block_on(rpc::edit_file(&nvim, other))?;
    let switched = runtime.block_on(async {
        let buffer = nvim.get_current_buf().await?;
        buffer.get_name().await
    });
    let switched = switched.map_err(|e| format!("cannot read the active buffer: {e}"))?;
    println!("tab switch active buffer: {switched}");
    let expected = other
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if Path::new(&switched).file_name() != Some(std::ffi::OsStr::new(&expected)) {
        io.abort();
        return Err(format!(
            "expected the active buffer to be {expected}, got {switched}"
        ));
    }
    let modified = runtime.block_on(rpc::buffer_modified(&nvim, other))?;
    if modified {
        io.abort();
        return Err("the freshly opened second buffer should not be modified".to_string());
    }

    io.abort();
    println!(
        "elapsed_ms: {:.1}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    if edit {
        println!("smoke-rpc-edit ok");
    } else {
        println!("smoke-rpc ok");
    }
    Ok(())
}

fn smoke_connect(
    runtime: &tokio::runtime::Runtime,
    address: &str,
) -> Result<(rpc::Rpc, rpc::IoTask), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match runtime.block_on(rpc::connect(address)) {
            Ok(pair) => return Ok(pair),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("cannot connect to {address}: {e}")),
        }
    }
}

fn smoke_snapshot(
    runtime: &tokio::runtime::Runtime,
    nvim: &rpc::Rpc,
    file: &Path,
) -> Result<LiveSnapshot, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = runtime.block_on(rpc::read_snapshot(nvim, file, 0));
        if snapshot.error.is_none() && !snapshot.name.is_empty() {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no live snapshot after 10 s: {}",
                snapshot
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn smoke_send_text(child: &mut Pty, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        let bytes = encode_key(&key).ok_or_else(|| format!("cannot encode smoke key {ch:?}"))?;
        child
            .send(&bytes)
            .map_err(|e| format!("cannot send smoke key {ch:?}: {e}"))?;
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}

fn screen_tail(text: &str, count: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(count);
    lines.drain(..start);
    lines
}

fn restore_terminal() -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        cursor::Show
    )
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            cursor::Show
        );
        previous(info);
    }));
}

/// A temp directory removed on drop. Swap files inside go with it.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("ccn-workbench-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn temp_file(name: &str, contents: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let file = dir.path.join(name);
        fs::write(&file, contents).expect("write test source");
        (dir, file)
    }

    #[test]
    fn keys_encode_like_a_terminal() {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(encode_key(&press(KeyCode::Char('a'))).unwrap(), b"a");
        assert_eq!(
            encode_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap(),
            vec![3]
        );
        assert_eq!(encode_key(&press(KeyCode::Up)).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(&press(KeyCode::Enter)).unwrap(), b"\r");
        assert_eq!(encode_key(&press(KeyCode::Backspace)).unwrap(), vec![0x7f]);
        assert_eq!(
            encode_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)).unwrap(),
            b"\x1bx"
        );
        // F9/F10 are owned by the workbench, not forwarded.
        assert!(encode_key(&press(KeyCode::F(9))).is_none());
        assert!(encode_key(&press(KeyCode::F(10))).is_none());
    }

    #[test]
    fn save_detection_rescans_and_refreshes_the_sidebar() {
        let (_dir, file) = temp_file(
            "save.c",
            "int one(int x) { if (x) { return 1; } return 0; }\n",
        );
        let mut app = App::new(file.clone(), display_path(&file), "nvim".to_string());
        assert_eq!(app.function_count(), 1);
        assert!(app.status.starts_with("scan:"));

        std::thread::sleep(Duration::from_millis(30));
        fs::write(
            &file,
            "int one(int x) { return x; }\nint two(int y) { if (y) { return 2; } return 0; }\n",
        )
        .expect("rewrite test source");
        app.check_mtime();

        assert_eq!(app.function_count(), 2);
        assert!(app.status.contains("saved"));
        assert!(app.scan_error.is_none());
    }
}
