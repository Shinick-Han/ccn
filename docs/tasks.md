# Build and test tasks

`ccn` runs project commands without leaving the TUI. The editor stays
interactive: the command runs on a worker thread, output is captured, and
`Esc` closes the output view without killing the process.

## Configuration

Declare tasks in `<root>/.ccn.toml`:

```toml
task.build = "cargo build"
task.test = "cargo test"
```

Task names are ASCII letters, digits, `_` and `-`; commands must be non-empty.
Configured tasks suppress auto-detection. Duplicate names overwrite: the last
declaration wins.

## Auto-detection

When no `task.*` entries exist:

| Project file | Build | Test |
|---|---|---|
| `Cargo.toml` | `cargo build` | `cargo test` |
| `CMakeLists.txt` | `cmake -S . -B build && cmake --build build` | `ctest --test-dir build --output-on-failure` |
| `Makefile` | `make` | `make test` |
| `meson.build` | `meson compile -C build` | `meson test -C build` |
| `package.json` | `npm run build` if the script exists | `npm run test` if the script exists |

## TUI

- `F5` → **Build** / **Test** / **Run last task** / **Run task...**.
  `Run task...` lists every configured or detected task, including custom names.
- While the output overlay is open: `j/k`, `PgUp/PgDn`, `g/G` scroll;
  `n/N` select diagnostics; `Enter` opens the selected `file:line`;
  `r` reruns; `Esc`/`q` closes the view.
- Output is capped at 4,000 lines / 200 KB. Compiler errors from GCC/Clang,
  MSVC and Rust/Cargo are parsed best-effort.

The task shell is `cmd /C` on Windows and `sh -c` elsewhere; commands therefore
use the user's normal shell syntax and environment.
