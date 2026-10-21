# ccn 0.7.7: second review and highest-ROI next work

2026-10-21. Reviewer2: `terminal_f372514a0b7448719481a2fc6d105b4e`; report-to: `terminal_22ac0364e8b44d05a9475fbd0911e3e3`.
Checkout: clean `main`, `9e5573ce20eb30e09492ba43e83529d0fbcc1e67`. This document is the only intended repository change. No source edits, commit, push, version change, or worker delegation.

## 1. Executive decision

**0.7.7 substantially improves the scanner, but does not reverse the first review's verdict for enforced C/C++/P4 gates or PT runtime claims.** Sell a bounded, explicitly advisory design-partner pilot if someone will pay for a demonstrated review workflow. Do not sell a generally trustworthy change gate, a validated code-health score, or runtime optimization yet.

The output-status fix, as-of P4 selector, cache carry-forward/versioning, ordinary overload identities, header fallback, and release checks are real progress. However, I reproduced both false-positive identity deltas and an incomplete comparison returning success. P4 view handling now models more layouts, but its approximation can silently omit files that actually belong to the view. Confidence labels are still incomplete in full SARIF. These are trust problems, not missing chart types.

**The highest-ROI two-week item is one buyer-backed, reproducible CL review pilot:** one source-owning team, one module, one submitted baseline and one real candidate CL, with every changed file accounted for and a short actionable delta report. Spend the engineering portion on the bounded correctness contract below. Obtain an explicit paid-pilot/continuation decision. This tests whether the workflow replaces work, while fixing defects that would otherwise invalidate its evidence. A faster synthetic scan or a speculative profile importer cannot answer that commercial question.

First settle **who owns the code**. An EDA tool developer can review its C++ source. A chip-design team using a closed PrimeTime binary generally cannot apply this scanner to the vendor's internal C++. For that buyer, the available inputs may be Tcl orchestration and job/timing logs; current Tcl is heuristic and has no function delta. Do not treat these as one validated customer segment. Source access, relevant profile access and a budget holder are unverified here.

## 2. What was actually checked

| Evidence | Result / limit |
|---|---|
| `cargo test --release --locked` | **382 library + 1 binary + 2 golden + 10 CLI tests passed**, not the assignment's 13 CLI count. No test failure. |
| `cargo clippy --release --locked --all-targets -- -D warnings`; `cargo fmt --check` | Passed. |
| Native `target/release/ccn.exe` | Reports 0.7.7; SHA256 `0A69A1CCF3AED96855253F534F422A3B7089EAD19F0F7A76965CABD8DB651BA4`. Temporary Git fixtures reproduced the findings below. |
| Positive controls | Plain `f(int)` / `f(double)` reorder: no delta, exit 0. Original `.h` phantom-function case: Exact `cfun`, `N::X::method`, no `N`/`X` functions. Four repeated cache probes loaded four entries each; the oscillation is gone. |
| Cache contract | `src/cache.rs:68` is **v4**, including identity; `:170` checks format + package version + root. Carry-forward and restored-mtime `--verify-cache` tests passed. Default metadata-only hits remain intentional; gate invocations must opt into verification. |
| Release workflow | All three build/publish jobs require `checks` (`release.yml:49`, `:105`, `:137`); checks execute fmt, clippy and release tests (`:38-45`). Checksums depend on all three jobs (`:173`). This workflow has no tag-publication path that skips its checks. |
| P4 probes | No `p4`/`p4d` on PATH. A tiny temporary `p4.exe` response stub plus a Rust harness linked to the current release library exercised public `p4::file_at` / `baseline::collect`. These are **mocked protocol reproductions, not live-server certification**. |
| Storage / artifacts | No `ARCHIVED.json`, `.codegraph/`, or verified WorkBite binding. Existing `target/` retained, not enrolled or relocated. Probe scratch is under `target/roi-probes`; initial native fixtures also used `%TEMP%/ccn-roi-*`. No cleanup of unrelated outputs. |

Source references below are relative to this checkout. The first review remains the detailed background: [commercial review](codex-commercial-review-2026-10-21.md). This is a correctness/product review, not a new security audit or market-size study.

## 3. Re-review findings

P0 blocks enforced gating; P1 blocks a credible supported pilot unless bounded out; P2 is follow-on accuracy/polish. Static conclusions and mocked results are identified explicitly.

### P0-1 — Unknown P4 mappings become harmless `Unmapped`; view precedence is wrong

**Evidence:** unsupported `*`/`%%n` patterns are discarded (`src/p4.rs:962-970`), with no unsupported-view status. A partially parsed spec remains usable. Failure to find a mapping becomes `FileAt::Unmapped` (`:254-257`), which never makes the baseline incomplete (`src/baseline.rs:17-21`; `src/main.rs:168`). Separately, mappings use depth/single-file/overlay ranking (`p4.rs:506`, `:621`), while all exclusions win permanently (`:627`; `:994`), losing ordered include/exclude semantics. If `client -o` fails, `:796` silently accepts the first usable `where` triple (`:1120`), with no completeness flag.

Mocked public-API reproductions, with simple `good/control.c` and CCN-12 `src/a.cpp`, baseline contents CCN 1:

| `View:` / response | Observed |
|---|---|
| `//depot/good/... //review/good/...` then `//depot/src/*.cpp //review/src/*.cpp` | `src/a.cpp` is **Unmapped**; compared=1, unavailable=0, unmapped=1, changes=[] despite an in-view worsening. |
| `//depot/... //review/...`, `-//depot/src/... //review/src/...`, then `//depot/src/... //review/src/...` | Re-included `src/a.cpp` is still **Unmapped**; same empty-change counters. |
| `//depot/specific/... //review/src/...`, then `//depot/broad/... //review/...` | Requests **`//depot/specific/a.cpp@10`**, rather than the later mapping's `//depot/broad/src/a.cpp@10`. It can compare the wrong history. |
| `client -o` fails; `where` returns a usable `good/...` triple | Other relevant files become **Unmapped**, although the full view was never established. |

The observed empty baseline passes the CLI's gate conditions by source inspection; these P4 API probes did not themselves run the CLI gate. Perforce documents that later overlapping mappings override earlier ones, and describes supported view wildcards: [view semantics](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/views.html), [wildcards](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/P4Guide/configuration.workspace_view.wildcards.html). The code comment claiming exclusion always wins (`p4.rs:574`) is not a valid general rule.

**Acceptance:** distinguish server-confirmed out-of-view from unsupported/unknown/partial mapping. Unknown relevant paths must produce incomplete/exit 2, not ordinary unmapped. The smallest safe release can reject unsupported views; it need not implement all P4 mapping syntax. Validate ordered overrides/re-inclusions against `p4 where` on actual files in live p4d tests. Resolve overlay file selection, not just prefix priority; retain original rule order and exclusion pattern kind. Add mixed supported/wildcard specs, client-spec failure, quoted paths, AltRoots, case-sensitive/insensitive servers and overlay collisions. AltRoots/case behavior beyond the inspected code is **unverified**, not a reproduced failure.

### P0-2 — Parse incompleteness can still pass `--fail-on-new`

**Reproduced with the native CLI:**

```text
ccn <fixture-file> --baseline HEAD --fail-on-new --report-json --no-cache
base:    int f(int x) { /* eleven if(x) x++; statements */ return x; }
current: int f(int x) { if (
=> exit 0; compared=0, skipped=0, unavailable=0, unmapped=0, changes=[]

base:    int f(int x) { if (
current: int f(int x) { /* eleven if(x) x++; statements */ return x; }
=> exit 0; compared=0, skipped=1, unavailable=0, changes=[]
```

In the first case the real fixture contained eleven literal `if` statements, CCN 12; the damaged current file is heuristic with no function details. `src/baseline.rs:287` removes it before outcome accounting. In the second case `:394-397` skips the unparseable old copy, but `src/main.rs:168` checks only unavailable files. Read/walk failures can similarly disappear (`src/loc.rs:469`, `:517`, `:527`); those filesystem-error cases were inspected, **not fault-injected** here. A partially parsed file can also fail the gate as a supposedly advisory best-effort finding: my CCN 1→12 partial fixture exited 1 with `tier=best-effort`, while health was absent.

**Acceptance:** compute the intended changed-file set independently of successful parses. Account for every selected relevant file: exact comparison, explicitly accepted exclusion, unsupported, unreadable, oversize, current/baseline parse degradation, unknown match. Exit 2 for incomplete required comparisons. Define an explicit opt-in advisory policy if best-effort results are allowed; a label alone is not a policy. Cover both directions of parse failure and mixed exact/degraded files. A repository with no relevant changes can pass; an unmeasured relevant change cannot masquerade as that case.

### P1-1 — Identity improvement is partial, and the matching algorithm is locally greedy

`src/ccn/engine.rs:525` builds scope+name+parameters. It excludes method cv/ref qualifiers and template constraints; parameter names remain (`:587`). Anonymous functions collide. `src/baseline.rs:158` immediately falls back to a name match before reserving exact matches for later current functions; `:254` tests uniqueness only among remaining baseline names. Duplicate identities are consumed in order (`:226`).

Native Git-baseline probes, all reported Exact:

| Change with no worsening of existing behavior | False result |
|---|---|
| Reorder `X::f(int x)` CCN 12 and `X::f(int x) const` CCN 11 | Both identities `X::f(int x)`; **11→12 worsened**, exit 1. |
| Swap two same-parameter local lambdas, CCN 12 and 11 | Both `(anonymous)(int x)`; **11→12 worsened**. |
| Rename `x`→`y` consistently in both `f(int x)` and `f(double x)` bodies/signatures | Both unchanged overloads become **Added**, CCN 12/11. |
| Baseline only `f(int x)` CCN 12; insert simple `f(double x)` before unchanged `f(int x)` | New simple overload consumes the baseline via name fallback; old complex function is falsely **Added**. |

Reproduce by generating flat bodies with eleven/ten repetitions of `if(x) x++;`, committing the baseline in a temp Git repo, making only the listed edit, then using the command in P0-2. Ordinary distinct-type overload reorder is fixed; this does not invalidate that narrow claim. Templates/generated conditional variants were inspected conceptually but **not independently reproduced**.

**Acceptance:** reserve exact matches across the whole file before fallback; require unambiguous correspondence on both sides and compatible scope. Include C++ cv/ref qualifiers and relevant template context, or mark ambiguity explicitly. Parameter-name-only edits must not fabricate additions. Do not invent stable lambda identity from line number; use defensible context/body evidence or report unknown. Add the four cases above plus true body growth, constrained overloads, generated duplicates and file-renaming controls. Ambiguity should be reviewable/incomplete under enforcement, not silently guessed.

### P1-2 — Full SARIF still hides best-effort confidence

`src/sarif.rs:125-165` labels **delta** findings and exports their identities. `:82-116` full findings do neither. Reproduction: CCN-12 complete `f`, followed by `int truncated(void) {`, scanned with `--sarif - --no-cache --no-report`. Its JSON scan is BestEffort; SARIF says only “Function f has cyclomatic complexity 12”, with no confidence or identity property. Thus “SARIF marks best-effort” is true only for baseline mode.

**Acceptance:** identical confidence/identity semantics in full and delta findings; run-level incomplete diagnostics for omitted files. Correct length/nesting-only delta messages (`:138` still says CCN increased), repository-relative escaped URIs for subroot scans (`:260`) and stable fingerprints before adding review integrations. URI/consumer failures were **not newly reproduced** here.

### P1-3 — A reproducible measurement contract and economical P4 gate remain open

`src/metrics.rs:365-407` has no full source/tool/rules/config/completeness manifest. A report root is not source identity; snapshots' version checks do not establish equal scan scope. `src/loc.rs:272-284` walks everything before `--since` filtering, and `:254` disables the file cache in since mode. Single-file mode already exists (`:221`); do not fund it as a new feature. P4 still probes then prints per measured file (`src/p4.rs:261`, `:276`), so 20k comparable existing files require about **40k file-read processes**, excluding setup/history, with CPU-pool concurrency rather than a negotiated server budget.

**Acceptance:** minimal versioned manifest: tool/rules identity, submitted baseline, candidate file hashes and dirty/opened state, selected files/exclusions, mapping resolution status, per-file outcome and matching reason. Budget/count server calls, concurrency, bytes and timeouts. Compare a changed-file gate against full-scan relevant results. Validate batching without mixing file bodies or losing per-file errors. P4 supports multi-file printing and argument-file batches, but that does not automatically solve result framing: [p4 print](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/p4_print.html), [global options](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/global.options.html).

### P1-4 — Action release pinning is conditional; artifact verification is still incomplete

`action.yml:94-101` honors `version` for a successful prebuilt download. On download failure, `:135` builds **the action checkout**, irrespective of that requested version. Pin action checkout A and request release B: if download B is unavailable, execution silently changes to A. Prebuilt acceptance checks a nonempty download (`:110`), not SHA256SUMS/version/architecture. This is a **source-confirmed fallback contract defect; a hosted download-failure run was not executed**.

**Acceptance:** fail on unavailable explicitly pinned artifacts, or fetch/build exactly that release's immutable source and verify identity. Verify downloaded digests and executable version/target before use. An intentionally missing release must never run an unrelated source version. Fix the macOS asset/architecture contract: workflow builds host-default on `macos-latest` under an x64 filename (`release.yml:106`, `:120`), npm assigns it to darwin-arm64 (`npm/prepare.js:18`), and action selects it for all Darwin hosts (`action.yml:77`). Actual published architecture is **unverified** here. Checksums are useful integrity metadata; they are not signing/provenance. Manual uploads, repository protections and historic workflow execution cannot be certified from this YAML.

### P2 — Claims and inherited gaps to close cheaply

* **De-emphasize health before recalibrating it.** `src/metrics.rs:238` correctly excludes BestEffort, but `:412` still sets `HEALTH_EXPERIMENTAL=false`. The 11-repository calibration predates that denominator change. Physical function span (`engine.rs:375`) still supplies health weight (`metrics.rs:245`), so comments/layout/nesting overlap can change the score. README's “share of lines ... log-scaled” and “one ... file cannot decide the grade” (`README.md:82-84`) are too strong. Call it an advisory weighted complexity index, expose coverage/span weighting, and remove the validated-health implication. Re-running eleven repos cannot establish PT runtime or defect prediction. No new labeled EDA calibration was performed.
* **Correct stale operating instructions.** README still says name-order matching (`:182`) and P4 `@=CL` (`:188`); source now does identity-first and `@CL`. HTML still labels P4's bounded CL history “last 365 days” (`src/report/html.rs:180`, `:225`). Acceptance: examples match emitted fields/selectors, history states actual window and missing data, all output modes disclose measured scope. No current README promise of PT runtime improvement was found; this report rejects inference from CCN, not an invented README claim.
* **Keep persistence out of a concurrent shared pilot until fixed.** Fixed temp names + destination removal remain in `src/cache.rs:193-204` and `src/history.rs:184-187`; history read/append/write is unlocked (`:233`). Concurrent-loss/interruption behavior is **not newly reproduced**. Acceptance from the first review still applies: unique temp files, safe replacement, protected history updates and interruption/concurrency checks. Per-job private cache/history is a bounded pilot workaround, not a shared-mode fix.
* **Do not relabel the NTFS benchmark as target-environment evidence.** [Scale report](../benchmarks/2026-10-21-scale.md) lines 18–22 record 20k/1.41M LOC at 0.625s cache-cold / 0.272s warm, after OS prewarm. Its identity is 0.7.6 `730e235` plus disclosed dirty reporting/P4 files (lines 56–72), not a clean 0.7.7 certification. It explicitly lacks NFS/LSF/P4 and deletes raw results. Preserve a machine-readable result artifact next time; rerun a nominated customer workload before performance commitments. Linear 1k–20k local scaling does not prove 100k network scaling.

## 4. ROI: explicit assumptions and arithmetic

**ROI = (V × E × A) / (C × R × K).** All factors are 1–5 ordinal judgments, not measured revenue or probabilities. V=commercial value if successful; E=current evidence that the slice addresses a real need; A=adoption leverage in the chosen P4/source-owning segment. C=engineering effort (1 ≤2 days, 2=3–5, 3=6–10, 4=11–20, 5=>20); R=regression exposure (1 isolated process/docs, 5 core-wide); K=claim/support risk (1 directly verifiable workflow, 5 unsupported runtime/causal promise). Higher numerator and lower denominator are better. Estimates assume one engineer, accessible partner and existing binary; partner waiting/procurement time is additional.

Defect evidence makes gate work technically well supported; it does **not** establish willingness to pay. The pilot scores E=2 for that reason. Scores should be replaced after partner interviews. A small numerical gap is not a mandate to reorder dependencies.

| Rank | Small candidate, not an unlimited program | V | E | A | C | R | K | Arithmetic = ROI |
|---:|---|---:|---:|---:|---:|---:|---:|---|
| **1** | Paid design-partner offer + manually assembled one-CL dossier + bounded support terms | 5 | 2 | 5 | 2 | 1 | 1 | 50 / 2 = **25.00** |
| **2** | Trustworthy changed-file contract: P0s, exact-match reservation, ambiguity, minimal manifest | 5 | 5 | 5 | 3 | 2 | 1 | 125 / 6 = **20.83** |
| **3** | P4 baseline batching / reusable baseline measurements, with bounded server budget | 5 | 4 | 5 | 3 | 2 | 1 | 100 / 6 = **16.67** |
| **4** | De-emphasize health; honest coverage/window/selector/confidence labels | 3 | 5 | 1 | 1 | 1 | 1 | 15 / 1 = **15.00** |
| **5** | Function delta UX: one module map, identities/reasons, portable review attachment | 4 | 3 | 4 | 2 | 2 | 1 | 48 / 4 = **12.00** |
| 6 | True VCS-list-driven `--since` delta path; reuse existing single-file mode | 4 | 4 | 5 | 4 | 2 | 1 | 80 / 8 = **10.00** |
| 7 | Safe concurrent persistence + scoped/redacted export/private storage | 3 | 4 | 3 | 2 | 2 | 1 | 36 / 4 = **9.00** |
| 8= | Minimum enterprise install: offline bundle, notices/SBOM, verified version/architecture, proxy guide | 4 | 3 | 3 | 3 | 2 | 1 | 36 / 6 = **6.00** |
| 8= | One demanded Jenkins/GitLab/P4-trigger adapter around CLI exit/artifact contract | 3 | 2 | 4 | 2 | 2 | 1 | 24 / 4 = **6.00** |
| 10= | NFS-aware parallel walk + 100k-file nominated-host measurement | 4 | 2 | 4 | 4 | 3 | 1 | 32 / 12 = **2.67** |
| 10= | Broader C/C++ macro/template depth beyond bounded correctness repair | 4 | 4 | 4 | 4 | 3 | 2 | 64 / 24 = **2.67** |
| 12 | Full RPM/deb/signing/air-gap/proxy certification matrix | 3 | 2 | 2 | 4 | 2 | 1 | 12 / 8 = **1.50** |
| 13 | Recalibrate/replace health as a predictive score | 2 | 2 | 2 | 3 | 2 | 3 | 8 / 18 = **0.44** |
| 14 | Expand language breadth | 2 | 2 | 2 | 4 | 3 | 2 | 8 / 24 = **0.33** |
| 15 | General runtime/profile importer + PT module dossier / runtime correlation | 5 | 1 | 3 | 5 | 3 | 5 | 15 / 75 = **0.20** |

The low profile score is for the **general claim-bearing program**, not importing a known CSV. If a paying partner supplies usable profiles, build IDs and a reproducible workload, a bounded importer might score 5×4×5/(2×2×2)=**12.50** and earn promotion. Likewise, if NFS walking consumes most real wall time, its E and A rise. Measure first; do not automatically parallelize based on a hypothetical 1ms stat. Health de-emphasis has a high cheap-fix score but little adoption leverage; ship it immediately as part of the pilot, not as a product strategy. The top two exchange rank if the commercial evidence/effort estimates move one point: fund them as one two-week experiment.

## 5. Top five: smallest slices, buyer and falsification

| Item | Smallest shippable slice | Proof with one partner/module/CL | What makes it worthless | Buyer and displaced work |
|---|---|---|---|---|
| **1. Buyer-backed pilot** | **3–5 engineer-days:** written offer, supported environment/limits, baseline+candidate manifest, manually assembled HTML/JSON delta attachment, one observed review, named support owner and response window. Test one fixed-fee scoped pilot quote; no billing system. | Reviewer reproduces one actual CL dossier, acts on one finding, compares task time against existing method; budget holder accepts a paid pilot or records a concrete purchase barrier. | Nobody changes a decision; only the author likes the chart; code/profile access is impossible; buyer only wants free LOC counts. | EDA software engineering/CAD infrastructure manager who controls review/tooling budget; replaces hand-run scripts, manual function comparisons and report preparation. This buyer hypothesis needs confirmation. |
| **2. Trusted change contract** | **6–10 days:** count all intended files, fail unknown required comparisons, reject unsupported P4 mappings, reserve exact matches globally, expose unresolved identities and minimal manifest. Full view grammar/semantic C++ analysis is unnecessary. | One real CL plus seeded malformed-current, malformed-base, wildcard/reinclude, reorder and true-growth controls: no silent omissions, no fabricated seeded regressions, real growth detected. Second engineer obtains the same outcomes. | “Complete” still means only successfully parsed files; ambiguity is guessed; the partner's ordinary view is rejected so often the workflow is unusable. | Review/quality or tool-development lead; replaces manual checks that the scanner read the right baseline/functions. Prevents the pilot from measuring misleading reports. |
| **3. Economical P4 baseline** | **6–10 days:** batch supported metadata/content requests with explicit file framing, per-file outcomes, bounded parallelism/deadline; reuse immutable depot-revision measurements keyed by parser/rules. Start with the nominated module and CL. | Same results as trusted per-file reference, injected missing/auth/truncated responses remain distinct, cold and repeat runs record commands/bytes/server time. Batch process count tracks batches rather than files; p95 meets a pre-agreed real-host budget. | Parser of multi-file output silently merges bodies; batching floods p4d; repeat runs still spawn per unchanged file; network/permissions make cache reuse unsafe. | P4/CAD infrastructure owner plus development manager; replaces long gate waits and repeated baseline retrieval. No claim that local 0.27s predicts this latency. |
| **4. Honest metric/report contract** | **1–2 days:** label index advisory, expose excluded/unmeasured scope, correct P4 history/selector docs, add confidence/identity to full SARIF. Defer new grade bands. | Partner can explain what was measured and refuses a simulated incomplete report; same input has consistent confidence across JSON/HTML/SARIF. No runtime/defect inference from a grade. | Still sells the grade as a validated release decision; extra labels obscure rather than clarify missing coverage. | Same buyer as pilot; removes reconciliation/explanation work and prevents support disputes. It is a prerequisite bundled with paid workflow value, not a standalone purchase. |
| **5. Function delta attachment** | **3–5 days:** one declarative path→module map, qualified identity + before/after/reason/confidence, top actionable functions, source locations and portable attachment. Use existing JSON/HTML/SARIF; manual review attachment first. | Owner of the one module/CL starts review from the attachment, reaches the right source, and resolves a finding without another scanner UI. Compare against their current attachment. | Mostly top-20 backlog, wrong links/symbols, ambiguity hidden, or an automated comment stream nobody reads. | Module tech lead/engineering manager; replaces ad hoc diff summaries and review-note assembly. No new enterprise dashboard. |

## 6. Two-week / 30 / 60 / 90-day plan

These are conditional increments, not authorization to implement everything. Commercial gates can stop the program earlier.

| Horizon | Work and smallest deliverable | Decision / acceptance |
|---|---|---|
| **Next two weeks** | Day 1–2: choose source-owning segment, recruit one named reviewer and budget holder, obtain one module/CL and current workflow timing. Days 2–10: bounded P0/identity/manifest repairs and honest labels; manually assemble one dossier. Days 10–14: second-engineer reproduction, observed review, price/continuation discussion. Keep enforcement off until contract tests pass. | One genuinely used dossier, all relevant files accounted for, seeded controls pass, named buyer makes a paid-pilot decision. A friendly compliment is not evidence. If partner access is delayed, fix the reproduced defects but do not start a speculative importer. |
| **By day 30** | Run 10–20 adjudicated real changes with this partner; ship minimal module delta attachment. Measure P4 request cost and NFS phase time on its host. Implement the 6–10-day batching slice if retrieval dominates; private per-job persistence until safe sharing is done. | Record comparison errors, median/p95 workflow time and support minutes. Require zero false seeded identity deltas and zero silent seeded omissions; target ≤5% disputed real findings, without claiming statistical certification from a tiny sample. One accepted paid continuation / written procurement path. |
| **By day 60** | Productize only the observed bottleneck: VCS-list delta traversal if full walks dominate; NFS parallel walk only after measurement; demand-led install bundle and **one** CI adapter. Safe persistence, exact artifact/version binding, notices/SBOM and redaction for the supported path. | On nominated AlmaLinux/NFS/P4 host, repeat cold/warm/content-verified/1%-changed runs. Agree targets before the run; starting proposals: 20k/200-changed review p95 ≤30s, 100k warm inventory ≤10s, cold ≤60s, RSS ≤2GiB. These are acceptance proposals, not existing guarantees; include OS-cold costs and call counts. |
| **By day 90** | Reproduce deployment with another engineer; offer a narrowly supported package/support agreement. Only if the partner can supply relevant profiles: a **3–5-day known-format import spike**, one module, explicit build/source joins and unmatched rows, followed by a separately budgeted implementation decision. | Sell review productivity on observed evidence. Runtime positioning requires paired repeated runs above noise with identical host/workload/thread/license conditions and preserved timing/functional outputs. Separate queue time from job runtime. If there is no paid demand, stop commercial expansion. |

Do not fill days 51–70 with a runtime importer just because the first review placed it there. The input format, source ownership and user decision must determine that work. One useful delta workflow can justify a product without a runtime story; failure to sell it is information.

## 7. Evidence plan for the top two-week item

1. **Partner contract:** one source-owning EDA development/CAD team, one module owner, one reviewer and one budget holder. Record why existing scripts/lizard/Sonar or manual review are insufficient. Confirm approved offline deployment, source visibility and the specific budget being considered. No partner is currently verified by this checkout.
2. **Freeze scope before results:** one submitted baseline CL and one candidate CL/opened snapshot, actual P4 view, candidate hashes, compiler/build context if relevant, tool/rules/version, excludes and expected changed-file list. Never quietly substitute a simpler corpus. Tcl-only input is a segment mismatch for today's function gate; record it rather than calling heuristic output coverage.
3. **Establish the reference:** owner independently lists intended files and expected changes; record time spent with their current process. Include unchanged, ordinary overload reorder, const/lambda ambiguity, parameter rename, real complexity growth, bad current/base parse, wildcard/reinclude, auth failure and out-of-view controls. The first CL need not contain every case: inject controls in a separate replayable fixture.
4. **Execute under real constraints:** approved AlmaLinux binary, NFS mount, P4 server, allocated CPUs; content-verified caching and a private cache/history location. Capture wall/CPU/RSS, file outcomes, request count/concurrency/bytes and actual history window. Record LSF queue and execution independently if used. No source upload is required.
5. **Make review evidence concrete:** deliver the small attachment; observe whether it changes review order, identifies a real issue, or avoids a manual comparison. Get a second engineer to reproduce it from the manifest. A reduced CCN is not a runtime win, and a detected complex function is not automatically a defect.
6. **Pre-agreed go/no-go:** first slice requires one reproducible, useful real-CL result and no silent seeded omissions. Over the following 10–20 CLs, target ≥30% median reduction in the specific review-preparation task, or ≥1 hour/week saved for the team, with no increase in adjudicated comparison errors. These are proposed customer goals, not measured benefits. Log support time so apparent savings do not merely move work to the vendor.
7. **Ask for money against displaced work:** one scoped fixed-fee pilot quote and one continuation offer with supported versions, artifact delivery, response hours and exclusions. Compare fee with the team's measured labor/waiting cost. Do not invent a market price or promise 24/7 SLA. Record acceptance, explicit price objection or absent budget; “interested” alone fails the commercial test.

## 8. Positioning, kill list, and when not to sell

**Retain the scanner as the delivery mechanism; reposition the paid wedge around reproducible offline P4 change-review evidence.** LOC/CCN inventory + baseline + HTML/SARIF is a useful entry point, but those outputs alone are not a durable paid distinction. Lizard already documents function CCN, thresholds, automation and C/C++ support; Sonar documents complexity metrics. This is capability evidence, not a new performance or price comparison: [Lizard](https://github.com/terryyin/lizard), [Sonar metrics](https://docs.sonarsource.com/sonarqube-server/user-guide/code-metrics/metrics-definition). The opportunity is a trusted, low-friction workflow in the buyer's constrained environment, proven against what it currently uses.

Do **not** build in the next 90 days without a demonstrated purchase blocker:

* A general profiler, CCN→runtime predictor, automatic refactoring/optimization recommendations, or universal defect/health score.
* A general Tcl interpreter or compiler-complete C++ semantic analyzer. Deepen the exact source patterns demanded by the chosen partner; expose unsupported cases.
* Ten more languages, 3D code cities, a chart redesign, SaaS accounts/database, telemetry, collaboration dashboard or fleet analytics.
* A broad RPM/deb/OS/CPU/proxy/signing certification program. Deliver the one supported offline artifact correctly first; signing/SBOM can become mandatory if the partner requires them.
* Three CI integrations at once, automated PR/CL commenting before a manually attached report is useful, P4 cross-stream history mining, or direct LSF scheduling/control.
* More synthetic speed marketing, a 100k-file parallel walker without target-host evidence, or re-tuning grade bands as a substitute for product validation.

**Recommend against commercializing at all** if the available buyers cannot access relevant source, do not have an actionable review decision, or find their current free scripts sufficient; if two budget-holder conversations plus a completed pilot yield no credible paid continuation; if useful findings require so much manual identity adjudication that support cost exceeds displaced work; if normal P4 views/EDA macro patterns cannot be supported honestly; or if selling depends on unsupported runtime savings, a health-grade authority claim or enterprise guarantees the team cannot staff. In those cases keep it an internal/open-source advisory utility. Do not manufacture revenue rationale by adding features.

## 9. Claims not independently verified from this checkout

* Live P4 server behavior for the implementer's shipped four-CL/complex-view demonstrations; permission/expired-ticket cases, streams, overlays with missing depot files, AltRoots, case rules, shelves, moves and full multi-root semantics. Mocked results establish client logic defects, not server certification.
* GitHub v0.7.7 asset existence/bytes, checksum verification against downloaded Windows release, actual workflow run status, tag protections, manual publication paths, npm registry contents/OTP publication and downloaded platform binary identity. The local tag/release implementation is inspectable; remote delivery claims were not re-tested.
* Locked MSRV 1.90 execution, native AlmaLinux/glibc smoke behavior, macOS/Intel/ARM or clean-machine offline installs. Workflow now tests release prerequisites on Ubuntu, not every target runtime; no second CI/MSRV workflow is present in `.github/workflows/` in this checkout.
* NFS/LSF/100k-file full or delta performance, real P4 baseline latency/server load, true storage-cold behavior and concurrent scanners. The scale document is transparent but its raw results and frozen binary are not committed; I did not rerun its 20k harness.
* EDA holdout calibration, customer demand, available budgets, saved engineer time, profile export/symbol availability, causal runtime correlation or an accepted optimization. No commercial success or PT runtime benefit has been established.
* A comprehensive audit of generated code/templates/all ten grammars, cache/history concurrency faults, SARIF consumer behavior or interactive TUI accessibility. Passing the current suite does not answer these questions.

## 한국어 요약 — 10줄

0.7.7은 크게 개선됐지만 강제 P4 gate와 PT 실행시간 개선을 판매할 단계는 아니다.
출력 상태·캐시·일반 overload 재정렬·헤더 파싱·릴리스 검사 개선은 확인했다.
P4 wildcard·재포함·조회 실패 fallback은 실제 대상 파일을 정상 unmapped로 누락한다.
현재 파일이나 baseline 파싱이 깨져도 비교 없이 gate가 성공하는 사례를 재현했다.
const overload·lambda 재정렬·매개변수 이름 변경·새 overload 삽입은 허위 변경을 만든다.
최우선 2주 과제는 코드 소유 팀 한 곳의 실제 모듈·CL로 유료 파일럿을 검증하는 것이다.
그 과정에서 누락 없는 비교 계약과 identity 오류를 고치고 짧은 리뷰 증거를 제공한다.
P4 batching과 실제 NFS 측정은 고객 병목에 맞춰 진행하고 health 등급은 부각하지 않는다.
프로파일 importer·다언어 확장·대시보드는 입력과 구매 이유가 확인될 때까지 미룬다.
리뷰 시간 절감과 유료 지속 의사가 없으면 기능을 늘리지 말고 내부 도구로 남긴다.
