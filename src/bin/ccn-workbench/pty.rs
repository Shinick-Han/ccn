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

/// Upper bound on bytes retained while waiting for the end of a query.
const MAX_PENDING: usize = 64 * 1024;

const ESC: u8 = 0x1b;

struct Query {
    bytes: &'static [u8],
    reply: &'static [u8],
}

/// Static queries the bridge answers on behalf of the host terminal.
///
/// Neovim probes capabilities at startup and blocks on colour/CPR answers.
/// OSC replies use ST (`ESC \`), accepted by Neovim and unambiguous in a raw stream.
const QUERIES: &[Query] = &[
    Query {
        bytes: b"\x1b]11;?\x07",
        reply: b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\",
    },
    Query {
        bytes: b"\x1b]11;?\x1b\\",
        reply: b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\",
    },
    Query {
        bytes: b"\x1b]10;?\x07",
        reply: b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",
    },
    Query {
        bytes: b"\x1b]10;?\x1b\\",
        reply: b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",
    },
    Query {
        bytes: b"\x1b[6n",
        reply: b"\x1b[1;1R",
    },
    Query {
        bytes: b"\x1b[5n",
        reply: b"\x1b[0n",
    },
    Query {
        bytes: b"\x1b[c",
        reply: b"\x1b[?1;2c",
    },
    Query {
        bytes: b"\x1b[>c",
        reply: b"\x1b[>0;276;0c",
    },
    Query {
        bytes: b"\x1b[?2004$p",
        reply: b"\x1b[?2004;4$y",
    },
    Query {
        bytes: b"\x1b[?2026$p",
        reply: b"\x1b[?2026;4$y",
    },
    Query {
        bytes: b"\x1b[?u",
        reply: b"\x1b[?0u",
    },
];

/// Answers terminal queries found in the child's output stream.
///
/// The reader feeds arbitrary chunks, so a query may be split across reads; a
/// short pending suffix is kept until the sequence is complete. Each query
/// occurrence is answered exactly once: the scanner consumes the bytes, so a
/// reply can never be duplicated for one probe. Later repeats are real new
/// queries (Neovim re-sends OSC 11 + DSR after its DA1 handshake and waits for
/// the DSR), so they must be answered too.
#[derive(Default)]
struct QueryResponder {
    pending: Vec<u8>,
}

impl QueryResponder {
    fn new() -> Self {
        Self::default()
    }

    /// Consume `chunk`, returning zero or more reply sequences to write back.
    ///
    /// Normal child output produces no reply and is left untouched.
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
            return Vec::new();
        }

        let mut replies = Vec::new();
        let mut search = 0usize;
        while let Some(offset) = self.pending[search..].iter().position(|&byte| byte == ESC) {
            let start = search + offset;
            match parse_query(&self.pending[start..]) {
                Some((length, reply)) => {
                    self.pending.drain(..start + length);
                    replies.extend_from_slice(&reply);
                    search = 0;
                }
                None => {
                    if is_partial_query(&self.pending[start..]) {
                        break;
                    }
                    search = start + 1;
                }
            }
        }
        self.trim();
        replies
    }

    /// Drop everything except a possible partial query at the very end.
    fn trim(&mut self) {
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
            return;
        }
        let Some(last_escape) = self.pending.iter().rposition(|&byte| byte == ESC) else {
            self.pending.clear();
            return;
        };
        if is_partial_query(&self.pending[last_escape..]) {
            self.pending.drain(..last_escape);
        } else {
            self.pending.clear();
        }
    }
}

/// A complete query at the start of `data`, with its reply and consumed length.
fn parse_query(data: &[u8]) -> Option<(usize, Vec<u8>)> {
    for query in QUERIES {
        if data.starts_with(query.bytes) {
            return Some((query.bytes.len(), query.reply.to_vec()));
        }
    }
    parse_decrqm(data)
}

/// Any `DECRQM` probe (`ESC [ ? <mode> $ p`) reports "permanently reset",
/// which is true for every private mode this bridge does not implement.
fn parse_decrqm(data: &[u8]) -> Option<(usize, Vec<u8>)> {
    let rest = data.strip_prefix(b"\x1b[?")?;
    let mut digits = 0usize;
    while rest.get(digits).is_some_and(u8::is_ascii_digit) {
        digits += 1;
    }
    if digits == 0 || rest.get(digits) != Some(&b'$') || rest.get(digits + 1) != Some(&b'p') {
        return None;
    }
    let mut reply = Vec::with_capacity(3 + digits + 4);
    reply.extend_from_slice(b"\x1b[?");
    reply.extend_from_slice(&rest[..digits]);
    reply.extend_from_slice(b";4$y");
    Some((3 + digits + 2, reply))
}

/// Could `data` (which starts at an ESC) grow into a known query with more
/// bytes? Used to retain a split query and to discard ordinary escape output.
fn is_partial_query(data: &[u8]) -> bool {
    if !data.starts_with(&[ESC]) {
        return false;
    }
    if QUERIES
        .iter()
        .any(|query| query.bytes.len() > data.len() && query.bytes.starts_with(data))
    {
        return true;
    }
    if let Some(rest) = data.strip_prefix(b"\x1b[?") {
        if rest.is_empty() {
            return true;
        }
        let mut digits = 0usize;
        while rest.get(digits).is_some_and(u8::is_ascii_digit) {
            digits += 1;
        }
        if digits == rest.len() {
            return true;
        }
        if rest[digits] == b'$' && digits + 1 == rest.len() {
            return true;
        }
    }
    false
}

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
                let mut responder = QueryResponder::new();
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
                            // ConPTY and Neovim probe the host terminal; the
                            // bridge answers those probes so startup is not
                            // blocked waiting for a reply that never comes.
                            let replies = responder.feed(&buf[..n]);
                            if !replies.is_empty() {
                                if let Ok(mut writer) = input.lock() {
                                    let _ = writer.write_all(&replies);
                                    let _ = writer.flush();
                                }
                            }
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

    #[test]
    fn query_responder_answers_a_split_query_once_per_occurrence() {
        let mut responder = QueryResponder::new();
        // Ordinary output and half a colour query must produce no reply.
        assert!(responder.feed(b"plain text ").is_empty());
        assert!(responder.feed(b"\x1b]11;").is_empty());
        assert_eq!(
            responder.feed(b"?\x07"),
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"
        );
        // A later repeat is a new query (Neovim re-sends it before its DSR
        // wait), so it must be answered as well.
        assert_eq!(
            responder.feed(b"\x1b]11;?\x07"),
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"
        );
    }

    #[test]
    fn query_responder_answers_dec_rqm_split_across_chunks() {
        let mut responder = QueryResponder::new();
        assert!(responder.feed(b"\x1b[?202").is_empty());
        assert_eq!(responder.feed(b"6$p"), b"\x1b[?2026;4$y");
        // The generic path covers modes Neovim probes but does not block on.
        assert_eq!(responder.feed(b"\x1b[?69$p"), b"\x1b[?69;4$y");
        assert_eq!(responder.feed(b"\x1b[?2004$p"), b"\x1b[?2004;4$y");
    }

    #[test]
    fn query_responder_answers_dsr_da_and_kitty() {
        let mut responder = QueryResponder::new();
        assert_eq!(responder.feed(b"\x1b[6n"), b"\x1b[1;1R");
        assert_eq!(responder.feed(b"\x1b[5n"), b"\x1b[0n");
        assert_eq!(responder.feed(b"\x1b[c"), b"\x1b[?1;2c");
        assert_eq!(responder.feed(b"\x1b[>c"), b"\x1b[>0;276;0c");
        assert_eq!(responder.feed(b"\x1b[?u"), b"\x1b[?0u");
    }

    #[test]
    fn query_responder_ignores_ordinary_escape_output() {
        let mut responder = QueryResponder::new();
        assert!(responder
            .feed(b"\x1b[31mred\x1b[0m and \x1b[?25lplain")
            .is_empty());
        assert!(responder.feed(b"\x1b[2J").is_empty());
        // The retained buffer must not turn into unbounded state.
        responder.feed(&vec![b'x'; MAX_PENDING + 1]);
        assert!(responder.feed(b"\x1b[6n") == b"\x1b[1;1R");
    }
}
