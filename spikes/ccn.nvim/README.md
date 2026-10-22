# ccn.nvim

Show per-function cyclomatic complexity from the [`ccn`](../../README.md) CLI
inside Neovim: a virtual-text badge at the function's first line plus a
diagnostic entry, refreshed when you write the buffer.

This is a spike, not a shipped plugin. It is deliberately tiny (two Lua files,
no dependencies) so that "ccn in the editor" can be evaluated without buying
into an LSP or a daemon.

```
  1  int messy(int x) {                    <-- diagnostic: WARN
  2      int total = 0;
  3      if (x > 0) {
  4          if (x > 10) { total += 1; } else { total += 2; }
  5      } else if (x < -5) {
  6          for (int i = 0; i < 3; ++i) {
  7              if (i % 2 == 0) { total++; } else if (i == 1) { total--; }
  8          }
  9      } else {
 10          while (x != 0) { x++; if (x > 100) break; }
 11      }
 12      switch (x) {
 13      case 1: total += 1; break;
 14      case 2: total += 2; break;
 15      case 3: total += 3; break;
 16      default: break;
 17      }
 18      return total;
 19  }                                        CCN 13
```

Virtual text is appended to the end of the function's first line:

```
int messy(int x) {                                    CCN 13
```

Above twice the threshold the badge turns into the error colour:

```
int branchy(int x) {                                  CCN 27
int enormous(int x) {                                 CCN 57 !!
```

## Requirements

- Neovim >= 0.10 (`vim.system`). A `jobstart` fallback exists for 0.9 but is
  not covered by the headless checks below.
- `ccn` reachable as `ccn` on `PATH`, or an absolute path in
  `vim.g.ccn_bin`.
- A language `ccn` has a grammar for: JS/TS/TSX, Python, Rust, Go, Java, C,
  C++, C#. The extension list mirrors `Lang::from_path` in
  `src/ccn/types.rs`, so `.h` is treated as C and `.hpp` as C++.

Nothing is installed or downloaded by the plugin at runtime.

## Install

With a plugin manager, point it at this directory (or copy the two Lua files
into your config).

Using the built-in `packpath` (no manager):

```vim
" In init.lua / init.vim
vim.opt.runtimepath:append("/absolute/path/to/spikes/ccn.nvim")
```

The `plugin/ccn.lua` file calls `setup()` on startup. `setup()` is idempotent,
so calling it again from your config is safe and is the place to pass options.

lazy.nvim:

```lua
{
  dir = "/absolute/path/to/spikes/ccn.nvim",
  name = "ccn.nvim",
  event = { "BufWritePost" },   -- or "VeryLazy"
  opts = {},
}
```

packer.nvim:

```lua
use {
  "/absolute/path/to/spikes/ccn.nvim",
  config = function()
    require("ccn").setup()
  end,
}
```

You can also skip startup plugin loading entirely and wire it by hand:

```lua
vim.opt.runtimepath:append("/absolute/path/to/spikes/ccn.nvim")
require("ccn").setup()
```

## Commands

| Command        | Effect                                                       |
| -------------- | ------------------------------------------------------------ |
| `:CCN`         | Toggle on/off. Turning on refreshes the current buffer.       |
| `:CCN on`      | Enable and refresh.                                           |
| `:CCN off`     | Disable and clear all ccn marks in every tracked buffer.      |
| `:CCN status`  | Print enabled state, binary, threshold, timeout, last error.  |
| `:CCNRefresh`  | Re-run `ccn` for the current buffer (no-op while disabled).   |

`BufWritePost` triggers the same refresh automatically. Nothing runs on
`TextChanged`, `CursorMoved`, or `InsertLeave`.

## Configuration

Read at refresh time, so `:let g:ccn_threshold = 15` takes effect on the next
refresh without restarting.

| Variable            | Default | Meaning                                             |
| ------------------- | ------- | --------------------------------------------------- |
| `vim.g.ccn_bin`     | `"ccn"` | Executable name (PATH lookup) or absolute path.     |
| `vim.g.ccn_threshold` | `10`  | Report functions with `ccn >` this value.            |
| `vim.g.ccn_timeout` | `30000` | Kill the CLI after this many milliseconds.          |
| `vim.g.ccn_languages` | nil   | List of extensions to override the built-in set.    |
| `vim.g.ccn_enabled` | `1`     | Initial state; toggled by `:CCN`.                    |

Severity bands are relative to the threshold: over threshold = warning,
over `2 * threshold` = error, and over `5 * threshold` adds `!!` to the badge.
With the default threshold that is the familiar `>10` / `>20` / `>50` split
used by the CLI's own `over_10` / `over_20` / `over_50` counters.

```lua
vim.g.ccn_bin = "C:/Tools/ccn/ccn.exe"
vim.g.ccn_threshold = 12
require("ccn").setup()
```

Highlight groups: `CcnWarn` links to `DiagnosticWarn`, `CcnError` links to
`DiagnosticError`. Override them in your colorscheme to change the badges.

## What it runs

One process per refresh, with the same flags the CLI documents for machine
consumption:

```
ccn <absolute-path-to-file> --json --no-cache --no-report
```

`cwd` is the file's directory. `ccn` derives its scan root from the path it is
given, so the JSON then contains one `files[]` entry whose `path` is the
basename. The plugin reads `files[0].ccn.detail[]` (`name`, `identity`, `line`,
`ccn`, `nloc`, `max_nesting`) and ignores everything else. No cache is written,
no HTML report is produced, and the buffer is never modified or saved.

## Verified behaviour (headless, NVIM v0.12.6, ccn 0.7.8)

Fixture: one `.cpp` with `simple()` (CCN 1) and `messy()` (CCN 13).

Note: `-u NONE` turns plugin loading off, so `plugin/ccn.lua` is not read. Add
either `-c "lua require('ccn').setup()"` (used below) or
`--cmd "set loadplugins"`; without one of them `:CCNRefresh` is undefined
(`E492`). A normal Neovim config loads `plugin/` automatically.

```text
$ nvim --headless -u NONE --cmd "set rtp+=spikes/ccn.nvim" \
    -c "edit sample.cpp" -c "lua require('ccn').setup()" -c "CCNRefresh" \
    -c "sleep 1500m" -c "lua print(vim.inspect(vim.diagnostic.get(0)))" -c "qa!"

{ { _extmark_id = 1, bufnr = 1, code = "ccn", col = 0, end_col = 0,
    end_lnum = 6, lnum = 6,
    message = "messy(int): CCN 13 (threshold 10), 19 lines, max nesting 5",
    namespace = 4, severity = 2, source = "ccn" } }
```

Exactly one diagnostic, `source = "ccn"`, `lnum = 6` (0-based) for the function
declared on line 7.

```text
ns=3
{ { 1, 6, 0, { hl_mode = "combine", ns_id = 3, priority = 4096,
      right_gravity = true,
      virt_text = { { " CCN 13 ", "CcnWarn" } },
      virt_text_pos = "eol" } } }
```

Exactly one extmark in namespace `ccn.marks`, at 0-based line 6, with the
expected badge text.

Toggle and race checks (single session):

```text
on: diags=1 marks=1 enabled=true
off: diags=0 marks=0 enabled=false
on-again: diags=1 marks=1 enabled=true
stale-guard: diags=1 (before 1)     <- older token ignored
after-BufWritePost: diags=1 marks=1 enabled=true
modified=false
```

Severity and threshold checks:

```text
mid.cpp:   marks=1 diags=1 label=" CCN 27 "    hl=CcnError sev=1
huge.cpp:  marks=1 diags=1 label=" CCN 57 !!"  hl=CcnError sev=1
threshold-30 (on a CCN 27 function): marks=0 diags=0
after-write (function simplified):  marks=0 diags=0
```

Edge cases:

```text
unnamed:        diags=0 last_error=nil
txt file:       diags=0 last_error=nil
nonzero exit:   diags=0 last_error="ccn exited 2: ..."
missing binary: diags=0 last_error="ccn executable not found (...)"
buffer_same=true                      <- content untouched
```

File integrity: the fixture's SHA-256 and `LastWriteTimeUtc` were identical
before and after a refresh, and `vim.bo.modified` stayed `false`.

## Limitations

- **Saved state only.** The CLI reads from disk, so badges describe the last
  write. Editing a function without saving leaves the old badge in place until
  the next `BufWritePost` / `:CCNRefresh`. Marks are not cleared on keystrokes
  by design (that would flicker on every character).
- **Single-file run.** One `ccn` process per write, scanning exactly that file.
  No repository-wide cache reuse, no cross-file ranking, no "worst in repo"
  view. This is fine for a 200-line file and wasteful if you save 50 files in a
  loop; `--no-cache` is chosen so the plugin never writes `.ccn/` state into
  the repo.
- **No baseline delta.** It shows absolute CCN only. "This function grew from
  8 to 13 since HEAD" needs `ccn --baseline`, which the plugin does not call
  yet.
- **No LSP.** Diagnostics are refreshed on write, not recomputed per edit, and
  there is no code action, hover, or workspace symbol support. For a
  language-server-shaped experience you would wrap the CLI in an LSP server
  instead of this plugin.
- **Heuristic-tier files produce nothing.** They carry one file-level number
  and no lines to point at; the plugin intentionally stays silent rather than
  inventing a position.
- **Ignore rules follow `ccn`.** If the CLI reports no entry for the file, the
  plugin clears its marks silently; it does not attempt its own filtering.
- **Neovim 0.9 fallback is unexercised.** Only the `vim.system` path was run in
  the checks above.
- **No performance claims.** The plugin adds one process spawn per write; it
  does not make `ccn` faster and was not benchmarked against it.

## How this relates to the workbench

The workbench idea is a dedicated surface (TUI/dashboard) for exploring
complexity. This plugin is the low-risk distribution channel: it puts the same
number in the place people already look, with no new UI to learn and no daemon
to keep alive. It can ship before the workbench does, it validates the JSON
contract from a second consumer, and it gives the workbench a migration story
("install the plugin now; the dashboard is additive"). If both exist, the
plugin stays the ambient signal and the workbench owns history, baselines and
triage.

## Files

```
spikes/ccn.nvim/
  plugin/ccn.lua     -- commands + autocmds, calls setup()
  lua/ccn/init.lua   -- config, async runner, JSON parsing, extmarks, diagnostics
  README.md
```
