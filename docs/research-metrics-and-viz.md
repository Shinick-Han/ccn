# Code Health Scanner — Metric Methodology & Visualization Design Research

**Status:** research report, no code written.
**Scope:** per-file LOC + CCN scan, histogram presentation, "how healthy is this codebase" verdict. Local Windows tool.
**Date:** 2026-10-20

Legend used throughout:

- **[VERIFIED]** — claim is stated at the cited URL, which I fetched this session. URL in §8.
- **[JUDGMENT]** — my design opinion, derived from the verified material but not itself sourced.
- **[UNVERIFIED]** — plausible, commonly repeated, but I did not confirm a primary source. Treat as a hypothesis to calibrate.

---

## 0. Bottom line first

1. **Do not histogram per-file summed CCN.** It is nearly a re-plot of the LOC histogram. Summed CCN is a size proxy; the literature is explicit that CCN correlates with LOC and adds little at aggregate level. **[VERIFIED]** (§1.4)
2. **The unit of meaning is the function.** Compute CCN per function, threshold at 10 (McCabe) with 15 as the NIST tolerance. File-level surface should be `max function CCN` and `count of functions over 10`, not the sum. **[VERIFIED]** (§1.2, §1.3)
3. **A histogram cannot answer "which file?".** The product needs three panels, not one: distribution (histogram/ECDF), outliers (ranked table), verdict (a single number with its derivation visible). **[JUDGMENT]**
4. **Ship one self-contained HTML file (~30–60 KB, hand-rolled inline SVG, no chart library) plus a 12-line terminal summary.** `--open` by default, `--no-open` / `--json` for CI. **[JUDGMENT]**, sizes **[VERIFIED]** (§4)
5. **Health score = size-weighted average of a log-scaled excess-over-10 penalty per function.** This is the only formulation I found that is simultaneously (a) anchored to a published threshold, (b) immune to the size confound, (c) insensitive to tiny files, and (d) explainable in one sentence. Exact formula in §6.2.
6. If git is available, churn should outrank CCN in the *ranking* layer. Empirically, change-based metrics beat static code metrics at predicting faults. **[VERIFIED]** (§2.2)

---

## 1. CCN methodology

### 1.1 McCabe's original definition

McCabe (1976), *A Complexity Measure*, IEEE TSE SE-2(4):308–320. **[VERIFIED]** (bibliographic record and formulas via Wikipedia; the paper PDF itself is not text-fetchable by my tooling — see §9)

The measure is graph-theoretic, over the control-flow graph:

```
M = E − N + 2P
```

where `E` = edges, `N` = nodes, `P` = number of connected components (subroutines). For a single subroutine `P = 1`, so `M = E − N + 2`. **[VERIFIED]**

Equivalently, using the original strongly-connected formulation where every exit is connected back to entry:

```
M = E − N + P     (the cyclomatic number / first Betti number)
```

This equals the number of linearly independent cycles, i.e. the number of linearly independent paths from entry to exit, which is why it doubles as a basis-path-testing test-count bound. **[VERIFIED]**

The practically important identity: **for a single-entry/single-exit structured routine, M = (number of decision points) + 1.** McCabe showed this holds when decision points are counted at the lowest (machine) level. **[VERIFIED]**

**The compound-predicate wrinkle that most people get wrong.** `IF cond1 AND cond2 THEN` counts as **two** decision points, because at machine level it is `IF cond1 THEN IF cond2 THEN`. **[VERIFIED]** This is why C/C++/Java analyzers increment on `&&` and `||` — see the Sonar per-language tables in §1.5.

**Multiple exits.** McCabe's formula extends as `M = π − s + 2`, where `π` = decision points, `s` = exit points. **[VERIFIED]** Note the sign: extra exits *reduce* CCN, which is why some tools refuse the correction and others (PL/I in Sonar) add +1 per extra return instead. This is a real interoperability hazard: **two tools can disagree on the same function by a few points purely on multi-exit and switch policy.** **[VERIFIED]** that Sonar's PL/I does the opposite of the Harrison correction; **[JUDGMENT]** that this matters for cross-tool comparison.

Also from the original paper, worth knowing because it is the honest caveat: McCabe himself flagged that the limit of 10 "has seemed unreasonable ... when a large number of independent cases followed a selection function (a large case statement)". **[VERIFIED]** (quoted by Sonar)

**Essential complexity.** McCabe §VI also defined *essential complexity*: iteratively collapse single-entry/single-exit subgraphs (i.e. pretend you extracted them into subroutines); the CCN of the irreducible remainder is the essential complexity. It is exactly 1 for all structured programs, >1 for unstructured ones (goto-driven spaghetti). **[VERIFIED]** This is a genuinely different and arguably more interesting signal than raw CCN, and almost nobody surfaces it. **[JUDGMENT]** — worth a `--essential` flag later.

### 1.2 Per function vs per file: sum, max, mean, or density?

There is no single "correct" aggregation; each answers a different question. The failure mode is picking one and presenting it as "the file's complexity".

| Aggregate | Formula | Answers | Good for | Fails because |
|---|---|---|---|---|
| **Per-function CCN** | `M(f)` | "Is this routine hard to test/understand?" | Thresholds, refactor targets | Not a file/repo number |
| **Sum** | `Σ M(f)` | "How much branch load lives in this file?" | Effort/refactor budgeting across *similar-sized* files | ≈ a rescaled LOC. Dominated by file size |
| **Max** | `max M(f)` | "Is there a bomb in here?" | Outlier detection, review routing | Ignores 40 mediocre functions for one bad one |
| **Mean** | `Σ M(f) / n` | "Typical routine complexity" | Comparing files of different function counts | Mean of a right-skewed distribution; masks the tail |
| **Median / p90** | `median M(f)`, `p90 M(f)` | "Typical / worst-decile routine" | Robust summaries | Needs enough functions per file (≥5) for stability |
| **Density** | `Σ M(f) / LOC` | "Branch decisions per line" | Size-normalized comparison | Explodes on tiny files; language-dependent |

Industry practice splits along exactly this line:

- **SonarQube** computes per function and then *sums* for the project: "Cyclomatic complexity = 1 + number of conditional branches", and "the calculation of the overall code's cyclomatic complexity is basically the sum of all complexity scores calculated at the function level." **[VERIFIED]**
- **scc** computes a single per-file number by keyword scanning and is explicit that it is an approximation: "It's my own definition, but tries to be an approximation of cyclomatic complexity ... although done only on a file level", "calculated by looking for branch and loop statements in the code and incrementing a counter for that file", and — critically — "**The complexity estimate is really just a number that is only comparable to files in the same language.** ... it does not build an AST of the code as it only scans through it." **[VERIFIED]**

That scc quote is the single best argument for **not** building a cross-language summed-CCN histogram: the most popular "fast complexity" tool already disclaims cross-language comparability of its per-file sum. **[JUDGMENT]** inference, but a strong one.

### 1.3 Thresholds, and where each actually comes from

This matters because the famous four-band table is *not* from the 1976 paper, and citing it as such is the most common error in this space.

| Threshold | Claim | Provenance |
|---|---|---|
| **10** | "Limit routines to 10" | McCabe 1976, §"Limiting complexity during development": he recommended splitting modules whenever CCN exceeded 10. **[VERIFIED]** |
| **10 (corroborated), 15 (relaxed)** | NIST Structured Testing observed that "since McCabe's original publication, the figure of 10 had received substantial corroborating evidence"; in some circumstances appropriate to "relax the restriction and permit modules with a complexity as high as 15"; and recommended the policy phrasing "For each module, either limit cyclomatic complexity to [the limit] or provide a written explanation of why the limit was exceeded." | NIST SP 500-235 (Watson & McCabe, 1996), as quoted at **[VERIFIED]** |
| **1–10 simple / 11–20 moderate / 21–50 complex / >50 untestable** | The four-band risk table everyone quotes | **Tom McCabe's DHS presentation "Software Quality Metrics to Identify Risk" (2008)**, *not* the 1976 paper. **[VERIFIED]** as the attribution; the primary artifact is a `.ppt` (archived copy linked from Wikipedia) |
| **15** | Default threshold for Sonar rule S3776 (cognitive, not cyclomatic) | SonarQube rule S3776 default parameter `threshold: 15`; **[VERIFIED]** via multiple running Sonar instances' rule pages found in search (e.g. the Scala rule page shows "The maximum authorized complexity. Default Value: 15") and Sonar community threads. Not a first-party doc page I could load — see §9. |
| **~10 for CCN, ~15 for cognitive** | Industry folklore | Convergence of the above. The coincidence of 15 is real but the two metrics are not interchangeable. |

**Practical takeaway [JUDGMENT]:** treat **10** as the "attention" line and **20** as the "act" line, both per function. Do not present 50 as "untestable" as a hard gate — it comes from a vendor slide deck, and the population of functions above 50 in any real repo is small enough that the label mostly serves as a scary color. Use it as the top bin label, not as a policy.

### 1.4 The size confound — the central design problem

This is well documented and should be stated plainly in the tool's own docs:

- "The correlation between cyclomatic complexity and program size (typically measured in lines of code) has been demonstrated many times." **[VERIFIED]**
- Les Hatton claimed "complexity has the same predictive ability as lines of code." **[VERIFIED]**
- "Studies that controlled for program size ... are generally less conclusive, with many finding no significant correlation, while others do find correlation." **[VERIFIED]**
- Sonar's own position: "at the application level, studies have shown that Cyclomatic Complexity correlates to lines of code, so it really doesn't tell you anything new." **[VERIFIED]** — from the company that ships the metric.

So: at **function** level CCN is informative; at **file/repo** level raw CCN is mostly a proxy for size. Four normalizations, in increasing order of usefulness:

**(a) Per-function distribution.** Plot the distribution of `M(f)` across all functions in the repo, not files. This is the most honest "how complex is this codebase" chart there is, and it is what almost no tool shows. **[JUDGMENT]**

**(b) Max function CCN per file.** Collapses the file to its worst routine. Robust to size, directly actionable, and matches how a reviewer thinks ("this file has a 60-CCN method").

**(c) Density.** `density(F) = Σ M(f) / LOC(F)`, i.e. decisions per line. Origin: Gill & Kemerer (1991), *Cyclomatic complexity density and software maintenance productivity* (IEEE TSE), which studied density rather than raw CCN against maintenance productivity. **[VERIFIED]** existence and title; **[UNVERIFIED]** the specific coefficient they found.

Density is the right *shape* of normalization but is statistically nasty on small files: a 6-line function with CCN 3 gives density 0.5, an order of magnitude above any real codebase. **Fix with shrinkage [JUDGMENT]:**

```
density_shrunk(F) = (Σ M(f) + k · d₀) / (LOC(F) + k)
    k  = 200                      # pseudo-lines of prior evidence
    d₀ = repo median density      # e.g. 0.15 – 0.25 decisions/line
```

This is a standard empirical-Bayes/Bayesian-shrinkage estimator: a 10-line file is pulled almost entirely to `d₀`; a 5,000-line file is essentially its own value. Two lines of code, no special cases, no minimum-file-size cutoff. **[JUDGMENT]**

**(d) Concentration.** Not a normalization, but the complementary statistic the histogram keeps failing to express: `share_of_LOC(top 10% of files)`. For heavy-tailed software artifact sizes this is typically 50–80% — the Louridas/Spinellis result gives the general form: pick the top `a` fraction by rank and you cover ≈ `a^θ` of the mass, with θ derived from the power-law exponent. **[VERIFIED]** for the math and the phenomenon; **[UNVERIFIED]** the exact number for *your* repos — compute it.

### 1.5 Counting rules: the per-language divergence you must document

SonarQube's per-language tables are the best public reference for what counts as a decision point, and they disagree across languages in ways that matter. Selected, **[VERIFIED]**:

- **C / C++ / Objective-C:** `if`, `while`, `do while`, `for`, each `case`, `default`, `&&`, `||`, `?:`, lambda definitions. Also: when a header is scanned as part of multiple compilation units, each measure may be computed more than once — Sonar "stores the largest value". (A good warning about header double-counting in general.)
- **Java:** `if`, `for`, `while`, `case`, `&&`, `||`, `?`, `->`.
- **JS/TS, PHP:** function declarations themselves, `if`/`elsif`, `&&`, `||`, ternary, loops, each `case`, `throw`, `catch`, `goto` (PHP only).
- **C#:** method/ctor/dtor/property/accessor/operator/local-function declarations, conditional expressions, conditional access, switch case *or switch expression arm*, and/or patterns, loop statements, `??`, `??=`, `||`, `&&`.
- **COBOL:** a 22-keyword list including `ALSO`, `ALTER`, `DEPENDING`, `EXCEPTION`, `INVALID`, `OVERFLOW`, `VARYING`.

**Design consequence [JUDGMENT]:** store the counting rule version alongside every number (`ccn_rule: "sonar-java@1"`). If you ever change what counts, every stored number silently shifts. This is cheap now and unaffordable later.

---

## 2. What actually predicts "unhealthy"

### 2.1 Size

The uncomfortable summary: **size predicts defects about as well as complexity does.** LOC is the strongest single static predictor in most comparisons, and it is also the thing you cannot reduce by refactoring (extracting functions moves lines, doesn't delete them). Any health score that is monotone in LOC is measuring "this codebase is big", not "this codebase is unwell". **[VERIFIED]** (Hatton; Wikipedia's summary) + **[JUDGMENT]** (the design conclusion).

Consequence: the tool must **not** report a repo-level score that improves simply because the repo shrank. Use per-line normalization, or report the score as a distribution property rather than a total.

### 2.2 Churn / hotspots — the strongest empirical signal

Adam Tornhill's behavioural code analysis: **hotspots = complex code that we have to work with often**, i.e. the intersection of a *quality* dimension and a *relevance* dimension. **[VERIFIED]**

Tornhill's argument against complexity-only prioritization, verbatim: "the main danger in prioritizing improvements ... based on code complexity alone is that we miss the most important decision point: impact. What if all those modules in react-reconciler have been stable for 5 years?" And: change frequency (number of commits) is used "as a proxy for developer impact". **[VERIFIED]**

The distributional fact that makes this work: "most development activity is in a small part of the codebase ... most of the code is in the long tail, meaning it's code that we rarely if ever need to work on." **[VERIFIED]** — this is the same heavy-tail structure as LOC, in a different variable.

Independent empirical support for churn > static metrics:

- Nagappan & Ball (ICSE 2005), *Use of Relative Code Churn Measures to Predict System Defect Density*: **relative** churn measures (churn normalized against other dimensions, not raw line counts) predict system defect density. **[VERIFIED]** title + abstract gist.
- Hassan (ICSE 2009), *Predicting Faults Using the Complexity of Code Changes*: "our change complexity metrics are better predictors of [faults] than ..." — the abstract's claim is that change-complexity models outperform code-complexity models. **[VERIFIED]** abstract gist.
- Defect clustering: "between 60 and 90 percent of the defects arise from 20 percent of the modules, with a median of about 80 percent" (Boehm & Basili, quoted in Louridas et al.). **[VERIFIED]** as quoted.

**Note the double meaning of "complexity" in that literature** — Hassan's "complexity of code changes" is entropy over the *set of files touched per commit*, not CCN. That is a distinct, cheap-to-compute metric worth adding later (`--entropy`): high-entropy commits (touching many files at once) are riskier. **[JUDGMENT]** that it's worth adding; **[VERIFIED]** that it exists and predicts.

### 2.3 Cognitive Complexity (SonarSource) as a CCN alternative

Motivation, verbatim: CCN "works very well for measuring testability, but not for maintainability." The canonical demonstration:

```java
int sumOfPrimes(int max) {                 // CCN 4, cognitive 7
  OUT: for (int i = 1; i <= max; ++i) {    // +1
    for (int j = 2; j < i; ++j) {          // +2 (nesting=1)
      if (i % j == 0) {                    // +3 (nesting=2)
        continue OUT;                      // +1
      }
    }
  }
}
String getWords(int number) { /* switch with 3 cases */ }  // CCN 4, cognitive 1
```

Both are CCN 4; cognitive complexity separates them 7 vs 1. **[VERIFIED]**

The three rules, verbatim: **[VERIFIED]**
1. "Increment when there is a break in the linear (top-to-bottom, left-to-right) flow of the code"
2. "Increment when structures that break the flow are nested"
3. "Ignore 'shorthand' structures that readably condense multiple lines of code into one"

Consequences that are design-relevant:
- **`switch` costs 1 total**, regardless of case count. `case`s are free. **[VERIFIED]**
- **It does not increment for the function itself**, so a trivial getter scores 0 — which makes *class- and file-level* aggregation meaningful in a way CCN aggregation never is: "when you see a high number, it really means there's a lot of logic in the class, not just a lot of methods." **[VERIFIED]**
- Boolean operator sequences: increments per *sequence* of like operators, not per operator (the white paper detail; the blog defers to it). **[UNVERIFIED]** exact rule — I could not load the white paper PDF.

**Recommendation [JUDGMENT]:** ship CCN as the primary (it's what every tool and every threshold refers to, and it has a real testability meaning), and compute cognitive complexity as a **secondary column** where a parser is available. Do not average them; they answer different questions. The zero-at-trivial-getter property makes cognitive complexity the better choice for a *file-level* aggregate; the McCabe bands make CCN the better choice for a *threshold gate*.

### 2.4 Defensible health scores

Three formulas, ordered by how much I trust them. All are mine **[JUDGMENT]** unless noted.

#### A. Complexity Risk Index (primary, no git required)

Per function, a smooth penalty anchored at the McCabe limit:

```
p(c) = clamp( log10(c / 10) / log10(10), 0, 1 )
```

| c | 1–10 | 15 | 20 | 31.6 | 50 | 100+ |
|---|---|---|---|---|---|---|
| p | 0 | 0.18 | 0.30 | 0.50 | 0.70 | 1.00 |

Properties: zero exactly at the published limit (so "healthy" means "at or under McCabe's 10", not an arbitrary scale); log-scaled, so the ladder 10→20→50→100 maps to 0.3→0.7→1.0 and compresses the meaningless 100→500 range; bounded, so one 900-CCN generated file cannot set the repo's score.

Per file, weighted by function size (a 300-line CCN-15 method is worse than a 4-line CCN-15 method):

```
R(F) = Σ_f LOC(f)·p(M(f)) / Σ_f LOC(f)          ∈ [0,1]
```

Repo:

```
D = Σ_all_f LOC(f)·p(M(f)) / Σ_all_f LOC(f)     ∈ [0,1]
Health = 100 · (1 − D)
```

Read it as: **"the fraction of this codebase's lines that live above the McCabe limit, averaged on a log scale."** That sentence is the whole justification, which is the property I care most about.

Grades [JUDGMENT], chosen to be conservative — most real codebases should land B/C, not A:

| Health | Grade | Meaning |
|---|---|---|
| ≥ 92 | A | essentially nothing above CCN 10 |
| 85–92 | B | a few hot methods, well contained |
| 75–85 | C | routine accumulation; refactor when touched |
| 60–75 | D | concentrated risk; the outliers are load-bearing |
| < 60 | E | systemic |

**Calibration warning [UNVERIFIED]:** I have not run this against real repos. Before shipping, compute D for ~5 known codebases (a clean small one, a mid-size OSS app, a monorepo, a vendored-heavy repo) and re-anchor the grade boundaries so the distribution of grades is not degenerate. Also: **exclude generated/vendored files from D** or the score measures the dependency tree, not the codebase.

#### B. Hotspot score (requires git)

CodeScene's concept is `complexity × change frequency`; the exact formula is not published. **[VERIFIED]** that the concept is the product's core; **[UNVERIFIED]** any specific weighting.

Use percentile ranks instead of raw products — this is important, because both factors are heavy-tailed and a raw product is dominated by whichever has the fatter tail:

```
h(F) = rank_pct(commits_365d(F)) × rank_pct(R(F))        ∈ [0,1]
```

`rank_pct` ties → average rank; files with zero commits get 0. Then:

```
HotspotDebtShare = Σ_{F ∈ top decile of h} LOC(F) / Σ LOC(F)
```

A single repo-level number with a real management meaning: "X% of our lines are both above-median complexity and above-median churn." **[JUDGMENT]**

Also worth a column, cheap and informative: `recent_growth(F) = LOC(F, now) − LOC(F, 1y ago)` from `git log --numstat`. CodeScene calls this a "complexity trend"; the diagnostic value is distinguishing "long-standing ugly" from "actively getting worse". **[VERIFIED]** that CodeScene ships complexity trends for exactly this purpose.

#### C. What I would *not* do

- **Do not multiply CCN by LOC.** That is `LOC²`-ish; it produces a number whose only meaning is "big files are bad".
- **Do not present an unweighted mean of per-file scores.** It makes a repo of 900 four-line files score better than a repo of 90 four-hundred-line files with identical per-line risk.
- **Do not show a single letter grade without the histogram next to it.** A grade with no distribution behind it invites exactly the "so which file do I fix?" failure that CodeScene's whole pitch is built on. **[VERIFIED]** for the critique; **[JUDGMENT]** for the layout conclusion.

---

## 3. Histogram / binning design for heavy-tailed data

### 3.1 The distribution you are actually plotting

Software artifact sizes are heavy-tailed, at many levels of abstraction, across languages and systems: "distributions with long, fat tails in software are much more pervasive than already established, appearing at various levels of abstraction, in diverse systems and languages." **[VERIFIED]** (Louridas, Spinellis & Vlachos 2008, ACM TOSEM 18(1):1–26)

PL-specific fits are usually reported with exponents around k ≈ 1.7–2.6 for incoming dependency counts; for *file sizes* the tail is at minimum log-normal-ish and often Pareto-like. **[VERIFIED]** for the general claim and the k values in the dependency case; **[UNVERIFIED]** for the exact file-size family — do not assert "power law" in the UI. **[JUDGMENT]**

This kills the default histogram: with linear bins on a distribution whose median is ~80 lines and whose max is ~12,000, you get **one bar and a flat line**.

### 3.2 Why the textbook bin rules fail here

- **Sturges' rule** (`k = ⌈log₂ n + 1⌉`): assumes approximate normality.
- **Freedman–Diaconis** (`width = 2·IQR·n^(-1/3)`): explicitly "a **normal reference rule** like Scott's rule", differing only in using a rank-based spread estimate. **[VERIFIED]** (astropy docs, describing the FD rule) On a heavy tail the IQR is tiny relative to the range, so FD produces a huge bin count and a comb-like histogram. In the wild: someone applying FD to severely skewed data got *126,044 bins*. **[VERIFIED]** (a StackExchange/Reddit report; anecdotal but exactly the observed failure).
- Verdict: **neither is appropriate.** **[JUDGMENT]**

### 3.3 The three viable schemes, and when each wins

| Scheme | Bins | Pros | Cons |
|---|---|---|---|
| **Fixed interpretive bins** (domain thresholds) | 6–12 | Every bin has a *meaning* ("over 1,000 lines" is a real category), stable across repos, comparable between runs, label-able | Bin edges are arbitrary; can hide structure inside a bin |
| **Log-equal-width** (constant width in log space) | 8–14 | Matches the distribution; no dead bins; every bar has comparable counts | Bar *widths on screen* become unequal or you must lie about them; visually unfamiliar |
| **Quantile bins** (equal file counts per bin) | 5–10 | Every bar has equal height by construction → shape invisible; but excellent for "which decile is this file in" | **Destroys the shape signal entirely.** Useless for the "distribution shape" job |

**Recommendation: fixed interpretive bins with a log-ish progression, rendered on a linear x-axis.** This is the only option that simultaneously gives a readable shape, stable cross-repo comparability, and labels a developer can act on. **[JUDGMENT]**

Log-equal-width is a reasonable `--bins log` escape hatch for very large repos. Quantile bins should be used **only** for the file-ranking view, never for the distribution chart.

### 3.4 Recommended default bins

**LOC per file (11 bins).** Progression: fine resolution where the mass is, coarse where the tail is. Doubling to 1,000, then wider.

```
0–49 · 50–99 · 100–199 · 200–299 · 300–499 · 500–749 · 750–999
· 1,000–1,999 · 2,000–4,999 · 5,000–9,999 · ≥10,000
```

Why these edges: 100–500 is where most hand-written source files live (that's where you need resolution); 1,000+ is where "this file is a problem" starts; the final open bin catches generated/vendored monsters without changing the axis. **[JUDGMENT]**; the |shape| is consistent with the heavy-tail evidence.

**CCN per function (6 bins).** These are the literature bands, deliberately:

```
1 · 2–4 · 5–10 · 11–20 · 21–50 · ≥51
```

Rationale: bin edges *are* the published thresholds, so the chart is self-documenting — "the bar to the right of the gap is the over-10 population." Do not invent edges here; the whole value is that 11 and 21 and 51 mean something. **[JUDGMENT]** on the split of 1–10 into three sub-bins (the McCabe band 1–10 is too coarse for shape; CCN 1 vs CCN 10 is a 10× behavioral difference). **[VERIFIED]** for the band boundaries.

**CCN-max per file (6 bins).** Same six edges as above. Note in the axis label that this is `max`, not sum — otherwise readers will compare it to a Sonar "file complexity" number and conclude you're wrong.

**Density (CCN per line), 6 bins [JUDGMENT, needs calibration]:**

```
<0.10 · 0.10–0.15 · 0.15–0.20 · 0.20–0.25 · 0.25–0.30 · ≥0.30
```

Anchor for that range, computed from scc's own published redis output (per-language totals ÷ code lines):
- C: 190,252 code lines, 48,269 complexity → **0.254**
- Python: 3,610 / 621 → **0.172**
- C Header: 31,881 / 3,097 → **0.097**
**[VERIFIED]** as arithmetic on scc's README numbers; **[UNVERIFIED]** as a claim about your codebases — scc's counter is a keyword approximation, so treat this as a sanity check on the order of magnitude, not a standard.

### 3.5 The long tail: what to do with it

1. **Never let one file set the x-axis max.** Use `p99` of the metric as the axis end and put everything above it in a final open `≥ p99` bin. Otherwise a single 40,000-line generated file compresses 99.9% of the chart into one pixel. **[JUDGMENT]**
2. **Label the open bin with three numbers:** file count, % of files, and the max value inside it (`"≥10,000 · 3 files · 0.2% · max 41,208"`). This is exactly the pitfall where "the log histogram hides outliers in count": a bar of height 3 looks identical whether it holds 3 files at 12k or 3 files at 900k. **[JUDGMENT]**
3. **Show `n` and `%` on every bar**, not just height. Height is count; the reader needs both count and share, because the bins have wildly different widths.
4. **Cross-encode mass vs count.** Two honest options: (a) a second, lighter bar per bin showing **% of lines** (not files) — this immediately reveals that the ≥2,000 bin holds 1% of files and 30% of lines; or (b) sort nothing and just print both numbers. I prefer (a): it is the single most informative addition to a LOC histogram, and it's what makes the "20% of files = 80% of lines" fact visible rather than asserted. **[JUDGMENT]**

### 3.6 Pitfalls checklist

| Pitfall | Consequence | Fix |
|---|---|---|
| Linear bins on a heavy tail | One bar; all information lost | Fixed log-ish bins (§3.4) |
| Log-*count* y-axis ("log histogram") | Visually flattens the tail so 3 files and 30 files look similar; readers habituated to linear bar height misread it | Keep **linear count**; compress the *x* axis instead |
| Reporting the mean LOC | Mean of a right-skewed distribution sits above the median and describes no actual file | median, p90, p99, max. Never mean. |
| Summed CCN per file | Re-plots LOC; near-collinear | Per-function distribution + max + count-over-10 |
| Different bin edges between two runs | Two reports look like different repos | Version the bin spec in the report footer |
| Excluding generated/vendored silently | Score flatters or damns with no visible cause | Explicit `files_excluded: {generated: 12, vendored: 340}` in the header |
| Percentages of a tiny repo | "50% of files are complex" = 2 of 4 | Below ~50 files, show counts only and say so |
| Comparing bins across languages | Python's density ≠ C's density | Language filter chip on the chart; default to all, label the caveat |

---

## 4. Visualization approaches compared

### 4.1 The comparison

Requirements from the brief: zero-install, small artifact, hover to see file names, exportable/shareable, dark mode, accessible. Scored:

| Approach | Zero-install | Artifact size | Interactivity | Shareable | Dark mode | Accessibility |
|---|---|---|---|---|---|---|
| **Terminal, ANSI + Unicode blocks** (`▁▂▃▄▅▆▇█`) | High if shipped as a single static binary; **zero** if it needs a runtime | ~2–5 KB of stdout | None (no hover); at best a full TUI with keyboard nav | Screenshot only; pasting into Slack destroys alignment | Inherits terminal theme (usually fine) | Poor: a bar chart made of glyphs is unreadable by a screen reader and by many terminal emulators' font fallbacks |
| **Terminal, ratatui TUI** | Same as above; Rust lib, "sub-millisecond rendering", charts/sparklines/tables widgets built in **[VERIFIED]** | ~1–3 MB binary | Keyboard navigation, drill-down panes, scrolling file list | No. Cannot be pasted or attached. | Inherits terminal theme | Poor-moderate. TUI semantics are invisible to AT. |
| **Static HTML, hand-rolled inline SVG** | **Highest** — every dev machine has a browser | **~30–60 KB** self-contained (data + CSS + ~150 lines of JS) | Yes: hover tooltips, click-to-drill, sortable table, all vanilla JS | **Yes** — one file, email/Slack/commit-able | `prefers-color-scheme` + manual toggle | **Best**: real DOM, `role="img"` + `aria-label`, hidden `<table>` equivalent, keyboard-focusable bars, text scales with browser zoom |
| **Static HTML + uPlot** | High | 47.9 KB lib + data ≈ **90 KB** **[VERIFIED]** size table: uPlot v1.6.24 = 47.9 KB | Yes, excellent (cursor, legend, zoom) | Yes | Plugin/theme | Moderate: canvas → no DOM semantics, needs an a11y layer anyway |
| **Static HTML + Chart.js** | High | 254 KB lib ≈ **300 KB** **[VERIFIED]** | Yes | Yes | Yes | Moderate (canvas) |
| **Static HTML + ECharts** | High | 1,000 KB lib ≈ **1.1 MB** **[VERIFIED]** | Yes | Yes, but a 1 MB attachment is annoying | Yes | Moderate (canvas) |
| **Static HTML + Observable Plot / D3** | High | Plot ~250 KB, D3 modular but you own the composition | Yes | Yes | Manual | SVG → good, *if* you add the semantics yourself |
| **TUI + HTML hybrid** (`--open`) | High | = HTML + ~200 lines of terminal code | Full HTML interactivity, plus a glanceable terminal summary | Yes (the HTML half) | Both | Both, and the terminal half can be `--quiet` for CI |

Size numbers are from uPlot's own benchmark table (2023-03-11, including required deps). **[VERIFIED]**

### 4.2 Recommendation, with reasoning

**Primary artifact: one self-contained HTML file, ~30–60 KB, inline SVG bars, no chart library.** `--open` by default; `--no-open` and `--json` for CI.

Reasoning, in priority order:

1. **The chart has ~11 bars.** A 48 KB time-series library with zoom, streaming, and OHLC support is solving a problem I don't have. uPlot is the *right* choice among libraries precisely because it's 48 KB and explicitly refuses to do data processing ("No general-purpose data parsing, aggregation, or statistical processing") **[VERIFIED]** — but even 48 KB is more than the whole chart. With 11 `<rect>` elements and a `<title>` per bar, hover tooltips work natively in every browser with **zero JS**. **[JUDGMENT]**
2. **Hover-to-see-file-names is a *listing* interaction, not a *chart* interaction.** The histogram bar tooltip should say "100–199 lines · 412 files · 18% of files · 31% of lines · examples: src/api/handler.ts, …". But the real "which file" job is the ranked table underneath. A canvas chart library makes the table harder, not easier.
3. **Shareability is the feature that sells the tool.** "Run this and send the HTML to your tech lead" is a qualitatively different product from "run this and read the terminal". A 40 KB file attaches to anything; a 1.1 MB ECharts bundle does not.
4. **Accessibility is free with SVG/DOM and expensive with canvas.** With inline SVG you get: text that scales with browser zoom, selectable/copyable labels, `aria-label` on each bar, a visually-hidden `<table>` carrying the same numbers, and `prefers-reduced-motion` for free. With canvas you must rebuild all of it.
5. **Dark mode** is ~15 lines of CSS custom properties + `@media (prefers-color-scheme: dark)`. Worth doing properly rather than inverting: keep the same *lightness ordering* in both themes so the color encoding doesn't flip meaning.

**Secondary artifact: a 12–15 line terminal summary**, always printed, structured as:

```
  CCN-1 health   72 / 100   grade C
  1,284 files · 214,903 lines · 3,102 functions
  LOC     median 84 · p90 312 · p99 1,940 · max 12,408   (top 10% of files = 61% of lines)
  CCN     median 2 · p90 7 · p99 19 · max 74             (43 functions > 10, 6 > 20, 1 > 50)
  worst   src/parser/grammar.ts  CCN 74  R 0.62  1,930 lines
  → report: file:///C:/.../ccn-report.html
```

This is the CI-friendly surface: greppable, pipeable, diffable, and it degrades gracefully when stdout is a pipe (`--ci` forces ASCII, exactly as scc does with its `--ci` flag **[VERIFIED]**).

**Why not a TUI?** A TUI is a second UI to build, test, and make accessible, and it cannot be shared. ratatui is excellent **[VERIFIED]** for tools whose value is *exploration* (browsing a repo tree, filtering). Here the value is a *report* — computed once, read once, forwarded. The right place for interactivity is the HTML. **[JUDGMENT]**

**Why not hybrid-with-a-TUI?** `--open` + terminal summary already is the hybrid, at 5% of the cost of a TUI. **[JUDGMENT]**

### 4.3 Concrete layout

Three stacked panels in one column, vertical scroll, no navigation chrome:

```
┌──────────────────────────────────────────────────────────────────┐
│  CCN-1  72/100  grade C        <repo name> · 1,284 files         │  ← verdict
│  ──── 0.18 of lines above the McCabe limit (log-scaled avg)      │  ← derivation
├──────────────────────────────────────────────────────────────────┤
│  CCN per function                    ▁▃█▆▂▁                      │  ← shape
│  1  ████████████████████████ 1,412  45.5%                        │
│  2–4 ██████████████ 812             26.2%                        │
│  5–10 ██████ 361                    11.6%                        │
│  11–20 ███ 190                      6.1%   ← McCabe limit        │
│  21–50 █ 70                         2.3%                         │
│  ≥51 ▏ 6                            0.2%   max 74                │
├──────────────────────────────────────────────────────────────────┤
│  LOC per file    (dual bar: % of files | % of lines)             │  ← size + tail
├──────────────────────────────────────────────────────────────────┤
│  Top 20 by risk   file · LOC · max CCN · over-10 · churn · R     │  ← outliers
└──────────────────────────────────────────────────────────────────┘
```

Design notes **[JUDGMENT]**:

- **Order matters:** verdict → shape → outliers. A developer who reads only the top 200px must still get the answer.
- **Mark the threshold on the chart** with a vertical rule at the 10 boundary and the words "McCabe limit" — this is the single most useful annotation you can add, and it converts a histogram into an argument.
- **Color:** one hue, ordered lightness ramp, plus a neutral. Never red/green as the only carrier of "bad/good" (WCAG 1.4.1 Use of Color); put the grade letter and the number next to the color so color is redundant. Ensure ≥3:1 contrast for the bar edges against background (WCAG 1.4.11 Non-text Contrast).
- **The duality of the LOC chart** (files vs lines) is the thing that makes the report feel smart. Two 6px bars per bin, or one bar with a lighter inner bar.
- **No pie charts, no 3D, no animations over 150ms.** A 3D city (CodeCity) is a wonderful research artifact and a terrible daily tool: occlusion, perspective distortion, and no way to read a number off it.

---

## 5. Prior art: how existing tools actually present this

| Tool | What it shows | Encoding | What to steal | What to avoid |
|---|---|---|---|---|
| **SonarQube** | Per-function CCN and cognitive complexity, summed to project; project/folder/file measures; A–E ratings. Ratings are ratio-based, e.g. maintainability `sqale_rating`: A ≤5%, B 5–10%, C 10–20%, D 20–50%, E ≥50% of technical-debt ratio **[VERIFIED]** | Letter grades + treemap + per-file drill-down; not a distribution chart | The **threshold-relative rating with published cut points**. The idea that a grade must come with its percentage. | The letter-grade-only summary: it tells you the repo is "C" and nothing about whether that's one monster file or a thousand mediocre ones. |
| **CodeScene** | "Code health" per module (a composite of many factors), plus **hotspots** = complexity × change frequency, plus **complexity trends** over history **[VERIFIED]** | Scatter/system map: complexity on one axis, change frequency on the other, color = code health **[VERIFIED]**; separate trend line per hotspot | The **two-axis scatter** for the *ranking* view: it makes "complex but dormant" visibly different from "complex and hot". And the explicit "here is what you do *not* need to fix" message. | Closed, unpublished composite weights. Their marketing claim of "6x more accurate than SonarQube" **[VERIFIED]** as a claim, but unverifiable. Don't imitate opaque scoring. |
| **CodeClimate** | Maintainability per file as a letter grade, from a fixed set of per-language "checks" with point deductions | Per-file list, repo GPA | Per-file grade granularity; language-specific checks | The GPA framing invites gaming and says nothing about distribution. Also: it was historically SaaS-first, not a local artifact. |
| **scc** | Per-language table with `Complexity` **summed per language**; `--by-file` for per-file; `--sort complexity`; `--hotspots` "files ranked by complexity × change frequency"; `--cognitive` "nesting-weighted complexity"; `--report` writes a **self-contained HTML infographic** **[VERIFIED]** | Tabular, aligned, fixed-width, with thousands separators. HTML report is a card-based infographic. | (a) `--hotspots` as a first-class flag — validation that this is the expected feature; (b) the single-file HTML report; (c) `--ci` ASCII mode; (d) the honest disclaimer about cross-language comparability, surfaced in the README rather than buried. | The complexity column being a language sum with no unit of meaning. Reading redis's "C: 48,269" tells you nothing you can act on. |
| **cloc** | `--by-file` gives a plain table of blank/comment/code per file; language omitted in `--by-file` "to save screen real estate" **[VERIFIED]** | Plain text table, aligned columns | The **table as a first-class output**, and `--by-file` as the obvious companion to the aggregate. The `--diff` mode (added/removed/modified/same) is a great model for a `--since` flag. **[VERIFIED]** | No aggregation, no shape, no thresholds. It's a counter, not an analyzer. |
| **CodeCity** (Wettel & Lanza, 2008) | Language-independent interactive 3D "city": classes as buildings, metrics mapped to footprint and height; designed for large-scale system comprehension and evolution **[VERIFIED]** | 3D, perspective, interactive | The intuition that **area should encode size and height should encode a ratio**, so a "tall thin" building = small file, high complexity. That intuition is worth stealing into 2D. | Literally everything else. Perspective + occlusion + no numbers on screen. |
| **Gource** | Animated force-directed visualization of commits over time | Animation | Nothing for this tool. | Animations that encode history you can't pause, scrub, or cite. |

**Synthesis [JUDGMENT]:** every one of these tools picks *one* of {shape, outliers, verdict} and does it well. The gap is a tool that does all three in one artifact, with the verdict's derivation visible. That is the product.

The closest existing thing is CodeScene's two-axis system map. Its weakness for a *local, zero-config* tool is that it requires ~12 months of git history and a composite health model. A tool that works with **zero git** (CCN-only mode) and *upgrades* to hotspot mode when `.git` exists is a real differentiator. **[JUDGMENT]**

---

## 6. Recommended specification

### 6.1 Per file

```
path                  string
lang                  string            # from extension + shebang
loc                   int               # non-blank, non-comment physical lines
loc_total             int               # physical lines
comments              int
functions             int
ccn_sum               int               # Σ M(f)   — for budgeting, NOT health
ccn_max               int               # max M(f) — primary outlier signal
ccn_p90               int               # needs ≥5 functions, else null
ccn_over_10           int               # |{f : M(f) > 10}|
ccn_over_20           int
ccn_over_50           int
cog_sum               int?              # Sonar cognitive, if parser available
cog_max               int?
density               float             # ccn_sum / loc
density_shrunk        float             # (ccn_sum + 200·d₀) / (loc + 200)
risk                  float ∈ [0,1]     # Σ LOC(f)·p(M(f)) / Σ LOC(f)
commits_365d          int?              # git only
lines_changed_365d    int?              # Σ added+deleted
loc_delta_365d        int?
last_touched          date?
hotspot               float? ∈ [0,1]    # rank_pct(commits) × rank_pct(risk)
is_generated          bool              # marker scan; excluded from repo score
is_vendored           bool              # path heuristic; excluded from repo score
```

with

```
p(c)      = clamp( log10(max(c,1)/10) / log10(10), 0, 1 )
risk(F)   = Σ_f loc(f)·p(ccn(f)) / Σ_f loc(f)      # Σ_f loc(f) > 0
```

### 6.2 Per repo

```
files_analyzed, files_excluded_by_reason
loc_total, functions_total
loc_median, loc_p90, loc_p99, loc_max
loc_share_top10pct                      # concentration
ccn_median, ccn_p90, ccn_p99, ccn_max   # over FUNCTIONS, not files
pct_functions_over_10 / _20 / _50
ccn_per_kloc                            # Σccn / (loc/1000)
health       = 100 · (1 − D),  D = Σ_all_f loc·p(ccn) / Σ_all_f loc
grade        = A/B/C/D/E per §2.4 table
hotspot_debt_share                      # git only, §2.4B
languages[]  = {lang, files, loc, median_ccn}
```

### 6.3 Bin spec (versioned, printed in the report footer)

```json
{
  "bins_version": 1,
  "loc":     [0,50,100,200,300,500,750,1000,2000,5000,10000,null],
  "ccn":     [1,2,5,11,21,51,null],
  "density": [0.0,0.10,0.15,0.20,0.25,0.30,null],
  "tail_cap": "p99"
}
```

`null` = open-ended final bin. `tail_cap: p99` means: if p99 exceeds the last finite edge, keep the fixed edges (they're interpretive) and label the final bin with `max`. Do **not** dynamically rescale the interpretive bins.

### 6.4 Visualization

- **One self-contained `.html`**, inline `<svg>` bars, inline CSS custom properties, ~150 lines of vanilla JS for tooltip + sort + filter.
- **Panels:** verdict → CCN-per-function histogram (with McCabe rule annotation) → LOC-per-file histogram (dual-encoded files/lines) → top-20 risk table.
- **Terminal summary always printed**; `--json` for machines; `--ci` for ASCII.
- **Dark mode** via `prefers-color-scheme` + a toggle that persists in `localStorage`.
- **A11y:** `role="img"` + `aria-label` per chart, a `sr-only` `<table>` with identical numbers, bars as `<g tabindex="0">` with focus rings, color never the sole carrier, `prefers-reduced-motion` respected.
- **Empty/degenerate states:** <50 files → counts only, no percentages, and say why. 0 functions parsed → skip the CCN panel with a note rather than rendering an empty chart.

---

## 7. Honest gaps in this research

1. **McCabe 1976 and NIST SP 500-235 were read through secondary sources.** Both are PDFs my fetch tool rejects (`Unsupported fetched file content type: application/pdf`). The Wikipedia article quotes NIST 500-235 directly and I have cited it as such, but **before publishing the tool's docs, read the actual NIST PDF** — it is the one document that would let you defend the 10/15 numbers.
2. **The Sonar cognitive-complexity white paper's boolean-operator rule is unverified.** I have the three top-level rules and the worked examples from the blog, but the "increment per sequence of like operators" detail is from memory. Verify against the PDF or the `eslint-plugin-sonarjs` implementation before implementing.
3. **My health-score constants (the log base-10 mapping, the 200-pseudo-line shrinkage, the A–E cut points) are uncalibrated.** They are internally consistent and anchored to a real threshold, but I have not run them on a corpus. This is the single highest-risk part of the design.
4. **The density bins are estimated,** anchored only on arithmetic over scc's redis table.
5. **No measurement of my own** — everything quantitative here is either quoted from a source or arithmetic on a quoted source.

---

## 8. Sources actually visited (fetched this session)

Primary / methodology:
- McCabe 1976 bibliographic record, formulas, NIST quote, threshold attribution — https://en.wikipedia.org/wiki/Cyclomatic_complexity
- SonarSource, *Cognitive Complexity, Because Testability != Understandability* (2016), G. Ann Campbell — https://www.sonarsource.com/blog/cognitive-complexity-because-testability-understandability/
- Sonar metric definitions (CCN = 1 + branches; sum at project level; ncloc; issue_density; ratings) — https://docs.sonarsource.com/sonarqube-cloud/managing-your-projects/metric-definitions.md
- Louridas, Spinellis & Vlachos, *Power Laws in Software*, ACM TOSEM 18(1), 2008 — https://www.spinellis.gr/pubs/jrnl/2008-TOSEM-PowerLaws/html/LSV08.html

Churn / hotspots:
- Tornhill, *Managing technical debt: measure the impact and prioritize improvements guided by development data*, CodeScene (2021) — https://codescene.com/blog/prioritize-technical-debt-by-impact/

Prior art / tool behavior:
- scc README (complexity disclaimer, `--hotspots`, `--cognitive`, `--report`, redis output table) — https://github.com/boyter/scc
- cloc README (`--by-file`, `--diff`, `--sql`) — https://github.com/AlDanial/cloc
- uPlot README (size + benchmark table: uPlot 47.9 KB / Chart.js 254 KB / ECharts 1000 KB) — https://github.com/leeoniya/uPlot
- Ratatui — https://ratatui.rs/

Supporting (found via search, abstract/metadata read):
- Gill & Kemerer, *Cyclomatic complexity density and software maintenance productivity* (1991) — https://sites.pitt.edu/~ckemerer/CK%20research%20papers/CyclomaticComplexityDensity_GillKemerer91.pdf
- Nagappan & Ball, *Use of Relative Code Churn Measures to Predict System Defect Density* (ICSE 2005) — https://www.microsoft.com/en-us/research/publication/use-of-relative-code-churn-measures-to-predict-system-defect-density/
- Hassan, *Predicting Faults Using the Complexity of Code Changes* (ICSE 2009) — https://dl.acm.org/doi/10.1109/ICSE.2009.5070510
- Freedman–Diaconis rule described as a normal reference rule — https://docs.astropy.org/en/stable/api/astropy.stats.freedman_bin_width.html
- CodeCity, Wettel & Lanza (2008) — https://dl.acm.org/doi/10.1145/1370175.1370188

Referenced but **not** fetched (do not cite as verified):
- NIST SP 500-235, Watson & McCabe 1996 — http://www.mccabe.com/pdf/mccabe-nist235r.pdf (PDF, blocked)
- McCabe, *Software Quality Metrics to Identify Risk* (DHS presentation, 2008) — archived at https://web.archive.org/web/20220329072759/http://www.mccabe.com/ppt/SoftwareQualityMetricsToIdentifyRisk.ppt
- SonarSource cognitive complexity white paper — https://www.sonarsource.com/docs/CognitiveComplexity.pdf (PDF, blocked)
- WCAG 2.2 SC 1.4.1 Use of Color — https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html
- WCAG 2.2 SC 1.4.11 Non-text Contrast — https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html
- Tornhill, *Your Code as a Crime Scene* (Pragmatic Bookshelf) and *Software Design X-Rays* — book references; not read this session.
