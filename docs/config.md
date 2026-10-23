# Repository configuration

`ccn` reads an optional `.ccn.toml` from the directory being scanned (the
command's positional path, default `.`):

```toml
# .ccn.toml
[ccn]
editor = "code --wait"
baseline = "origin/main"
keymap = "advanced"
```

All keys are optional and all values are strings.

| Key | Meaning |
|---|---|
| `editor` | Editor command used by the dashboard's `e` key. |
| `baseline` | Revision used when `--baseline` is not given (Git rev or Perforce changelist). |
| `keymap` | `simple` or `advanced`; `advanced` shows the advanced keys in the dashboard help. |

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
- Flags that depend on a baseline keep clap's validation: `--fail-on-new` and
  `--manifest` still require `--baseline` on the command line, even when
  `.ccn.toml` supplies one.

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
- Any other malformed line rejects the whole file: the defaults apply, silently.
  A file is never applied partially.
- Only `<root>/.ccn.toml` is read. There is no parent-directory or home-directory
  search, no includes, and no environment-variable expansion inside values.
