# Changelog

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
