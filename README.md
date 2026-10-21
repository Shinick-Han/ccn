# ccn

Repository code-health scanner: per-file LOC, per-function CCN, distribution
histograms, trend and hotspot ranking. One static binary, ten languages.

```console
$ ccn .                       # terminal summary + ccn-report.html
$ ccn . --tui                 # interactive dashboard (browse all files)
$ ccn . --view                # write to a temp file and open it
$ ccn . --open                # ...or open the report in the repo
$ ccn . --json                # machine-readable contract (no report)
$ ccn . --ci                  # ASCII-only, with terminal histograms
$ ccn . --hotspots            # + git churn ranking and ownership (runs git log)
$ ccn . --trend               # compare with the previous scan, and record this one
$ ccn . --since HEAD~1        # only files changed since a revision
$ ccn . --no-cache            # ignore the per-file cache
$ ccn . --fail-over 2.0       # exit 1 if >2% of functions exceed CCN 10
$ ccn . --lang rust,python    # restrict to languages
```

The report is one self-contained HTML file: verdict, CCN-per-function histogram,
LOC-per-file histogram (files vs lines), riskiest files, and git hotspots when
the repo has history. No CDN, no chart library, no telemetry — ~27 KB.

`--tui` keys: `j/k` move, `Tab` chart, `s` sort, `/` filter, `Enter` detail,
`?` help, `q` quit. On a non-TTY it falls back to the summary.

```
ccn 0.1.0
  C:/work/project-c
  5,948 files · 1,384,567 lines · 1,084,993 code · 178,191 comment · 121,383 blank
  LOC   median 26 · p90 309 · p99 2,053 · max 172,692
  langs TypeScript 479,067 (2,662) · TSX 129,624 (602) · CSS 36,188 (173)
```

## Status

| Phase | Scope | State |
|---|---|---|
| M0 | CLI, gitignore-aware walk, per-file LOC, JSON | done |
| M1 | per-function CCN (TS/JS, Python, Rust, Go) | ✅ done |
| M2 | health score, histogram report, terminal verdict | ✅ done |
| M3 | more languages, generated/vendored exclusion | ✅ done |
| M4 | git hotspots | ✅ done |
| M5 | calibration + methodology docs | ✅ done |
| M6 | TUI dashboard + instant view | ✅ done |
| M7 | per-file cache | ✅ done |
| M8 | trend, diff mode, ownership | ✅ done |

`docs/PLAN.md` is the design document; `docs/research-metrics-and-viz.md` is
the methodology research behind the metric choices.

## How health is computed

```
p(c)  = clamp(log10(max(c,1)/10), 0, 1)          # 0 at the McCabe limit of 10
debt  = Σ nloc(f)·p(ccn(f)) / Σ nloc(f)          # over measured functions
score = 100 · (1 − debt)
```

Read it as **"the share of lines living above the McCabe limit, log-scaled"**.
It is deliberately not monotone in repository size, and one 900-CCN generated
file cannot decide the grade.

| Grade | Debt | Score |
|---|---|---:|
| A | < 2.5% | ≥ 97.5 |
| B | 2.5–6% | ≥ 94 |
| C | 6–12% | ≥ 88 |
| D | 12–21% | ≥ 79 |
| E | ≥ 21% | < 79 |

Bands were calibrated on 11 real repositories on 2026-10-21; the corpus, the
spread, and the limitations are in [`docs/calibration.md`](docs/calibration.md).
Re-run `pwsh -File tools/calibrate.ps1` before changing them.

**The score and `%func > 10` answer different questions.** A repo can have 27%
of its functions above CCN 10 and still score 90 if those functions are small.
Read both numbers; neither is "the" answer.

## Limits

- **Tier-1 languages** for function-level CCN: TypeScript/TSX, JavaScript,
  Python, Rust, Go, Java, C, C++, C#. Everything else is a file-level keyword
  heuristic, labelled `heuristic`, and excluded from the function histogram and
  the score.
- Generated and vendored files are excluded from the histograms and the score
  (marker + path detection); the report states how many were dropped. Override
  with `--include-generated`.
- The calibration corpus is one developer's machine, mostly TypeScript.
  Treat absolute grades as indicative outside that population.

## Trend, diff mode, ownership

- **`--trend`** appends a small snapshot to `<repo>/.ccn/history.json` (max 200)
  and prints the delta from the previous run: health, grade, `%func >10`. The
  file is plain JSON so it diffs cleanly in a commit. First run has no trend.
- **`--since <rev>`** analyses only the files changed between `<rev>` and the
  working tree (`git diff --name-only --diff-filter=ACMR`). The resulting health
  describes **the change**, not the repository — do not compare it to a
  full-repo score. Deleted files are skipped; untracked files are not included.
- **`--hotspots`** now also reports the top author per hotspot (`alice +2`) from
  the same single `git log` pass.

## Cache

The second scan of a repository is nearly free: results are cached per file in
the OS user cache directory (`%LOCALAPPDATA%\ccn` on Windows) keyed by
`(path, mtime, size, cache version)`. Nothing is written inside the repository.
A changed file is re-parsed; the rest are served from the cache. Invalidation is
by mtime+size, so touching a file without changing it also re-parses — that is
the conservative direction.

Measured on a 1.7M-line repo: **3.7 s before caching → ~1.2 s with an empty
cache and ~0.3-0.8 s warm**. Cache load itself is single-digit milliseconds on
a small repo; the variance is Windows Defender scanning the cache file.

## Design decisions that are not obvious

- **LOC comes from `tokei`, CCN will come from `tree-sitter`.** `scc`'s
  complexity number is a file-level keyword heuristic, not CCN. `lizard` has
  per-function CCN but cannot produce a blank/comment/code split and is an
  order of magnitude slower.
- **`loc` means non-blank, non-comment lines**; `loc_total` includes blanks and
  comments. Every record satisfies `loc_total == loc + comments + blanks`.
- **Paths are relative to the scan root and use forward slashes**, so a Windows
  report and a Linux CI report are textually comparable.
- **Build output is excluded by default** (`target`, `node_modules`, `dist`,
  `build`, ...) even when a repo has no `.gitignore`. Override with
  `--no-default-excludes`.

## Build

```console
cargo build --release
cargo test
```

Single binary at `target/release/ccn.exe`; no runtime dependencies.
