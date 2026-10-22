//! RPC connection and buffer I/O for the Neovim probe.
//!
//! Every read goes through the live RPC channel, never through the file on
//! disk: the point of the spike is measuring unsaved edits.

use std::path::Path;

use nvim_rs::compat::tokio::Compat;
use nvim_rs::error::LoopError;
use nvim_rs::Neovim;
use tokio::io::WriteHalf;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::ProbeResult;

/// The concrete writer `new_tcp` produces on this build.
pub type Writer = Compat<WriteHalf<TcpStream>>;
pub type Rpc = Neovim<Writer>;
pub type BufferHandle = nvim_rs::Buffer<Writer>;
pub type IoTask = JoinHandle<Result<(), Box<LoopError>>>;

/// Connect to a Neovim instance started with `--listen host:port`.
pub async fn connect(server: &str) -> ProbeResult<(Rpc, IoTask)> {
    let handler = nvim_rs::rpc::handler::Dummy::<Writer>::new();
    let (nvim, io) = nvim_rs::create::tokio::new_tcp(server, handler).await?;
    Ok((nvim, io))
}

/// The buffer Neovim considers current. Kept as a handle, not a copy, so
/// annotations land in the same buffer we read.
pub async fn current_buffer(nvim: &Rpc) -> ProbeResult<BufferHandle> {
    Ok(nvim.get_current_buf().await?)
}

pub async fn buffer_id(buffer: &BufferHandle) -> ProbeResult<i64> {
    let value = buffer.get_value();
    handle_number(value)
        .ok_or_else(|| format!("cannot decode the buffer handle Neovim returned: {value:?}").into())
}

/// Decode a Neovim API handle.
///
/// Neovim sends `Buffer`/`Window`/`Tabpage` as msgpack ext values (buffer is
/// ext type 0) whose payload is itself a msgpack-encoded integer. That means
/// `Value::as_i64()` returns `None` for a perfectly valid buffer handle, and
/// `nvim_buf_get_number` is deprecated on newer Neovim versions.
fn handle_number(value: &nvim_rs::Value) -> Option<i64> {
    match value {
        nvim_rs::Value::Ext(0, payload) => msgpack_integer(payload),
        other => other.as_i64(),
    }
}

/// Minimal msgpack integer decoder: enough for an API handle payload.
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

/// Read every line of the buffer and join them into one source string.
///
/// `nvim_buf_get_lines` fails when a line is not valid UTF-8; we surface that
/// as an error instead of letting the conversion panic.
pub async fn read_source(buffer: &BufferHandle) -> ProbeResult<String> {
    let lines = buffer
        .get_lines(0, -1, false)
        .await
        .map_err(|err| format!("cannot read buffer lines (invalid UTF-8?): {err}"))?;
    Ok(lines.join("\n"))
}

/// Insert one physical line after 1-based `line` without saving.
///
/// `line` must be between 1 and the buffer's line count (inclusive), which is
/// where a user would expect "insert after this line" to land.
pub async fn insert_after(buffer: &BufferHandle, line: usize) -> ProbeResult<()> {
    let line_count = buffer
        .get_lines(0, -1, false)
        .await
        .map_err(|err| format!("cannot read buffer lines: {err}"))?
        .len();
    if line < 1 || line > line_count {
        return Err(
            format!("--test-insert line {line} is outside buffer (1..={line_count})").into(),
        );
    }
    let index = line as i64;
    buffer
        .set_lines(
            index,
            index,
            false,
            vec![crate::TEST_INSERT_TEXT.to_string()],
        )
        .await?;
    Ok(())
}

/// Map the buffer name to a wired grammar.
///
/// The extension decides first (`.tsx` vs `.ts`); tokei's filename table is a
/// fallback for names without a usable extension. Unknown languages return
/// `None` so the caller can fail loudly instead of reporting a bogus tier.
pub fn detect_lang(name: &str) -> Option<ccn::ccn::Lang> {
    ccn::ccn::Lang::from_path(name).or_else(|| {
        tokei::LanguageType::from_path(Path::new(name), &tokei::Config::default())
            .and_then(|lang| ccn::ccn::Lang::from_tokei_name(lang.name()))
    })
}
