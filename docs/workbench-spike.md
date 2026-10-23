# ccn workbench spike

First spike for a Neovim-centric workbench: a real Neovim TUI runs inside a
ratatui pane next to ccn's per-file complexity sidebar.

Status: **Workbench v1.1: PTY editor + live RPC verified on Windows, WSL
Ubuntu 26.04 (Neovim 0.12.6) and AlmaLinux 8 (EPEL Neovim 0.8, CI).**
The unified dashboard now has a directory tree, `Ctrl+P` fuzzy open,
multi-buffer tabs in one Neovim, and session restore. `src/workbench/` is the
implementation; the public entry point is `ccn workbench` (single file) and
`ccn . --tui` (dashboard + split).

## What works

- Spawns the editor in a PTY (`portable-pty`; ConPTY on Windows) sized to the
  editor pane, not the whole terminal. Neovim is started with
  `--listen 127.0.0.1:<free port>`.
- Reads PTY output in 8 KiB chunks into a shared `vt100::Parser`; renders it
  with `tui-term`.
- Forwards keystrokes byte-for-byte: UTF-8 chars, Enter, Esc, Tab/BackTab,
  Backspace, arrows, Home/End, Insert/Delete, PageUp/PageDown, F1-F8/F11-F12,
  `Ctrl+<letter>` as `0x01..0x1a`, and Alt as an `ESC` prefix.
- RPC worker (own thread + current-thread tokio runtime) reads the live buffer
  every ~150 ms when `nvim_buf_get_changedtick` changes, measures CCN with
  `ccn::ccn::analyze_file`, and streams snapshots to the UI.
- Header shows path, `rpc live (N ms)` / `rpc connecting` / `rpc off`, `unsaved`
  when the buffer tick differs from connect time, LOC, live vs initial function
  count, CCN max/sum, tier, grade, `delta +N`, and focus.
- Right sidebar renders the live buffer (same worst-first layout: line, CCN,
  name, identity). Deltas against the initial file scan are shown in a compact
  column: `+new`, `~worsened`, `-improved`.
- Inline extmarks in namespace `ccn_workbench`: one end-of-line
  ` CCN <n> ` (plus ` (+<d>)` for new/worsened) per function, with
  `DiagnosticWarn` above 10 and `DiagnosticError` above 20.
- Sidebar `Enter`/`o` jumps Neovim's cursor to the selected function and runs
  `normal! zz`, then focuses the editor.
- Focus model: editor by default. `F9` toggles sidebar; `j/k`, `g/G` move,
  `r` rescans, `F9`/`Esc` returns. While the editor is focused, every key
  except `F9`/`F10` and the tab keys goes to Neovim.
- Split navigation: the left pane is a collapsible directory tree (`j/k`,
  `g/G`, `Enter`/Space toggles a directory, `e`/`Enter` opens a file). `F8`
  switches tree/editor focus; `[`/`]` or dragging the divider changes the
  split ratio (20-70%, minimum pane widths). Tree rows show Git/P4 change
  markers; `c` toggles changed-files-only.
- `Alt+E` opens the Explain CCN view for the selected function: a heuristic
  decision breakdown (if/for/while/case/catch/&&/||/?) with the top decision
  lines. The tree-sitter CCN remains authoritative.
- Expanded tree directories are persisted in the session.
- `Ctrl+P` fuzzy file picker ranks measured source paths with a dependency-free
  subsequence scorer and opens the selected file as a tab.
- Multi-buffer tabs use one Neovim process: a new file is opened with a Lua
  `:edit` RPC (modified buffers are hidden, not discarded), and `F2`/`F3`
  switch, `F4` closes a clean buffer (`:bdelete`), dirty buffers must be saved.
- With `--baseline <rev>`, `F6` opens the baseline review panel. `f` toggles
  current-file/all-files, `a`/`w`/`u` filter added/worsened/all, `{`/`}` jump
  between files, `n/p` or `j/k` move, `d` opens the baseline-vs-live line diff
  (`s` toggles side-by-side), `x` marks the selected finding reviewed, Enter
  opens the owning tab and jumps to the line. It consumes `crate::baseline::Change` rows already produced by the
  scanner; the diff reads baseline bytes via `vcs::file_at` and live bytes from
  the RPC snapshot, so unsaved edits are included and no file is written.
- `F7` opens diagnostics: scan read errors/oversize/no-grammar and
  best-effort/heuristic files, baseline completeness categories, and live
  editor/RPC errors. `e`/`w`/`i` filter errors/warnings/info, `u` clears the
  filter. `F5` opens the fuzzy command palette.
- Baseline diffs support `[`/`]` hunk navigation; the subtitle shows the active
  hunk position.
- The dashboard header shows the git branch when available (Perforce shows the
  backend label without blocking startup on network commands).
- `Alt+O` opens the current-file symbol palette; fuzzy-match a function and
  Enter jumps to it. `Alt+F` searches workspace text and opens the selected
  `path:line` in a tab. The function sidebar is display-only for navigation:
  click a function row to jump, or use Alt+O. `F9` remains an advanced focus
  alias for compatibility.
- `.ccn.toml` can set `editor`, `baseline`, and `keymap`; CLI flags win. See
  [`docs/config.md`](config.md).
- The header shows the selected function's breadcrumb
  (`namespace > class > function(params)`), derived from the stable identity.
  The embedded editor remains the only place that performs edits.
- The editor pane is a real Neovim process in a PTY, not a reimplementation:
  modes, motions/operators, undo, `:` commands, registers and the user's vimrc
  apply. ccn adds `-c "set number"` after the vimrc so line numbers are on by
  default; `:set nonumber` still works. Mouse clicks are not forwarded into the
  editor, and some modifier+arrow sequences use the plain xterm form.
- Session state (split ratio, dashboard sort/filter/selection/tab, open and
  active buffers, reviewed findings) is stored in the OS cache sidecar, never in
  the scanned tree. Restored tabs are lazy: the first switch issues `:edit`.
- Window resize calls `MasterPty::resize` and `vt100::Parser::set_size`.
- Child exit leaves the workbench open, marks `[editor exited]`, stops the RPC
  worker, and `F10` quits.
- `--no-rpc`, a non-Neovim editor, or a failed connection keeps the existing
  mtime-based refresh. When RPC is live, mtime polling is skipped.
- `--smoke`, `--smoke-nvim`, `--smoke-utf8`, `--smoke-rpc`, and
  `--smoke-rpc-edit` run without an interactive terminal.

## Run

```text
cargo run --release -- workbench path/to/file.c
cargo run --release -- workbench --editor "<path-to-nvim>" file.cpp
cargo run --release -- workbench --smoke
cargo run --release -- workbench --smoke-nvim
cargo run --release -- workbench --editor "<path>" --smoke-nvim
cargo run --release -- workbench --smoke-utf8
cargo run --release -- workbench --editor "<path>" --smoke-utf8
cargo run --release -- workbench --smoke-rpc
cargo run --release -- workbench --editor "<path>" --smoke-rpc
cargo run --release -- workbench --editor "<path>" --smoke-rpc-edit
cargo run --release -- workbench --no-rpc path/to/file.c
```

`--editor` defaults to `nvim`. A command with arguments must be quoted at the
shell level; the spike understands double quotes inside it but performs no
shell expansion. A directory argument is rejected with an explicit message
(this spike opens one file). `--smoke-*` modes exit 0 with a `skip:` line when
the editor is not found. `--no-rpc` (or a non-Neovim editor) uses mtime polling
only.

The UTF-8 smoke is the byte-path test, suitable for CI:

```text
# Windows
cargo run --release -- workbench --smoke-utf8 --editor "$env:TEMP\nvim-portable\nvim-win64\bin\nvim.exe"

# WSL Ubuntu: build, then run under a PTY
wsl -d Ubuntu -- bash -c 'export PATH=/home/jcpuk/.cargo/bin:/usr/local/bin:/usr/bin:/bin; cd /mnt/c/Users/user/source/CCN; CARGO_TARGET_DIR=/home/jcpuk/ccn-target cargo build --release --locked'
wsl -d Ubuntu -- bash -c 'export PATH=/usr/local/bin:/usr/bin:/bin; script -qec "/home/jcpuk/ccn-target/release/ccn workbench --smoke-utf8 --editor /home/jcpuk/tools/nvim-linux-x86_64/bin/nvim" /dev/null'
```

RPC smoke commands:

```text
# Windows
cargo run --release -- workbench --smoke-rpc --editor "$env:TEMP\nvim-portable\nvim-win64\bin\nvim.exe"

# WSL
wsl -d Ubuntu -- bash -c 'export PATH=/usr/local/bin:/usr/bin:/bin; script -qec "/home/jcpuk/ccn-target/release/ccn workbench --smoke-rpc --editor /home/jcpuk/tools/nvim-linux-x86_64/bin/nvim" /dev/null'
```

Gates (with the feature enabled):

```text
cargo build --release --locked
cargo test --release --locked
cargo clippy --release --locked --all-targets -- -D warnings
cargo fmt --check
```

## Layout (condensed ASCII capture, 130x40 host, live RPC)

```text
 ccn workbench  manual.c                     rpc live (1 ms)  unsaved
 C - 6 LOC - 2 functions live - CCN max 2 - sum 3 - tier exact
 grade A - low complexity - focus: editor - scan 6.0 ms  d +1
 + editor - nvim -----------------------------+ functions - live - F9 back -+
 | int one(int x) {                 CCN 2 (+1)| manual.c - 6 LOC              |
 |     return x;                             | 2 functions live - max 2 ...  |
 |     if (x) x++;                           | tier exact - grade A - d +1   |
 | }                                         | unsaved buffer - +new ~worse  |
 | int two(int x) {                 CCN 1     | >  1   2   ~1 one             |
 |     return x;                             |       one(int)                |
 | }                                         |    5   1      two             |
 |                                           |       two(int)                |
 |~                                          |                               |
 |~\AppData\Local\Temp\...\manual.c   3,15 All|                              |
 +-------------------------------------------+-------------------------------+
 F10 quit - F9 focus sidebar - typing goes to the editor - r rescans in the sidebar
 jump -> line 1
```

The screen capture is condensed; the real render uses box-drawing borders and
the Aura palette. `CCN 2 (+1)` is the extmark, `~1` is the sidebar delta, and
`d +1` is the sum of positive CCN changes (`delta` in the UI, kept ASCII here).

## RPC architecture

- A free loopback port is reserved by binding `127.0.0.1:0`, released, and
  passed as `nvim --listen 127.0.0.1:<port> <existing args> <file>`. Only an
  editor basename containing `nvim` gets this flag; `--no-rpc` or another
  editor skips it.
- A dedicated OS thread owns a current-thread tokio runtime and one
  `nvim-rs` TCP connection. It retries every 500 ms for up to 10 s, reports
  `connecting` then `live`, and keeps retrying quietly after the initial
  window while the PTY child is alive. UI channel closure, `F10`, or child exit
  stops it.
- Every 150 ms the worker reads `nvim_buf_get_changedtick`. A changed tick (or
  the first connection) triggers `nvim_buf_get_name` +
  `nvim_buf_get_lines(0,-1,false)`, `ccn::ccn::analyze_file`, and a snapshot
  `{name, tier, functions, sum, max, changedtick, base_tick, read_ms, error}`.
  `base_tick` is captured on connect and drives the `unsaved` label; saves are
  not fabricated.
- Snapshots go to the UI through `std::sync::mpsc`; the crossterm loop drains
  them with `try_recv` and never waits on RPC. A dropped receiver cannot panic
  the worker; it exits.
- The UI compares live functions to the initial file scan by
  `FunctionCcn.identity` (falling back to the name when identity is empty).
  New/worsened/improved deltas are displayed and the sum of positive changes is
  shown as `delta +N`. The scan snapshot remains the initial value until the
  session ends; a reconnect also keeps it.
- Extmarks are written after every successful changedtick snapshot. The Lua
  script receives the buffer handle and an argument array; it creates/clears
  only namespace `ccn_workbench`, then sets one `virt_text` mark at `line - 1`
  with `virt_text_pos = "eol"`. User marks and other namespaces are untouched.
  A Lua error becomes a non-fatal `rpc:` note while the sidebar keeps its data.
- `Enter`/`o` in the sidebar sends a `Jump { line }` command. The worker calls
  `nvim_win_set_cursor(0, {line, 0})` and `normal! zz`; the UI switches focus
  back to the editor. Without a live connection the footer says so instead.
- When RPC is live, the 500 ms mtime poll is disabled. On `off`, `--no-rpc`, a
  non-Neovim editor, or a dropped connection, the scan/mtime fallback runs
  again. A manual `r` still rescans while live (the sidebar keeps using the
  live buffer).

Neovim version matrix:

| Version | Platform | Result |
| --- | --- | --- |
| 0.12.6 | Windows (portable, ConPTY) | `--smoke-rpc` and `--smoke-rpc-edit` pass |
| 0.12.6 | WSL Ubuntu 26.04 | both RPC smokes pass under `script` |
| 0.8 EPEL | AlmaLinux CI | not run here; only APIs present in 0.8 are used (`get_current_buf`, `get/set lines`, `changedtick`, `exec_lua`, `create_namespace`, `clear_namespace`, `set/get extmarks`, `win_set_cursor`) |

## Windows result

Verified with Neovim 0.12.6 (portable) under a ConPTY-driven harness at
130x40, and with the release binary:

- Neovim paints its splash/statusline inside the pane and opens the passed
  file (`manual.c` in the statusline).
- Typing `i ... <Esc> :w` into the pane saved the file; the sidebar refreshed
  to the new function list and footer showed
  `saved -> rescanned: 3 functions - 7 LOC - 5 ms`.
- Editing the file from another process while Neovim was open triggered the
  500 ms mtime rescan in `--no-rpc` mode and showed the new function.
- `F9` changed focus to the sidebar, `j` moved the selection, `F9` returned.
- Resizing to 100x30 and to 12x10 redrew correctly and did not panic; `F10`
  exited.
- `:q` left the workbench open with `[editor exited: exit 0]`; `F10` exited 0.
- `--smoke` output: `smoke ok` (exit 0).
- `--smoke-nvim` (portable Neovim path) printed the screen tail ending in
  `ccn_workbench_smoke.c   1,1   All` and then `smoke-nvim ok` (exit 0).
- `--smoke-utf8` printed `smoke-utf8 ok` (exit 0): the Korean text typed via
  `encode_key` survived ConPTY and was saved to the file.
- Interactive ConPTY harness (Neovim 0.12.6, 130x40): header showed
  `rpc live (0-1 ms)` and `unsaved` after typing `o` + `if (x) x++;` without
  saving. The editor showed `CCN 2  (+1)` at the function's line; the sidebar
  refreshed to `max 2 - sum 3`, `d +1`, and the row `1  2  ~1 one`. The file on
  disk was byte-identical afterwards. `Enter` in the sidebar moved Neovim from
  `3,15` to the selected function's `1,1`, and `F10` exited.
- `--smoke-rpc`: 2 functions, `read_ms` 0.8, 2 extmarks placed/dumped, 1.4 ms
  post-connect, exit 0.
- `--smoke-rpc-edit`: changedtick 3 -> 14, sum 3 -> 4 without any `:w`, disk
  unchanged, extmark `CCN 2  (+1)`; 397 ms post-connect including typing
  delays, exit 0.

### Linux (WSL Ubuntu 26.04, Neovim 0.12.6)

Run under `script` (a real PTY, no host terminal answering probes):

```text
$ script -qec ".../ccn workbench --smoke-nvim --editor .../nvim" /dev/null
--- nvim screen tail ---
~
...
ccn_workbench_smoke.c                                         1,1            All
smoke-nvim ok

$ script -qec ".../ccn workbench --smoke-utf8 --editor .../nvim" /dev/null
smoke-utf8 ok

$ script -qec ".../ccn workbench --smoke-rpc --editor .../nvim" /dev/null
... read_ms: 0.6 ...
extmarks placed: 2  dumped: 2
smoke-rpc ok

$ script -qec ".../ccn workbench --smoke-rpc-edit --editor .../nvim" /dev/null
edit: changedtick 3 -> 14  sum 3 -> 4 (disk unchanged)
smoke-rpc-edit ok
```

Before the query bridge, Neovim logged
`E1568: Terminal did not respond to DSR request for 'background' color`.
It is now gone. Neovim sends `OSC 11;?` + `CSI 5n` in two phases (the first
after its DA1 probe); the bridge answers each occurrence, including the repeat,
so the 100 ms synchronous DSR wait is satisfied.

## Terminal query bridge

The PTY reader also acts as the missing terminal emulator for capability
probes. It scans the raw output stream for complete queries, keeps a bounded
partial suffix so a query split across `read()` calls is still recognized, and
writes the reply back to the PTY. Ordinary output is never modified.

| Query | Reply |
| --- | --- |
| `OSC 11;?` (BEL or ST) | `OSC 11;rgb:1e1e/1e1e/1e1e` ST |
| `OSC 10;?` (BEL or ST) | `OSC 10;rgb:ffff/ffff/ffff` ST |
| `CSI 6n` | `CSI 1;1R` |
| `CSI 5n` | `CSI 0n` |
| `CSI c` | `CSI ?1;2c` (DA1) |
| `CSI >c` | `CSI >0;276;0c` (DA2) |
| `CSI ?<mode>$p` | `CSI ?<mode>;4$y` (DECRQM, generic) |
| `CSI ?u` | `CSI ?0u` (kitty keyboard: unsupported) |

The generic DECRQM reply covers the modes Neovim probes (`69`, `2004`, `2026`,
`2027`, `2031`, `2048`); "permanently reset" is accurate because the bridge
does not implement any of them. `CSI 14t` / `CSI 16t` (pixel size) are
deliberately **not** answered: pixel dimensions are unknown, Neovim does not
block on them, and a guessed `CSI 4;...t` would be worse than silence. DCS
`XTGETTCAP` (`ESC P $ q ... ST`) and title-stack/window operations are also
left alone.

Duplicate replies are avoided by construction: the scanner consumes each query
occurrence, so one probe produces exactly one reply even when it spans chunks.
Later *repeated* probes are new occurrences and are answered again. Suppressing
them as "already answered" is what reproduced `E1568` on Linux, because Neovim
re-issues `OSC 11;?` + `CSI 5n` and waits for that second DSR response.

Windows/ConPTY details worth keeping:

- ConPTY emits `ESC[6n` before forwarding child output and waits for a cursor
  position report. The reader thread answers `ESC[1;1R`; without that the pane
  stays blank and the child looks frozen.
- `ClosePseudoConsole` can deadlock while the reader thread is parked in
  `ReadFile`. `Pty::drop` deliberately forgets the master handle; the process
  exits immediately afterwards and the OS reclaims it. Do not copy this into a
  long-lived multi-PTY process without a real shutdown path.

## vt100 / Neovim caveats

- **Cursor**: tui-term draws its own block cursor over the cell and the real
  terminal cursor is hidden. Neovim's beam/underline shape and blinking are not
  reflected; insert-mode cursor still looks like a block.
- **Truecolor**: `TERM=xterm-256color`, `COLORTERM=truecolor`, so Neovim emits
  truecolor SGR. vt100 0.15 carries RGB cell colours; ratatui renders them.
  The 256-colour path is fine. Colourless terminals are not detected.
- **Undercurl/underdot/underdashed**: ratatui has one underline modifier, so
  Neovim's styled underlines collapse to a plain underline (or are lost).
  Strikethrough and reverse work.
- **Wide/combining glyphs**: vt100 tracks double-width cells; box drawing and
  Hangul generally render, but glyphs whose width differs between the PTY and
  the host font can leave a stray cell.
- **IME**: the *UTF-8 byte path* is verified on Windows ConPTY and Linux:
  `--smoke-utf8` sends the Hangul string `U+D55C U+AE00` through `encode_key`, Neovim saves it, and the
  file contains the bytes. This does **not** test OS IME composition: the
  characters enter already composed, so the pinyin/Hangul candidate window,
  composition preview, and key-event ordering on a real terminal are still
  unverified. A human IME test in the target terminal (Windows Terminal and the
  WSL terminal) is still required.
- **Clipboard**: no bracketed-paste mode is enabled. `Ctrl+V` is forwarded as
  `0x16` (Neovim paste-from-register). Console-host paste (e.g. right-click)
  arrives as a stream of chars and works but is slow and has no single paste
  event. `"+y`/`"+p` are Neovim's responsibility.
- **Mouse**: not enabled; Neovim mouse mappings never fire.
- **Modified keys**: Shift/Alt+arrow send the plain xterm sequence or an `ESC`
  prefix, not the CSI `1;3A` forms; Shift+arrow selection in Neovim will not
  extend. Fine for the spike, not for daily use.
- **Scrollback**: `vt100::Parser` is created with zero scrollback; scrolling is
  Neovim's job and the sidebar is not scrollable except by selection.
- Save detection trusts mtime only (the scan itself uses `verify_cache:
  false`). A same-mtime rewrite can be missed, same as the scanner's warm path.

## RPC work now done vs remaining

Done in Workbench v1:

- Live buffer over `--listen` + msgpack-RPC; no disk reads while live.
- Per-function CCN extmarks in `ccn_workbench` with severity highlight and
  `(+delta)` for new/worsened functions.
- Sidebar deltas vs the initial file scan, worst-first, with identity preserved
  across refreshes.
- `Enter`/`o` jump to a function, `normal! zz`, focus returned to the editor.
- `unsaved` from changedtick; connect-time tick captured, no fake save events.
- `--no-rpc`/non-Neovim/connection failure falls back to mtime scanning.
- Terminal probes (OSC/DSR/DA/DECRQM/kitty) answered in the PTY bridge.

Remaining limitations / next steps:

- The baseline is the *initial* scan for the whole session. Saving does not
  reset it, so deltas keep accumulating relative to startup; a deliberate
  baseline reset (`BufWritePost` or a key) is still needed.
- Polling changedtick every 150 ms is simple but not event-driven. Replace with
  `nvim_buf_attach` notifications when the workbench moves to a real RPC UI
  loop; keep 150 ms as a safety net.
- One Neovim process with a ccn tab bar over its buffers. Neovim window splits,
  multiple tabpages, cross-file health summaries and project-wide symbol search
  are not managed by ccn.
- Mouse input is captured by ccn only inside the split, for the divider drag;
  clicks inside the editor are not forwarded to Neovim. Use Shift+drag for the
  terminal's own text selection/copy. On the dashboard mouse reporting is off.
- Paste is bracketed: pasted text goes to the live filter/fuzzy query, or to
  Neovim as `ESC[200~...ESC[201~`. A terminal that does not implement bracketed
  paste can still send text, but Ctrl+V may arrive as `0x16` in the editor.
- Reconnect resets `base_tick` to the new connect tick; edits made while the
  connection was down can temporarily appear "saved". Edits made before the
  first successful connect are likewise the baseline, because `unsaved` is
  changedtick-since-connect, not changedtick-since-last-disk-write.
- If the initial file scan produced no function detail (heuristic or unreadable
  file), the delta baseline is empty and every live function appears `+new`
  with its full CCN as the delta.
- Extmarks use fixed priority 200 and no sign column; they are deliberately
  limited to our namespace and cleared every snapshot.
- Neovim 0.8 was not run in this environment; only 0.8-compatible API calls are
  used. The AlmaLinux CI is the authoritative 0.8 check.
- Linux interactive keyboard handling and SIGWINCH resize were not manually
  exercised; Windows drove the full key loop, WSL covered RPC/PTY smokes under
  `script`.
- `nvim_ui_attach`/mode-aware cursor rendering, bracketed paste, and mouse are
  still out of scope while the editor stays byte-forwarded PTY.

## Compatibility matrix (2026-10-22)

| Host / toolchain | Editor | PTY render | UTF-8 byte path | RPC + extmarks |
|---|---|---|---|---|
| Windows 11 (ConPTY) | portable Neovim 0.12.6 | pass | pass | pass (workbench) |
| Ubuntu 26.04 (WSL, glibc 2.43) | portable Neovim 0.12.6 | pass | pass | pass (workbench) |
| AlmaLinux 8 (glibc 2.28) | EPEL `neovim` 0.8.0 | pass | pass | 0.8-compatible calls; CI only |
| AlmaLinux 8 | official Neovim 0.12 tarball | **not runnable** | - | - |

The official Neovim Linux tarball requires **GLIBC_2.34**; AlmaLinux 8 ships
2.28. Use the EPEL 8 `neovim` package (0.8.0) or a locally built Neovim on EL8.
The workbench's PTY path works with 0.8, and the workbench RPC worker uses only
APIs present in 0.8 (`nvim_buf_get_lines`, `nvim_buf_get_changedtick`,
`nvim_exec_lua`, `nvim_buf_set_extmark`/`get_extmarks`, `nvim_win_set_cursor`);
the AlmaLinux CI is the authoritative 0.8 runtime check. `ccn.nvim`'s
`jobstart` fallback produced a `source = "ccn"` diagnostic on 0.8.

Verification is in `.github/workflows/workbench-check.yml`:
AlmaLinux 8 build (glibc 2.28 symbol gate), PTY smoke, EPEL-nvim render,
UTF-8 smoke, RPC probe, and the plugin. Artifacts are retained for 14 days.
Linux interactive resize/keyboard was exercised only on Windows and via
`script` smokes; a full interactive Linux session and a human IME test remain
open.

## Packaging status

The workbench is merged into the single `ccn` binary and is enabled by the
default `workbench` feature:

```text
cargo build --release --locked                       # ccn with workbench
cargo run --release -- workbench file.c               # from source
ccn workbench file.c                                 # installed binary
cargo build --release --locked --no-default-features # lean scanner only
```

The lean build prints a clear message if `ccn workbench` is invoked. Release
artifacts are the four usual `ccn-*` binaries; there are no separate workbench
assets. Neovim remains an external runtime dependency (`nvim` on `PATH` or
`--editor <path>`): EPEL 0.8 on AlmaLinux 8, a normal install on Windows/macOS.
`--no-rpc` keeps PTY editing and the mtime-based sidebar when Neovim cannot be
reached.

The AlmaLinux workflow builds the merged `ccn`, checks glibc 2.28, and runs
PTY, UTF-8, RPC and plugin smokes against EPEL Neovim 0.8.
