# Changelog

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
