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
$ ccn . --since HEAD~1        # only files changed since a revision (git)
$ ccn . --since 123456        # ...or a Perforce changelist (or `opened`)
$ ccn . --baseline HEAD~1     # functions that became complex since a revision
$ ccn . --baseline HEAD~1 --fail-on-new   # PR gate: exit 1 if any
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

## Look

The report borrows its visual language from AuraDeck's "cyber-pastel" palette
(Catppuccin Mocha base): `#12121C` canvas, `#1A1B2A` cards, a mauve-to-sky
hairline on every panel, uppercase micro-labels, tabular numerals, and semantic
grade colours (mint / sky / gold / peach / coral). The bar ramp runs
mauve → blue → teal, with the over-limit bins in gold and coral.

Everything is static: no animation, no shader loop, no external font or image.
The light theme is the same palette on lilac paper, not an inversion.

The TUI (`--tui`) uses the same tokens: `#12121C` canvas, `#1A1B2A` cards, a
mauve focused border, selected rows on `#2D2F46`, the grade badge in its
semantic colour, the CCN bar ramp, and risk-coloured `max CCN` values.

## Trend, diff mode, ownership

- **`--trend`** appends a small snapshot to `<repo>/.ccn/history.json` (max 200)
  and prints the delta from the previous run: health, grade, `%func >10`. The
  file is plain JSON so it diffs cleanly in a commit. First run has no trend.
  Snapshots record the tool version, bin-spec version and grade calibration;
  when they differ from the previous snapshot the report prints
  `! not comparable: ...` instead of silently comparing apples to oranges.
- **`--since <rev>`** analyses only the files changed between `<rev>` and the
  working tree (`git diff --name-only --diff-filter=ACMR`). The resulting health
  describes **the change**, not the repository — do not compare it to a
  full-repo score. Deleted files are skipped; untracked files are not included.
- **`--hotspots`** now also reports the top author per hotspot (`alice +2`) from
  the same single `git log` pass.

## PR baseline

```powershell
ccn . --baseline origin/main --fail-on-new
```

Compares every measured function against its counterpart at the revision and
reports only the delta:

```
Since origin/main  1 new >10 · 0 worsened   (66 files compared, 0 skipped)
  + 11   parse_changes            src/p4.rs:510
  ~ 18   handle                   src/api.ts:88 (was 9)
```

- **added**: the function did not exist at the revision and is already over 10.
- **worsened**: it existed at or under 10 and crossed it.
- matching is by function name, consuming duplicates in source order.
- a file that did not exist at the revision counts as an empty baseline, so its
  complex functions are reported as added. The revision itself is validated
  first, so a typo cannot turn the whole repository into "new".
- P4: `--baseline 123456` uses `p4 print @=123456` for the old contents.

## GitHub Actions

The repository ships a composite action and a PR workflow. Minimal usage in
another repository:

```yaml
permissions:
  contents: read
  security-events: write

steps:
  - uses: actions/checkout@v4
    with: { fetch-depth: 0 }

  - uses: Shinick-Han/ccn@main
    with:
      baseline: origin/${{ github.base_ref }}
      sarif: ccn.sarif

  - uses: github/codeql-action/upload-sarif@v3
    if: always()
    with: { sarif_file: ccn.sarif }
```

- the action builds the binary from source (`cargo build --release --locked`)
  and always passes `--no-report`.
- `fail-on-new: "true"` makes the step fail when the PR adds or worsens a
  function above CCN 10. SARIF still uploads because of `if: always()`.
- with `--baseline`, SARIF contains only the delta; without it, it contains
  every function at or above `--sarif-min-ccn` (default 10).
- results appear in the repository's **Security → Code scanning** tab.

## Perforce

The scan itself is VCS-agnostic; git and Perforce differ only in the history
features, which are auto-detected (`.git` wins, otherwise `p4 info`):

| Feature | Git | Perforce |
|---|---|---|
| scan / CCN / report / TUI / cache | same | same |
| `--since` | `HEAD~1`, branch, commit | changelist number, or `opened` |
| `--hotspots` | `git log --numstat` | `p4 changes` + `p4 describe` |
| ignore rules | `.gitignore` | `.gitignore` + `.p4ignore` |
| trend snapshots | `<repo>/.ccn/history.json` | same, or the user cache when the workspace is read-only |

P4 limits, by design:

- **`lines changed` is 0.** `p4 describe -s` does not report diffs, and asking
  for them per changelist is too expensive. Hotspot ranking is driven by
  commits, so it still works; the column is simply empty for P4.
- **300 changelists by default** (`--history-limit`). Each one is a server
  round-trip, so a year of history would take minutes.
- **Read-only workspaces**: `p4 sync` writes files read-only, so
  `--trend` falls back to `%LOCALAPPDATA%\ccn\history-<hash>.json` when the
  repository path cannot be written.

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
