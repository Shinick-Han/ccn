//! `ccn-workbench` spike: a real Neovim TUI in a ratatui pane, next to the
//! current file's complexity breakdown.
//!
//! The workbench itself owns no editing logic: Neovim runs in a PTY, its output
//! is rendered through `vt100` + `tui-term`, and keys are forwarded as bytes.
//! See `docs/workbench-spike.md`.

mod pty;
mod ui;

use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use ccn::ccn::types::{CcnTier, FunctionCcn};
use ccn::loc::{scan, ScanOptions};
use ccn::model::{CcnSkip, FileLoc};

use pty::Pty;

/// Re-scan the open file at most this often.
const MTIME_INTERVAL: Duration = Duration::from_millis(500);

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
    name = "ccn-workbench",
    version,
    about = "Neovim in a ratatui pane, with ccn complexity side panels"
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Editor,
    Sidebar,
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
        };
        app.rescan("scan");
        app
    }

    fn function_count(&self) -> usize {
        self.view.as_ref().map_or(0, |view| view.functions.len())
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

    pub(crate) fn scan_label(&self) -> String {
        if self.scan_error.is_some() {
            "scan error".to_string()
        } else {
            format!("scan ok · {} ms", self.scan_ms)
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.smoke {
        return report_smoke(run_smoke(&cli, false));
    }
    if cli.smoke_nvim {
        return report_smoke(run_smoke(&cli, true));
    }
    if cli.smoke_utf8 {
        return report_smoke(run_smoke_utf8(&cli));
    }
    match run_workbench(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("ccn-workbench: {message}");
            ExitCode::FAILURE
        }
    }
}

fn report_smoke(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("smoke failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_workbench(cli: &Cli) -> Result<(), String> {
    let requested = cli.target.as_deref().ok_or_else(|| {
        "missing <path-or-file> (usage: ccn-workbench [--editor <cmd>] <file>)".to_string()
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
    let mut app = App::new(target.clone(), display_path(&target), label);

    terminal::enable_raw_mode().map_err(|e| format!("cannot enable raw mode: {e}"))?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)
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
                "{e}\n  editor: {}\n  hint: install Neovim on PATH or pass --editor \"<path>\"",
                cli.editor
            ));
        }
    };

    let loop_result = event_loop(&mut terminal, &mut app, &mut child);

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
        if event::poll(Duration::from_millis(16)).map_err(|e| format!("poll failed: {e}"))? {
            match event::read().map_err(|e| format!("read event failed: {e}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(app, child, key) {
                        return Ok(());
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
                app.set_status(
                    format!("[editor exited: {note}]{hint} — F10 quits"),
                    note != "exit 0" || empty,
                );
            }
        }

        if last_mtime_check.elapsed() >= MTIME_INTERVAL {
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
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
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
