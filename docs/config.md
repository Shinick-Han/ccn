# Repository configuration

`ccn` reads an optional `.ccn.toml` from the directory being scanned (the
command's positional path, default `.`):

```toml
# .ccn.toml
[ccn]
editor = "code --wait"
baseline = "origin/main"
keymap = "advanced"
color = "never"
lsp = "auto"
task.build = "cargo build"
task.test = "cargo test"
```

All keys are optional and all values are strings.

| Key | Meaning |
|---|---|
| `editor` | Editor command used by the dashboard's `e` key. |
| `baseline` | Revision used when `--baseline` is not given (Git rev or Perforce changelist). |
| `keymap` | `simple` or `advanced`; `advanced` shows the advanced keys in the dashboard help. |
| `color` | `auto` (default) or `never`; `never` disables colour in the dashboard. |
| `lsp` | `auto` (default) or `off`; `off` disables the embedded LSP. |
| `task.<name>` | A named command, e.g. `task.build = "cargo build"`. |

### LSP

`lsp = "auto"` leaves the embedded LSP enabled (the default when the key is
absent). `lsp = "off"` disables it for this repository. Any other value rejects
the whole file.

```toml
lsp = "off"
```

### Tasks

`task.<name> = "command"` declares a named command. The name must be non-empty
and contain only ASCII letters, digits, `_`, and `-`; the command must be a
non-empty string. Tasks are stored sorted by name, so declaration order does not
matter, and a repeated name keeps its last declaration.

```toml
task.build = "cargo build"
task.test = "cargo test"
task.fmt_all = "cargo fmt --all"
```

An invalid task name (empty, containing `.`, spaces, slashes, or non-ASCII
characters) or an empty command rejects the whole file. `task` on its own, with
no `.` and name, is an unknown key and is ignored.

## Precedence

Command line > `.ccn.toml` > built-in defaults.

- `--baseline <rev>` overrides `baseline`.
- `--editor <cmd>` overrides `editor`, with one caveat: `nvim` is the built-in
  default, so an explicit `--editor nvim` cannot be distinguished from the flag
  being absent. A configured `editor` therefore wins over an explicit
  `--editor nvim`. Use `--editor ./nvim` or a different value if you must
  override the file with Neovim.
- `CCN_KEYMAP` already set in the environment overrides `keymap`. Otherwise
  `keymap = "advanced"` sets `CCN_KEYMAP=advanced` for this process.
- `--no-color` disables colour. `color = "never"` does the same; `color = "auto"`
  is the default and leaves the decision to `NO_COLOR`. The flag wins over
  `auto` because it always disables colour; `never` and an existing non-empty
  `NO_COLOR` are equally authoritative (all three mean "no colour").
- `NO_COLOR` follows the de-facto standard: colour is disabled when the
  variable is present and non-empty. `NO_COLOR=` (empty) keeps colour on.
- Flags that depend on a baseline keep clap's validation: `--fail-on-new` and
  `--manifest` still require `--baseline` on the command line, even when
  `.ccn.toml` supplies one.
- `lsp` and `task.*` have no command-line equivalents: the file is their only
  source.

If a configured baseline is used without a Git or Perforce workspace, the
normal `--baseline needs a git or Perforce workspace` error applies (exit 2).

## Parser limits

The file is parsed by hand to keep `ccn` dependency-free, so it supports a small
subset of TOML:

- `key = "value"` lines, with `#` comments (whole-line or trailing) and blank
  lines.
- Table headers such as `[ccn]` are accepted and ignored; keys are flat.
- Unknown keys are ignored, so a newer file still works with an older binary.
- Values are double-quoted and taken literally: there are no escape sequences,
  and a value cannot contain another `"`. An empty value is rejected.
- `keymap` must be exactly `simple` or `advanced`.
- `color` must be exactly `auto` or `never`; any other value, including an
  empty string, rejects the whole file.
- `lsp` must be exactly `auto` or `off`; any other value rejects the whole file.
- `task.<name>` requires a non-empty name made of ASCII letters, digits, `_`,
  and `-`, and a non-empty command. Invalid names or empty commands reject the
  whole file. Duplicate names are allowed; the last one wins.
- Any other malformed line rejects the whole file: the defaults apply, silently.
  A file is never applied partially. Because a malformed file falls back to
  `Config::default`, a bad `lsp` or `task.*` line also discards otherwise valid
  `editor`/`baseline`/`keymap`/`color` settings.
- Only `<root>/.ccn.toml` is read. There is no parent-directory or home-directory
  search, no includes, and no environment-variable expansion inside values.
