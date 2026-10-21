# CCN change-review dossier: `<module>` @ `<candidate CL>`

Advisory review evidence. Not a gate. Not a health score. Not a runtime claim.
Fill every field. Write `unknown` explicitly; never leave a field blank and never
silently drop a file.

## 1. Identity

| Field | Value |
|---|---|
| Team / owner | `<team> / <module owner>` |
| Module | `<name>` |
| Scan root (repo-relative) | `<path>` |
| Baseline CL (submitted) | `<CL>` |
| Candidate CL / shelf / opened tree | `<CL or shelf or captured tree hash>` |
| Candidate capture method | `<submitted CL / shelf / opened files + hashes>` |
| Tool version | `<ccn --version, e.g. 0.7.8>` |
| Binary SHA256 | `<hash>` |
| Binary target | `<e.g. linux-x86_64, glibc 2.28>` |
| Rules / config identity | `<rules version or config hash>` |
| Host | `<hostname, OS, kernel, CPU count, RAM>` |
| Filesystem | `<NFS server/mount options, or local>` |
| Perforce view | `<client spec name, view hash, or "recorded in appendix">` |
| View / mapping status | `<resolved / partially resolved / unsupported construct: ...>` |
| Manifest path | `<path to manifest.json>` |
| Manifest hash | `<sha256>` |
| Run timestamp (UTC) | `<YYYY-MM-DDThh:mm:ssZ>` |
| Cache mode / location | `<verify-cache or no-cache> / <private per-job dir>` |

## 2. Scope

Intended changed-file set, produced by the module owner **before** tool output.

| Item | Count |
|---|---:|
| Intended changed files | `<n>` |
| Files in the view under the module root | `<n>` |
| Measured and compared | `<n>` |
| Measured, no change | `<n>` |
| Explicitly excluded (reason given below) | `<n>` |
| Unsupported (grammar / view construct) | `<n>` |
| Unreadable / inaccessible | `<n>` |
| Degraded parse (current or baseline) | `<n>` |
| Identity unknown / ambiguous | `<n>` |
| **Unaccounted** | **`<must be 0>`** |

### Exclusions

| Path | Reason | Category |
|---|---|---|
| `<path>` | `<generated / vendored / out of view / oversize / ignore rule / other>` | `<...>` |

### Scope statement

`<One sentence: what this dossier covers and what it does not. Example: "Covers src/engine under //depot/prj/engine/...; Tcl under flow/ is out of scope; wildcard view lines are unsupported on this build.">`

## 3. Delta table

One row per function with a change. Sort by path, then by source line.

| Path | Identity | Before CCN | After CCN | Before len | After len | Before nest | After nest | Confidence | Matching reason |
|---|---|---:|---:|---:|---:|---:|---:|---|---|
| `<path>` | `<scope::name(params)>` | `<n>` | `<n>` | `<nloc>` | `<nloc>` | `<n>` | `<n>` | `<exact / best-effort / ambiguous>` | `<identity / unambiguous name fallback / body-diff evidence>` |

Notes:

- CCN is cyclomatic complexity per function, computed from a static parse. It is
  not a runtime or defect measure.
- `before` values come from the frozen baseline CL, not from HEAD.
- If an identity is ambiguous, keep the row and mark it `ambiguous`; do not
  merge or drop it.

## 4. Top findings

Maximum 10. One line of "why it matters" per finding. No finding without a
source location.

| # | Path:line | Identity | Change | Why it matters (one line) | Suggested next check |
|---:|---|---|---|---|---|
| 1 | `<path>:<line>` | `<identity>` | `<ccn +n / nloc +n / nesting +n / added>` | `<e.g. new branch per protocol variant; review error paths>` | `<read function / compare with baseline / none>` |
| 2 | | | | | |
| 3 | | | | | |

## 5. Omissions and ambiguity

Explicit and non-empty. `None known` is allowed only if the control report
passed M1 and M2.

| Item | Detail | Impact on this dossier |
|---|---|---|
| Unmeasured relevant files | `<none or list/count + reason>` | `<...>` |
| Degraded parses | `<none or path + direction>` | `<...>` |
| Ambiguous identities | `<none or identity + why>` | `<...>` |
| Unsupported view constructs | `<none or construct>` | `<...>` |
| Unsupported language / heuristic-only files | `<none or path count>` | `<...>` |
| Baseline substitutions | `<none or what was substituted and why>` | `<...>` |
| Other | `<none or ...>` | `<...>` |

Control report: `<path>`, summary `<x/12 pass, known limits: ...>`.

## 6. Runtime

Leave empty unless a real paired workload exists: same host, same build, same
input, same thread/memory limits, repeated runs, preserved timing and functional
outputs. A static complexity change is not runtime evidence.

| Field | Value |
|---|---|
| Workload / build ID | `<empty unless paired runs exist>` |
| Baseline runtime (median, n runs) | |
| Candidate runtime (median, n runs) | |
| Variance / spread | |
| Queue time (separate from runtime) | |
| Correctness or timing-output check | |
| Verdict | `<not measured>` |

`Runtime not measured. No runtime improvement is claimed or implied.`

## 7. Reviewer notes and decisions

| Field | Value |
|---|---|
| Reviewer | `<name>` |
| Review date | `<YYYY-MM-DD>` |
| Time to first action | `<minutes>` |
| Findings accepted | `<n>` |
| Findings rejected | `<n>` |
| Findings disputed (adjudication needed) | `<n>` |
| Did the dossier change review order? | `<yes/no + how>` |
| Would this have been found without the dossier? | `<yes/no/unknown>` |
| Action taken | `<bug filed / refactor scheduled / no action>` |
| Reviewer comment | `<free text, short>` |

## Appendix A: command lines

Record exact commands, including the binary path, working directory and
environment variables. No abbreviations.

```text
# version and identity
ccn --version
sha256sum ./ccn

# baseline / candidate scan (run from the scan root)
CCN_CACHE_DIR=<private dir> ccn . --baseline <CL> --report-json --no-report

# seeded controls
<one line per control case, or the fixture runner command>

# manifest / cache location
CCN_CACHE_DIR=<private dir>
```

## Appendix B: artifacts

| Artifact | Path | SHA256 |
|---|---|---|
| Manifest | `<path>` | `<hash>` |
| Control report | `<path>` | `<hash>` |
| Raw report JSON | `<path>` | `<hash>` |
| HTML (if generated) | `<path>` | `<hash>` |
| Candidate file hashes | `<path>` | `<hash>` |

Retention: `<delete on request or retain until YYYY-MM-DD>`. Cache and history
contain paths and function names; do not distribute them with the dossier unless
requested.
