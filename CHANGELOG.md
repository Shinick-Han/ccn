# Changelog

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
