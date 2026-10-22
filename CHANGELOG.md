# Changelog

## 0.9.0 (2026-10-22)

Split-screen dashboard + editor.

- `e` now opens a **split view**: the dashboard file table on the left and the
  workbench (Neovim + live function sidebar) on the right, in the same
  terminal and event loop. No handoff, no alternate-screen churn.
- `F8` switches focus between the file table and the workbench; `F9` switches
  editor/sidebar inside the workbench; `F10` hides the split and returns to the
  full dashboard; `q` in the file table also hides the split (so unsaved
  buffers are not discarded by accident). `q` on the full dashboard exits.
- The left table keeps the selected path, sort, filter and chart tab; the
  workbench session and nvim/RPC live across split/full-dashboard switches.
- The PTY is sized to the right pane; resizing recomputes the split and
  resizes the editor.
- Opening a different file from the split's left table is not supported yet:
  `F10`, select the file, then `e` (the session is replaced). The full
  dashboard remains the multi-file navigator.
- WSL+tmux capture shows the left table (`Files (2)`, sort/risk) and the right
  workbench (`engine.cpp`, RPC live, function sidebar) simultaneously.
- 439 lib + 17 CLI tests, clippy/fmt clean (default and lean builds).

## 0.8.2 (2026-10-22)

Keep the workbench session and dashboard position across tab switches.

- `F10` now **returns to the dashboard without closing the workbench**. The
  Neovim PTY and RPC connection stay alive while the dashboard is visible and
  are reused when `e` opens the same file again; a different file (or an exited
  editor) replaces the session. `q` on the dashboard closes it.
- The dashboard preserves the selected path, sort key/direction, filter, and
  chart tab across the workbench round trip and the rescan. If the selected
  path disappeared, the nearest index is retained.
- The dashboard still ticks the hidden workbench (RPC/PTY/mtime) so the live
  state does not stall while it is in the background.
- WSL+tmux check: dashboard -> `e` -> nvim (PID 93625) -> `F10` -> `e` reuses
  the same PID -> `F10` -> `q` closes it; no nvim process remains.
- 439 lib + 17 CLI tests, clippy/fmt clean (default and lean builds).

## 0.8.1 (2026-10-22)

Unified dashboard + workbench tab.

- `ccn . --tui` now switches between the dashboard and the workbench **without
  leaving the alternate screen**. `e` opens the selected file in the workbench,
  `F10` returns to a freshly rescanned dashboard, and `q` exits once; gates run
  on the final state.
- One terminal guard, one event loop, one binary. The workbench session is
  created/closed on switch, so the editor's PTY and RPC connection do not
  outlive the view.
- `--editor <cmd>` selects the editor used by `e`; the workbench footer says
  `F10 back/quit` because the same view is used standalone and embedded.
- `ccn workbench <file>` still runs standalone.
- `run_unified` is covered by the WSL+tmux interactive check and the dashboard
  `e` action unit test; scanner behavior is unchanged.

## 0.8.0 (2026-10-22)

The workbench is merged into the main `ccn` binary.

### One binary, one command

- `ccn workbench <file>` opens the embedded-Neovim workbench. The
  `workbench` Cargo feature is now **on by default**; release assets are the
  four usual `ccn-*` binaries (no separate `ccn-workbench-*` assets).
- `cargo build --release --locked --no-default-features` still produces a
  lean scanner; `ccn workbench` then prints an explicit "built without"
  message.
- The merged binary grows by about 1 MB (19.3 MB -> 20.3 MB locally) and adds
  the PTY/RPC dependencies to the default build. Neovim stays an external
  runtime dependency (`nvim` or `--editor`).
- The scanner CLI, npm packages and GitHub action are otherwise unchanged.

### Dashboard integration

- `ccn . --tui` now accepts `e` on the selected file: the dashboard restores
  the terminal, runs `ccn workbench` on that file, then rescans and reopens the
  dashboard. Gates (`--fail-over`, `--fail-on-new`) run on the final state.
- `--editor <cmd>` selects the editor used by the dashboard's `e` key.
- The dashboard key help and footer document `e`.

### Workbench RPC (carried from 0.7.9)

- Neovim is spawned with `--listen`; a worker thread reads the live buffer on
  `changedtick`, parses CCN off the UI thread, shows unsaved deltas in the
  sidebar, writes `CCN n (+d)` extmarks, and jumps to a function with `Enter`.
- `--no-rpc` and non-Neovim editors keep the PTY/mtime fallback.
- `ccn workbench --smoke`, `--smoke-nvim`, `--smoke-utf8`, `--smoke-rpc` and
  `--smoke-rpc-edit` remain available for CI.

## 0.7.9 (2026-10-22)

Experimental workbench integration and packaging. The scanner behavior is
unchanged; this release adds a separate, feature-gated editor workbench.

### ccn-workbench (RPC)

- Spawns Neovim with a free `--listen` port and reads the **live buffer**
  through `nvim-rs`, every 150 ms when `changedtick` moves.
- Sidebar compares the live functions to the initial scan by identity and shows
  `+new` / `~worsened` / `-improved`, a positive-delta sum, and `unsaved`.
- Inline extmarks in the `ccn_workbench` namespace show `CCN n (+d)` with
  warn/error severity; `Enter`/`o` jumps the editor to the function line.
- `--no-rpc`, non-Neovim editors, and connection loss fall back to the
  mtime-based PTY-only path. The RPC work runs off the UI thread.
- `--smoke-rpc` and `--smoke-rpc-edit` verify the live path; Windows, WSL
  Ubuntu and AlmaLinux 8 (EPEL Neovim 0.8) pass, with the file on disk
  unchanged after an unsaved edit.

### Packaging and compatibility

- Workbench binaries are behind the `workbench` Cargo feature and are **not**
  compiled by the default scanner build. Release assets gain
  `ccn-workbench-{linux-x64,macos-x64,macos-arm64,windows-x64.exe}`; the
  scanner assets and npm packages are otherwise unchanged.
- `SHA256SUMS` covers all eight binaries.
- Linux compatibility is verified in CI on AlmaLinux 8 / glibc 2.28 with EPEL
  Neovim 0.8 (PTY, UTF-8, RPC extmarks, plugin). The official Neovim Linux
  tarball requires GLIBC 2.34, so EL8 uses the distro/EPEL package.
- The `ccn.nvim` plugin is verified on Neovim 0.12 (`vim.system`) and 0.8
  (`jobstart` fallback).

## 0.7.8 (2026-10-22)

The trust-contract release: the reproduced P0/P1 holes from the second review
(`docs/reviews/codex-roi-review-2026-10-21.md`) are closed or fail closed.

### Comparison completeness (P0-2)

- Every expected file now has one recorded outcome. `Baseline` publishes
  `files_expected`, `files_compared`, `files_skipped`, `files_unmeasured`,
  `files_oversize`, `files_unreadable`, `files_unmapped`, `files_unknown`,
  `files_unavailable`, `files_ambiguous`, and `complete`.
- A changed file that cannot be measured (truncated current, unparseable
  baseline, read failure, ambiguous match, unsupported view) makes
  `--fail-on-new` exit **2** with a per-reason message. It can no longer look
  like "no changes".
- Oversize and genuinely out-of-view files are explicit accepted exclusions,
  not silent omissions.
- New `--manifest <path>` writes a versioned per-file outcome manifest
  (tool version, baseline, counts, outcome, reason, baseline presence).
- New `--allow-best-effort` opts into advisory best-effort findings; without it
  a best-effort finding makes the enforced gate incomplete (exit 2).
- Cache format 5 records why CCN was skipped (`oversize`, `read-error`,
  `no-grammar`).

### Identity and matching (P1-1)

- Identity now carries C++ cv/ref/noexcept qualifiers and template parameters,
  and excludes parameter names: `X::f(int) const` vs `X::f(int)`,
  `f<T>(T)`.
- Bound lambdas use their variable name (`lambda:a(int)`) instead of a shared
  `(anonymous)`; unbound lambdas stay explicitly anonymous and ambiguous.
- Baseline matching reserves all exact identities before any name fallback;
  fallback requires uniqueness on both sides and compatible scope. Const
  overload reorder, lambda reorder, parameter rename, and a new overload
  inserted before an unchanged function no longer fabricate deltas.
- Ambiguous matching is withheld and counted (`files_ambiguous`), never
  guessed.

### Perforce views (P0-1)

- View lines are evaluated in client-spec order: later lines override, and a
  later include re-adds an earlier exclusion.
- Unsupported patterns (`*`, `%%n`) and the `p4 where` fallback mark the
  mapping **incomplete** instead of silently dropping files.
- New `FileAt::Unknown` distinguishes an incomplete mapping from a
  server-confirmed out-of-view file (`Unmapped`); `Unknown` fails the enforced
  gate with exit 2.

### Reports, release, and docs

- Full-mode SARIF now includes `properties.confidence` and
  `properties.functionIdentity`, a best-effort message suffix, and stable
  `partialFingerprints`; delta messages distinguish CCN growth from
  length/nesting growth; sub-root URIs stay in-repo and are escaped.
- Pinned `action.yml` versions never fall back to building the action checkout:
  a missing pinned asset, checksum mismatch, or version mismatch fails. Both
  `latest` and pinned downloads are checksum- and `--version`-verified.
- macOS ships two native assets: `ccn-macos-x64` (Intel, `macos-15-intel`) and
  `ccn-macos-arm64` (`macos-latest`). npm gains
  `@shinick/ccn-darwin-x64`; the action selects by `uname -m`.
- Health is labelled experimental again (the 11-repo calibration predates
  best-effort exclusion); README no longer implies a validated grade or a
  runtime/defect prediction.
- New design-partner pilot kit: `docs/pilot/design-partner-pilot-2026-10.md`,
  `dossier-template.md`, `checklist.md`.

## 0.7.7 (2026-10-21)

Complex Perforce views and the first large-repository scale evidence.

### Perforce client views

- The client **view** is now read from `p4 client -o` instead of assuming one
  `p4 where` triple. Supports:
  - multiple include lines and remapped directories
    (`//depot/prj/lib/... //ws/third_party/lib/...`),
  - single-file mappings (`//depot/a.c //client/a.c`),
  - exclusions (`-//depot/prj/secret/...`) and `+` overlay lines,
  - quoted paths with spaces.
- Mapping picks the **longest matching** local/depot prefix, with overlays
  winning ties and exclusions always applied last.
- History (`--since`, hotspots) queries every include depot prefix, so files
  from a second view line are not silently dropped.
- A file that is **not in the client view** is reported as
  `files_unmapped` and skipped: it is neither `Added` nor a read failure, and it
  does not make `--fail-on-new` exit 2. Real read failures still count in
  `files_unavailable` and still fail the gate.
- The view is resolved once per process and cached by workspace + Perforce
  environment, instead of running `p4 info` and `p4 client -o` per file.

Live `p4d` verification: a client with `main` + `third_party/lib` mappings and
a `secret` exclusion produced `files_compared: 2`, `files_unmapped: 1`,
`files_unavailable: 0`, with `main/main/a.c` compared against its as-of
baseline and `third_party/lib/vendor.c` correctly attributed to
`//depot/ccnlib/...`.

### Scale benchmark

- New `scripts/bench_scale.py`: deterministic 1k/5k/20k-file corpora, isolated
  caches, cold/warm/`--verify-cache`/`--no-cache`/`--no-ccn`/CI/JSON/HTML
  medians, peak working set, cache size, and scale-correctness checks.
- New `docs/benchmarks/2026-10-21-scale.md`. Headline on this host (Ryzen 9
  9950X, Windows/NTFS): 20,000 files / 1.41 M LOC in **0.63 s cold**, **0.27 s
  warm**, 32k files/s cold, 83 MB peak RSS, 13.45 MiB cache (load 43 ms,
  write 24 ms). Scaling is linear (exponent 0.84-0.96).
- No Rust performance change was needed at this size. The report documents the
  remaining risks: the serial directory walk on NFS, cold-cache/100k
  extrapolation, and that NFS/LSF/large-P4 baselines are still unmeasured.

## 0.7.6 (2026-10-21)

The second batch from
[`docs/reviews/codex-commercial-review-2026-10-21.md`](docs/reviews/codex-commercial-review-2026-10-21.md):
function identity, MSRV, `.tokeignore`, and release integrity.

### Function identity and baseline matching

- Every function now carries a stable `identity`:
  `[scope::]name(normalized parameters)`, e.g. `N::X::m(int v)` or
  `Widget::draw(&self, x: i32)`. Scope comes from the enclosing
  namespace/class/struct/impl/trait/mod, so two classes with the same method
  name no longer collide.
- Baseline matching is identity-first. Reordering C++/Java/C# overloads is no
  longer reported as `worsened` or `added`; moving the complex body between
  overloads attributes the finding to the right function.
- When the identity changed (for example a parameter type edit), matching falls
  back to the name **only if it is unambiguous**, so a signature edit does not
  look like a new function. Renames remain `Added`.
- `Change` objects now include `identity`, and SARIF results expose it as
  `properties.functionIdentity`.
- Cache format 4 (function records changed).

### Ignore files

- `.tokeignore` is honored with gitignore syntax, at every directory level,
  including negations. `.p4ignore` keeps working.
- `--no-ignore` now disables **both** custom ignore files as well as
  `.gitignore`/`.ignore` (previously `.p4ignore` leaked through).

### Toolchain and releases

- `rust-version` is now **1.90**, the real requirement of the locked
  dependency set (tree-sitter 0.27 declares 1.90). Previously the manifest
  claimed 1.80 and failed mid-build on older compilers. `docs/COMPATIBILITY.md`
  now separates the source-build MSRV from the prebuilt-binary glibc contract.
- Release tags run a required `checks` job (fmt, clippy `-D warnings`, full
  test suite) before any asset is built.
- Each release now publishes `SHA256SUMS` alongside the three binaries.
- Every third-party action is pinned to a commit SHA (with a version comment).
- `action.yml` gains a `version` input so a consumer can pin the scanner
  release instead of following `latest`.

## 0.7.5 (2026-10-21)

Fixes for the P0/P1 findings in
[`docs/reviews/codex-commercial-review-2026-10-21.md`](docs/reviews/codex-commercial-review-2026-10-21.md).

### Exit status and gates

- The exit status no longer depends on the output mode. `--json`,
  `--report-json` and the TUI now fall through to `--fail-over` and
  `--fail-on-new` instead of returning early (`--report-json --fail-over 0` used
  to exit 0 where `--ci` exited 1).
- Precedence is fixed: operational error (2) > gate violation (1) > success (0),
  so a SARIF/report write failure can neither be masked by a failing gate nor
  mask one.
- `--sarif -` together with `--json`/`--report-json` is a usage error (both
  would write a document to stdout).
- `--fail-over` rejects non-finite and out-of-range values, and exits 2 when the
  gate cannot be evaluated (no exactly parsed functions) instead of passing.

### Baselines and Perforce

- Perforce baselines read the **as-of tree** (`p4 print <depot>@<cl>`) rather
  than `@=<cl>`, which select only the revision created by that exact change.
  Verified against a local `p4d`: a file last edited in CL5 and queried at CL6
  now returns its CL5 contents instead of "no such file(s)".
- A VCS read failure is no longer reported as a new function. `file_at` returns
  `Found`/`Absent`/`Unavailable`; unavailable reads land in
  `baseline.files_unavailable`, stay out of `files_compared`, and make
  `--fail-on-new` exit 2 because the gate result is incomplete.
- Every baseline change carries its parse confidence (`tier`); best-effort
  findings are marked in the terminal, HTML and SARIF output.

### Parsing confidence

- Best-effort (syntax-error) functions are excluded from the health score and
  reported separately (`files_best_effort`, `health.best_effort_functions`).
  They still appear in the charted histograms.
- `.h` files are no longer measured by the wrong grammar when the C parse is
  degraded: C and C++ are both tried and the cleaner tree wins, so a C++ header
  yields `cfun, method` instead of the phantom `cfun, N, X`.

### Cache

- Cache hits are carried into the next cache, so four identical scans keep
  `2, 2, 2, 2` entries instead of oscillating `2, 0, 2, 0`; a fully warm scan
  does not rewrite the file.
- The cache records the tool version and rejects a mismatch (version 3).
- `--verify-cache` re-reads hits and compares a content digest, catching a
  same-size rewrite with a restored mtime. The default remains the fast
  `(mtime, size)` path.
