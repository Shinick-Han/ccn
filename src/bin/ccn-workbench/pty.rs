//! PTY plumbing for the workbench spike.
//!
//! One child process (Neovim by default) is attached to a ConPTY on Windows and
//! a normal pty on Unix. Output is parsed continuously into a shared
//! [`vt100::Parser`]; input is written back as raw bytes.
//!
//! Deliberately no RPC here: another spike owns `nvim --embed`. This module is
//! only "start a terminal program and give me its screen".

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

/// A running child attached to a pseudo-terminal.
pub struct Pty {
    parser: Arc<RwLock<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    exit: Option<String>,
}

impl Drop for Pty {
    fn drop(&mut self) {
        // `ClosePseudoConsole` can block forever when a reader is still parked
        // in `ReadFile` on the output pipe. The process is about to exit, so
        // let the OS reclaim the pseudo-console instead of calling close here.
        if let Some(master) = self.master.take() {
            std::mem::forget(master);
        }
    }
}

impl Pty {
    /// Spawn `program` with `args` in `cwd`, sized exactly like the editor pane.
    ///
    /// `TERM` and `COLORTERM` are set so Neovim emits 256-colour/truecolor
    /// sequences regardless of the host terminal; the user's config is kept.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        rows: u16,
        cols: u16,
    ) -> Result<Self, String> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("cannot open a PTY: {e}"))?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("cannot start {program}: {e}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot read from the PTY: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot write to the PTY: {e}"))?;
        let writer = Arc::new(Mutex::new(writer));
        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, 0)));

        let sink = Arc::clone(&parser);
        let input = Arc::clone(&writer);
        thread::Builder::new()
            .name("ccn-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut tail: Vec<u8> = Vec::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            {
                                let mut parser = match sink.write() {
                                    Ok(guard) => guard,
                                    Err(poisoned) => poisoned.into_inner(),
                                };
                                parser.process(&buf[..n]);
                            }
                            // ConPTY emits a cursor-position query (`ESC [ 6 n`)
                            // before it forwards the child's output; until the
                            // terminal answers, the child appears frozen. vt100
                            // consumes the query silently, so answer it here.
                            let mut probe = Vec::with_capacity(tail.len() + n);
                            probe.extend_from_slice(&tail);
                            probe.extend_from_slice(&buf[..n]);
                            if probe.windows(4).any(|window| window == b"\x1b[6n") {
                                if let Ok(mut writer) = input.lock() {
                                    let _ = writer.write_all(b"\x1b[1;1R");
                                    let _ = writer.flush();
                                }
                            }
                            let keep = probe.len().saturating_sub(3);
                            tail = probe[keep..].to_vec();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("cannot start the PTY reader thread: {e}"))?;

        Ok(Self {
            parser,
            writer,
            master: Some(pair.master),
            child,
            exit: None,
        })
    }

    /// Shared screen state, read by the UI and written by the reader thread.
    pub fn parser(&self) -> &Arc<RwLock<vt100::Parser>> {
        &self.parser
    }

    /// Forward raw bytes to the child.
    pub fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("PTY writer lock poisoned"))?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Resize both the kernel PTY (SIGWINCH on Unix) and the vt100 screen.
    pub fn resize(&self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let _ = self.master.as_ref().map(|master| {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
        });
        let mut parser = match self.parser.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        parser.set_size(rows, cols);
    }

    /// Non-blocking child poll. Returns the exit note once, then the cached one.
    pub fn exit_status(&mut self) -> Option<String> {
        if self.exit.is_some() {
            return self.exit.clone();
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let note = match (status.success(), status.signal()) {
                    (true, _) => "exit 0".to_string(),
                    (false, Some(signal)) => format!("terminated by {signal}"),
                    (false, None) => format!("exit {}", status.exit_code()),
                };
                self.exit = Some(note.clone());
                Some(note)
            }
            Ok(None) => None,
            Err(e) => {
                let note = format!("wait failed: {e}");
                self.exit = Some(note.clone());
                Some(note)
            }
        }
    }

    /// Terminate the child; safe to call after it already exited.
    pub fn kill(&mut self) {
        if self.exit.is_some() {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.exit = Some("killed".to_string());
    }

    /// Plain-text snapshot, for smoke assertions and post-mortem tails.
    pub fn screen_text(&self) -> String {
        let parser = match self.parser.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        parser.screen().contents()
    }
}

/// Split an editor command string into program + arguments.
///
/// Double quotes group a path containing spaces; backslashes are literal (a
/// Windows path is not a shell escape). Single quotes are not special.
pub fn parse_command(line: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err(format!("unbalanced quote in editor command: {line}"));
    }
    if started {
        parts.push(current);
    }
    if parts.first().is_none_or(String::is_empty) {
        return Err("empty editor command".to_string());
    }
    Ok(parts)
}

/// Best-effort executable lookup, used to turn a missing editor into a skip
/// instead of a failed smoke run.
pub fn program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file();
    }
    let mut names = vec![program.to_string()];
    if cfg!(windows) {
        for ext in ["exe", "cmd", "bat", "com"] {
            names.push(format!("{program}.{ext}"));
        }
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| names.iter().any(|name| dir.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_command_splits_on_quotes_and_spaces() {
        let parts = parse_command(r#""C:\Program Files\nvim\nvim.exe" --clean"#).unwrap();
        assert_eq!(parts[0], r"C:\Program Files\nvim\nvim.exe");
        assert_eq!(parts[1], "--clean");
    }

    #[test]
    fn editor_command_rejects_unbalanced_quotes() {
        assert!(parse_command(r#""nvim"#).is_err());
        assert!(parse_command("   ").is_err());
    }
}
