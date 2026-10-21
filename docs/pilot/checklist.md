# Pilot checklist: one module, one CL

One page. Tick only what is actually done. A blank box at a gate means stop, not
"probably fine". Companion to
[the pilot plan](design-partner-pilot-2026-10.md).

## Before the call

- [ ] Partner owns the C/C++ source under review (not a vendor binary)
- [ ] Perforce in use, with one named module and a real baseline/candidate pair
- [ ] Named reviewer confirmed, with time for one 60-90 min session
- [ ] Named budget holder confirmed, with the budget line identified
- [ ] Approved offline run path: AlmaLinux host, NFS checkout, local execution
- [ ] No disqualifier: no vendor-only C++, no "runtime first", no missing review decision
- [ ] Existing process identified (scripts / lizard / Sonar / manual) and why it is insufficient
- [ ] Security constraints recorded (NDA, export control, artifact sharing limits)
- [ ] Scope shape agreed: fixed fee, 2 weeks, one pair, advisory only

## Before the run

- [ ] Binary version recorded (`ccn --version`)
- [ ] Binary SHA256 recorded and re-checked on the host
- [ ] Binary target/arch matches the host; offline install succeeded
- [ ] Private per-job cache directory chosen outside the depot
- [ ] Private per-job history path chosen (no shared concurrent use)
- [ ] Baseline CL is submitted and frozen; number recorded
- [ ] Candidate CL/shelf/opened tree frozen; file hashes recorded
- [ ] Intended changed-file list written by the owner **before** any tool output
- [ ] Tool/rules/config identity recorded; excludes listed with reasons
- [ ] View recorded (`p4 client -o`, `p4 where` on 3 known files); unsupported constructs noted
- [ ] Residual 0.7.8 limits disclosed in writing (unsupported view patterns fail closed; unbound lambda ambiguity)
- [ ] Seeded control fixture present, hashed, separate from the partner depot
- [ ] Reference timing method agreed (what task, what stopwatch, how many repetitions)

## During the run

- [ ] All 12 seeded controls executed on the same binary/rules version
- [ ] Every control has a recorded outcome, including known-limit cases
- [ ] Every intended file has an outcome: compared / excluded / unsupported / unreadable / degraded / unknown
- [ ] No silent omission: no missing file, no success exit for an unmeasured required file
- [ ] No fabricated identity delta on unchanged controls
- [ ] Disputed identities adjudicated by the reviewer, not by the vendor alone
- [ ] Wall time, CPU, RSS and P4 request count recorded
- [ ] Baseline retrieval window and any truncation recorded
- [ ] Review-preparation time measured, not estimated
- [ ] Support minutes logged per event
- [ ] Runtime column left empty (no paired workload exists)

## After

- [ ] Second engineer reproduces the dossier from the manifest alone
- [ ] Every ambiguity or undocumented step from the reproduction logged
- [ ] Dossier delivered per template, omissions section non-empty
- [ ] Control report delivered with pass/fail/known-limit per case
- [ ] Findings adjudicated; error counts recorded for both methods
- [ ] Measured savings compared with logged support minutes
- [ ] Go/no-go recorded: continue / narrow / stop, with name, role, date
- [ ] If continue: written price basis and next scope; if stop: segment mismatch written down
- [ ] Support minutes totalled and reported in the memo
- [ ] Artifacts deleted or retained per the agreed rule; sharing scope confirmed
