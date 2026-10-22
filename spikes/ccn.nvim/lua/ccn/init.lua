-- ccn.nvim - show per-function cyclomatic complexity from the `ccn` CLI.
--
-- Design constraints, in order:
--   1. Never touch the buffer. Only extmarks and diagnostics.
--   2. One process per refresh, never per keystroke.
--   3. Only the latest result may be applied (monotonic token per buffer).
--   4. Degrade quietly: a missing binary or an unparsable file must not spam.

local M = {}

local api = vim.api

-- Extmarks and diagnostics get separate namespaces: clearing diagnostics must
-- not clear virtual text, and the test inspects the extmark namespace by name.
M.ns_marks = api.nvim_create_namespace("ccn.marks")
M.ns_diag = api.nvim_create_namespace("ccn.diag")

-- Mirrors `Lang::from_path` in src/ccn/types.rs. `.h` is C there, deliberately.
local DEFAULT_LANGUAGES = {
  js = true, jsx = true, mjs = true, cjs = true,
  ts = true, mts = true, cts = true, tsx = true,
  py = true, pyi = true,
  rs = true,
  go = true,
  java = true,
  c = true, h = true,
  cc = true, cpp = true, cxx = true, ["c++"] = true,
  hpp = true, hh = true, hxx = true,
  cs = true,
}

local SEVERITY_ERROR_MAX = 2 -- error above threshold * 2 (default: > 20)
local SEVERITY_CRIT_MAX = 5  -- "critical" wording above threshold * 5 (default: > 50)

-- Per-buffer: { seq = <last issued token>, last_error = <string|nil> }.
local state = {}
local notified = {}

M.enabled = true
M.last_error = nil

local function get_cfg()
  local bin = vim.g.ccn_bin
  if type(bin) ~= "string" or bin == "" then
    bin = "ccn"
  end

  local threshold = tonumber(vim.g.ccn_threshold)
  if not threshold then
    threshold = 10
  end
  threshold = math.floor(threshold)
  if threshold < 1 then
    threshold = 1
  end

  local timeout = tonumber(vim.g.ccn_timeout)
  if not timeout or timeout <= 0 then
    timeout = 30000
  end

  local languages = DEFAULT_LANGUAGES
  if type(vim.g.ccn_languages) == "table" then
    languages = {}
    for _, ext in ipairs(vim.g.ccn_languages) do
      languages[tostring(ext):lower():gsub("^%.", "")] = true
    end
  end

  return { bin = bin, threshold = threshold, timeout = timeout, languages = languages }
end

local function buf_state(buf)
  local s = state[buf]
  if not s then
    s = { seq = 0 }
    state[buf] = s
  end
  return s
end

local function notify_once(key, msg, level)
  if notified[key] then
    return
  end
  notified[key] = true
  vim.notify("ccn.nvim: " .. msg, level or vim.log.levels.WARN)
end

local function basename(path)
  return (path:gsub("\\", "/")):match("([^/]+)$") or path
end

local function norm(path)
  local ok, normalized = pcall(vim.fs.normalize, path)
  if not ok then
    normalized = path
  end
  return (normalized:gsub("\\", "/"))
end

local function same_path(a, b)
  if a == b then
    return true
  end
  if vim.fn.has("win32") == 1 then
    return a:lower() == b:lower()
  end
  return false
end

local function clear(buf)
  if not api.nvim_buf_is_valid(buf) then
    return
  end
  api.nvim_buf_clear_namespace(buf, M.ns_marks, 0, -1)
  vim.diagnostic.set(M.ns_diag, buf, {})
end

local function clear_all()
  for buf in pairs(state) do
    clear(buf)
  end
end

local function ext_for(path)
  local ext = path:match("%.([%w%+]+)$")
  return ext and ext:lower() or nil
end

local function supported_language(path, cfg)
  local ext = ext_for(path)
  return ext ~= nil and cfg.languages[ext] == true
end

-- An executable name without a separator is looked up on PATH; anything else is
-- a path the user pointed at explicitly.
local function bin_available(bin)
  if bin:find("[/\\]") then
    return vim.fn.filereadable(bin) == 1
  end
  return vim.fn.executable(bin) == 1
end

--- Run `ccn` asynchronously. `on_done` always receives a table shaped like
--- vim.system's result: { code = <number>, signal = <number|0>, stdout, stderr }.
--- Returns true when a job was started.
local function run_async(cmd, opts, on_done)
  if vim.system then
    -- vim.system can raise on spawn failure (e.g. a missing binary), so the
    -- call itself is guarded.
    local ok = pcall(vim.system, cmd, {
      cwd = opts.cwd,
      text = true,
      timeout = opts.timeout,
    }, function(obj)
      vim.schedule(function()
        on_done(obj)
      end)
    end)
    return ok
  end

  -- Neovim 0.9 fallback. Same contract, buffered, still asynchronous.
  local stdout, stderr = {}, {}
  local job = vim.fn.jobstart(cmd, {
    cwd = opts.cwd,
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = function(_, data)
      if data then
        stdout = data
      end
    end,
    on_stderr = function(_, data)
      if data then
        stderr = data
      end
    end,
    on_exit = function(_, code)
      on_done({
        code = code,
        signal = 0,
        stdout = table.concat(stdout, "\n"),
        stderr = table.concat(stderr, "\n"),
      })
    end,
  })
  return job > 0
end

--- Pick the FileLoc entry for `path` out of a ScanResult.
--- `ccn <file> --json` reports `root` as the file's directory and `path` as the
--- basename, so files[1] is the right answer; the matches are belt and braces
--- for future whole-tree scans and for Windows separator drift.
local function pick_file(data, path)
  local files = data.files
  if type(files) ~= "table" or #files == 0 then
    return nil
  end

  local want = norm(path)
  for _, entry in ipairs(files) do
    if type(entry.path) == "string" then
      local candidate = norm(entry.path)
      if same_path(candidate, want) then
        return entry
      end
      if type(data.root) == "string" and same_path(norm(data.root .. "/" .. entry.path), want) then
        return entry
      end
      if same_path(basename(candidate), basename(want)) then
        return entry
      end
    end
  end
  return files[1]
end

--- Turn one CcnSummary into extmarks + diagnostics. Pure apart from the buffer
--- writes, so the counting rules are easy to audit.
local function apply(buf, summary, cfg)
  clear(buf)

  local detail = summary and summary.detail
  if type(detail) ~= "table" or #detail == 0 then
    -- Heuristic-tier files carry one file-level number and no functions; there
    -- is no line to point at, so they intentionally produce nothing.
    return 0
  end

  local line_count = api.nvim_buf_line_count(buf)
  local diags = {}
  local placed = 0

  for _, fn in ipairs(detail) do
    local ccn = tonumber(fn.ccn) or 0
    if ccn > cfg.threshold then
      local lnum = (tonumber(fn.line) or 1) - 1
      if lnum >= 0 and lnum < line_count then
        local is_error = ccn > cfg.threshold * SEVERITY_ERROR_MAX
        local label = string.format(" CCN %d ", ccn)
        if ccn > cfg.threshold * SEVERITY_CRIT_MAX then
          label = string.format(" CCN %d !! ", ccn)
        end

        api.nvim_buf_set_extmark(buf, M.ns_marks, lnum, 0, {
          virt_text = { { label, is_error and "CcnError" or "CcnWarn" } },
          virt_text_pos = "eol",
          hl_mode = "combine",
        })

        local identity = fn.identity
        if type(identity) ~= "string" or identity == "" then
          identity = fn.name or "(anonymous)"
        end
        local message = string.format(
          "%s: CCN %d (threshold %d), %d lines, max nesting %d",
          identity,
          ccn,
          cfg.threshold,
          tonumber(fn.nloc) or 0,
          tonumber(fn.max_nesting) or 0
        )

        diags[#diags + 1] = {
          bufnr = buf,
          lnum = lnum,
          col = 0,
          end_lnum = lnum,
          source = "ccn",
          code = "ccn",
          severity = is_error and vim.diagnostic.severity.ERROR or vim.diagnostic.severity.WARN,
          message = message,
        }
        placed = placed + 1
      end
    end
  end

  vim.diagnostic.set(M.ns_diag, buf, diags)
  return placed
end

local function fail(buf, token, message)
  local s = buf_state(buf)
  if s.seq ~= token then
    return
  end
  s.last_error = message
  M.last_error = message
  clear(buf)
  notify_once("failure:" .. message, message)
end

--- Apply one finished CLI run. Stale runs are dropped by token.
function M._on_result(buf, token, path, obj)
  local s = buf_state(buf)
  if s.seq ~= token then
    return -- a newer refresh already started
  end
  if not api.nvim_buf_is_valid(buf) then
    return
  end

  local code = tonumber(obj.code) or -1
  if obj.signal and obj.signal ~= 0 then
    fail(buf, token, string.format("ccn was killed (signal %d)", obj.signal))
    return
  end
  if code == 124 then
    fail(buf, token, "ccn timed out; raise vim.g.ccn_timeout or lower the file size")
    return
  end
  if code ~= 0 then
    local stderr = (obj.stderr or ""):gsub("%s+$", "")
    if stderr == "" then
      stderr = "no stderr"
    end
    fail(buf, token, string.format("ccn exited %d: %s", code, stderr))
    return
  end

  local ok, data = pcall(vim.json.decode, obj.stdout or "")
  if not ok or type(data) ~= "table" then
    fail(buf, token, "could not parse ccn --json output")
    return
  end

  local entry = pick_file(data, path)
  if not entry then
    -- A file ccn chose not to report (ignored, unsupported) is not an error.
    clear(buf)
    return
  end

  local cfg = get_cfg()
  local placed = apply(buf, entry.ccn, cfg)
  s.last_error = nil
  s.placed = placed
end

--- Refresh one buffer. `buf` defaults to the current buffer; 0 is accepted.
function M.refresh(buf)
  if not M.enabled then
    return false
  end

  buf = (buf == nil or buf == 0) and api.nvim_get_current_buf() or buf
  if not api.nvim_buf_is_valid(buf) then
    return false
  end

  -- Only real files: no scratch buffers, no terminals, no help pages.
  if vim.bo[buf].buftype ~= "" then
    clear(buf)
    return false
  end

  local name = api.nvim_buf_get_name(buf)
  if name == "" then
    clear(buf)
    return false
  end

  local path = norm(vim.fn.fnamemodify(name, ":p"))
  if vim.fn.filereadable(path) ~= 1 then
    clear(buf)
    return false
  end

  local cfg = get_cfg()
  if not supported_language(path, cfg) then
    clear(buf)
    return false
  end

  if not bin_available(cfg.bin) then
    fail(buf, buf_state(buf).seq, string.format(
      "ccn executable not found (%s); install ccn or set vim.g.ccn_bin",
      cfg.bin
    ))
    return false
  end

  local s = buf_state(buf)
  s.seq = s.seq + 1
  local token = s.seq

  -- ccn derives its root from the path it is given, so run it from the file's
  -- directory: relative paths in the JSON then line up with the buffer, and any
  -- ignore files next to the source still apply.
  local cmd = { cfg.bin, path, "--json", "--no-cache", "--no-report" }
  local started = run_async(cmd, {
    cwd = vim.fs.dirname(path),
    timeout = cfg.timeout,
  }, function(obj)
    M._on_result(buf, token, path, obj)
  end)

  if not started then
    fail(buf, token, string.format("could not start %s", cfg.bin))
    return false
  end

  return true
end

function M.enable()
  M.enabled = true
  vim.g.ccn_enabled = 1
  M.refresh()
end

function M.disable()
  M.enabled = false
  vim.g.ccn_enabled = 0
  clear_all()
end

function M.toggle()
  if M.enabled then
    M.disable()
    vim.notify("ccn.nvim: off", vim.log.levels.INFO)
  else
    M.enable()
    vim.notify("ccn.nvim: on", vim.log.levels.INFO)
  end
  return M.enabled
end

--- `:CCN` with no argument toggles; explicit verbs are accepted so scripts and
--- tests do not have to track the current state.
function M.command(args)
  local verb = (args or ""):lower():gsub("%s+", "")
  if verb == "" or verb == "toggle" then
    M.toggle()
  elseif verb == "on" then
    M.enable()
  elseif verb == "off" then
    M.disable()
  elseif verb == "status" then
    local cfg = get_cfg()
    local s = state[api.nvim_get_current_buf()] or {}
    print(string.format(
      "ccn.nvim: %s | bin=%s threshold=%d timeout=%dms | cached marks=%s last_error=%s",
      M.enabled and "on" or "off",
      cfg.bin,
      cfg.threshold,
      cfg.timeout,
      tostring(s.placed or 0),
      tostring(M.last_error or s.last_error or "none")
    ))
  else
    vim.notify("ccn.nvim: unknown argument '" .. verb .. "' (use on/off/toggle/status)", vim.log.levels.ERROR)
  end
end

local function define_highlights()
  api.nvim_set_hl(0, "CcnWarn", { link = "DiagnosticWarn", default = true })
  api.nvim_set_hl(0, "CcnError", { link = "DiagnosticError", default = true })
end

local augroup
function M.setup(opts)
  opts = opts or {}

  if opts.enabled ~= nil then
    M.enabled = opts.enabled and true or false
  elseif vim.g.ccn_enabled ~= nil then
    M.enabled = vim.g.ccn_enabled ~= 0
  end
  vim.g.ccn_enabled = M.enabled and 1 or 0

  define_highlights()

  api.nvim_create_user_command("CCN", function(cmd)
    M.command(cmd.args)
  end, {
    nargs = "*",
    desc = "Toggle ccn.nvim, or pass on|off|status",
  })

  api.nvim_create_user_command("CCNRefresh", function()
    M.refresh(0)
  end, { desc = "Re-run ccn for the current buffer" })

  augroup = api.nvim_create_augroup("ccn_nvim", { clear = true })

  api.nvim_create_autocmd("BufWritePost", {
    group = augroup,
    desc = "Refresh ccn complexity for the written file",
    callback = function(ev)
      M.refresh(ev.buf)
    end,
  })

  api.nvim_create_autocmd("BufWipeout", {
    group = augroup,
    callback = function(ev)
      state[ev.buf] = nil
    end,
  })

  return M
end

return M
