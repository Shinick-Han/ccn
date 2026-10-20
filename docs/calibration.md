# Health score calibration

**Date:** 2026-10-21
**Bin/corpus version:** bin spec v1, 11 repositories
**Reproduce:** `cargo build --release && pwsh -File tools/calibrate.ps1`

This document is the evidence behind the grade bands in `PLAN.md` §5.2 and the
numbers printed in every report footer. It exists so that "grade C" is a
defensible statement rather than an arbitrary constant.

---

## 1. What is being calibrated

The score is fixed and is **not** adjusted here:

```
p(c)    = clamp(log10(max(c,1)/10), 0, 1)
debt    = Σ_functions nloc(f)·p(ccn(f)) / Σ_functions nloc(f)
score   = 100 · (1 − debt)
```

The score is therefore "the share of measured lines that live above McCabe's
limit of 10, averaged on a log scale". Only the **grade cut points** are
calibrated, because the absolute scale is compressed: real codebases spend most
of their lines in small, simple functions, so a healthy repo lands in the
mid-90s and an unhealthy one in the 70s.

## 2. Corpus

Selected to span language, size, and age. All are local checkouts, scanned with
`ccn <path> --report-json` (the same numbers the report renders).

| repo | files | LOC | functions | score | grade | %func >10 | max CCN |
|---|---:|---:|---:|---:|:--:|---:|---:|
| `CCN` | 68 | 9,582 | 554 | 97.83 | A | 1.63 | 21 |
| `project-j` | 7,996 | 1,344,071 | 81,640 | 97.81 | A | 1.17 | 358 |
| `project-c` | 5,948 | 1,352,833 | 53,568 | 97.30 | B | 1.26 | 180 |
| `project-l` | 7 | 1,356 | 108 | 97.22 | B | 2.78 | 19 |
| `project-g` | 5,048 | 545,178 | 22,360 | 95.52 | B | 2.79 | 83 |
| `project-e` | 435 | 614,425 | 5,531 | 92.19 | C | 3.29 | 141 |
| `project-f` | 20 | 25,593 | 59 | 90.80 | C | 27.12 | 42 |
| `project-i` | 25 | 6,983 | 376 | 90.58 | C | 4.52 | 104 |
| `project-h` | 372 | 68,062 | 9,943 | 89.74 | C | 3.37 | 108 |
| `project-d` | 8,316 | 752,588 | 11,267 | 81.20 | D | 12.74 | 142 |
| `project-k` | 9 | 6,482 | 194 | 80.99 | D | 12.37 | 80 |

Languages with a real grammar — TypeScript/TSX/JS, Python, Rust, Go, Java, C, C++, C# — only. Files in other languages are `heuristic` and never enter the score.

> **M3 re-run (2026-10-21).** The corpus was re-measured after Java/C/C++/C#
> landed and generated/vendored exclusion was added. Two rows moved
> materially and both movements are the point of M3:
> `project-d` 85.52 → **81.20** because 4,161 C functions are now
> actually parsed (its Python-only number flattered the repo);
> `project-e` max CCN 822 → **141** because its minified bundles are now
> classified generated/vendored. The grade distribution did not change.

**Spread:** min 80.99 · Q1 90.58 · median 95.52 · Q3 97.30 · max 97.83.

## 3. Chosen bands

| Grade | Debt (share of lines above CCN 10) | Score | Meaning |
|---|---|---:|---|
| **A** | < 2.5% | ≥ 97.5 | essentially nothing above the limit |
| **B** | 2.5% – 6% | ≥ 94 | a few hot methods, contained |
| **C** | 6% – 12% | ≥ 88 | routine accumulation; refactor when touched |
| **D** | 12% – 21% | ≥ 79 | concentrated risk |
| **E** | ≥ 21% | < 79 | systemic |

The cut points are round numbers **in debt terms** (2.5 / 6 / 12 / 21), not in
score terms. Score decimals are a consequence of `score = 100(1 − debt)`; this
avoids the appearance of false precision like "97.3 is the threshold".

Resulting distribution on the corpus: **A 2 · B 3 · C 4 · D 2 · E 0**.

Before calibration (A≥92/B≥85/C≥75/D≥60) the corpus produced A 6 · B 4 · C 1 ·
D 0 · E 0 — A was the default, which is exactly the degeneracy M5 was meant to
remove.

## 4. Why a line-weighted score can disagree with `%func >10`

`project-f` is the instructive case: 27% of its functions exceed CCN 10, yet it
scores 90.80 (C), because those functions are **small** — the repo's lines are
mostly in simple code. A function-count metric would call it unhealthy; a
line-count metric calls it mediocre.

This is not a bug, and it is why every report prints both numbers:

```
Health 90.80 / 100   grade C
27.1% of functions > 10 · ...
```

Neither number is "the" answer. The grade answers "how much of this codebase
lives in complex code"; `%func >10` answers "how often will a reviewer meet
one". Read them together. A future revision may add a separate function-count
grade; it is deliberately not blended into one opaque number.

## 5. Limitations (read before trusting a grade)

1. **Corpus bias.** 11 repositories, all from one developer's machine, mostly
   TypeScript with some Python/Rust/Go/C. No Java or C# repository, so those
   grammars are fixture-verified but not corpus-verified.
2. **No E observed.** The corpus contains no genuinely unmaintainable repo, so
   the bottom of the scale is anchored only by a synthetic test
   (`a_genuinely_complex_repo_reaches_the_bottom_of_the_scale`) and by the
   formula. Treat E as "below anything I measured".
3. **Small repos are noisy.** `project-l` (7 files) and `project-k` (9 files) move
   several points on a single function. The 50-file rule that hides
   percentages on tiny repos is still not implemented.
4. **Heuristic files do not count.** Files in languages without a grammar are
   excluded from the score; their share is visible as `files_heuristic`. A repo
   measured 40% heuristically gets a grade for the other 60%.
5. **Generated/vendored exclusion is heuristic.** Since M3, generated and
   vendored files are removed from the histograms and the score, and the
   report states how many. Detection is marker/path based (see
   `src/generated.rs`); a generator that writes no comment banner is missed.
6. **The score is not monotone in repo size** by design. Growing a repo does
   not lower the grade unless the new code is complex.

## 6. Re-running

```powershell
cargo build --release
pwsh -File tools/calibrate.ps1                      # default corpus -> docs/calibration.csv
pwsh -File tools/calibrate.ps1 -Repo C:\a,C:\b -Out docs/calibration-2.csv
```

`tools/calibrate.ps1` reads `ccn --report-json`; it does not re-implement the
formula, so the harness cannot drift from the product. Re-run it before
changing `grade_for`, and record the new corpus here — a band change without a
corpus is just a preference.
