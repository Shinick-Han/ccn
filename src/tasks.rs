//! Build and test tasks: discovery, a non-blocking runner and error parsing.
//!
//! A task is a name plus one shell command (`task.build = "cargo build"` in
//! `<root>/.ccn.toml`). When the configuration declares none, [`detect`]
//! recognises the common build systems and synthesises a `build` and a `test`
//! task for them.
//!
//! The runner owns one worker thread per started task. The worker runs the
//! command through the platform shell, captures stdout and stderr and sends a
//! single [`TaskRun`] back over a channel; [`TaskState::poll`] drains that
//! channel, so the UI thread never waits for the child. Output is capped while
//! it is collected, and diagnostics are parsed best-effort so `Enter` can jump
//! to a compiler error.
//!
//! Rendering lives in [`crate::tasks_ui`]; [`render`] re-exports it so callers
//! can treat the whole feature as one module.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::config::Config;

/// Maximum number of tasks shown by the palette.
pub const MAX_TASKS: usize = 12;

/// Maximum number of distinct errors kept from one run.
pub const MAX_ERRORS: usize = 200;

/// Maximum number of captured output lines.
pub const MAX_OUTPUT_LINES: usize = 4_000;

/// Maximum number of captured output bytes.
pub const MAX_OUTPUT_BYTES: usize = 200_000;

/// Fallback page size before the overlay has reported its height.
const DEFAULT_PAGE: usize = 10;

/// What a task is for. Only affects the overlay's heading and ordering hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Build,
    Test,
    Other,
}

impl TaskKind {
    /// Heading word used by the overlay while this kind runs.
    pub fn label(self) -> &'static str {
        match self {
            TaskKind::Build => "Build",
            TaskKind::Test => "Test",
            TaskKind::Other => "Task",
        }
    }
}

/// One runnable task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub command: String,
    pub kind: TaskKind,
}

/// One diagnostic lifted from the task output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    /// Path exactly as printed by the compiler; the caller resolves it against
    /// the scan root.
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, when the tool printed one.
    pub column: Option<usize>,
    pub message: String,
}

/// What the task overlay asks the host to do with one key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAction {
    /// Hide the overlay. A running process keeps running.
    Close,
    /// Open `path` at `line`; the caller resolves `path` against the scan root.
    Jump { path: String, line: usize },
    /// Nothing for the host to do.
    None,
}

/// Tasks implied by `config` and, when it declares none, by the files in
/// `root`.
///
/// Configured tasks win: they are listed first and suppress detection, so a
/// project that adds one task does not also get the detected pair. Names are
/// unique and order is preserved; at most [`MAX_TASKS`] tasks are returned.
pub fn detect(root: &Path, config: &Config) -> Vec<Task> {
    let mut tasks = Vec::new();
    let mut seen = BTreeSet::new();

    for (name, command) in &config.tasks {
        push_task(&mut tasks, &mut seen, name, command);
    }

    if config.tasks.is_empty() {
        for (name, command) in detected_commands(root) {
            push_task(&mut tasks, &mut seen, &name, &command);
        }
    }

    tasks
}

fn push_task(tasks: &mut Vec<Task>, seen: &mut BTreeSet<String>, name: &str, command: &str) {
    if tasks.len() >= MAX_TASKS || name.trim().is_empty() || command.trim().is_empty() {
        return;
    }
    if !seen.insert(name.to_string()) {
        return;
    }
    tasks.push(Task {
        name: name.to_string(),
        command: command.to_string(),
        kind: kind_for(name),
    });
}

/// A test-ish name makes a `Test` task, a build-ish name a `Build` task; the
/// rest stay `Other` and are never filtered out.
fn kind_for(name: &str) -> TaskKind {
    let name = name.to_ascii_lowercase();
    if name.contains("test") {
        TaskKind::Test
    } else if name.contains("build") || name.contains("compile") {
        TaskKind::Build
    } else {
        TaskKind::Other
    }
}

/// Detected `(name, command)` pairs in preference order. Later duplicates lose
/// in [`detect`], so a repository with both `Cargo.toml` and `CMakeLists.txt`
/// gets the Cargo pair.
fn detected_commands(root: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();

    if root.join("Cargo.toml").is_file() {
        found.push(("build".to_string(), "cargo build".to_string()));
        found.push(("test".to_string(), "cargo test".to_string()));
    }

    if root.join("CMakeLists.txt").is_file() {
        found.push((
            "build".to_string(),
            "cmake -S . -B build && cmake --build build".to_string(),
        ));
        found.push((
            "test".to_string(),
            "ctest --test-dir build --output-on-failure".to_string(),
        ));
    }

    if root.join("Makefile").is_file() || root.join("makefile").is_file() {
        found.push(("build".to_string(), "make".to_string()));
        found.push(("test".to_string(), "make test".to_string()));
    }

    if root.join("meson.build").is_file() {
        found.push(("build".to_string(), "meson compile -C build".to_string()));
        found.push(("test".to_string(), "meson test -C build".to_string()));
    }

    found.extend(node_scripts(root));
    found
}

/// Build/test scripts from `package.json`. A script only counts when it is
/// present and a string; the command is always `npm run <script>` so the
/// package manager's own environment is applied.
fn node_scripts(root: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Object(package)) = serde_json::from_str(&text) else {
        return Vec::new();
    };
    let Some(serde_json::Value::Object(scripts)) = package.get("scripts") else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for script in ["build", "test"] {
        if scripts
            .get(script)
            .is_some_and(serde_json::Value::is_string)
        {
            found.push((script.to_string(), format!("npm run {script}")));
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Error parsing
// ---------------------------------------------------------------------------

/// Extract distinct compiler diagnostics from captured output.
///
/// Recognised shapes:
///
/// - `path:line:col: error: message` and `path:line: error` (gcc, clang, tsc)
/// - `path(line,col): error C1234: message` (MSVC)
/// - `error[E0308]: message` followed by `--> path:line:col` (Rust, clippy)
///
/// The parser is best-effort: it never fails, never panics and silently skips
/// anything it does not recognise. Results are unique on
/// `(path, line, column, message)` and capped at [`MAX_ERRORS`].
pub fn parse_errors(output: &str) -> Vec<BuildError> {
    parse_errors_with_lines(output)
        .into_iter()
        .map(|(_, error)| error)
        .collect()
}

/// As [`parse_errors`], but keeping the 0-based output line each error came
/// from so the overlay can highlight it.
fn parse_errors_with_lines(output: &str) -> Vec<(usize, BuildError)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut found: Vec<(usize, BuildError)> = Vec::new();
    let mut seen: BTreeSet<(String, usize, Option<usize>, String)> = BTreeSet::new();

    for (index, raw) in lines.iter().enumerate() {
        if found.len() >= MAX_ERRORS {
            break;
        }
        let line = raw.trim_end_matches('\r');
        let candidate = rust_diagnostic(&lines, index)
            .map(|error| (index, error))
            .or_else(|| parse_diagnostic_line(line).map(|error| (index, error)));
        let Some((at, error)) = candidate else {
            continue;
        };
        let key = (
            error.path.clone(),
            error.line,
            error.column,
            error.message.clone(),
        );
        if seen.insert(key) {
            found.push((at, error));
        }
    }

    found
}

/// Rust prints the location on a `-->` line after the message, so the message
/// line alone never carries a path.
fn rust_diagnostic(lines: &[&str], index: usize) -> Option<BuildError> {
    let head = lines[index].trim();
    let is_head = head.starts_with("error[")
        || head.starts_with("error:")
        || head.starts_with("warning[")
        || head.starts_with("warning:");
    if !is_head {
        return None;
    }

    for ahead in 1..=2 {
        let Some(line) = lines.get(index + ahead) else {
            break;
        };
        let line = line.trim_start();
        // A new diagnostic starts its own group; never borrow its arrow for
        // the previous message (for example `error: could not compile`).
        if line.starts_with("error[")
            || line.starts_with("error:")
            || line.starts_with("warning[")
            || line.starts_with("warning:")
        {
            break;
        }
        let Some(rest) = line.strip_prefix("-->") else {
            continue;
        };
        let (path, line_number, column) = parse_location(rest.trim())?;
        return Some(BuildError {
            path,
            line: line_number,
            column,
            message: head.to_string(),
        });
    }
    None
}

/// `path:line[:col]: error|warning|fatal error [: message]`.
fn parse_diagnostic_line(line: &str) -> Option<BuildError> {
    if let Some(error) = msvc_diagnostic(line) {
        return Some(error);
    }

    for marker in [": fatal error", ": error", ": warning"] {
        let Some(position) = line.find(marker) else {
            continue;
        };
        let Some((path, line_number, column)) = parse_location(line[..position].trim()) else {
            continue;
        };
        return Some(BuildError {
            path,
            line: line_number,
            column,
            message: message_after(&line[position + 1..]),
        });
    }
    None
}

/// `path(line,col): error C1234: message` and `path(line): warning ...`.
fn msvc_diagnostic(line: &str) -> Option<BuildError> {
    let open = line.find('(')?;
    let close = open + line[open..].find(')')?;
    let after = line[close + 1..].trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if !(after.starts_with("error") || after.starts_with("warning")) {
        return None;
    }

    let inner = &line[open + 1..close];
    let (line_text, column_text) = match inner.split_once(',') {
        Some((line, column)) => (line.trim(), Some(column.trim())),
        None => (inner.trim(), None),
    };
    let line_number: usize = line_text.parse().ok()?;
    if line_number == 0 {
        return None;
    }
    let column = column_text
        .map(str::parse)
        .transpose()
        .ok()
        .flatten()
        .filter(|column| *column > 0);

    let path = line[..open].trim();
    if path.is_empty() {
        return None;
    }
    Some(BuildError {
        path: path.to_string(),
        line: line_number,
        column,
        message: message_after(after),
    })
}

/// Split `path:line`, `path:line:col`, or `path(line[,col])`-free text into a
/// path and numbers. Windows drive letters survive because only the trailing
/// numeric segments are peeled off.
fn parse_location(text: &str) -> Option<(String, usize, Option<usize>)> {
    let mut parts = text.rsplitn(3, ':');
    let last: usize = parts.next()?.parse().ok()?;
    let second = parts.next()?;
    let head = parts.next();

    let (line, column) = if is_number(second) {
        (second.parse().ok()?, Some(last))
    } else {
        (last, None)
    };
    if line == 0 {
        return None;
    }

    // With a column the head is the path; without one `second` is the path
    // unless a Windows drive (or another colon) precedes it.
    let path = if is_number(second) {
        head?.to_string()
    } else {
        match head {
            Some(head) => format!("{head}:{second}"),
            None => second.to_string(),
        }
    };
    if path.trim().is_empty() {
        return None;
    }
    Some((path, line, column))
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Drop the severity prefix from the text after the location.
fn message_after(rest: &str) -> String {
    let rest = rest.trim_start();
    for prefix in ["fatal error", "error", "warning"] {
        if let Some(tail) = rest.strip_prefix(prefix) {
            let tail = tail.trim_start();
            let tail = tail.strip_prefix(':').unwrap_or(tail).trim();
            return if tail.is_empty() {
                prefix.to_string()
            } else {
                tail.to_string()
            };
        }
    }
    rest.to_string()
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Final result of one worker thread.
#[derive(Debug)]
struct TaskRun {
    lines: Vec<String>,
    truncated: bool,
    exit: Option<i32>,
    failure: Option<String>,
    duration: Duration,
}

/// State of the task overlay and of the process it is watching.
///
/// Only one command runs at a time. Closing the overlay while it runs detaches
/// the view, not the child: the worker and the process keep going until they
/// finish on their own.
#[derive(Debug)]
pub struct TaskState {
    open: bool,
    running: bool,
    task: Option<Task>,
    root: PathBuf,
    started: Instant,
    elapsed: Duration,
    rx: Option<Receiver<TaskRun>>,
    lines: Vec<String>,
    truncated: bool,
    scroll: usize,
    /// Body height reported by the last render, used for paging and `G`.
    viewport: Cell<usize>,
    errors: Vec<BuildError>,
    /// For each error, the 0-based output line it was parsed from.
    error_lines: Vec<usize>,
    selected: usize,
    exit: Option<i32>,
    failure: Option<String>,
}

impl Default for TaskState {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskState {
    pub fn new() -> Self {
        Self {
            open: false,
            running: false,
            task: None,
            root: PathBuf::new(),
            started: Instant::now(),
            elapsed: Duration::ZERO,
            rx: None,
            lines: Vec::new(),
            truncated: false,
            scroll: 0,
            viewport: Cell::new(DEFAULT_PAGE),
            errors: Vec::new(),
            error_lines: Vec::new(),
            selected: 0,
            exit: None,
            failure: None,
        }
    }

    /// Open the overlay and start `task` in `root`.
    ///
    /// Starting a second command detaches the first receiver; the first child
    /// is not killed, matching `Esc` while running.
    pub fn start(&mut self, task: Task, root: &Path) {
        self.open = true;
        self.running = true;
        self.task = Some(task.clone());
        self.root = root.to_path_buf();
        self.started = Instant::now();
        self.elapsed = Duration::ZERO;
        self.lines.clear();
        self.truncated = false;
        self.scroll = 0;
        self.viewport.set(DEFAULT_PAGE);
        self.errors.clear();
        self.error_lines.clear();
        self.selected = 0;
        self.exit = None;
        self.failure = None;

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let command = task.command;
        let root = root.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("ccn-task".to_string())
            .spawn(move || {
                let run = run_command(&command, &root);
                // A detached view drops the receiver; the child still runs.
                let _ = tx.send(run);
            });
        if let Err(error) = spawned {
            self.rx = None;
            self.running = false;
            self.failure = Some(format!("could not start worker: {error}"));
        }
    }

    /// Drain the worker channel. Cheap enough to call once per frame.
    pub fn poll(&mut self) {
        if self.running {
            self.elapsed = self.started.elapsed();
        }
        let Some(rx) = self.rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(run) => self.apply(run),
            Err(TryRecvError::Empty) => self.rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                self.running = false;
                self.elapsed = self.started.elapsed();
            }
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The task most recently started, running or not.
    pub fn task(&self) -> Option<&Task> {
        self.task.as_ref()
    }

    /// Distinct errors parsed from the last finished run.
    pub fn errors(&self) -> &[BuildError] {
        &self.errors
    }

    /// Exit code of the last finished run, when the process reported one.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    /// Apply one key press.
    ///
    /// `Esc`/`q` only hide the overlay; the worker keeps running. `r` reruns
    /// the last task, `Enter` jumps to the selected error.
    pub fn handle_key(&mut self, key: KeyEvent) -> TaskAction {
        if key.kind == KeyEventKind::Release {
            return TaskAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.open = false;
                TaskAction::Close
            }
            KeyCode::Char('q') if !ctrl => {
                self.open = false;
                TaskAction::Close
            }
            KeyCode::Char('r') if !ctrl => {
                self.rerun();
                TaskAction::None
            }
            KeyCode::F(10) => {
                self.open = false;
                TaskAction::Close
            }
            KeyCode::Enter => self.jump(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_by(1);
                TaskAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_up(1);
                TaskAction::None
            }
            KeyCode::PageDown => {
                self.scroll_by(self.viewport.get().max(1));
                TaskAction::None
            }
            KeyCode::PageUp => {
                self.scroll_up(self.viewport.get().max(1));
                TaskAction::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.scroll = 0;
                TaskAction::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll = self.max_scroll();
                TaskAction::None
            }
            KeyCode::Char('n') => {
                self.select_error(1);
                TaskAction::None
            }
            KeyCode::Char('N') => {
                self.select_error(-1);
                TaskAction::None
            }
            _ => TaskAction::None,
        }
    }

    /// Refresh the running clock. Rendering never calls this: the host ticks
    /// once per frame.
    pub fn tick(&mut self) {
        if self.running {
            self.elapsed = self.started.elapsed();
        }
    }

    fn apply(&mut self, run: TaskRun) {
        self.running = false;
        self.rx = None;
        self.lines = run.lines;
        self.truncated = run.truncated;
        self.exit = run.exit;
        self.failure = run.failure;
        self.elapsed = run.duration;
        self.scroll = 0;
        self.selected = 0;
        self.parse_output();
    }

    fn parse_output(&mut self) {
        let text = self.lines.join("\n");
        let found = parse_errors_with_lines(&text);
        self.errors = found.iter().map(|(_, error)| error.clone()).collect();
        self.error_lines = found.iter().map(|(line, _)| *line).collect();
    }

    /// Re-run the most recent task. No-op when nothing has run yet.
    pub fn rerun(&mut self) {
        let Some(task) = self.task.clone() else {
            return;
        };
        let root = self.root.clone();
        self.start(task, &root);
    }

    /// Re-open the overlay for the last run without starting a new process.
    pub fn show(&mut self) {
        if self.task.is_some() {
            self.open = true;
        }
    }

    fn jump(&mut self) -> TaskAction {
        if self.errors.is_empty() {
            return TaskAction::None;
        }
        let error = &self.errors[self.selected.min(self.errors.len() - 1)];
        TaskAction::Jump {
            path: error.path.clone(),
            line: error.line,
        }
    }

    fn scroll_by(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_add(delta).min(self.max_scroll());
    }

    fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }

    fn select_error(&mut self, delta: isize) {
        let count = self.errors.len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        if let Some(line) = self.error_lines.get(self.selected).copied() {
            let page = self.viewport.get().max(1);
            if line < self.scroll {
                self.scroll = line;
            } else if line >= self.scroll.saturating_add(page) {
                self.scroll = line.saturating_sub(page.saturating_sub(1));
            }
        }
    }

    /// Scroll offset that still shows a full body.
    pub(crate) fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport.get().max(1))
    }

    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// Called by the renderer; `&self` because the overlay is rendered through
    /// a shared reference and this is only a layout hint.
    pub(crate) fn set_viewport(&self, height: usize) {
        self.viewport.set(height.max(1));
    }

    pub(crate) fn selected_error_line(&self) -> Option<usize> {
        self.error_lines.get(self.selected).copied()
    }

    /// 0-based output lines that carry an error.
    pub(crate) fn error_lines(&self) -> &[usize] {
        &self.error_lines
    }

    /// The single overlay header.
    pub(crate) fn header(&self) -> String {
        let kind = self.task.as_ref().map_or("Task", |task| task.kind.label());
        let command = self.task.as_ref().map_or("", |task| task.command.as_str());
        let elapsed = format_elapsed(self.elapsed);
        if self.running {
            return format!("{kind} \u{b7} {command} \u{b7} running {elapsed} \u{b7} Esc close");
        }
        if let Some(failure) = &self.failure {
            return format!(
                "{kind} \u{b7} {command} \u{b7} failed: {failure} \u{b7} {elapsed} \u{b7} Esc close"
            );
        }
        match self.exit {
            Some(code) => format!(
                "{kind} \u{b7} {command} \u{b7} exit {code} \u{b7} {elapsed} \u{b7} Esc close"
            ),
            None => {
                format!("{kind} \u{b7} {command} \u{b7} exited \u{b7} {elapsed} \u{b7} Esc close")
            }
        }
    }

    /// Test-only seeding so navigation and rendering can be checked without
    /// spawning a process.
    #[cfg(test)]
    pub(crate) fn seed_for_test(
        &mut self,
        task: Task,
        output: &str,
        exit: Option<i32>,
        running: bool,
    ) {
        self.open = true;
        self.running = running;
        self.task = Some(task);
        self.lines = output.lines().map(str::to_string).collect();
        self.exit = exit;
        self.elapsed = Duration::from_millis(1_800);
        self.started = Instant::now();
        self.scroll = 0;
        self.selected = 0;
        self.failure = None;
        self.truncated = false;
        self.parse_output();
    }

    #[cfg(test)]
    pub(crate) fn mark_truncated_for_test(&mut self) {
        self.truncated = true;
    }
}

/// Draw the task overlay.
///
/// The implementation lives in [`crate::tasks_ui`] so the runner stays free of
/// layout code.
pub fn render(frame: &mut Frame, area: Rect, state: &TaskState) {
    crate::tasks_ui::render(frame, area, state);
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let minutes = elapsed.as_secs() / 60;
        let rest = seconds - (minutes * 60) as f64;
        format!("{minutes}m {rest:04.1}s")
    }
}

/// Build the platform shell invocation. The command is passed as one string
/// and interpreted by the shell, exactly as typed in `.ccn.toml`; this is the
/// intended behaviour, not an injection point.
#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

fn run_command(command: &str, root: &Path) -> TaskRun {
    let started = Instant::now();
    let mut child = match shell_command(command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let message = format!("could not run `{command}`: {error}");
            return TaskRun {
                lines: vec![format!("ccn: {message}")],
                truncated: false,
                exit: None,
                failure: Some(message),
                duration: started.elapsed(),
            };
        }
    };

    // Both pipes feed one channel so stdout and stderr interleave roughly in
    // the order the child produced them.
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, tx.clone());
    }
    drop(tx);

    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for line in rx {
        if lines.len() >= MAX_OUTPUT_LINES || bytes.saturating_add(line.len()) > MAX_OUTPUT_BYTES {
            truncated = true;
            continue;
        }
        bytes = bytes.saturating_add(line.len());
        lines.push(line);
    }

    let waited = child.wait();
    let (exit, failure) = match waited {
        Ok(status) => (status.code(), None),
        Err(error) => (
            None,
            Some(format!("could not wait for `{command}`: {error}")),
        ),
    };

    TaskRun {
        lines,
        truncated,
        exit,
        failure,
        duration: started.elapsed(),
    }
}

/// Read one pipe line-by-line into `tx` until EOF. Long lines are kept whole;
/// the cap is applied by the collector.
fn pump<R: Read + Send + 'static>(pipe: R, tx: mpsc::Sender<String>) {
    let _ = std::thread::Builder::new()
        .name("ccn-task-io".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut buffer = Vec::new();
            loop {
                buffer.clear();
                match reader.read_until(b'\n', &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&buffer);
                        let line = text.trim_end_matches(['\n', '\r']).to_string();
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos());
            let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ccn-tasks-{tag}-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, name: &str, contents: &str) {
            fs::write(self.0.join(name), contents).expect("write fixture");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn task(name: &str, command: &str, kind: TaskKind) -> Task {
        Task {
            name: name.to_string(),
            command: command.to_string(),
            kind,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn wait_for(state: &mut TaskState) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            state.poll();
            if !state.is_running() {
                return;
            }
            assert!(Instant::now() < deadline, "task did not finish in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn detects_cargo_build_and_test() {
        let dir = TempDir::new("cargo");
        dir.write("Cargo.toml", "[package]\nname = \"demo\"\n");

        let tasks = detect(dir.path(), &Config::default());

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].name, "build");
        assert_eq!(tasks[0].command, "cargo build");
        assert_eq!(tasks[0].kind, TaskKind::Build);
        assert_eq!(tasks[1].name, "test");
        assert_eq!(tasks[1].command, "cargo test");
        assert_eq!(tasks[1].kind, TaskKind::Test);
    }

    #[test]
    fn detects_cmake_and_ignores_empty_dirs() {
        let dir = TempDir::new("cmake");
        assert!(detect(dir.path(), &Config::default()).is_empty());

        dir.write("CMakeLists.txt", "cmake_minimum_required(VERSION 3.20)\n");
        let tasks = detect(dir.path(), &Config::default());

        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks[0].command,
            "cmake -S . -B build && cmake --build build"
        );
        assert_eq!(
            tasks[1].command,
            "ctest --test-dir build --output-on-failure"
        );
    }

    #[test]
    fn detects_make_and_meson() {
        let make = TempDir::new("make");
        make.write("Makefile", "all:\n\techo ok\n");
        let tasks = detect(make.path(), &Config::default());
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].command, "make");
        assert_eq!(tasks[1].command, "make test");

        let meson = TempDir::new("meson");
        meson.write("meson.build", "project('demo')\n");
        let tasks = detect(meson.path(), &Config::default());
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].command, "meson compile -C build");
        assert_eq!(tasks[1].command, "meson test -C build");
    }

    #[test]
    fn first_build_system_wins_when_several_are_present() {
        let dir = TempDir::new("mixed");
        dir.write("Cargo.toml", "[package]\n");
        dir.write("CMakeLists.txt", "cmake_minimum_required(VERSION 3.20)\n");
        dir.write("Makefile", "all:\n");

        let tasks = detect(dir.path(), &Config::default());

        assert_eq!(tasks.len(), 2, "one build and one test task");
        assert_eq!(tasks[0].command, "cargo build");
        assert_eq!(tasks[1].command, "cargo test");
    }

    #[test]
    fn detects_node_scripts_that_exist() {
        let dir = TempDir::new("node");
        dir.write(
            "package.json",
            r#"{"scripts": {"build": "tsc", "lint": "eslint ."}}"#,
        );

        let tasks = detect(dir.path(), &Config::default());

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "build");
        assert_eq!(tasks[0].command, "npm run build");
    }

    #[test]
    fn malformed_package_json_detects_nothing() {
        let dir = TempDir::new("node-broken");
        dir.write("package.json", "{not json");
        assert!(detect(dir.path(), &Config::default()).is_empty());
    }

    #[test]
    fn configured_tasks_win_and_suppress_detection() {
        let dir = TempDir::new("config");
        dir.write("Cargo.toml", "[package]\n");

        let config = Config {
            tasks: BTreeMap::from([
                ("lint".to_string(), "cargo clippy".to_string()),
                ("build".to_string(), "make all".to_string()),
            ]),
            ..Config::default()
        };

        let tasks = detect(dir.path(), &config);

        assert_eq!(tasks.len(), 2, "detected cargo tasks must be suppressed");
        assert_eq!(tasks[0].name, "build");
        assert_eq!(tasks[0].command, "make all");
        assert_eq!(tasks[0].kind, TaskKind::Build);
        assert_eq!(tasks[1].name, "lint");
        assert_eq!(tasks[1].kind, TaskKind::Other);
    }

    #[test]
    fn task_names_are_deduplicated_and_capped() {
        let dir = TempDir::new("cap");
        let mut config = Config::default();
        for index in 0..20 {
            let name = if index == 0 {
                "build".to_string()
            } else {
                format!("task{index}")
            };
            config.tasks.insert(name, format!("echo {index}"));
        }

        let tasks = detect(dir.path(), &config);

        assert_eq!(tasks.len(), MAX_TASKS);
        assert_eq!(tasks[0].name, "build");
        let mut names: Vec<&str> = tasks.iter().map(|task| task.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tasks.len());
    }

    #[test]
    fn parses_gcc_style_errors() {
        let errors = parse_errors(
            "src/main.c:12:5: error: 'x' undeclared\n\
             src/main.c:20: error: expected ';'\n\
             src/main.c:30:1: warning: unused variable\n",
        );

        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].path, "src/main.c");
        assert_eq!(errors[0].line, 12);
        assert_eq!(errors[0].column, Some(5));
        assert_eq!(errors[0].message, "'x' undeclared");
        assert_eq!(errors[1].line, 20);
        assert_eq!(errors[1].column, None);
        assert_eq!(errors[2].message, "unused variable");
    }

    #[test]
    fn parses_msvc_errors() {
        let errors = parse_errors(
            "C:\\work\\main.cpp(41,7): error C2065: 'x': undeclared identifier\n\
             other.cpp(9): warning C4996: deprecated\n",
        );

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].path, r"C:\work\main.cpp");
        assert_eq!(errors[0].line, 41);
        assert_eq!(errors[0].column, Some(7));
        assert!(
            errors[0].message.starts_with("C2065"),
            "{}",
            errors[0].message
        );
        assert_eq!(errors[1].line, 9);
        assert_eq!(errors[1].column, None);
    }

    #[test]
    fn parses_rust_errors_with_arrow() {
        let errors = parse_errors(
            "error[E0308]: mismatched types\n\
             \x20--> src/main.rs:7:9\n\
             \x20 |\n\
             error: could not compile `demo` (bin \"demo\") due to 1 previous error\n\
             warning: unused import\n\
             \x20--> src/lib.rs:2:5\n",
        );

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].path, "src/main.rs");
        assert_eq!(errors[0].line, 7);
        assert_eq!(errors[0].column, Some(9));
        assert_eq!(errors[0].message, "error[E0308]: mismatched types");
        assert_eq!(errors[1].path, "src/lib.rs");
        assert_eq!(errors[1].column, Some(5));
    }

    #[test]
    fn keeps_unique_errors_only() {
        let errors = parse_errors(
            "a.rs:1:1: error: boom\n\
             a.rs:1:1: error: boom\n\
             a.rs:2:1: error: boom\n",
        );

        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn error_list_is_capped() {
        let mut output = String::new();
        for index in 1..=(MAX_ERRORS + 20) {
            output.push_str(&format!("src/a.rs:{index}:1: error: boom {index}\n"));
        }

        assert_eq!(parse_errors(&output).len(), MAX_ERRORS);
    }

    #[test]
    fn unrecognised_output_yields_no_errors() {
        assert!(parse_errors("Compiling demo v0.1.0\nFinished `dev` profile\n").is_empty());
    }

    #[test]
    fn navigation_scrolls_and_clamps() {
        let mut state = TaskState::new();
        let output: String = (0..60).map(|line| format!("line {line}\n")).collect();
        state.seed_for_test(
            task("build", "cargo build", TaskKind::Build),
            &output,
            Some(0),
            false,
        );

        assert!(state.is_open());
        assert!(!state.is_running());
        assert_eq!(state.scroll(), 0);

        state.handle_key(key(KeyCode::Char('j')));
        assert_eq!(state.scroll(), 1);
        state.handle_key(key(KeyCode::Char('k')));
        assert_eq!(state.scroll(), 0);
        state.handle_key(key(KeyCode::Char('k')));
        assert_eq!(state.scroll(), 0, "scrolling up at the top is a no-op");

        state.handle_key(key(KeyCode::End));
        assert_eq!(state.scroll(), state.max_scroll());
        state.handle_key(key(KeyCode::Char('g')));
        assert_eq!(state.scroll(), 0);

        state.handle_key(key(KeyCode::PageDown));
        assert_eq!(state.scroll(), DEFAULT_PAGE);
    }

    #[test]
    fn enter_jumps_to_selected_error() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "cc", TaskKind::Build),
            "a.rs:2:1: error: one\nb.rs:4:2: error: two\n",
            Some(1),
            false,
        );
        assert_eq!(state.errors().len(), 2);

        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            TaskAction::Jump {
                path: "a.rs".to_string(),
                line: 2
            }
        );

        state.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            TaskAction::Jump {
                path: "b.rs".to_string(),
                line: 4
            }
        );

        state.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            TaskAction::Jump {
                path: "a.rs".to_string(),
                line: 2
            },
            "selection wraps"
        );
    }

    #[test]
    fn enter_without_errors_does_nothing() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "true", TaskKind::Build),
            "ok\n",
            Some(0),
            false,
        );
        assert_eq!(state.handle_key(key(KeyCode::Enter)), TaskAction::None);
    }

    #[test]
    fn close_keys_only_hide_the_view() {
        let mut state = TaskState::new();
        state.seed_for_test(
            task("build", "true", TaskKind::Build),
            "ok\n",
            Some(0),
            true,
        );

        assert_eq!(state.handle_key(key(KeyCode::Esc)), TaskAction::Close);
        assert!(!state.is_open());
        assert!(state.is_running(), "closing must not stop the process");

        state.seed_for_test(
            task("build", "true", TaskKind::Build),
            "ok\n",
            Some(0),
            false,
        );
        assert_eq!(state.handle_key(key(KeyCode::Char('q'))), TaskAction::Close);
        assert!(!state.is_open());
    }

    #[test]
    fn runs_command_and_reports_exit_zero() {
        let dir = TempDir::new("run");
        let mut state = TaskState::new();
        state.start(task("build", "echo hello", TaskKind::Build), dir.path());
        assert!(state.is_running());

        wait_for(&mut state);

        assert!(!state.is_running());
        assert_eq!(state.exit_code(), Some(0));
        assert!(
            state.lines().iter().any(|line| line.contains("hello")),
            "{:?}",
            state.lines()
        );
        assert!(state.errors().is_empty());
        let header = state.header();
        assert!(header.contains("Build"), "{header}");
        assert!(header.contains("exit 0"), "{header}");
        assert!(header.contains("Esc close"), "{header}");
        assert!(!header.contains("running"), "{header}");
    }

    #[test]
    fn propagates_non_zero_exit() {
        let dir = TempDir::new("run-fail");
        let mut state = TaskState::new();
        state.start(task("test", "exit 3", TaskKind::Test), dir.path());
        wait_for(&mut state);

        assert_eq!(state.exit_code(), Some(3));
        assert!(state.header().contains("exit 3"));
        assert!(state.header().starts_with("Test"), "{}", state.header());
    }

    #[test]
    fn rerun_repeats_the_last_task() {
        let dir = TempDir::new("rerun");
        let mut state = TaskState::new();
        state.start(task("build", "echo again", TaskKind::Build), dir.path());
        wait_for(&mut state);
        assert!(!state.is_running());

        assert_eq!(state.handle_key(key(KeyCode::Char('r'))), TaskAction::None);
        assert!(state.is_running());
        wait_for(&mut state);
        assert_eq!(state.exit_code(), Some(0));
        assert!(state.lines().iter().any(|line| line.contains("again")));
    }

    #[test]
    fn idle_state_is_safe_for_every_entry_point() {
        let mut state = TaskState::new();
        assert!(!state.is_open());
        assert!(!state.is_running());
        state.poll();
        state.tick();
        assert_eq!(state.handle_key(key(KeyCode::Char('r'))), TaskAction::None);
        assert_eq!(state.handle_key(key(KeyCode::Enter)), TaskAction::None);
        state.handle_key(key(KeyCode::Char('j')));
        assert_eq!(state.scroll(), 0);
        assert_eq!(state.handle_key(key(KeyCode::Esc)), TaskAction::Close);
    }

    #[test]
    fn elapsed_formatting_is_readable() {
        assert_eq!(format_elapsed(Duration::from_millis(2_100)), "2.1s");
        assert_eq!(format_elapsed(Duration::from_millis(61_500)), "1m 01.5s");
    }
}
