//! Live Neovim RPC for the workbench.
//!
//! The editor is still a real Neovim in a PTY; this module only reads the live
//! buffer over msgpack-RPC, measures CCN, sends snapshots to the UI thread, and
//! writes our own extmarks. It never writes to disk.
//!
//! All calls used here exist in Neovim 0.8 (the EPEL build) and 0.12:
//! `nvim_get_current_buf`, `nvim_buf_get_name`, `nvim_buf_get_changedtick`,
//! `nvim_buf_get_lines`, `nvim_exec_lua`, `nvim_buf_set_extmark`,
//! `nvim_buf_get_extmarks`, and `nvim_win_set_cursor` (via Lua).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::ccn::{analyze_file, CcnTier, FunctionCcn, Lang};
use nvim_rs::compat::tokio::Compat;
use nvim_rs::error::LoopError;
use nvim_rs::Neovim;
use tokio::io::WriteHalf;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

/// The concrete writer `new_tcp` produces on this build.
pub type Writer = Compat<WriteHalf<TcpStream>>;
pub type Rpc = Neovim<Writer>;
pub type IoTask = JoinHandle<Result<(), Box<LoopError>>>;

/// Extmarks written by the workbench. Nothing else is ever cleared.
pub const NAMESPACE: &str = "ccn_workbench";

const TICK_INTERVAL: Duration = Duration::from_millis(150);
const COMMAND_POLL: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcStatus {
    Off,
    Connecting,
    Live,
}

#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub name: String,
    /// `None` when the snapshot carries an error instead of a measurement.
    pub tier: Option<CcnTier>,
    pub functions: Vec<FunctionCcn>,
    pub sum: u32,
    pub max: u32,
    pub changedtick: i64,
    /// Changedtick of the first successful snapshot (connect time). The UI
    /// compares this with `changedtick` to show `unsaved`.
    pub base_tick: i64,
    pub read_ms: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MarkInfo {
    pub row: i64,
    pub text: String,
    pub hl: String,
}

#[derive(Debug)]
pub enum RpcEvent {
    Status(RpcStatus),
    Snapshot(LiveSnapshot),
    /// Non-fatal problem (annotation, jump). The sidebar keeps its data.
    Note(String),
}

pub enum RpcCommand {
    Jump { line: usize },
    Shutdown,
}

/// UI-side handle. Dropping it closes the command channel and stops the worker.
pub struct RpcHandle {
    tx: Sender<RpcCommand>,
}

impl RpcHandle {
    pub fn jump(&self, line: usize) {
        let _ = self.tx.send(RpcCommand::Jump { line });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(RpcCommand::Shutdown);
    }
}

/// Reserve a port by binding `127.0.0.1:0`, then release it for Neovim.
///
/// There is a small TOCTOU window; acceptable for a local editor session.
pub fn free_listen_address() -> Result<String, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("cannot reserve a local port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read the reserved port: {e}"))?
        .port();
    drop(listener);
    Ok(format!("127.0.0.1:{port}"))
}

/// Does this editor basename look like Neovim?
pub fn is_nvim_program(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase().contains("nvim"))
        .unwrap_or(false)
}

/// Start the RPC worker thread. Returns the command handle and event receiver.
///
/// `baseline` is the initial file scan; it is used only for deltas/extmark
/// suffixes and is never mutated by the worker.
pub fn spawn(
    address: String,
    fallback_path: PathBuf,
    baseline: Vec<FunctionCcn>,
    child_alive: Arc<AtomicBool>,
) -> Result<(RpcHandle, Receiver<RpcEvent>), String> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let baseline = baseline_map(&baseline);
    thread::Builder::new()
        .name("ccn-rpc".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = event_tx.send(RpcEvent::Status(RpcStatus::Off));
                    let _ = event_tx.send(RpcEvent::Note(format!("rpc runtime: {e}")));
                    return;
                }
            };
            runtime.block_on(worker(
                &address,
                &fallback_path,
                &baseline,
                &child_alive,
                &cmd_rx,
                &event_tx,
            ));
        })
        .map_err(|e| format!("cannot start the RPC thread: {e}"))?;
    Ok((RpcHandle { tx: cmd_tx }, event_rx))
}

async fn worker(
    address: &str,
    fallback_path: &Path,
    baseline: &HashMap<String, u32>,
    child_alive: &AtomicBool,
    cmd_rx: &Receiver<RpcCommand>,
    events: &Sender<RpcEvent>,
) {
    let mut status = RpcStatus::Off;
    loop {
        if !child_alive.load(Ordering::Relaxed) || channel_closed(cmd_rx) {
            break;
        }
        send_status(events, &mut status, RpcStatus::Connecting);

        let Some((nvim, io)) =
            connect_with_retry(address, child_alive, cmd_rx, events, &mut status).await
        else {
            if !child_alive.load(Ordering::Relaxed) && !channel_closed(cmd_rx) {
                break;
            }
            if channel_closed(cmd_rx) {
                break;
            }
            // Initial 10 s window expired; keep retrying quietly while the
            // editor is alive, but report `off` and back off.
            send_status(events, &mut status, RpcStatus::Off);
            tokio::time::sleep(CONNECT_RETRY).await;
            continue;
        };

        send_status(events, &mut status, RpcStatus::Live);
        let end = session(&nvim, fallback_path, baseline, child_alive, cmd_rx, events).await;
        io.abort();
        match end {
            SessionEnd::Shutdown | SessionEnd::ChannelClosed | SessionEnd::ChildGone => break,
            SessionEnd::Error(message) => {
                let _ = events.send(RpcEvent::Note(format!("rpc: {message}")));
                send_status(events, &mut status, RpcStatus::Off);
                tokio::time::sleep(CONNECT_RETRY).await;
            }
        }
    }
    if status != RpcStatus::Off {
        let _ = events.send(RpcEvent::Status(RpcStatus::Off));
    }
}

async fn connect_with_retry(
    address: &str,
    child_alive: &AtomicBool,
    cmd_rx: &Receiver<RpcCommand>,
    events: &Sender<RpcEvent>,
    status: &mut RpcStatus,
) -> Option<(Rpc, IoTask)> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if !child_alive.load(Ordering::Relaxed) || channel_closed(cmd_rx) {
            return None;
        }
        match connect(address).await {
            Ok(pair) => return Some(pair),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(CONNECT_RETRY).await;
            }
            Err(message) => {
                let _ = events.send(RpcEvent::Note(format!("rpc connect: {message}")));
                send_status(events, status, RpcStatus::Off);
                return None;
            }
        }
    }
}

enum SessionEnd {
    Shutdown,
    ChannelClosed,
    ChildGone,
    Error(String),
}

async fn session(
    nvim: &Rpc,
    fallback_path: &Path,
    baseline: &HashMap<String, u32>,
    child_alive: &AtomicBool,
    cmd_rx: &Receiver<RpcCommand>,
    events: &Sender<RpcEvent>,
) -> SessionEnd {
    let mut last_tick: Option<i64> = None;
    let mut base_tick: Option<i64> = None;
    let mut last_check: Option<Instant> = None;

    loop {
        while let Ok(command) = cmd_rx.try_recv() {
            match command {
                RpcCommand::Shutdown => return SessionEnd::Shutdown,
                RpcCommand::Jump { line } => {
                    if let Err(e) = jump(nvim, line).await {
                        let _ = events.send(RpcEvent::Note(format!("jump: {e}")));
                    }
                }
            }
        }
        if channel_closed(cmd_rx) {
            return SessionEnd::ChannelClosed;
        }
        if !child_alive.load(Ordering::Relaxed) {
            return SessionEnd::ChildGone;
        }

        let due = last_check.is_none_or(|when| when.elapsed() >= TICK_INTERVAL);
        if due {
            last_check = Some(Instant::now());
            let tick = match current_tick(nvim).await {
                Ok(tick) => tick,
                Err(e) => return SessionEnd::Error(format!("changedtick: {e}")),
            };
            if base_tick.is_none() {
                base_tick = Some(tick);
            }
            if last_tick != Some(tick) {
                last_tick = Some(tick);
                let snapshot = read_snapshot(nvim, fallback_path, base_tick.unwrap_or(tick)).await;
                let failed = snapshot.error.is_some();
                if events.send(RpcEvent::Snapshot(snapshot.clone())).is_err() {
                    return SessionEnd::ChannelClosed;
                }
                if !failed {
                    let _ = annotate(nvim, &snapshot.functions, baseline)
                        .await
                        .map_err(|e| {
                            let _ = events.send(RpcEvent::Note(format!("extmarks: {e}")));
                        });
                }
            }
        }

        tokio::time::sleep(COMMAND_POLL).await;
    }
}

fn channel_closed(rx: &Receiver<RpcCommand>) -> bool {
    matches!(rx.try_recv(), Err(TryRecvError::Disconnected))
}

fn send_status(events: &Sender<RpcEvent>, status: &mut RpcStatus, next: RpcStatus) {
    if *status != next {
        *status = next;
        let _ = events.send(RpcEvent::Status(next));
    }
}

/// Connect to a Neovim started with `--listen host:port`.
pub async fn connect(address: &str) -> Result<(Rpc, IoTask), String> {
    let handler = nvim_rs::rpc::handler::Dummy::<Writer>::new();
    nvim_rs::create::tokio::new_tcp(address, handler)
        .await
        .map_err(|e| format!("cannot connect to {address}: {e}"))
}

async fn current_tick(nvim: &Rpc) -> Result<i64, String> {
    let buffer = nvim
        .get_current_buf()
        .await
        .map_err(|e| format!("get_current_buf: {e}"))?;
    buffer
        .get_changedtick()
        .await
        .map_err(|e| format!("buf_get_changedtick: {e}"))
}

/// Read the current buffer and measure it. Never panics or writes to disk.
pub async fn read_snapshot(nvim: &Rpc, fallback_path: &Path, base_tick: i64) -> LiveSnapshot {
    let started = Instant::now();
    let buffer = match nvim.get_current_buf().await {
        Ok(buffer) => buffer,
        Err(e) => return error_snapshot(base_tick, started, format!("get_current_buf: {e}")),
    };
    let changedtick = buffer.get_changedtick().await.unwrap_or(base_tick);
    let raw_name = buffer.get_name().await.unwrap_or_default();
    let name = if raw_name.is_empty() {
        fallback_path.display().to_string()
    } else {
        raw_name
    };
    let lang = match detect_lang(&name) {
        Some(lang) => lang,
        None => {
            return error_snapshot(
                base_tick,
                started,
                format!("no CCN grammar for buffer {name}"),
            )
        }
    };
    let lines = match buffer.get_lines(0, -1, false).await {
        Ok(lines) => lines,
        Err(e) => {
            return error_snapshot(
                base_tick,
                started,
                format!("buf_get_lines (invalid UTF-8?): {e}"),
            )
        }
    };
    let source = lines.join("\n");
    let summary = analyze_file(Path::new(&name), source.as_bytes(), lang.name());
    LiveSnapshot {
        name,
        tier: Some(summary.tier),
        functions: summary.detail,
        sum: summary.sum,
        max: summary.max,
        changedtick,
        base_tick,
        read_ms: started.elapsed().as_secs_f64() * 1000.0,
        error: None,
    }
}

fn error_snapshot(base_tick: i64, started: Instant, message: String) -> LiveSnapshot {
    LiveSnapshot {
        name: String::new(),
        tier: None,
        functions: Vec::new(),
        sum: 0,
        max: 0,
        changedtick: base_tick,
        base_tick,
        read_ms: started.elapsed().as_secs_f64() * 1000.0,
        error: Some(message),
    }
}

fn detect_lang(name: &str) -> Option<Lang> {
    Lang::from_path(name).or_else(|| {
        tokei::LanguageType::from_path(Path::new(name), &tokei::Config::default())
            .and_then(|lang| Lang::from_tokei_name(lang.name()))
    })
}

/// Stable comparison key between the initial scan and the live buffer.
pub fn function_key(function: &FunctionCcn) -> String {
    if function.identity.is_empty() {
        function.name.clone()
    } else {
        function.identity.clone()
    }
}

pub fn baseline_map(functions: &[FunctionCcn]) -> HashMap<String, u32> {
    functions
        .iter()
        .map(|function| (function_key(function), function.ccn))
        .collect()
}

/// Positive CCN delta versus the initial scan: new functions count from 0.
pub fn positive_delta(function: &FunctionCcn, baseline: &HashMap<String, u32>) -> u32 {
    let ccn = function.ccn as i64;
    baseline
        .get(&function_key(function))
        .map_or(ccn, |before| ccn - i64::from(*before))
        .max(0) as u32
}

const ANNOTATE_LUA: &str = r#"
local buf, entries = ...
local ns = vim.api.nvim_create_namespace('ccn_workbench')
vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)

local line_count = vim.api.nvim_buf_line_count(buf)
local placed = 0
for _, entry in ipairs(entries) do
  local row = entry.line - 1
  if row < 0 then row = 0 end
  if row >= line_count then row = line_count - 1 end

  local hl = 'DiagnosticInfo'
  if entry.ccn > 20 then
    hl = 'DiagnosticError'
  elseif entry.ccn > 10 then
    hl = 'DiagnosticWarn'
  end

  local label = ' CCN ' .. entry.ccn .. ' '
  if entry.delta > 0 then
    label = label .. ' (+' .. entry.delta .. ')'
  end

  local ok = pcall(vim.api.nvim_buf_set_extmark, buf, ns, row, 0, {
    virt_text = { { label, hl } },
    virt_text_pos = 'eol',
    hl_group = hl,
    priority = 200,
  })
  if ok then placed = placed + 1 end
end
return placed
"#;

/// Clear `ccn_workbench` and place one end-of-line mark per function.
pub async fn annotate(
    nvim: &Rpc,
    functions: &[FunctionCcn],
    baseline: &HashMap<String, u32>,
) -> Result<usize, String> {
    let buffer = nvim
        .get_current_buf()
        .await
        .map_err(|e| format!("get_current_buf: {e}"))?;
    let id = buffer_id(&buffer)?;
    let entries = functions
        .iter()
        .map(|function| {
            nvim_rs::Value::Map(vec![
                (
                    nvim_rs::Value::from("line"),
                    nvim_rs::Value::from(function.line as i64),
                ),
                (
                    nvim_rs::Value::from("ccn"),
                    nvim_rs::Value::from(i64::from(function.ccn)),
                ),
                (
                    nvim_rs::Value::from("delta"),
                    nvim_rs::Value::from(i64::from(positive_delta(function, baseline))),
                ),
            ])
        })
        .collect();
    let placed = nvim
        .exec_lua(ANNOTATE_LUA, vec![nvim_rs::Value::from(id), entries])
        .await
        .map_err(|e| format!("exec_lua annotate: {e}"))?
        .as_i64()
        .unwrap_or(0);
    Ok(placed as usize)
}

const MARKS_LUA: &str = r#"
local buf = ...
local ns = vim.api.nvim_create_namespace('ccn_workbench')
local raw = vim.api.nvim_buf_get_extmarks(buf, ns, 0, -1, { details = true })
local out = {}
for _, mark in ipairs(raw) do
  local details = mark[4] or {}
  local virt = details.virt_text
  local text = ''
  local hl = details.hl_group or ''
  if virt and virt[1] then
    text = virt[1][1] or ''
    hl = virt[1][2] or hl
  end
  out[#out + 1] = { row = mark[2], text = text, hl = hl }
end
return out
"#;

pub async fn dump_marks(nvim: &Rpc) -> Result<Vec<MarkInfo>, String> {
    let buffer = nvim
        .get_current_buf()
        .await
        .map_err(|e| format!("get_current_buf: {e}"))?;
    let id = buffer_id(&buffer)?;
    let value = nvim
        .exec_lua(MARKS_LUA, vec![nvim_rs::Value::from(id)])
        .await
        .map_err(|e| format!("exec_lua marks: {e}"))?;
    let items = match value {
        nvim_rs::Value::Array(items) => items,
        other => return Err(format!("unexpected extmark payload: {other:?}")),
    };
    Ok(items
        .iter()
        .map(|item| MarkInfo {
            row: field(item, "row")
                .and_then(nvim_rs::Value::as_i64)
                .unwrap_or(-1),
            text: field(item, "text")
                .and_then(nvim_rs::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hl: field(item, "hl")
                .and_then(nvim_rs::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

async fn jump(nvim: &Rpc, line: usize) -> Result<(), String> {
    const JUMP_LUA: &str = r#"
local line = ...
vim.api.nvim_win_set_cursor(0, { line, 0 })
vim.cmd('normal! zz')
"#;
    nvim.exec_lua(JUMP_LUA, vec![nvim_rs::Value::from(line as i64)])
        .await
        .map_err(|e| format!("exec_lua jump: {e}"))?;
    Ok(())
}

fn buffer_id(buffer: &nvim_rs::Buffer<Writer>) -> Result<i64, String> {
    let value = buffer.get_value();
    handle_number(value).ok_or_else(|| format!("cannot decode the buffer handle: {value:?}"))
}

/// Neovim sends Buffer as a msgpack ext value whose payload is an integer.
fn handle_number(value: &nvim_rs::Value) -> Option<i64> {
    match value {
        nvim_rs::Value::Ext(0, payload) => msgpack_integer(payload),
        other => other.as_i64(),
    }
}

fn msgpack_integer(bytes: &[u8]) -> Option<i64> {
    let first = *bytes.first()?;
    let rest = &bytes[1..];
    let unsigned = |len: usize| -> Option<i64> {
        let slice = rest.get(..len)?;
        slice.iter().try_fold(0i64, |acc, byte| {
            acc.checked_mul(256)?.checked_add(i64::from(*byte))
        })
    };
    match first {
        0x00..=0x7f => Some(i64::from(first)),
        0xe0..=0xff => Some(i64::from(first as i8)),
        0xcc => unsigned(1),
        0xcd => unsigned(2),
        0xce => unsigned(4),
        0xcf => unsigned(8),
        0xd0 => Some(i64::from(rest.first().copied()? as i8)),
        0xd1 => Some(i64::from(i16::from_be_bytes(
            rest.get(..2)?.try_into().ok()?,
        ))),
        0xd2 => Some(i64::from(i32::from_be_bytes(
            rest.get(..4)?.try_into().ok()?,
        ))),
        0xd3 => Some(i64::from_be_bytes(rest.get(..8)?.try_into().ok()?)),
        _ => None,
    }
}

fn field<'a>(map: &'a nvim_rs::Value, key: &str) -> Option<&'a nvim_rs::Value> {
    map.as_map()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(identity: &str, ccn: u32) -> FunctionCcn {
        FunctionCcn {
            name: identity.to_string(),
            identity: identity.to_string(),
            line: 1,
            end_line: 1,
            nloc: 1,
            ccn,
            params: 0,
            max_nesting: 0,
        }
    }

    #[test]
    fn deltas_are_new_worse_and_improved() {
        let baseline = baseline_map(&[
            function("same(int)", 3),
            function("worse(int)", 2),
            function("better(int)", 5),
        ]);
        assert_eq!(positive_delta(&function("same(int)", 3), &baseline), 0);
        assert_eq!(positive_delta(&function("worse(int)", 4), &baseline), 2);
        assert_eq!(positive_delta(&function("better(int)", 2), &baseline), 0);
        assert_eq!(positive_delta(&function("new(int)", 7), &baseline), 7);
    }

    #[test]
    fn only_nvim_program_names_enable_listen() {
        assert!(is_nvim_program("nvim"));
        assert!(is_nvim_program(r"C:\tools\nvim.exe"));
        assert!(is_nvim_program("/usr/bin/nvim-qt"));
        assert!(!is_nvim_program("vim"));
        assert!(!is_nvim_program("code"));
    }

    #[test]
    fn free_listen_address_is_loopback() {
        let address = free_listen_address().expect("reserve a port");
        assert!(address.starts_with("127.0.0.1:"));
    }
}
