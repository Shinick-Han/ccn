# Commercial readiness review of ccn 0.7.4

Date: 2026-10-21. Independent reviewer: Codex Stackbite seat `terminal_ccd674039c2c41b8a340809a21610bc2`. Report-to: `terminal_22ac0364e8b44d05a9475fbd0911e3e3`.

Reviewed checkout: `main`, commit `4748cf039eef8cb155e4c27d0ea847540dd0b4f4`, initially clean. Target: C/C++/PrimeTime/Tcl on AlmaLinux 8, Perforce, NFS and LSF. This report is the only intended repository change; no tracked source was edited and nothing was pushed.

## Executive verdict

**Not commercial-ready for enforced C/C++/Perforce change gates or claims about PT runtime improvement.** It is a useful internal advisory scanner for exploring source complexity and small, understood changes. Its strongest narrow use case is an offline source inventory with a portable HTML summary, interpreted by an engineer who checks parsing coverage and understands build configuration. Clean JS/TS, Rust, Python and Go projects can also benefit from its implemented rules.

The obstacle is not missing screens. The product already has fixed histograms, a transparent metric pipeline, an accessible HTML foundation, TUI browsing, baseline deltas and packaging. Several failure paths nevertheless produce confident wrong findings or successful gate exits. I reproduced an ineffective JSON-mode gate, cache eviction on every warm run, stale cached numbers, a false overload regression and a C++ header measured as phantom C functions. Perforce baseline semantics add another correctness blocker. These precede broader visualization or language work.

The credible commercial direction is **a reproducible local change-review dossier for one EDA module**, later joined to measured runtime evidence. CCN is a prioritization signal; it does not estimate timing-analysis runtime, algorithmic complexity, functional correctness or defect probability. Prove that the shortlist changes a team's review or optimization decisions before claiming economic value.

## Evidence and validation boundary

Reviewed implementation/tests for scan/cache, rules/engine/heuristic, metrics/model, baseline, Git/P4/VCS, history, SARIF, HTML, CLI/main and TUI; `README.md`, `docs/PLAN.md`, `calibration.md`, `COMPATIBILITY.md`, `RELEASES.md`, calibration CSV/harness, `action.yml`, both workflows and `npm/`. PLAN contains historical records through section 28. A separate PT strategy document was not present in the repository inventory; the PT assessment uses the assignment and PLAN's delta/module notes, not an assumed unseen specification.

Executed on Windows with Rust/Cargo 1.99.1:

| Check | Result |
|---|---|
| `cargo test --locked` | 334 library tests + 2 golden integration tests passed; binary/doc suites have 0 tests |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo fmt --check` | Passed |
| CLI probes on isolated files and a temporary Git repo | Confirmed cases below |
| P4 availability | `p4` absent from PATH; no new live-server validation |
| AlmaLinux/NFS/LSF, downloaded release, installed npm package | Not executed; Windows results do not certify those environments |
| Browser/real SSH terminal accessibility | Source/test review only; no pixel, screen-reader or interactive terminal certification |

The existing `target/` layout was retained. No `ARCHIVED.json`, CodeGraph index or verified WorkBite Task binding existed; outputs were not enrolled in StorageGuardian. Probes used temporary directories and ignored `target/commercial-review-94c7979c`. Public documentation queries did not include source or proprietary repository contents. External primary sources below verify protocol semantics and competitor capabilities, not ccn performance.

### Reproduced cases

1. **Cache oscillation:** two C files, four identical `--json` scans, isolated `CCN_CACHE_DIR`. Saved entries: **2, 0, 2, 0**. `CCN_TIMING=1` confirmed corresponding loaded counts.
2. **Stale content:** replace `if(x)` with equal-length `/*x*/`, restore original nanosecond mtime. Cached CCN stays **2**; `--no-cache` gives **1**.
3. **Gate bypass:** CCN-12 function plus simple function gives 50% over 10. `--ci --no-report --fail-over 0` exits **1**; `--report-json --fail-over 0` exits **0**. A baseline reporting a change also exits 0 in report-JSON mode and 1 in normal mode.
4. **Masked write error/mixed stdout:** `--report-json --sarif <missing-directory>/out.sarif` prints a write error but exits **0**. `--report-json --sarif -` emits two JSON documents; a JSON decoder fails with `Extra data`.
5. **Header parser selection:** the following `.h` reports BestEffort functions **cfun, N, X**; identical `.cpp` reports Exact functions **cfun, method**:

   ```cpp
   int cfun() { return 1; }
   namespace N { class X { public: int method() { if(true) return 1; return 0; } }; }
   ```

6. **Identity:** swap unchanged C++ overloads `f(int)` and `f(double)` with CCN 11 and 12. Baseline reports a false **11 → 12 worsened** function.
7. **Ignore/exclusion:** `.tokeignore` containing `ignoreme.c` does not exclude it. A C file with `// Code generated by tool. DO NOT EDIT.` is excluded normally but reports `generated: 0` under `--no-ccn`.
8. **Controls:** `#if 0` hides the dead function and preserves live `#else`. Tcl `proc` is heuristic with zero measured functions. A tested branch-expanding macro gives caller CCN 1 at BestEffort. A low-CCN function growing by 30 comment lines produces no baseline change, consistent with current threshold policy.

## Strengths worth keeping

- **Separate heuristic and function measurements.** `src/ccn/types.rs:205` keeps heuristic function counts zero; `src/metrics.rs:224` excludes them from health. Empty function sets have no health (`metrics.rs:245`), rather than a fictitious perfect score.
- **Inspectable language-specific rules.** `src/ccn/rules.rs:44`, `:77`, `:107`, `:128`, `:152`, `:184`, `:206`, `:240` document decisions/operators and deliberate divergences. Nested functions are measured separately (`engine.rs:384`). This is a stronger foundation than one cross-language keyword count.
- **Useful C/C++ resilience.** Iterative traversal, error-subtree exclusion, literal preprocessor filtering and partial recovery exist (`engine.rs:200`, `:246`, `:317`, `:384`); sibling grammar fallback is explicit (`ccn/mod.rs:25`). Preserve them for exploration while making confidence honest.
- **Deterministic presentation.** Paths/findings are sorted (`loc.rs:308`, `baseline.rs:297`, `sarif.rs:45`); histogram edges are fixed (`metrics.rs:34`). HTML tests cover determinism, escaping, empty states and table size (`report/html.rs:1081` onward).
- **Practical offline HTML.** SVG titles, keyboard focus, text tables, skip link and themes exist (`report/html.rs:44`, `:478`, `:528`, `:702`). No remote chart/font dependency is needed; user text is escaped (`:247`).
- **VCS separated from metrics.** `vcs.rs:82` normalizes backend paths; Git baseline follows file renames. Commands have deadlines/capture caps (`git.rs:83`, `p4.rs:62`, `:714`) and arguments avoid shell concatenation.
- **Appropriate distribution starting point.** Linux builds inside AlmaLinux 8 and checks GLIBC symbols (`release.yml:23`, `:36`). npm uses exact-version optional platform packages without install scripts (`npm/package.json:34`, `npm/bin/ccn.js:15`). Native scans require neither Node nor Python.
- **Useful tests.** Golden expectations are separate from the engine (`tests/ccn_fixtures.rs:69`); baseline, normalization, report and TUI behaviors have focused tests. All 336 passed here. This supports bounded hardening, not a production certification.

## Ranked gaps and acceptance criteria

P0 blocks trusted gating in the target environment. P1 blocks a supported commercial pilot or materially weakens its evidence. P2 is follow-on polish. Acceptance criteria are proposed work, not completed features.

### P0-1 — Output mode changes CI correctness

**Evidence:** `src/main.rs:55` records SARIF failure, but `:65-79` returns early for report-JSON or successful TUI. Gates run only at `:133-158`; they can also overwrite operational-error code 2 with gate code 1. SARIF stdout (`:52`) can combine with JSON writers. Probes confirm report-JSON behavior; TUI bypass is a static finding.

**Impact:** adding machine-readable output can turn a failing gate into success; output failure becomes indistinguishable from success.

**Acceptance:** compute final status independently of rendering, preserve operational-error precedence, and make all permitted modes return the same gate outcome. Process tests must cover pass/fail/invalid baseline/write failure across text, JSON and report JSON. Reject multiple stdout documents. Either reject TUI with gates or enforce final status after quit. Validate fail-over as finite and 0–100.

### P0-2 — P4 baseline is not an as-of tree, and read failure means “new”

**Evidence:** `src/p4.rs:183-197` prints `<depot>@=CL`, selecting a revision at that change rather than the tree immediately after it. A file last changed at CL10 exists as of CL20 even if CL20 touched another file. P4 distinguishes these selectors in its [file specification reference](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/filespecs.html). `baseline.rs:260-270` treats every missing read as an empty baseline. `git.rs:282-298` and `p4.rs:700-703` collapse absence, access failure, timeout and process failure into `None`. Validating the revision once does not validate later reads. `p4.rs:173` can recognize a described shelved change without enforcing submitted-only snapshot semantics.

**Impact:** unchanged old complex functions can become Added, and network/permission errors can fabricate findings. Unreadable comparison data cannot be safely interpreted.

**Acceptance:** define a submitted as-of baseline, normally `@CL`; separate Found, ConfirmedAbsent, Unavailable and Invalid. Real-p4d test: A in CL1, unrelated B in CL2, A edited in CL3; baseline CL2 must read A's CL1 contents. Cover restricted paths, expired ticket, missing file, timeout, shelf, move/delete. Incomplete comparisons must be operational failure or explicit incomplete results, never invented Added. Apply the same distinction to Git.

### P0-3 — Confidence disappears before scoring/gating

**Evidence:** partial parses become BestEffort (`engine.rs:161-178`); error subtrees are omitted (`:392`). Every clean wired parse is Exact (`rules.rs:277`). All non-heuristic files increment **files_exact** (`metrics.rs:510-520`). Health (`:224-240`), baseline (`baseline.rs:190-197`) and SARIF (`sarif.rs:92-99`) include BestEffort alongside clean measurements. Walker errors vanish through `.flatten()` (`loc.rs:452`), LOC read errors through `.ok()?` (`:501`); omitted files do not reliably enter the no-CCN denominator.

**Impact:** breaking parsing or access can improve apparent results. A BestEffort macro function may look simple; the user cannot see how much behavior was unmeasured.

**Acceptance:** preserve per-file/function parse status, error ranges, chosen grammar and skip reason in every output. Separate Exact/BestEffort/Heuristic/unreadable/oversize and measured coverage. An enforced changed-file gate must explicitly handle degraded/unmeasured relevant functions, defaulting away from silent success. Fault-injection tests must show inaccessible directories, truncated functions and macro recovery cannot masquerade as complete clean gates.

### P0-4 — Header fallback can select phantom functions

**Evidence:** `.h` prefers C (`ccn/types.rs:101-118`); first partial success returns before trying C++ (`ccn/mod.rs:29-40`). The reproduced header recovers namespace/class names as functions. `--lang cpp` filters tokei classification, not explicit parser choice (`loc.rs:192`, `:456`, `:532`).

**Acceptance:** provide reproducible grammar overrides and evaluate plausible alternatives when the first C/C++ parse is degraded. Record selection/reason. The `.h`/`.cpp` probe must produce the same two real functions with no N/X phantoms. Include C linkage, namespaces, inline methods, templates, attributes and team macro conventions. Do not claim compiler-equivalent analysis.

### P1-1 — Function identity is too weak for C++ delta gates

**Evidence:** `baseline.rs:90-108` consumes duplicate names in source order. `FunctionCcn` has no signature/stable identity (`ccn/types.rs:127`). Some out-of-class qualified names are recovered; inline methods, overloads and anonymous callbacks still collide. The reorder probe creates a false regression.

**Acceptance:** match qualified scope + normalized signature first, then deterministic structural/diff evidence with explicit confidence. Expose unresolved matches rather than guessing. Pure overload reorder, same-name method insertion in another class, callback move and pure rename must not fabricate worsening; real body growth must match correctly. Export old/new identities and matching reasons.

### P1-2 — Cache loses hits, admits stale content and lacks effective version identity

**Evidence:** hit path returns no next entry (`loc.rs:479-494`); only returned entries populate next (`:299-317`), explaining 2/0 oscillation. mtime+size alone (`cache.rs:39`, `:121`) permits the stale-content probe. Load checks version 2/root only (`:90-96`); contrary to `:28`, package version is not checked. Rule/dependency changes without a cache bump can reuse old numbers. Fingerprint, LOC and CCN reads happen at different times (`loc.rs:477-524`).

**Acceptance:** preserve hits without rewriting fully warm caches; test cold/warm/warm/edit/delete/single-file/full-directory sequences. Key validity by effective parser/rule/classifier identity. Provide content-verified operation for gates/NFS and pre/post-read consistency. The restored-mtime probe must match cold scans. Expose hits/bytes/writes without source contents.

### P1-3 — NFS/LSF resource behavior is unproven

**Evidence:** serial walk/full candidate vector (`loc.rs:451`), metadata and two cold reads (`:477-524`), all results retained (`:279`), per-thread parsers (`engine.rs:98`). The 8 MiB guard follows the LOC read; excluded generated/vendor paths still incur CCN parsing. No explicit CLI worker/memory/scan-deadline budget (`cli.rs:8-151`). Baseline reads every measured file in Rayon (`baseline.rs:186`), with additional VCS commands per file. Metadata latency can dominate on NFS.

**Acceptance:** benchmark nominated SSD and team-NFS corpora, cold/warm/1%-changed, full/delta, recording p50/p95 wall time, peak RSS, stat/open/bytes and server requests. Respect explicit thread budgets/`RAYON_NUM_THREADS` and scheduler allocation. Agree budgets before implementation, for example 100k source files: warm p95 ≤10s, 1%-changed delta ≤30s, RSS ≤2 GiB on the nominated host. These are proposed targets, not measured performance. Bound parser work and total VCS cost; cancel cleanly. Do not change workload to make a benchmark pass.

### P1-4 — P4 mapping, windows and server cost need a real contract

**Evidence:** `parse_where` selects the first mapping (`p4.rs:466-479`), representing the scan with one depot prefix/local offset. This cannot model arbitrary multi-line views, exclusions/overlays/remapping. `file_at` repeats mapping resolution per file (`:190`). History describes up to 300 CLs in the CPU Rayon pool (`:209-255`) without whole-run deadline/separate server concurrency. Each capture can retain 64 MiB; truncation returns successful bytes without completeness metadata (`:754-772`). Since mode queries submitted changes plus opened paths (`:131-157`), not proof that local have contents equal a submitted snapshot. P4 rename mapping is empty (`vcs.rs:149`); `.p4ignore` is fixed-name (`loc.rs:432`), not discovered via P4IGNORE.

**Acceptance:** resolve workspace identity once; support or reject complex views explicitly. Define submitted/head/have/opened/shelved modes. Live tests must cover non-head syncs, unrelated valid CL, no opened files, remapped multi-root/overlay views, streams, moves and custom P4IGNORE. Batch or bound reads/commands and report actual window, requested/succeeded/skipped CLs, truncation and errors. Represent line-count absence as unavailable, not 0. Cross-stream history may remain deferred if clearly unsupported.

### P1-5 — Health is a source-layout statistic with unvalidated product meaning

**Evidence:** nloc is physical function span including comments/blanks (`engine.rs:352`). Health sums spans (`metrics.rs:232-249`), potentially double-weighting nested functions. Formatting/comment padding changes weights without behavior changes. Bounded penalty caps CCN's penalty, not a long function's share of total weight. Partial coverage/unsupported Tcl can flatter scores. Eleven local observations in `calibration.md` are not defect labels or PT outcomes; limitations acknowledge TS bias/no observed E. `HEALTH_EXPERIMENTAL=false` (`metrics.rs:398`) overstates validation.

**Acceptance:** name a defined measured-source complexity index; expose denominator, language/confidence coverage and physical span versus code lines. Document overlap or use disjoint weights. Show actual over-threshold line fraction separately from logarithmic debt. Keep grades advisory until EDA holdout data/expert review validate decisions. Add comment-only, reformatting, nested-function, giant-function and unsupported-language controls. Never infer runtime or defect probability from grade.

### P1-6 — Tcl/PT coverage and runtime evidence are absent

**Evidence:** no Tcl in ten grammars (`ccn/types.rs:53`); Tcl probe yields no function delta. `Report` lacks profile/build/module identity (`metrics.rs:351`). Module summaries remain unimplemented (`PLAN.md:1117`). Length/nesting growth is reported only at current CCN >10 (`baseline.rs:110`, `:125`), excluding long low-branch PT orchestration procedures.

**Acceptance:** inventory the team's C++/Tcl split first. Support a documented static Tcl proc/control-flow subset with confidence, or scope initial product to C++ and import trusted external Tcl identities. Add separately configurable length/nesting deltas if needed. Import one actual team profile format with units, self/inclusive time, run context and visible unmatched/ambiguous mappings. Do not infer runtime recommendations from CCN.

### P1-7 — Reports/snapshots cannot prove what was measured

**Evidence:** raw JSON has no schema/tool/rule identity (`model.rs:132`); report JSON lacks a full analysis manifest (`metrics.rs:351-394`). Snapshots record tool/bin/calibration (`history.rs:33-50`), not HEAD/CL, dirty contents, filter/exclusion settings, scope, grammars or build flags. Trend only conflicts with since, so filtered and full scans can compare without scope warnings. Report does not identify since mode as a distinct scope.

**Acceptance:** version a published schema/manifest including source/revision/dirty identity, selected paths, exclusions, config/rules/grammars, completeness and optional build/profile identity. Compare these and coverage, not only tool version. Label subset health everywhere. Export complete file/function/churn data; top 20 must not be the only machine export of churn. Add schema/golden compatibility tests and documented units/missing values.

### P1-8 — Concurrent persistence can lose data

**Evidence:** fixed `.tmp`, destination deletion, then rename (`cache.rs:109-118`, `history.rs:179-187`) is not atomic replacement. Concurrent scans share a temp file; interruption can lose the previous valid file. History read/append/write is unprotected (`history.rs:227`). Loading prioritizes a valid repo file over newer fallback (`:196-217`). P4 read-only files do not automatically make the parent directory unwritable.

**Acceptance:** unique same-directory temp files, correct replacement and locking/conflict detection for history. Interrupt each stage and run concurrent scans: retain old or complete new data and every accepted snapshot. Select/merge history sources consistently, expose actual location, test read-only directories and existing read-only files. Allow cache/history outside NFS.

### P1-9 — Release trust/version identity are incomplete

**Evidence:** release jobs build/publish directly (`release.yml:33`, `:60`, `:80`, `:89`, `:109`, `:118`) without required tests/clippy/fmt, executable smoke tests, checksum manifest, provenance, SBOM/notices or cross-package version assertions. Build inputs float. Composite action downloads latest independent of its commit (`action.yml:86`), checks only nonempty file (`:96`), and falls back only on download failure. A pinned action can run another version. npm prepare writes directly to final binary without digest/version/arch checks (`prepare.js:40-59`); publish checks directory existence/package versions, not binary identity. Platform manifests declare MIT but list only vendor binaries; published notice contents were not verified.

**Acceptance:** require test/lint/native smoke checks. Bind action source, binary version and immutable verified artifact; reject corrupt/wrong-version/wrong-arch files before execution. Check dynamic dependencies and execute fixture/baseline scans inside AlmaLinux 8, not GLIBC symbols alone. Pin enough build inputs for reproducible support. Check Cargo/tag/npm/platform/binary versions together. Test npm pack/ignore-scripts installation in an empty directory, include license/dependency notices and an offline mirror workflow. Add dependency auditing; this review makes no vulnerability-free assertion.

### P1-10 — Advertised source-build minimum contradicts locked dependencies

**Evidence:** `Cargo.toml:6` and `COMPATIBILITY.md:45` say Rust 1.80. Lock resolves ratatui → instability 0.3.14 → darling 0.24.1 (`Cargo.lock:1191`, `:758`, `:369`). Their installed manifests declare **1.88**; unicode-segmentation 1.13.3 (`Cargo.lock:1809`) declares **1.85**. They compiled in this run. A Rust-1.80 build was not attempted. Full offline metadata could not complete because unrelated target dependency linux-raw-sys 0.4.15 was not cached; nothing was fetched for that check.

**Acceptance:** raise minimum to a tested value or resolve compatible dependencies. CI must build/test locked dependencies on declared-minimum and current supported Rust. Documentation must match; Rust MSRV and native glibc requirements are different contracts.

### P1-11 — Privacy needs a precise boundary and local protections

**Evidence:** no HTTP client/telemetry path found in scanner source; runtime subprocesses are Git, P4 and opt-in OS report opening (`git.rs:122`, `p4.rs:701`, `main.rs:185`). P4 commands contact the configured server with path/revision arguments. Workflow explicitly uploads SARIF (`.github/workflows/ccn.yml:48` onward). Cache plaintext stores root, paths and function names (`cache.rs:57-77`); filename hashing does not anonymize content. HTML/JSON expose absolute root; hotspots expose author names. Writers inherit permissions, and temp HTML filename is predictable (`main.rs:174`).

**Acceptance:** document no telemetry/source upload by scanner, with configured P4 and caller-directed SARIF exceptions. Provide root/author redaction and private export/cache/temp storage with safe writes. Verify Unix permissions/symlink handling on shared hosts. Offline scan with network denied must work; history may contact only expected P4 infrastructure. Never silently upload source, design names or filesystem paths. This is not a full adversarial security audit.

### P2 — Engineer-facing polish after correctness

| Gap and evidence | Acceptance |
|---|---|
| HTML says last 365 days for P4's N-CL window (`report/html.rs:179`, `:222`); line absence is 0 (`p4.rs:279`). Zero-risk files with commits still get positive percentile products (`metrics.rs:445-456`). | Show backend window/completeness/unavailable values; distinguish activity ranking from complexity debt. A simple repo must not imply measured complexity debt. |
| SARIF is scan-root-relative without checkout rebasing/URI escaping (`sarif.rs:97`, `:245`); subroot scans can point at wrong files. Delta text always says CCN increased, even for length/nesting-only findings (`:135`); no stable fingerprint/incomplete-run diagnostics. | Schema/consumer tests for subroots, spaces, #, %, Unicode, duplicates; correct repository URI/reasons, stable identity, incomplete status. |
| Tokeignore is advertised but not configured (`cli.rs:34`, `loc.rs:422-437`); markers disappear in no-CCN/oversize cases (`:510-537`); classification reason is discarded. | One ignore/exclusion contract with per-file reasons, no-CCN parity and targeted overrides, including actual P4IGNORE configuration. |
| HTML lacks full file/function drilldown and module/tier filters (`html.rs:159`, `:745`); TUI duplicates risk calculation (`tui.rs:500`). | Verify keyboard/screen-reader/contrast and SSH resize; prioritize actionable locations/coverage over chart types; share one metric implementation. |
| No-functions diagnostics require separate Python; read errors vanish (`README.md` diagnostic section; `loc.rs:501`). Headless default writes HTML to cwd (`main.rs:113`). | Native CLI explains measured/unsupported/skipped counts, outputs/storage and actionable errors. Linux/P4/LSF quickstart documents read-only operation and exit codes. |

## Accuracy and engineering assessment

Exact means clean tree-sitter parse under a rule table, not an exact compiled control-flow graph. Literal #if handling works; arithmetic conditions, build macros, includes and macro expansion are not evaluated (`engine.rs:273`). Ifdef keeps both branches. A clean parse can count noncompiled branches or miss macro decisions. Define semantics for if-constexpr, generated headers and dialects using team fixtures. Keep a truthful fast source mode; add build-context enrichment only when the pilot needs it.

Language rules are useful contracts, not interchangeable metrics. Python's table omits assert/with while [Radon](https://radon.readthedocs.io/en/latest/intro.html) counts them. C/C++ default labels, Rust ?, JS optional chaining and guards need explicit interpretations. A repository-wide ±10% oracle comparison can hide compensating function errors. Compare agreed constructs and identities, not an unexplained accuracy percentage.

The golden harness checks CCN/optional params and clean parsing (`tests/ccn_fixtures.rs:175`, `:230`), not the executable contract. It does not certify lines/nesting/identity/status or representative EDA dialects. There is no dedicated .ts fixture directory although TS and TSX use distinct grammars; TSX does not cover all TypeScript syntax.

The two workflows scan complexity or build releases, not a cross-platform correctness suite. The self-scan action can download latest release rather than test the changed scanner. A green complexity scan therefore does not establish that PR implementation passes tests. Add process regressions, live P4 contracts, malformed-input fuzzing, Linux permissions/path tests and scale checks. Windows symlink tests may early-return without assertions (`loc.rs:901`, `:937`), so passed counts do not certify Unix links. Do not substitute a coverage percentage for these cases.

Memory grows with all function details and old/new cache/result copies. Determinism also depends on ignore config, filesystem encoding and history. Tokei decodes LOC while tree-sitter receives raw bytes (`loc.rs:498-524`); UTF-16/legacy encoding coverage must be defined/tested. POSIX non-UTF-8 filenames are lossy (`loc.rs:582`). Git lacks -z and parses quoted/tab/newline paths as ordinary strings (`git.rs:183`, `:220`, `:401`); use unambiguous output and define supported path behavior.

## Competitive positioning

Capability comparison from primary documentation checked for this review; not new benchmarks or pricing analysis. Main-branch documentation may exceed released functionality: pin versions in any trial.

| Alternative | Why teams choose it | Credible ccn choice |
|---|---|---|
| [scc](https://github.com/boyter/scc/blob/master/README.md) | Broad LOC, file-level estimates and repository reports. Current docs also describe HTML/hotspot features; HTML alone is not a durable distinction. | Function AST-rule deltas with transparent P4 evidence, once trustworthy. No speed claim without matched workloads. |
| [tokei](https://github.com/XAMPPRocky/tokei) | Focused multi-language LOC; already ccn's LOC engine. | Named function/baseline/review workflow above LOC. |
| [lizard](https://github.com/terryyin/lizard) | Function CCN, nloc, parameters, long names and automation; explicit partial-parser/preprocessor limitations. | One native local deployment and demonstrated EDA corpus fit. Distinguish lizard nloc from blank/comment/code decomposition in comparisons. |
| [radon](https://radon.readthedocs.io/en/latest/intro.html) | Python AST/maintainability metrics. | Mixed C++/Tcl/P4 workflow; a new grade alone does not justify replacing it in Python-only teams. |
| [SonarQube](https://www.sonarsource.com/products/sonarqube/) | Wider quality/security/governance integrations; offerings include IDE/CLI as well as server/cloud. | Restricted offline dossier without standing services. Do not describe all Sonar use as server-only. |
| [CodeScene](https://www.codescene.io/docs/guides/technical/hotspots.html) | Code health plus change activity, organizational context and deeper prioritization. | Lightweight P4-first EDA evidence. Simple percentile multiplication is not equivalent validated behavioral analysis. |
| [CodeCharta](https://codecharta.com/) | Local interactive architecture/hotspot visualization and multiple importers. Local privacy is not unique to ccn. | Fast function/job-specific evidence; export interoperable metrics instead of rebuilding a city view. |
| [clang-tidy](https://clang.llvm.org/extra/clang-tidy/) / [PMD](https://pmd.github.io/pmd/pmd_userdocs_tools.html) | Actual static checks and established rules/integrations; clang-tidy uses compile context. PMD is not a C++ compiler-analysis replacement. | Complement findings with inventory/change ranking; CCN cannot replace semantic/bug/security analysis. |

A team should choose ccn when one approved binary can answer: what changed, what was unmeasured, and where should review start? Today that full answer is unreliable. A paid proposition based only on a health grade/chart is weak; runtime improvement claims without profiles are weaker.

## Smallest PT/EDA slice that proves value

One volunteer owned module, a stable submitted baseline CL, one candidate CL/shelf or explicitly captured dirty tree, and a representative timing workload. Start with C++ if reliable Tcl identity is unavailable; do not silently grade Tcl as measured.

1. **Source/build/run identity:** depot/client mapping, baseline, candidate hashes, dirty/opened state, executable/build ID, compiler/flags/macros, tool/rules/config. Profiles must identify the exact binary/workload.
2. **Module/function review:** explicit path grouping, qualified identities, CCN/length/nesting deltas, matching confidence, parse/skip coverage. Static risk and runtime evidence remain separate columns.
3. **One actual profile/log importer:** native perf-derived CSV or actual PT/procedure timing export, chosen from team evidence. Do not assume PT exposes native internal symbols. Retain units, self/inclusive times, calls, source locations and unmatched/ambiguous records; never allocate samples by name guess.
4. **Short actionable shortlist:** reason and location per target plus omissions. No-change/rename/reorder controls must be empty; seeded real regression must be detected.
5. **Controlled benchmark:** same input, host/thread/memory limits, tool version, seed/cache policy. Record LSF allocation/queue time separately from runtime, CPU/elapsed and RSS. Repeat paired baseline/candidate runs and show variability. Preserve agreed PT correctness/quality invariants, including timing results, not only faster completion.

**Pilot success:** two engineers independently reproduce dossiers on AlmaLinux 8/NFS; at least 20 real changes with ≤5% false matches on adjudicated identity cases and 100% detection on an agreed seeded gate corpus; top-five lists identify an owner-confirmed actionable review/optimization target per evaluated workload. Demonstrate one accepted optimization preserving agreed PT outputs and improving runtime beyond measured noise. If no optimization appears, report it and retain only evidenced review value. CCN reduction alone does not qualify.

## Prioritized 90-day plan

| Period | Build in dependency order | Exit criterion |
|---|---|---|
| Days 1–14 | P0 status/output fixes, P4 snapshot/errors, parse completeness, header selection; process and live-p4d regressions; cache carry-forward. | All reproduced blockers covered; unreadable/degraded relevant code cannot silently pass. Keep use advisory until then. |
| Days 15–30 | Identity/confidence, manifest/schema, content-verified cache, concurrent persistence, true MSRV. Release tests/native AlmaLinux smoke, artifact/version binding. | No-change/reorder/rename controls pass; cached/full equality; offline artifact traces to tested source. |
| Days 31–50 | NFS/LSF measurement; bound CPU/VCS/deadlines; validate/reject complex P4 views; explicit coverage/window labels; owned module grouping and quickstart. | Meet agreed real-host resource budget; no silent partial history/truncation; new engineer completes documented workflow. |
| Days 51–70 | One real profile/log importer with source/build join; Tcl subset or trusted identity import only if required. Produce one module dossier. | Correct build/source joins, visible ambiguous/unmatched samples, no fabricated attribution. |
| Days 71–90 | Two-engineer/20-change pilot, paired workload benchmarks, manual error audit, onboarding, private export/notices/support diagnostics. | Publish value and limits; meet shortlist/matching/reproducibility criteria; support only tested matrix. |

**Do not build during these 90 days:** SaaS accounts/database, telemetry, AI refactoring, universal defect/security score, general Tcl evaluator, compiler replacement, ten additional grammars, 3D city, enterprise dashboard, unrestricted cross-stream mining or direct LSF submission/control. Existing batch tooling can schedule exported artifacts. Defer non-target architectures unless a pilot user requires them.

## Claims needing correction or qualification

- Second scan nearly free / unchanged cache not rewritten (README Cache; PLAN:709): carry-forward currently empties warm cache. Historical Windows speed numbers are not NFS guarantees.
- Cache cannot serve stale numbers / package version covers changes (`cache.rs:4`, `:28`): restored mtime/size gives stale results; package version is not checked.
- Exact and no silent tier mixing (PLAN:128): report files_exact and health mix BestEffort. Exact does not mean compiled control-flow accuracy.
- One function cannot decide grade (README health; `metrics.rs:156`): bounded penalty does not bound its physical-span weight share.
- Unchanged/improved functions silent (README baseline): false for overload reorder. P4 @=CL does not implement an as-of tree baseline.
- Rust 1.80+ (`COMPATIBILITY.md:45`): locked installed dependencies require 1.85/1.88; a minimum-toolchain build is still needed.
- Platform minimums (`COMPATIBILITY.md:13-15`): GLIBC guard exists, but published asset execution was not verified here. macOS minimum/Windows static-runtime behavior are not established by workflow alone.
- macOS x64 asset (`RELEASES.md`): docs admit arm64 on Apple Silicon; action still maps Intel to it. Use explicit target assets. Linux release docs should distinguish Ubuntu host from AlmaLinux build userland.
- P4 last-365-days labels (HTML:179/:222): actual N-CL window and unavailable line counts need honest representation. P4 info hint comment (`p4.rs:79`) is stale: implementation does not perform the hint check.
- PLAN oracle/performance/test gates (`PLAN.md:319-324`): workflows do not enforce them. Historical four-CL p4d validation is useful but not a persistent contract suite or new runtime evidence here.
- Early PLAN JSON/CLI examples are proposals, not current contract: functions_detail/accuracy/--detail/--exclude differ from implemented nested ccn.detail/tier/CLI. Distinguish design/history/current support so onboarding does not require reconciling 28 chronological sections.

## 오케스트레이터용 10줄 요약

1. 현재 ccn 0.7.4는 내부 참고용으로 유용하지만 C++/Perforce 강제 게이트용 상용 제품으로는 준비되지 않았습니다.
2. 독립 검증에서 334개 단위 테스트와 2개 골든 테스트, clippy와 fmt 검사가 모두 통과했습니다.
3. report-json 모드가 게이트 위반과 SARIF 쓰기 실패를 종료코드 0으로 숨기는 문제를 재현했습니다.
4. P4 baseline의 @=CL 선택과 읽기 실패를 신규 파일로 취급하는 계약은 우선 수정해야 합니다.
5. C++ 헤더 오파싱, 부분 파싱 신뢰도 혼합, 오버로드 재정렬 오탐이 분석 신뢰성을 떨어뜨립니다.
6. 캐시는 연속 실행에서 항목이 2→0→2→0으로 바뀌며 같은 크기와 mtime을 유지한 수정도 놓칩니다.
7. Health는 주석과 공백을 포함한 함수 길이 가중 지표이며 PT 실행시간이나 결함 확률을 보증하지 않습니다.
8. AlmaLinux 배포 기반은 있지만 실제 NFS·LSF 성능, P4 복잡 뷰, 릴리스 무결성 검증이 더 필요합니다.
9. 90일 목표는 한 모듈의 정확한 함수 delta와 빌드 식별, 실제 프로파일 한 종류를 연결하는 재현 가능한 파일럿입니다.
10. SaaS와 시각화 확장보다 신뢰성 수정 후 20개 실제 변경과 반복 성능 측정으로 사용자 가치를 증명해야 합니다.
