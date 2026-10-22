# ccn workbench spike

First spike for a Neovim-centric workbench: a real Neovim TUI runs inside a
ratatui pane next to ccn's per-file complexity sidebar.

Status: **builds and runs on Windows (verified); Linux runtime unverified.**
No RPC, no extmarks, no file picker. `src/bin/ccn-workbench/` is the only code.

## What works

- Spawns the editor in a PTY (`portable-pty`; ConPTY on Windows) sized to the
  editor pane, not the whole terminal.
- Reads PTY output in 8 KiB chunks into a shared `vt100::Parser`; renders it
  with `tui-term`.
- Forwards keystrokes byte-for-byte: UTF-8 chars, Enter, Esc, Tab/BackTab,
  Backspace, arrows, Home/End, Insert/Delete, PageUp/PageDown, F1-F8/F11-F12,
  `Ctrl+<letter>` as `0x01..0x1a`, and Alt as an `ESC` prefix.
- Header shows path, scan status/latency, LOC, function count, CCN max/p90/sum,
  tier, a max-CCN heuristic grade, and focus.
- Right sidebar lists the scanned file's functions worst-first with line, CCN,
  name, and identity; the selected entry is highlighted.
- Save detection: mtime is checked every 500 ms; a change re-runs
  `ccn::loc::scan` on that file and refreshes header + sidebar.
- Focus model: editor by default. `F9` toggles sidebar; `j/k`, `g/G` move,
  `r` rescans, `F9`/`Esc` returns. While the editor is focused, every key
  except `F9`/`F10` goes to Neovim.
- Window resize calls `MasterPty::resize` and `vt100::Parser::set_size`.
- Child exit leaves the workbench open, marks `[editor exited]`, and `F10`
  quits.
- `--smoke` and `--smoke-nvim` run PTY + vt100 without an interactive terminal.

## Run

```text
cargo run --release --bin ccn-workbench -- path/to/file.c
cargo run --release --bin ccn-workbench -- --editor "<path-to-nvim>" file.cpp
cargo run --release --bin ccn-workbench -- --smoke
cargo run --release --bin ccn-workbench -- --smoke-nvim
cargo run --release --bin ccn-workbench -- --editor "<path>" --smoke-nvim
```

`--editor` defaults to `nvim`. A command with arguments must be quoted at the
shell level; the spike understands double quotes inside it but performs no
shell expansion. A directory argument is rejected with an explicit message
(this spike opens one file). `--smoke-nvim` exits 0 with a `skip:` line when
the editor is not found.

Gates:

```text
cargo build --release --bin ccn-workbench
cargo clippy --release --locked --bin ccn-workbench -- -D warnings
cargo fmt --check
cargo test --release --bin ccn-workbench
cargo run --release --bin ccn-workbench -- --smoke
```

## Layout (condensed ASCII capture, 130x40 host)

```text
 ccn workbench  manual.c                              scan ok - 2 ms
 C - 11 LOC (total 13) - 3 functions - CCN max 3 - p90 0 - sum 6 - tier exact
 grade A - low complexity - focus: editor - scan 2.0 ms
 + editor - nvim ------------------------------+ functions - F9 back -------+
 | int low_one(int x) { return x + 1; }        | manual.c - 11 LOC          |
 |                                             | 3 functions - max 3 - p90 0|
 | int big_one(int x) {                        | tier exact - grade A       |
 |     if (x > 0) {                            | sorted by CCN              |
 |         for (int i = 0; i < x; i++) {       | >  8   3 big_one           |
 |             x += i;                         |       big_one(int)         |
 |         }                                   |    3   2 fresh_save        |
 |     }                                       |       fresh_save(int)      |
 |     return x;                               |    1   1 low_one           |
 | }                                           |       low_one(int)         |
 |                                             |                            |
 | ~                                           |                            |
 |~\AppData\Local\Temp\...\manual.c   1,1 All  |                            |
 +---------------------------------------------+----------------------------+
 F10 quit - F9 focus sidebar - typing goes to the editor - r rescans in the sidebar
 scan: 3 functions - 11 LOC - 2 ms
```

Real render uses box-drawing borders and the Aura palette; `>` marks the
selected function. The identity line under each name is `FunctionCcn::identity`.

## Windows result

Verified with Neovim 0.12.6 (portable) under a ConPTY-driven harness at
130x40, and with the release binary:

- Neovim paints its splash/statusline inside the pane and opens the passed
  file (`manual.c` in the statusline).
- Typing `i ... <Esc> :w` into the pane saved the file; the sidebar refreshed
  to the new function list and footer showed
  `saved -> rescanned: 3 functions - 7 LOC - 5 ms`.
- Editing the file from another process while Neovim was open also triggered
  the 500 ms mtime rescan and showed the new function.
- `F9` changed focus to the sidebar, `j` moved the selection, `F9` returned.
- Resizing to 100x30 and to 12x10 redrew correctly and did not panic; `F10`
  exited.
- `:q` left the workbench open with `[editor exited: exit 0]`; `F10` exited 0.
- `--smoke` output: `smoke ok` (exit 0).
- `--smoke-nvim` (portable Neovim path) printed the screen tail ending in
  `ccn_workbench_smoke.c   1,1   All` and then `smoke-nvim ok` (exit 0).

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
- **IME**: not verified. Windows commits composition as `Char` events and the
  spike forwards UTF-8, so Hangul input should arrive, but the OS composition
  preview window is outside the pane and may overlap it. Needs a manual test in
  Windows Terminal.
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

## Remaining for the RPC spike

- Replace the byte pipe with `nvim --embed` + `nvim-rs` (RPC probe is a
  separate agent's `src/bin/ccn-nvim-probe`).
- Attach to the buffer and use `nvim_buf_set_extmark` for per-function CCN
  severity, virtual text, and a sign column, instead of a separate sidebar.
- `Enter` on a sidebar function -> move the Neovim cursor to `FunctionCcn.line`
  and reveal it (`nvim_win_set_cursor` + `zz`), without `:` command typing.
- Use `nvim_buf_attach` / `BufWritePost` instead of 500 ms mtime polling, and
  scan the unsaved buffer (or write-then-scan) rather than the file on disk.
- Draw real health/CCN summaries across files, open a file picker for directory
  arguments, and persist panel focus/width.
- Re-evaluate cursor rendering with RPC (`nvim_ui_attach` gives mode info), and
  add bracketed paste / mouse if the editor stays PTY-based.
- Linux: build and run the PTY, resize (SIGWINCH), and clean shutdown paths;
  only Windows ConPTY was exercised here.
