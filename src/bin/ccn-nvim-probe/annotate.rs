//! Extmark annotation through `nvim_exec_lua`.
//!
//! All writes happen in the dedicated `ccn_rpc` namespace. Clearing that
//! namespace cannot touch user marks (`m`/`'` marks) or other plugins'
//! extmarks, which live in their own namespaces.

use ccn::ccn::CcnSummary;
use nvim_rs::Value;

use crate::client::{BufferHandle, Rpc};
use crate::report::MarkRow;
use crate::ProbeResult;

/// The only namespace this probe ever writes to.
pub const NAMESPACE: &str = "ccn_rpc";

/// Create the namespace, drop our previous annotations, and place one
/// end-of-line virtual text per function.
const ANNOTATE_LUA: &str = r#"
local buf, entries = ...
local ns = vim.api.nvim_create_namespace('ccn_rpc')
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

  local ok = pcall(vim.api.nvim_buf_set_extmark, buf, ns, row, 0, {
    virt_text = { { 'CCN ' .. entry.ccn, hl } },
    virt_text_pos = 'eol',
    hl_group = hl,
    priority = 200,
  })
  if ok then placed = placed + 1 end
end
return placed
"#;

/// Read back what `ANNOTATE_LUA` placed, including the rendered label so the
/// test can assert the CCN value actually made it into the buffer.
const MARKS_LUA: &str = r#"
local buf = ...
local ns = vim.api.nvim_create_namespace('ccn_rpc')
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
  out[#out + 1] = {
    id = mark[1],
    row = mark[2],
    col = mark[3],
    text = text,
    hl = hl,
  }
end
return out
"#;

pub async fn annotate(
    nvim: &Rpc,
    buffer: &BufferHandle,
    summary: &CcnSummary,
) -> ProbeResult<usize> {
    let id = crate::client::buffer_id(buffer).await?;
    let entries = Value::Array(summary.detail.iter().map(entry).collect());
    let placed = nvim
        .exec_lua(ANNOTATE_LUA, vec![Value::from(id), entries])
        .await?
        .as_i64()
        .unwrap_or(0);
    Ok(placed as usize)
}

fn entry(function: &ccn::ccn::FunctionCcn) -> Value {
    Value::Map(vec![
        (Value::from("line"), Value::from(function.line as i64)),
        (Value::from("ccn"), Value::from(i64::from(function.ccn))),
    ])
}

pub async fn marks(nvim: &Rpc, buffer: &BufferHandle) -> ProbeResult<Vec<MarkRow>> {
    let id = crate::client::buffer_id(buffer).await?;
    let value = nvim.exec_lua(MARKS_LUA, vec![Value::from(id)]).await?;
    let items = match value {
        Value::Array(items) => items,
        other => return Err(format!("nvim_buf_get_extmarks returned {other:?}").into()),
    };
    Ok(items
        .iter()
        .map(|item| MarkRow {
            id: field(item, "id").and_then(Value::as_i64).unwrap_or(-1),
            row: field(item, "row").and_then(Value::as_i64).unwrap_or(-1),
            col: field(item, "col").and_then(Value::as_i64).unwrap_or(-1),
            text: field(item, "text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hl: field(item, "hl")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

fn field<'a>(map: &'a Value, key: &str) -> Option<&'a Value> {
    map.as_map()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}
