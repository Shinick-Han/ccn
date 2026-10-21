# Design-partner pilot: one module, one CL, two weeks

Status: operating document for ROI item 1 in
[the ROI review](../reviews/codex-roi-review-2026-10-21.md) sections 5-7.
Draft 2026-10-21. Nothing here is a measured benefit, a price, or a commitment
to a partner. Replace every bracketed value after the discovery call.

This document is deliberately blunt. It exists to answer one commercial
question: **will a source-owning team pay for a reproducible, offline Perforce
change-review workflow after seeing it on one of its own CLs?** If the answer is
no, that is a valid and useful result. Do not add features to avoid it.

Related: [commercial review](../reviews/codex-commercial-review-2026-10-21.md),
[dossier template](dossier-template.md), [checklist](checklist.md),
[scale benchmark](../benchmarks/2026-10-21-scale.md).

---

## 1. Positioning

### What is sold

A **reproducible offline Perforce change-review workflow**:

- one frozen module, one submitted baseline CL, one candidate CL (or a captured
  opened tree);
- every intended changed file accounted for, with per-file outcome and reason;
- a short delta dossier that a reviewer can start from and that a second
  engineer can rebuild from the recorded manifest on the same host;
- run entirely on the partner's approved host against the partner's NFS
  checkout, with no source upload.

The scanner (ccn) is the delivery mechanism, not the product claim. LOC/CCN
inventory on its own is not a durable paid distinction; lizard, scc, Sonar and
clang-tidy already document complexity metrics and checks.

### The three claims that are NOT made

1. **Not a validated health score, and not an enforced release gate.** The
   grade/health index is a measured-source complexity index. At 0.7.8 the
   0.7.8 closes the reproduced 0.7.7 holes: an incomplete parse fails the comparison
   (exit 2), and an unsupported P4 view marks the mapping incomplete instead of
   silently dropping in-view files ([ROI review](../reviews/codex-roi-review-2026-10-21.md)
   P0-1, P0-2). The pilot output is still advisory evidence for a reviewer,
   never a pass/fail merge decision.
2. **Not a runtime optimizer, and no PrimeTime runtime improvement.** No
   CCN-to-runtime inference, no predicted speedup, no claim that a reduced
   complexity number shortens a timing job. Runtime needs paired repeated runs
   on identical host/workload/thread/license conditions; that is out of scope
   here and the runtime column of the dossier stays empty.
3. **Not a defect predictor and not a completeness guarantee.** CCN is a
   prioritization signal, not defect probability. "Complete" in this pilot means
   *the files in the frozen intended set each have a recorded outcome*, not that
   every file in an arbitrary view was correctly measured. Remaining 0.7.8
   limits (unsupported P4 wildcard makes the comparison incomplete; unbound
   lambdas with identical signatures are ambiguous) are disclosed to the partner
   in writing before the run, not discovered by them afterwards.

Anything a partner hears that resembles "trust this grade", "your PT jobs will
get faster", or "this cannot miss a regression" is a sales error and must be
corrected in the same conversation.

### Why the pilot is bounded

The review's own evidence: the scanner is useful, the gating contract is not yet
trustworthy, and no partner is verified. A paid, narrow, manually assembled
pilot tests willingness to pay while the bounded correctness repairs (review
item 2) are being made. It does not test a general product.

---

## 2. Ideal partner profile and disqualifiers

### Must have (all five)

1. **Owns the C/C++ source it reviews.** EDA tool development, CAD
   infrastructure, a solver/engine team, or an internal flow team with its own
   C++. The source is in the partner's depot, not behind a vendor binary.
2. **Uses Perforce, with one real module and a repeatable pair.** A submitted
   baseline CL and a real candidate CL, shelf, or explicitly captured opened
   tree for the same module, where the reviewer already knows roughly what
   changed. Not a synthetic demo repo.
3. **Has a named reviewer and a named budget holder.** Two different people is
   normal; one person who is both is acceptable only if that person controls the
   budget line.
4. **Can run an offline AlmaLinux binary on an NFS checkout.** Approved host,
   local execution permitted, no source upload required, and a place to write
   artifacts outside the depot.
5. **Has an actionable review decision.** The output must be able to change
   what gets reviewed, by whom, or how long it takes. "Nice chart" is not a
   decision.

### Disqualify (do not soften these)

- **Chip-design teams that cannot access the vendor's C++.** A team using a
  closed PrimeTime binary cannot apply this scanner to that vendor's internal
  C++. Tcl orchestration and job/timing logs are a different segment: Tcl is
  heuristic with no function delta today. Record the mismatch and leave; do not
  reframe heuristic output as coverage.
- **Teams that want runtime correlation before profile data exists.** The
  required inputs (usable profiles, build IDs, a reproducible paired workload)
  are unverified and absent from this offer. Selling the runtime story first
  would require inventing a claim.
- **Teams with no review decision to change.** If the answer to "what would you
  do differently on Monday" is "nothing", the pilot has no dependent variable.
- **Teams that only want free LOC counts**, a grade, or a dashboard demo.
- **Teams that cannot grant a reviewer's time** for the observed review and the
  second-engineer reproduction. Without those two events there is no evidence,
  only a report.

Record disqualification reasons in the pipeline notes. A disqualification is
cheaper than a pilot that cannot produce a decision.

---

## 3. Offer

### Shape

**Fixed-fee, two-week, single-scope pilot.** One module, one frozen
baseline/candidate pair, one dossier, one observed review, one reproduction.
Fixed fee is quoted after the discovery call (section 3.4). No open-ended
retainer, no success fee, no per-seat pricing at this stage.

Default calendar: 10 business days from scope signature, with up to 2 days of
slack for access. The engineering content is expected to fit 3-5 engineer-days
of vendor time (review section 5, item 1); the rest is partner waiting,
reviewer time and the second engineer.

### 3.1 Deliverables

| # | Deliverable | Form | Owner |
|---|---|---|---|
| D1 | Frozen scope record: module, baseline CL, candidate CL/shelf, view, excludes, expected file list | Markdown, signed off by partner | vendor |
| D2 | Per-file manifest: path, identity inputs, outcome (compared/excluded/unsupported/unreadable/degraded/unknown), exclusion reason | JSON + one-page table | vendor |
| D3 | One-CL dossier per [dossier-template.md](dossier-template.md): delta table, top findings with source locations, omissions/ambiguity, empty runtime column | Markdown + tool JSON/HTML attachment | vendor |
| D4 | Seeded control report: 12 cases from section 6, each with observed outcome and pass/fail/known-limit | Markdown | vendor |
| D5 | One observed review, 60-90 minutes, with the named reviewer; note where the dossier changed the review order or was rejected | Notes, 1 page | vendor + reviewer |
| D6 | Second-engineer reproduction from the manifest alone, on the same host; every ambiguity logged | Notes, 1 page | partner engineer |
| D7 | Go/no-go memo with the section 5 metric table, measured labor/waiting cost, and the continuation decision | Markdown, 1-2 pages | vendor + budget holder |

D3 is advisory. D4 is the honesty check on D3. D7 is the only commercial
artifact.

### 3.2 Included

- Discovery call and written scope freeze (one module, one pair).
- One approved offline AlmaLinux binary; version, target and SHA256 recorded
  before first run. Partner installs it on the approved host; no root required.
- One scan of the frozen pair, with `--verify-cache` on the gate-shaped run and
  a private per-job cache/history directory (shared concurrent
  cache/history is a known defect; do not use it during the pilot).
- Replay of the seeded control fixture on the same binary and rules version.
- Dossier assembly, including manual adjudication of ambiguous identities.
- One observed review session and one second-engineer reproduction.
- A named support owner, email/chat channel, response within **1 business day**
  for pilot-blocking questions, and a logged support budget of **[6] hours**
  across the pilot. This is a proposal, adjustable at scope time; it is not a
  24/7 SLA.
- Written go/no-go memo and one continuation conversation.

### 3.3 Excluded

- **New product features**: P4 batching, NFS parallel walk, profile importer,
  Tcl support, CI adapters, dashboards, new languages, chart redesign.
- **Runtime work**: profiling, build-ID joins, paired workload runs, runtime
  correlation, any PT runtime claim.
- **Enterprise packaging**: signing, SBOM, RPM/deb, proxy/air-gap
  certification, macOS/ARM artifacts, vulnerability attestations.
- **Enforced gating**: blocking CI, required merge checks, "zero regressions
  guaranteed", automated CL comments.
- **Arbitrary P4 view grammar**: only the frozen view plus the listed controls.
  Unsupported view constructs are reported as unsupported, not silently mapped.
- **Additional modules, additional CLs, cross-stream history mining.** Extra
  pairs are quoted on the same measured basis, not bundled free.
- **Source upload, remote access to the partner's tree, or running on partner
  data outside their host.**
- **Security/legal review, procurement paperwork beyond one written quote,
  or on-call incident response.**
- **Any claim of statistical validation** from a single module or a handful of
  CLs.

### 3.4 Pricing conversation worksheet (no price is set here)

Do not quote a number before the discovery call. The price is derived from the
partner's measured displaced work and the vendor's measured delivery cost, then
set in writing. Fill every cell; a blank cell means the number is not ready.

**A. Vendor delivery cost (pre-pilot estimate for the fee; measured values at day 9 inform the continuation price, not the pilot fee)**

| Line | Basis | Quantity | Unit | Notes |
|---|---|---|---|---|
| Discovery + scoping | call + written scope | [ ] | hours | |
| Environment / offline install / view check | on partner host | [ ] | hours | |
| Scope freeze + expected-file reconciliation | includes partner waiting | [ ] | hours | |
| Scan runs + control replay | 1 pair + 12 controls | [ ] | hours | |
| Dossier assembly + identity adjudication | per changed file | [ ] | hours | |
| Observed review | 60-90 min + notes | [ ] | hours | |
| Second-engineer reproduction support | | [ ] | hours | |
| Logged support | capped at [6] h | [ ] | hours | |
| **Total vendor hours** | sum | [ ] | hours | delivery floor |
| Internal loaded cost/hour | internal figure | [ ] | currency | |
| **Delivery cost floor** | hours x rate | [ ] | currency | |

**B. Partner displaced work (measured, not assumed)**

| Line | Basis | Value | Unit |
|---|---|---|---|
| Reviewers on this module | named | [ ] | people |
| Current review-prep time per CL | stopwatch, their process | [ ] | minutes |
| CLs per week for this module | owner estimate, labelled estimate | [ ] | CL/week |
| Loaded reviewer cost/hour | partner-provided or banded | [ ] | currency |
| Gate/wait time per CL (queue + retrieval) | recorded separately from review | [ ] | minutes |
| **Annual displaced hours (hypothesis)** | prep x CLs x 46 weeks | [ ] | hours |
| **Annual displaced value (hypothesis)** | hours x loaded rate | [ ] | currency |

**C. Decision rule for the fee**

- Pilot fee must cover the delivery cost floor plus a margin the vendor can
  defend; it must sit clearly below the *measured* annual displaced value, or
  the pilot is not commercially coherent.
- If the measured savings are under 1 hour/week for the team, do not price a
  continuation; offer a narrow second pilot or stop.
- If the partner cannot name a budget line, record the exact blocker (no line,
  wrong fiscal year, procurement threshold, security review). "Interested"
  is not a price signal.
- Any discount is a scope reduction, not a free feature.

The review's arithmetic is a hypothesis, not a market price: one engineer,
accessible partner, existing binary. Partner waiting and procurement time are
additional and are not billable.

### 3.5 Support window and data handling

**Support window.** Named support owner, one channel, response within 1 business
day for pilot-blocking issues, [6] logged hours total, no weekend or 24/7
coverage. Support minutes are recorded per event; they are subtracted from the
claimed savings in the go/no-go memo.

**Data handling.**

- No source upload. The binary runs locally on the partner's approved host
  against the partner's NFS checkout.
- The only network traffic is the partner's own `p4` client talking to the
  partner's Perforce server, plus Git if the module is also a Git repo. No
  telemetry.
- Cache, history, manifest and dossier are written to a partner-chosen directory
  outside the depot, with a **private per-job cache/history path**. Shared
  concurrent persistence is a known defect and is not used.
- Cache and history contain paths, function names and absolute roots in
  plaintext. Treat them as source-adjacent material.
- Artifacts stay local unless the partner explicitly shares them. If shared by
  email or a ticket, send only the dossier, manifest and control report; never
  the cache, the raw scan JSON, or absolute-path logs, unless requested.
- Redaction on request: absolute root, author names, depot paths.
- Delete all pilot artifacts on request at close-out; confirm in writing.

### 3.6 Continuation decision

At day 10, the budget holder makes one of three recorded choices:
**continue** (paid, next scope), **narrow** (smaller paid scope, e.g. one
module per month, or one module plus a bounded correctness repair), or **stop**.
Record: name, role, date, choice, amount if any, and the evidence used. A verbal
"let's keep talking" is recorded as *stop with an open question*, not as
continue.

---

## 4. Two-week plan

Business days. Two days of slack are assumed, not promised. If access slips more
than 3 business days, stop or re-scope; do not start speculative work.

| Day | Work | Output / gate |
|---|---|---|
| **-3 to 0** | One-page scope + mutual NDA if required; name reviewer and budget holder; confirm approved host and offline-install path | Signed scope or no start |
| **1** | Discovery call (90 min): module, CL pair, view, host, current review process, how its time is measured, security constraints, budget line, why existing scripts/lizard/manual review are insufficient | Filled positioning + worksheet sections A/B; partner profile confirmed |
| **2** | Environment: transfer binary, record version/target/SHA256, install offline, confirm NFS mount, P4 auth/ticket, allocated CPUs, private cache dir; resolve the view and record `p4 client -o` and `p4 where` for 3 known files | Binary identity + view record; `ccn --version` output |
| **3** | Freeze: baseline CL (submitted), candidate CL/shelf/opened tree, candidate file hashes, excludes, tool/rules version. Partner owner writes the intended changed-file list **before** seeing tool output | D1 frozen scope, signed |
| **4** | First run: cold scan of the pair, `--verify-cache`, capture manifest, per-file outcomes, wall/CPU/RSS, request count if observable. Reconcile intended list vs tool-selected list | Every intended file has an outcome; mismatches listed |
| **5** | Seeded control replay (section 6) on a separate fixture, same binary/rules | D4 control report; any known-limit case escalated before the dossier is trusted |
| **6** | Dossier assembly: delta table, identity + confidence + matching reason, top findings with locations, omissions/ambiguity section, runtime column left empty | D3 draft |
| **7** | Observed review (60-90 min): reviewer works from the dossier only; note first action, accepted/rejected findings, time to decision, and what they would have done otherwise | D5 notes |
| **8** | Second engineer reproduces D3 from the manifest alone on the same host; logs every ambiguity, missing command, or undocumented step | D6 notes; reproduction pass/fail |
| **9** | Consolidation: adjudicate disputed findings, compute the section 5 metrics, compare measured displaced work to support minutes, draft go/no-go memo | D7 draft |
| **10** | Go/no-go meeting with reviewer + budget holder: metrics, limits, price conversation, explicit continue/narrow/stop | Signed decision, or recorded refusal |

Rules that override the schedule:

- Never substitute a simpler corpus because the real pair is delayed.
- Never publish a dossier whose control report has an unexplained failure.
- Never let the dossier's presence imply enforcement. It is advisory.
- Engineering repairs (review item 2: P0-1, P0-2, identity reservation, minimal
  manifest) run in parallel only where they unblock the frozen pair; they are
  not a reason to expand pilot scope.

---

## 5. Success and failure metrics

All five are **proposals**, pre-agreed in D1, measured on this pilot only. They
are not claims about the product, and one module cannot certify them
statistically.

| # | Metric | Threshold (proposal) | Measurement | Failure reading |
|---|---|---|---|---|
| M1 | Silent omissions on seeded controls | **zero** | Each of the 12 cases in section 6 has an explicit outcome; a missing file, a silently skipped file, or a success exit for an unmeasured required file counts as a silent omission | Any silent omission => pilot cannot claim a trustworthy workflow; dossier is advisory-only |
| M2 | Fabricated seeded identity deltas | **zero** | Control cases with no real behavior change must produce no `added`/`worsened` identity delta | Any fabricated delta => identity work is incomplete; findings must carry a manual-adjudication warning |
| M3 | Review-preparation time | **>=30% median reduction** in the specific timed task, **or >=1 hour/week saved** for the team | Stopwatch the same task before (their process) and during (dossier-first), at least 3 timed repetitions or 3 CLs; report median and range, not a single run | Below threshold => no demonstrated displacement; price continuation from the residual manual work only |
| M4 | Adjudicated comparison errors | **no increase** vs the partner's current method | Reviewer adjudicates disputed findings; count errors per CL for both methods | Increase => the workflow moves work rather than removing it |
| M5 | Commercial continuation | **one paid continuation or a written procurement path** | Signed decision in D7 | Neither => stop commercial expansion for this segment |

Additional measurements (recorded, not gating): wall/CPU/RSS per run, P4
request count and history window actually used, files compared / excluded /
unsupported / unreadable / degraded / unknown, support minutes, reviewer's
first action, number of findings opened in the editor.

Do not report a percentage time saving from one stopwatch run. Do not net out
support minutes unless they are logged. Do not convert a CCN reduction into a
runtime or defect claim anywhere in the memo.

---

## 6. Seeded control set

The reviewer's exact cases, replayed on a fixture separate from the real module
(review section 7, item 3). Each case records: expected outcome, observed
outcome, evidence path, pass/fail. The 0.7.8 statuses below were reproduced
locally; the pilot must still replay every case on its own fixture because a
partner's view, macros and compiler configuration are different inputs.

| # | Case | Fixture | Expected under the pilot contract | 0.7.8 status |
|---:|---|---|---|---|
| 1 | Unchanged | identical baseline and current | no delta, exit 0, file compared | passes |
| 2 | Ordinary overload reorder | `f(int)` / `f(double)` swapped, distinct parameter types | no delta, correct identity per function | passes |
| 3 | Const overload reorder | `X::f(int)` vs `X::f(int) const`, bodies swapped | no delta; distinct identities including cv qualifier | passes (cv qualifier in identity) |
| 4 | Lambda reorder | two same-parameter local lambdas swapped | no delta, or explicit unknown identity; never a fabricated worsening | passes (binding-scoped `lambda:a` / `lambda:b`) |
| 5 | Parameter rename | `x`->`y` consistently in signatures and bodies of two overloads | no delta; rename alone is not an addition | passes (parameter names not in identity) |
| 6 | New overload insertion | insert simple `f(double)`, leave existing complex `f(int)` untouched | new function reported as added; old function unchanged | passes (exact matches reserved first) |
| 7 | True growth | body of one function genuinely grows, no other edit | exactly one worsened finding, correct identity and CCN before/after | passes (must keep passing) |
| 8 | Broken current | current file truncated mid-function, baseline intact | incomplete comparison, non-zero exit, file named; never compared=0 + exit 0 | passes: exit 2, `files_unmeasured=1` |
| 9 | Broken baseline | baseline copy unparseable, current intact | incomplete comparison, non-zero exit, file named; never skipped silently | passes: exit 2, `files_skipped=1` |
| 10 | Wildcard / re-include view | view with `...` plus an exclusion and a later re-include of the same path | in-view file compared; unsupported construct reported as unsupported, not `Unmapped` | passes: re-inclusion compared; wildcard mapping is incomplete (exit 2) |
| 11 | Out-of-view file | file genuinely outside the client view | explicit out-of-view outcome, excluded with reason, not compared, not "added" | passes when the view is complete; incomplete view -> exit 2 |
| 12 | Auth failure | expired ticket / denied read on a required file | operational failure, non-zero exit, file and cause named; never an empty baseline | passes by contract (`files_unavailable` -> exit 2); expired-ticket path to re-verify live |

Rules for this table:

- Cases 1, 2, 7, 11 must pass before any dossier is delivered as more than
  advisory.
- Cases 3-6, 8-10, 12 are the disclosure list. If unfixed at day 5, the partner
  is told in writing that these are limits of the current build, and the
  go/no-go memo cannot claim M1/M2.
- A control may be reclassified from fail to pass only with a reproducible
  before/after on the same fixture and binary identity.
- Do not add controls to the pass set by weakening the expectation (for example
  calling a `best-effort` finding acceptable).

Controls live in a versioned fixture outside the partner depot; record its hash
in the manifest appendix. The real CL does not need to contain every case.

---

## 7. Risks and stop rules

| Risk | Signal | Stop rule |
|---|---|---|
| No real source access (vendor binary, export control, org boundary) | Cannot see the C++ for the nominated module | Stop at discovery. Do not pivot to Tcl heuristics and call it coverage. |
| No usable profile / no runtime input | Runtime column would be empty and the buyer only wants timing | Stop the runtime pitch. Continue only if review workflow alone has a budget holder. |
| Support cost exceeds displaced work | Logged support minutes approach or exceed measured savings | Stop; the workflow is not replacing work, it is moving it to the vendor. |
| Normal views unsupported | The partner's ordinary view uses wildcards/overlays/re-includes that 0.7.8 cannot fully resolve | Stop the enforced framing. Pilot may continue read-only/advisory only if the partner explicitly accepts the limitation in writing. |
| Findings need too much adjudication | Reviewer cannot decide identity without source archaeology; >[30]% of findings disputed | Stop or narrow to the one module subset that adjudicates cleanly. Do not hide ambiguity to keep the demo smooth. |
| No reviewer time | Observed review or reproduction keeps slipping | Stop. Without those two events there is no evidence, and a report is not a pilot. |
| No budget holder / no budget line | "Interested", but no named person or line item | Stop at discovery; record the blocker. |
| Access delay > 3 business days | Environment not ready after day 2 | Re-scope the calendar once, then stop. |
| Data-handling objection | Source-adjacent artifacts cannot leave the host | Acceptable: run locally, deliver only the dossier. If even that cannot be shared, stop. |
| Enforcement pressure | Partner asks to block merges on the dossier | Refuse the claim, in writing. Offer the advisory workflow only. |

Hard stops: no source access, no actionable review decision, a request to
represent heuristic Tcl or a health grade as validated coverage, or a demand for
a PT runtime guarantee. In each case, say no and record why.

---

## 8. Post-pilot decision table

Owner abbreviations: V = vendor pilot lead, R = named reviewer, B = budget
holder, S = second engineer. R owns D5 (observed review), S owns D6
(reproduction); neither can be substituted by the vendor.

| Outcome | Condition (evidence) | Decision | Owner | Required evidence |
|---|---|---|---|---|
| **Continue** | M1 and M2 pass; M3 met; M4 no increase; M5 paid continuation or written procurement path | Quote the next scope (same module family, or one module + bounded P4/identity repair). Fixed fee, same terms. | B (signs), V (scopes) | D7 signed; metric table; adjudicated error log |
| **Narrow** | Real value seen but one dimension fails: e.g. view approximation blocks the module, or adjudication cost is high | Reduce to one module, one CL type, or advisory-only dossier review; or fund the correctness repair first, then re-pilot. Price against the narrowed displaced work. | B + V | D4 known-limit list; D6 ambiguity log; measured support minutes |
| **Stop** | M1 or M2 fails and cannot be bounded; M3 below threshold; M4 increases; no budget line; no reviewer time | End the pilot, deliver the advisory dossier as-is, keep the tool internal/open-source. Write down the segment mismatch. | V, acknowledged by B | D7 with the failing metric row named |

Decision hygiene:

- Every outcome needs a name, role, date and the specific evidence row, not a
  sentiment.
- "They liked it" maps to **stop**, unless a metric row and a budget holder are
  attached.
- A stop after two budget-holder conversations plus one completed pilot is the
  review's stated condition for not commercializing further. Do not respond by
  adding features.

---

## Appendix: assumptions that are not verified

Recorded so nobody later mistakes them for facts:

- No partner, budget, module, CL pair, or data-handling approval exists as of
  this draft. Everything in sections 3 and 4 is an offer design, not an
  agreement.
- No live P4 server, NFS mount, LSF allocation or AlmaLinux host was used to
  write this document. P4 behavior described here comes from the ROI review's
  mocked public-API reproductions plus the vendor's own four-CL local `p4d` run.
- The 20k-file timings in the scale benchmark are Windows/NTFS with a warm OS
  file cache. They do not predict NFS, cold-cache, LSF, or P4-baseline latency,
  and must not be quoted to a partner as target-host performance.
- The support window, support-hour cap, threshold values in square brackets, and
  the 10-business-day shape are proposals to negotiate, not promises.
- Pricing: no price, rate, or market comparison is asserted here. The fee is set
  after the discovery call from measured labor and waiting cost.
