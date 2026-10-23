# ccn

Repository code-health scanner: per-file LOC, per-function CCN, distribution
histograms, trend and hotspot ranking. One static binary, ten languages.

Linux builds target **glibc 2.28** (AlmaLinux/RHEL 8), and there is **no Python
dependency** at build or run time — see [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).

## Install

```console
# npm (downloads the platform binary, no install script)
$ npx @shinick/ccn . --view
$ npm i -g @shinick/ccn

# or a release binary
$ curl -LO https://github.com/Shinick-Han/ccn/releases/latest/download/ccn-linux-x64
```

npm packages: `@shinick/ccn` plus `@shinick/ccn-linux-x64`,
`@shinick/ccn-darwin-x64`, `@shinick/ccn-darwin-arm64`,
`@shinick/ccn-win32-x64`. ARM Linux/Windows has no prebuilt package; build from
source.

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

`--tui` keys: `j/k` move, `/` filter, `e` edit, `Ctrl+P` open, `?` help,
`q` quit. The left column shows the CCN/LOC histogram, a live preview of the
selected file, and a repo pulse (parse confidence, threshold counts, baseline).
On a non-TTY it falls back to the summary.

### Experimental: embedded workbench (ccn workbench)

The single `ccn` binary can host a real Neovim (in a PTY) with a live ccn
sidebar: per-function CCN, unsaved deltas, inline `CCN n (+d)` extmarks, and
`Enter` to jump to a function. It is part of the normal binary; `nvim` is the
only external runtime requirement:

```console
$ ccn workbench file.c --editor nvim
$ ccn . --tui   # press e on a file to open it here
```

Requirements: `nvim` on `PATH` or `--editor <path>`. In the dashboard, `e` opens the selected file in a **split view**: a collapsible file tree on the left, Neovim and the live function sidebar on the right. Drag the divider with the mouse to resize the panes. The primary keys are deliberately few: `j/k` move, `/` filter, `e` edit, `Ctrl+P` open by name, `F5` command palette, `F6` baseline review (`d` opens the line diff, `s` side-by-side, `x` reviewed), `F7` diagnostics, `Alt+W` panes, `Alt+O` function search, `F10` dashboard, `?` help, and `q` quit. The first `q`/`Esc`/`Ctrl+C` on a live editor asks for confirmation; press `q` or `Ctrl+C` again to discard. The nvim session, open buffers, split ratio, and dashboard selection are restored on the next run. Clipboard paste uses bracketed paste; mouse reporting is enabled only in the split (use Shift+drag for normal terminal selection/copy). In the split, click a file in the tree to open it and click either pane to focus it; the divider is draggable. Use `Alt+O` (or click a function row) to jump to a function in the current file. The embedded editor is real Neovim; line numbers are enabled by default (`:set nonumber` to turn them off). The review panel needs a baseline run: `ccn . --tui --baseline <rev>`.

Advanced keys still work but are not shown in the footer: `F8`/`Ctrl+Shift+E` pane switch (compatibility), `F9` function sidebar, `F2`/`F3` previous/next tab, `F4` close tab, `[`/`]` resize from the file pane, `Tab` chart tab, `s`/`S` sort key/direction, `g`/`G` first/last. Set `CCN_KEYMAP=advanced` to show the advanced section in `?`. On AlmaLinux/RHEL 8 use
the EPEL `neovim` (0.8 works; the official Neovim Linux tarball requires glibc
2.34 and cannot run on EL8). Use `--no-rpc` for PTY-only editing with the
mtime-based sidebar. See [`docs/workbench-spike.md`](docs/workbench-spike.md)
for the compatibility matrix, keys, and limitations. The workbench is
experimental; the scanner behavior is unchanged.

### Editor integration (`ccn lsp`)

`ccn lsp` runs the same binary as a **stdio Language Server Protocol server** —
editor-agnostic, no network, no TUI. It publishes advisory diagnostics for
functions above CCN 10, plus hover and document symbols; there is no formatting
or completion. See [`docs/lsp.md`](docs/lsp.md) for the Neovim 0.10+/0.11 setup.


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
debt  = Σ nloc(f)·p(ccn(f)) / Σ nloc(f)          # over exactly parsed functions
score = 100 · (1 − debt)
```

`nloc(f)` is the function's **physical span**, blanks and comments included, so
layout inside a function moves the score as well as its decision points. Read
the result as an **advisory, size-weighted complexity index**: it is not a
validated code-health score, it does not predict defects or runtime, and it is
not a release gate. Files parsed best-effort (syntax errors recovered) and
heuristic files are excluded from the denominator; the report exposes how many
functions were left out so the coverage is visible.

The index is deliberately not monotone in repository size, and generated or
vendored files are dropped before scoring. It is still a repository-level
summary, not a per-file verdict.

| Grade | Debt | Score |
|---|---|---:|
| A | < 2.5% | ≥ 97.5 |
| B | 2.5–6% | ≥ 94 |
| C | 6–12% | ≥ 88 |
| D | 12–21% | ≥ 79 |
| E | ≥ 21% | < 79 |

Bands were calibrated on 11 real repositories on 2026-10-21; the corpus, the
spread, and the limitations are in [`docs/calibration.md`](docs/calibration.md).
That calibration predates the best-effort exclusion, so grades are marked
**experimental** in the report until the corpus is re-measured. Re-run
`pwsh -File tools/calibrate.ps1` before changing the bands.

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

## "No parsed functions"

If a repository reports no functions, `tools/diagnose.py` groups every file by
language and tier and prints the reason (Python 3.8+, no dependencies):

```console
$ python3 tools/diagnose.py /path/to/workspace
language          files   exact    heur    none  note
Kotlin                2       0       2       0  no grammar yet (tier 3)
Rust                120     118       2       0
```

- **heur** with a "no grammar" note: the language is not in the ten tier-1
  grammars; those files only get a file-level number.
- **heur** in a supported language: the file failed to parse (truncated,
  unsupported dialect, invalid UTF-8).
- **none**: the file was not read (permissions, >8 MiB, or vanished mid-scan).

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
- **worsened**: it got worse — including functions that were **already over
  10**. A function is reported when CCN, nesting depth or length grows
  (`ccn +3`, `nesting +1`, `nloc +21`); identical or improved functions are
  silent. So `30 → 33` is reported, not just the 9 → 11 crossing. The finding's
  message names only what changed: a length-only regression says "length
  increased ... lines", not "complexity increased".
- matching is **identity-first**: the engine's scope-qualified name plus
  normalized parameters (`Widget::draw(int)`), so overload order does not look
  like a change; a name-only match is used only when exactly one unmatched
  baseline function carries that name.
- a file that did not exist at the revision counts as an empty baseline, so its
  complex functions are reported as added. The revision itself is validated
  first, so a typo cannot turn the whole repository into "new".
- **renames are followed** (`git diff -M`): a moved file is compared against its
  old path, so moving code does not look like a hundred new functions.
- P4: `--baseline 123456` reads each depot file's as-of tree revision
  (`<depot-path>@123456`, the revision in effect at that changelist), never
  `@=123456`.
- the baseline summary distinguishes files **compared**, **skipped** (baseline
  copy read but unparseable), **unavailable** (read/permission/revision error)
  and **unmapped** (the VCS view has no history for that path; not an error).
  The JSON exposes `files_unmapped` for the last case and `files_unknown` when a
  relevant path could not be resolved to a comparison. Skipped, unavailable and
  unknown comparisons mean the baseline is incomplete, so the gate does not
  silently pass on unmeasured files.

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

- the action first tries the prebuilt asset for the runner (`ccn-linux-x64`,
  `ccn-macos-x64` on Intel macOS, `ccn-macos-arm64` on Apple silicon,
  `ccn-windows-x64.exe`) and verifies its `SHA256SUMS` digest and `--version`
  before use. With `version: latest` a failed download falls back to
  `cargo build --release --locked`; with a pinned tag an unavailable,
  unverifiable or mismatched asset fails the step instead of silently running
  the action checkout. Private repositories cannot be downloaded anonymously,
  so they take the source path (which therefore requires `version: latest`).
- it always passes `--no-report`.
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
| ignore rules | `.gitignore` + `.tokeignore` | `.gitignore` + `.tokeignore` + `.p4ignore` |
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
- Validated against a local Helix Core server (`p4d`) with four changelists:
  scan, `--since <CL>`, `--baseline <CL>`, `--hotspots` and `--sarif` all
  produce the expected results. The fixes that came out of that run are in
  `docs/PLAN.md` §25.

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

The default lookup trusts mtime+size. On a gate or an NFS checkout — where mtime
can be restored, coarse, or rewritten by a copy — pass `--verify-cache` to hash
the file contents before reusing an entry. It costs more per file but cannot
serve a stale measurement as the candidate's.

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

Requires stable Rust **1.90+** (`rust-version` in `Cargo.toml`); prebuilt
binaries need no Rust at all. See [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).

```console
cargo build --release --locked
cargo test
```

Single binary at `target/release/ccn.exe`; no runtime dependencies.
