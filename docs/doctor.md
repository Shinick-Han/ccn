# Operational commands: `doctor`, `update`, `setup`

These three commands are dispatched before CLI parsing. Their names are
reserved only when they are the **first** argument:

```console
$ ccn doctor          # runs the self-check
$ ccn ./doctor        # scans the directory named "doctor"
```

The same rule applies to `update` and `setup`. A path such as `./update` is
still a scan root.

## `ccn doctor [PATH] [--json]`

Read-only diagnosis of the local setup. `PATH` defaults to `.`. The output is
plain ASCII with no colour, so it stays readable in CI logs. Every check
prints `OK`, `WARN`, or `FAIL`, followed by a summary line.

| Check | What it reports | Missing means |
|---|---|---|
| `ccn` | ccn version and the current executable path | - |
| `nvim` | `nvim` on `PATH`, plus the first line of `nvim --version` | `FAIL` |
| `editor` | `editor` from `<PATH>/.ccn.toml` (or the `nvim` default) and whether it is on `PATH` | `WARN` for a custom editor |
| `vcs` | git or Perforce workspace detected by `ccn::vcs::detect` | `WARN` |
| `compile_commands` | `compile_commands.json` in `PATH` | `WARN` (optional) |
| `compile_flags` | `compile_flags.txt` in `PATH` | `WARN` (optional) |
| `build-files` | `Cargo.toml`, `CMakeLists.txt`, `Makefile`, `meson.build`, `BUILD` | `WARN` when none exist |
| `cache` | sidecar path for the session cache; the parent is probed with a temporary file | `WARN` when not writable |
| `terminal` | whether stdout is a TTY | `WARN` without a TTY |

`nvim --version` is bounded to a few seconds; a hung wrapper is reported as
missing rather than blocking the command. Perforce detection is local only
(`p4 info` is the same call the scanner already makes); doctor runs no network
P4 commands.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | No `FAIL` checks (warnings are allowed) |
| `2` | At least one `FAIL` (currently a missing `nvim`) |
| `1` | Usage error, invalid `PATH`, or the report could not be written |

### JSON

`--json` prints one document with the same information:

```json
{
  "version": "0.27.0",
  "path": ".",
  "checks": [
    { "name": "nvim", "status": "ok", "detail": "found: NVIM v0.10.4" }
  ]
}
```

`status` is `ok`, `warn`, or `fail`. `path` is the path as given, not a
canonicalised one.

## `ccn update`

Compares the running version with the version published as `@shinick/ccn` on
npm. The lookup is `npm view @shinick/ccn version --silent`, bounded by a
short timeout; ccn itself adds no HTTP dependency.

```console
$ ccn update            # current/latest + update instructions
$ ccn update --check    # check only, no advice or changes
$ ccn update --apply    # update a global npm install
```

If the versions match, it prints `ccn is up to date` and exits `0`.

If an update is available:

- When `<npm root -g>/@shinick/ccn/package.json` exists, `--apply` runs
  `npm install -g @shinick/ccn@latest` with inherited stdio.
- Otherwise the binary is a native install. `ccn update` prints the GitHub
  release URL, the asset name for the current platform, a `curl` download
  command, and the replacement command. `--apply` refuses with exit `1`;
  ccn never overwrites a native binary on its own.

A missing npm, a registry/network failure, or an unparseable published
version exits `1` with an actionable message. An update being available is
not an error: `ccn update` and `ccn update --check` exit `0`.

## `ccn setup nvim [--print | --write [--force]]`

Prints a copy-paste Lua snippet that starts `ccn lsp` for the filetypes ccn
parses (C, C++, C#, Go, Java, JavaScript/JSX, Python, Rust,
TypeScript/TSX). The snippet uses the absolute path of the running
executable and sets `root_dir` to the buffer's working directory
(`vim.fn.getcwd()`). It targets Neovim 0.10+ and documents the 0.11
`vim.lsp.config` / `vim.lsp.enable` alternative in a comment.

- No flag or `--print` writes the snippet to stdout.
- `--write` installs it at:
  - `$XDG_CONFIG_HOME/nvim/after/plugin/ccn.lua` when set;
  - `%LOCALAPPDATA%\nvim\after\plugin\ccn.lua` on Windows;
  - `~/.config/nvim/after/plugin/ccn.lua` otherwise.
- Parent directories are created. An existing file is left untouched unless
  `--force` is given; `--print --write` does both.

The command only reads the environment needed for the target path. It does
not scan the repository or read any secret.
