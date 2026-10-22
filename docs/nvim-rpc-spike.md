# Neovim RPC spike (ccn-nvim-probe)

Status: spike, 2026-10-22. Scope: prove that ccn can measure the buffer the
user is editing, before it is saved, and paint the result back into Neovim.

Binary: `src/bin/ccn-nvim-probe/` (`main.rs`, `client.rs`, `annotate.rs`,
`report.rs`). No changes to the library or the workbench.

## Architecture

```
nvim (--listen 127.0.0.1:PORT)
  |  msgpack-RPC over TCP
  v
ccn-nvim-probe
  client.rs   new_tcp -> get_current_buf -> buf_get_name / buf_get_lines
  main.rs     Lang::from_path -> ccn::ccn::analyze_file(bytes from RPC)
  annotate.rs nvim_exec_lua -> nvim_buf_clear_namespace + nvim_buf_set_extmark
  report.rs   one JSON document, or a compact table
```

The source string is `buf_get_lines(0, -1, false).join("\n")`. The file on
disk is never opened, written, or stat'ed by the probe.

## Commands

Build:

```
cargo build --release --bin ccn-nvim-probe
```

Run against any Neovim started with a listen address:

```
ccn-nvim-probe --server 127.0.0.1:7777 [--annotate] [--json] [--dump-marks]
ccn-nvim-probe --server 127.0.0.1:7777 --test-insert 1
```

`--test-insert N` inserts `if (x) x++;` after 1-based line N (in memory only)
and analyzes the mutated buffer. `--dump-marks` prints the extmarks that
currently live in the `ccn_rpc` namespace (to stderr in `--json` mode so stdout
stays one JSON document).

Headless test setup on Windows:

```
& "$env:TEMP\nvim-portable\nvim-win64\bin\nvim.exe" --headless `
    --listen 127.0.0.1:7777 "$env:TEMP\a.cpp"
```

The probe exits 1 and prints `ccn-nvim-probe: ...` for RPC errors, unnamed
buffers, unsupported languages, and non-UTF-8 lines.

## Observed result (unsaved buffer)

Fixture (`a.cpp`), measured through the live buffer:

```json
{
  "file": "C:\\Users\\user\\AppData\\Local\\Temp\\ccn-nvim-probe-test\\a.cpp",
  "tier": "exact",
  "functions": [
    {"name": "add", "identity": "add(int,int)", "line": 1, "ccn": 2,
     "nloc": 6, "nesting": 1, "params": 2},
    {"name": "simple", "identity": "simple()", "line": 8, "ccn": 1,
     "nloc": 3, "nesting": 0, "params": 0}
  ],
  "sum": 3,
  "max": 2
}
```

After `--test-insert 1` (`if (x) x++;` inserted after line 1, `&modified` = 1,
line 2 confirmed as `if (x) x++;` by `--remote-expr`):

```
file: ...\a.cpp  tier: exact  functions: 2  sum: 4  max: 3
  line  ccn  nloc  nest  params  name
     1    3     7     1       2  add  (add(int,int))
     9    1     3     0       0  simple  (simple())
```

So `add` moved 2 -> 3 while the file on disk still hashed to
`82AC8B5B...C094` before and after the whole run.

## Annotation behavior

All marks go into one named namespace, `ccn_rpc`
(`nvim_create_namespace("ccn_rpc")`). Each run first clears only that
namespace, so other plugins' extmarks and the user's `'a`-`'z` marks are
untouched (verified: mark `'a` at `[0, 3, 1, 0]` survived two annotate runs).

Per function, one extmark at the function's start row (column 0):

```
virt_text     = { { "CCN <n>", hl } }
virt_text_pos = "eol"
hl_group      = DiagnosticInfo  (ccn <= 10)
                DiagnosticWarn  (ccn > 10)
                DiagnosticError (ccn > 20)
```

Everything is done in a single `nvim_exec_lua` call: buffer id and the
`{line, ccn}` list are passed as arguments, no shell, no generated Lua
string interpolation. Observed read-back via `nvim_buf_get_extmarks`:

```
extmarks in namespace ccn_rpc: 2
  id=1 row=0 col=0 text="CCN 3" hl=DiagnosticInfo
  id=2 row=8 col=0 text="CCN 1" hl=DiagnosticInfo
```

Thresholds, on a fixture with CCN 12 and 22:

```
extmarks in namespace ccn_rpc: 2
  id=1 row=0 col=0 text="CCN 12" hl=DiagnosticWarn
  id=2 row=15 col=0 text="CCN 22" hl=DiagnosticError
```

A second annotate run leaves exactly two marks (no duplicates).

## Round-trip latency

Measured from just before `TcpStream::connect` to after the JSON/table is
built (connect + buffer read + tree-sitter parse + annotation when enabled):

```
elapsed_ms=2.288   read only, 2 functions
elapsed_ms=3.381   read + insert + analyze
elapsed_ms=3.871   read + analyze + annotate + read back marks
```

Parse cost is dominated by C++ tree-sitter setup on the first call; a warm
workbench process should see the lower end. These numbers are one process per
run, so they include process startup in the shell measurement only, not in
`elapsed_ms`.

## API friction

- **Handles are msgpack ext values.** `Buffer::get_value()` returns
  `Ext(0, [<msgpack int>])`, not an integer; `Value::as_i64()` returns `None`.
  `client.rs` decodes the small integer payload itself because
  `nvim_buf_get_number` is deprecated. This is the one place the spike has
  wire-format knowledge.
- **Handler type parameter.** `new_tcp` needs a `Handler<Writer = ...>`; the
  concrete writer is `nvim_rs::compat::tokio::Compat<tokio::io::WriteHalf<TcpStream>>`.
  `Dummy::<Writer>::new()` avoids implementing an async trait (which would
  need `async-trait` as a direct dependency).
- **Lua varargs.** `exec_lua(code, args)` passes `args` to the chunk as `...`;
  the probe passes `buf, entries` and receives a plain integer.
- **Io task.** `new_tcp` returns a `JoinHandle` for the reader loop. The probe
  aborts it at the end; a long-lived integration would keep it and watch for
  channel close to trigger reconnect.

## Windows notes

- `127.0.0.1:PORT` TCP worked on the portable build; named pipes were not
  needed. nvim-rs also exposes `create::tokio::new_path` for
  `\\.\pipe\...` if a future setup prefers it.
- `nvim --remote-expr` without `--headless` opens a visible UI when the
  expression is evaluated from a script; use `--headless --remote-expr` for
  test assertions.
- `Start-Process nvim --headless --listen ...` plus a TCP connect poll is a
  reliable readiness check; a raw connect/close is enough and does not disturb
  the session.

## What the workbench needs

1. **Spawn with a listen address.** Start or attach to
   `nvim --listen 127.0.0.1:<port>`; never use `--embed` for the external
   editor path, because the workbench must reconnect independently.
2. **Reconnect and retry.** Treat a closed channel as "Neovim exited"; re-run
   `new_tcp` with backoff and re-read the buffer. Do not cache the `Buffer`
   handle across a reconnect.
3. **Debounce.** Measure on `BufModified`/`TextChanged`/`TextChangedI` with
   roughly 150-300 ms of quiet, coalescing rapid keystrokes; the 2-4 ms
   round trip here says debouncing is about CPU, not latency.
4. **Buffer attach.** For live deltas, `nvim_buf_attach(buf, true, {})` and
   consume `nvim_buf_lines_event`; the spike's poll-on-demand read is enough
   for the first integration, attach is the follow-up.
5. **Baseline/delta.** The annotate script already has the hook for
   `(+delta)`; the workbench should pass a `ccn` + `baseline` pair and render
   `CCN <n> (+<d>)` only when a baseline file is loaded.
6. **Clear on detach.** Always clear the `ccn_rpc` namespace before leaving a
   buffer or shutting down so stale labels never outlive the analysis.
