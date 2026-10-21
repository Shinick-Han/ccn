//! Report metrics: histogram binning, per-file risk, repo health.
//!
//! This module owns every number that appears in the HTML report. The report
//! renderer is deliberately dumb: it formats a `Report`, it does not compute.
//!
//! Design notes (see `docs/PLAN.md` §5):
//! - Bins are **fixed and interpretive**, never derived from the data. Two runs
//!   on different repos must be comparable, and every bin edge is a threshold
//!   a developer already knows (10 = McCabe, 1000 lines = "this file is a
//!   problem").
//! - Health is `100 * (1 - D)` where `D` is the size-weighted average of a
//!   log-scaled penalty per function. It is deliberately **not** monotone in
//!   LOC, so a repo does not get sicker just by growing.
//! - Heuristic files never enter function statistics. A file-level keyword
//!   count and an AST-derived per-function CCN are different measurements and
//!   mixing them is how "complexity" tools lose credibility.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use crate::ccn::types::CcnTier;
use crate::git::{self, Churn};
use crate::model::{percentile, FileLoc, ScanResult};

/// Fixed edges plus human labels. `edges.len() == labels.len()`: the last bin
/// is open-ended.
pub struct BinSpec {
    pub edges: &'static [u32],
    pub labels: &'static [&'static str],
}

pub static LOC_BINS: BinSpec = BinSpec {
    edges: &[0, 50, 100, 200, 300, 500, 750, 1000, 2000, 5000, 10000],
    labels: &[
        "0–49",
        "50–99",
        "100–199",
        "200–299",
        "300–499",
        "500–749",
        "750–999",
        "1,000–1,999",
        "2,000–4,999",
        "5,000–9,999",
        "≥10,000",
    ],
};

/// The bin edges *are* the published thresholds, so the chart explains itself.
pub static CCN_BINS: BinSpec = BinSpec {
    edges: &[1, 2, 5, 11, 21, 51],
    labels: &["1", "2–4", "5–10", "11–20", "21–50", "≥51"],
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Bin {
    pub label: String,
    /// Inclusive lower edge. Always present.
    pub lo: u32,
    /// Exclusive upper edge; `None` for the open-ended final bin.
    pub hi: Option<u32>,
    pub count: usize,
    /// Sum of the optional weight (lines for LOC bins); 0 when unweighted.
    pub weight: u64,
    pub share_count: f64,
    pub share_weight: f64,
    /// Largest value that landed in this bin. Makes the open bin honest: a bar
    /// of height 3 looks identical whether the max is 12k or 900k.
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Histogram {
    pub bins: Vec<Bin>,
    pub total: usize,
    pub total_weight: u64,
    /// `true` when the caller supplied weights (the LOC chart's dual encoding).
    pub weighted: bool,
}

impl Histogram {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// Build a histogram over `values`, optionally weighted (e.g. lines per file).
pub fn histogram(values: &[u32], weights: Option<&[u32]>, spec: &BinSpec) -> Histogram {
    assert_eq!(
        spec.edges.len(),
        spec.labels.len(),
        "bin spec must label every edge"
    );

    let total_weight: u64 = weights
        .map(|w| w.iter().map(|v| u64::from(*v)).sum())
        .unwrap_or(0);

    let mut bins: Vec<Bin> = spec
        .edges
        .iter()
        .enumerate()
        .map(|(i, lo)| Bin {
            label: spec.labels[i].to_string(),
            lo: *lo,
            hi: spec.edges.get(i + 1).copied(),
            count: 0,
            weight: 0,
            share_count: 0.0,
            share_weight: 0.0,
            max: 0,
        })
        .collect();

    for (i, value) in values.iter().enumerate() {
        // First bin is clamped at its lower edge so a 0-line file is counted
        // rather than dropped; the rest use [lo, hi).
        let idx = spec
            .edges
            .iter()
            .rposition(|e| value >= e)
            .unwrap_or_default();
        let bin = &mut bins[idx];
        bin.count += 1;
        bin.max = bin.max.max(*value);
        if let Some(w) = weights {
            bin.weight += u64::from(w.get(i).copied().unwrap_or(0));
        }
    }

    for bin in &mut bins {
        bin.share_count = if values.is_empty() {
            0.0
        } else {
            bin.count as f64 * 100.0 / values.len() as f64
        };
        bin.share_weight = if total_weight == 0 {
            0.0
        } else {
            bin.weight as f64 * 100.0 / total_weight as f64
        };
    }

    Histogram {
        bins,
        total: values.len(),
        total_weight,
        weighted: weights.is_some(),
    }
}

/// `p(c) = clamp(log10(max(c,1)/10), 0, 1)`.
///
/// Zero at McCabe's limit of 10, 0.30 at 20, 0.70 at 50, 1.0 at 100. Bounded so
/// one generated 900-CCN function cannot decide a repository's grade.
pub fn penalty(ccn: u32) -> f64 {
    let c = f64::from(ccn.max(1));
    (c / 10.0).log10().clamp(0.0, 1.0)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Health {
    /// 0..=100. Higher is healthier.
    pub score: f64,
    pub grade: char,
    /// Fraction of measured lines living above the McCabe limit,
    /// size-weighted on a log scale. `score == 100 * (1 - debt)`.
    pub debt: f64,
    /// Functions and lines that fed the score (heuristic files excluded).
    pub functions: usize,
    pub lines: usize,
    pub over_10: usize,
    pub over_20: usize,
    pub over_50: usize,
    /// Share of measured functions above 10, in percent.
    pub pct_over_10: f64,
}

/// Grade bands, calibrated on 11 real repositories (2026-10-21).
///
/// The cut points are round numbers **in debt terms**, which is what the score
/// actually measures:
///
/// | Grade | Debt (share of lines above CCN 10) | Score |
/// |-------|-----------------------------------|-------|
/// | A     | < 2.5%                            | >= 97.5 |
/// | B     | 2.5% - 6%                         | >= 94   |
/// | C     | 6% - 12%                          | >= 88   |
/// | D     | 12% - 21%                         | >= 79   |
/// | E     | >= 21%                            | < 79    |
///
/// A repo with ~1% of functions above CCN 10 lands at A because that is
/// genuinely simple code; the bands are not a percentile. See
/// `docs/calibration.md` for the corpus and the observed spread.
pub fn grade_for(score: f64) -> char {
    if score >= 97.5 {
        'A'
    } else if score >= 94.0 {
        'B'
    } else if score >= 88.0 {
        'C'
    } else if score >= 79.0 {
        'D'
    } else {
        'E'
    }
}

/// Size-weighted, log-scaled debt over every measured function.
///
/// Generated and vendored files are skipped here as well as in [`build`], so a
/// caller that hands over the raw `ScanResult.files` still cannot score a
/// checked-in bundle.
pub fn health(files: &[FileLoc]) -> Option<Health> {
    let mut debt_mass = 0.0f64;
    let mut lines = 0u64;
    let mut functions = 0usize;
    let mut over_10 = 0usize;
    let mut over_20 = 0usize;
    let mut over_50 = 0usize;

    for file in files {
        if file.is_excluded() {
            continue;
        }
        let Some(summary) = &file.ccn else { continue };
        if summary.tier == CcnTier::Heuristic {
            continue;
        }
        for f in &summary.detail {
            // A function with no recorded body still has a CCN; weight it as
            // one line rather than dropping it from the denominator.
            let w = f.nloc.max(1) as f64;
            debt_mass += w * penalty(f.ccn);
            lines += f.nloc.max(1) as u64;
            functions += 1;
            over_10 += usize::from(f.ccn > 10);
            over_20 += usize::from(f.ccn > 20);
            over_50 += usize::from(f.ccn > 50);
        }
    }

    if functions == 0 {
        return None;
    }

    let debt = (debt_mass / lines as f64).clamp(0.0, 1.0);
    let score = 100.0 * (1.0 - debt);
    Some(Health {
        score,
        grade: grade_for(score),
        debt,
        functions,
        lines: lines as usize,
        over_10,
        over_20,
        over_50,
        pct_over_10: over_10 as f64 * 100.0 / functions as f64,
    })
}

/// One row of the "worst files" table.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RankedFile {
    pub path: String,
    pub lang: String,
    pub loc: u32,
    pub ccn_max: u32,
    pub ccn_sum: u32,
    pub over_10: usize,
    pub functions: usize,
    /// Size-weighted penalty within the file, 0..=1. Same formula as the repo
    /// score, so the column is comparable to the health number.
    pub risk: f64,
}

fn file_risk(file: &FileLoc) -> f64 {
    let Some(summary) = &file.ccn else {
        return 0.0;
    };
    if summary.tier == CcnTier::Heuristic || summary.detail.is_empty() {
        return 0.0;
    }
    let mut mass = 0.0;
    let mut lines = 0u64;
    for f in &summary.detail {
        let w = f.nloc.max(1) as f64;
        mass += w * penalty(f.ccn);
        lines += f.nloc.max(1) as u64;
    }
    if lines == 0 {
        0.0
    } else {
        mass / lines as f64
    }
}

/// One row of the "hotspots" table.
///
/// `hotspot = rank_pct(commits_365d) × rank_pct(risk)`, both ranks taken over
/// the files in this table only. See [`score_hotspots`] for the tie rule.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HotspotRow {
    pub path: String,
    pub loc: u32,
    pub ccn_max: u32,
    /// Same size-weighted penalty as [`RankedFile::risk`].
    pub risk: f64,
    pub commits: u32,
    pub lines_changed: u32,
    /// 0..=1. A file with no commit in the window is always exactly 0.
    pub hotspot: f64,
    /// Author with the most commits to this path in the window. Empty when git
    /// history was not collected.
    pub top_author: String,
    /// Number of distinct authors that touched the path in the window.
    pub authors: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RepoStats {
    pub loc_median: u32,
    pub loc_p90: u32,
    pub loc_p99: u32,
    pub loc_max: u32,
    pub ccn_median: u32,
    pub ccn_p90: u32,
    pub ccn_p99: u32,
    pub ccn_max: u32,
    pub functions_total: usize,
    /// Share of all code lines contained in the largest 10% of files, percent.
    pub loc_share_top10pct: f64,
    pub files_exact: usize,
    pub files_heuristic: usize,
    pub files_without_ccn: usize,
    /// Files dropped from every statistic above: generated first, then
    /// vendored. Always 0 under `--include-generated`.
    pub files_excluded_generated: usize,
    pub files_excluded_vendored: usize,
}

/// Everything the renderer needs. Built once, rendered twice (terminal + HTML).
///
/// Every distribution, score and ranking here is computed over **measured**
/// files only: generated and vendored files are subtracted before any number is
/// taken (see `docs/PLAN.md` §10). They stay visible in the `--json` contract
/// via [`ScanResult::files`], which is why the two file counts differ.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Report {
    pub root: String,
    /// Every file found, including the excluded ones.
    pub files_analyzed: usize,
    /// Files that fed the numbers below: `files_analyzed - excluded`.
    pub files_measured: usize,
    /// Physical lines of the measured files. Excluded files' lines are not in
    /// here, matching the LOC histogram below.
    pub loc_total: usize,
    pub loc: Histogram,
    /// CCN per function (the primary chart).
    pub ccn: Histogram,
    /// Worst function per file.
    pub ccn_file: Histogram,
    pub health: Option<Health>,
    pub stats: RepoStats,
    pub top: Vec<RankedFile>,
    /// `true` when git history was collected; the hotspot fields below are only
    /// meaningful when it is.
    pub git_available: bool,
    /// Commit records seen in the 365-day window (`--no-merges`).
    pub commits_scanned: u32,
    /// `Σ LOC(top decile by hotspot) / Σ LOC(measured) * 100`, 0.0 when nothing
    /// was measured. Reads as "X% of our lines are both complex and hot".
    pub hotspot_debt_share: f64,
    /// Top 20 by hotspot, already sorted and truncated.
    pub hotspots: Vec<HotspotRow>,
    /// Per-file churn for measured files, keyed by the same relative path as
    /// [`RankedFile::path`]. Renderers join on it; it is not serialised, because
    /// a hash map has no stable order and the `hotspots` table already carries
    /// every number a consumer needs.
    #[serde(skip)]
    pub churn: HashMap<String, Churn>,
    /// Comparison with the previous recorded snapshot. `None` without
    /// `--trend`, or on the first recorded run.
    pub trend: Option<crate::history::Trend>,
    pub bin_spec_version: u32,
    /// Grade bands are not calibrated yet; the report must say so.
    pub health_experimental: bool,
}

pub const BIN_SPEC_VERSION: u32 = 1;
/// Grades are anchored to the reference corpus in `docs/calibration.md`.
pub const HEALTH_EXPERIMENTAL: bool = false;
/// Surfaced in the report footer so a reader knows what "A" was measured against.
pub const HEALTH_CALIBRATION: &str = "grades calibrated on 11 repositories, 2026-10-21";

/// Average-rank percentile of every value, normalised by the population size.
///
/// Ties share the mean of the ranks they span, so two equally-churned files
/// score identically instead of one winning by input order. The result is
/// `avg_rank / n`: a population of one ranks `1.0`, and the lowest value in a
/// larger population ranks `1/n`, never 0 — the only value forced to 0 is a
/// file with no commit, and that is done by the caller.
fn average_ranks<T: PartialOrd + Copy>(values: &[T]) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        values[a]
            .partial_cmp(&values[b])
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });

    let mut ranks = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && values[order[j]] == values[order[i]] {
            j += 1;
        }
        let average = ((i + 1) as f64 + j as f64) / 2.0;
        for &index in &order[i..j] {
            ranks[index] = average / n as f64;
        }
        i = j;
    }
    ranks
}

/// Fill in `hotspot` and put the rows in reporting order.
///
/// `hotspot = rank_pct(commits) × rank_pct(risk)` over the rows themselves
/// (measured files with function-level CCN). Percentile ranks, not raw values:
/// both factors are heavy-tailed, and a raw product is owned by whichever tail
/// is fatter — one 900-commit vendored file would otherwise set the scale.
fn score_hotspots(rows: &mut [HotspotRow]) {
    let commits: Vec<u32> = rows.iter().map(|row| row.commits).collect();
    let risks: Vec<f64> = rows.iter().map(|row| row.risk).collect();
    let commit_rank = average_ranks(&commits);
    let risk_rank = average_ranks(&risks);

    for (i, row) in rows.iter_mut().enumerate() {
        row.hotspot = if row.commits == 0 {
            0.0
        } else {
            commit_rank[i] * risk_rank[i]
        };
    }

    rows.sort_by(|a, b| {
        b.hotspot
            .partial_cmp(&a.hotspot)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.risk.partial_cmp(&a.risk).unwrap_or(Ordering::Equal))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// `Σ LOC(top decile by hotspot) / Σ LOC(measured) * 100`.
///
/// `rows` must already be sorted by [`score_hotspots`]. The denominator is every
/// measured file, not just the ranked ones: the claim is about the repository's
/// lines, and files without CCN are still lines we own.
fn hotspot_decile_share(rows: &[HotspotRow], measured_loc: u64) -> f64 {
    if rows.is_empty() || measured_loc == 0 {
        return 0.0;
    }
    let top_n = ((rows.len() as f64) * 0.10).ceil() as usize;
    let top_loc: u64 = rows.iter().take(top_n).map(|row| u64::from(row.loc)).sum();
    top_loc as f64 * 100.0 / measured_loc as f64
}

/// Build the default report: histograms and health, no git history.
///
/// Git churn costs a `git log` pass (hundreds of milliseconds to a few
/// seconds), so it is opt-in via [`build_with`]; a scan should feel instant by
/// default.
pub fn build(result: &ScanResult) -> Report {
    build_with(result, false)
}

/// Build a report, optionally collecting git churn for hotspot ranking.
pub fn build_with(result: &ScanResult, enable_git: bool) -> Report {
    // The single exclusion point for the report. Everything below reads
    // `measured` and never `result.files`, so a generated file cannot leak into
    // a histogram, the concentration stat, the health score or the top table by
    // being added to one call site and forgotten in another.
    let measured: Vec<&FileLoc> = result.files.iter().filter(|f| !f.is_excluded()).collect();

    let locs: Vec<u32> = measured.iter().map(|f| f.loc as u32).collect();
    let weights: Vec<u32> = measured.iter().map(|f| f.loc as u32).collect();
    let loc_total: usize = measured.iter().map(|f| f.loc_total).sum();

    let mut ccn_values: Vec<u32> = Vec::new();
    let mut ccn_file_values: Vec<u32> = Vec::new();
    let mut ccn_file_weights: Vec<u32> = Vec::new();
    let mut files_exact = 0usize;
    let mut files_heuristic = 0usize;
    let mut files_without_ccn = 0usize;

    for file in &measured {
        match &file.ccn {
            None => files_without_ccn += 1,
            Some(summary) if summary.tier == CcnTier::Heuristic => files_heuristic += 1,
            Some(summary) => {
                files_exact += 1;
                if summary.max > 0 {
                    ccn_file_values.push(summary.max);
                    ccn_file_weights.push(file.loc as u32);
                }
                ccn_values.extend(summary.detail.iter().map(|f| f.ccn));
            }
        }
    }

    let mut sorted_ccn: Vec<usize> = ccn_values.iter().map(|v| *v as usize).collect();
    sorted_ccn.sort_unstable();

    let mut locs_sorted: Vec<usize> = locs.iter().map(|v| *v as usize).collect();
    locs_sorted.sort_unstable();

    // Concentration: largest 10% of files, by line count.
    let top_n = ((measured.len() as f64) * 0.10).ceil() as usize;
    let mut by_loc: Vec<u32> = locs.clone();
    by_loc.sort_unstable_by(|a, b| b.cmp(a));
    let top_loc: u64 = by_loc.iter().take(top_n).map(|v| u64::from(*v)).sum();
    let all_loc: u64 = locs.iter().map(|v| u64::from(*v)).sum();

    let stats = RepoStats {
        loc_median: percentile(&locs_sorted, 50.0) as u32,
        loc_p90: percentile(&locs_sorted, 90.0) as u32,
        loc_p99: percentile(&locs_sorted, 99.0) as u32,
        loc_max: locs_sorted.last().copied().unwrap_or(0) as u32,
        ccn_median: percentile(&sorted_ccn, 50.0) as u32,
        ccn_p90: percentile(&sorted_ccn, 90.0) as u32,
        ccn_p99: percentile(&sorted_ccn, 99.0) as u32,
        ccn_max: sorted_ccn.last().copied().unwrap_or(0) as u32,
        functions_total: ccn_values.len(),
        loc_share_top10pct: if all_loc == 0 {
            0.0
        } else {
            top_loc as f64 * 100.0 / all_loc as f64
        },
        files_exact,
        files_heuristic,
        files_without_ccn,
        files_excluded_generated: result.files_excluded.generated,
        files_excluded_vendored: result.files_excluded.vendored,
    };

    // "Worst" means risk first, then the single worst function as a tiebreak.
    let mut top: Vec<RankedFile> = measured
        .iter()
        .filter_map(|f| {
            let s = f.ccn.as_ref()?;
            if s.tier == CcnTier::Heuristic || s.detail.is_empty() {
                return None;
            }
            Some(RankedFile {
                path: f.path.clone(),
                lang: f.lang.clone(),
                loc: f.loc as u32,
                ccn_max: s.max,
                ccn_sum: s.sum,
                over_10: s.over_10,
                functions: s.functions,
                risk: file_risk(f),
            })
        })
        .collect();
    top.sort_by(|a, b| {
        b.risk
            .partial_cmp(&a.risk)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.ccn_max.cmp(&a.ccn_max))
            .then_with(|| a.path.cmp(&b.path))
    });
    top.truncate(20);

    // --- git churn / hotspots ---------------------------------------------
    // One `git log` pass for the whole repository, auto-detected from the scan
    // root: no flag, no phase-1 hook. Joining here (rather than mutating
    // `FileLoc`) keeps `loc::scan` free of history and makes a scan of a
    // subdirectory of a monorepo work — `prefix_relative_to` rebases the
    // work-tree keys onto the scan root, and a file that does not match simply
    // stays at zero churn.
    let root_path = Path::new(&result.root);
    let history = if enable_git {
        git::collect_history(root_path)
    } else {
        None
    };
    let mut commits_scanned = 0u32;
    let mut churn: HashMap<String, Churn> = HashMap::new();
    // Rebase work-tree keys onto the scan root once; churn and authors share it.
    let prefix = history
        .as_ref()
        .map(|h| git::prefix_relative_to(root_path, &h.work_tree))
        .unwrap_or_default();
    if let Some(history) = &history {
        commits_scanned = history.commits;
        for file in &measured {
            let key = format!("{prefix}{}", file.path);
            if let Some(found) = history.churn.get(&key) {
                churn.insert(file.path.clone(), *found);
            }
        }
    }
    let git_available = history.is_some();

    let mut hotspots: Vec<HotspotRow> = measured
        .iter()
        .filter_map(|f| {
            let summary = f.ccn.as_ref()?;
            if summary.tier == CcnTier::Heuristic || summary.detail.is_empty() {
                return None;
            }
            let change = churn.get(&f.path).copied().unwrap_or_default();
            // Author attribution is keyed like churn (work-tree relative), so
            // it needs the same prefix rebase.
            let author_key = format!("{prefix}{}", f.path);
            let (top_author, authors) = history
                .as_ref()
                .and_then(|h| h.authors.get(&author_key))
                .filter(|map| !map.is_empty())
                .map(|map| {
                    let mut entries: Vec<(&String, &u32)> = map.iter().collect();
                    entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                    (entries[0].0.clone(), map.len())
                })
                .unwrap_or_default();
            Some(HotspotRow {
                path: f.path.clone(),
                loc: f.loc as u32,
                ccn_max: summary.max,
                risk: file_risk(f),
                commits: change.commits,
                lines_changed: change.lines_changed,
                hotspot: 0.0,
                top_author,
                authors,
            })
        })
        .collect();
    score_hotspots(&mut hotspots);
    let measured_loc: u64 = measured.iter().map(|f| f.loc as u64).sum();
    // Without history every hotspot is 0 and "the top decile" would be an
    // alphabetical accident, so the share is reported as unavailable instead.
    let hotspot_debt_share = if git_available {
        hotspot_decile_share(&hotspots, measured_loc)
    } else {
        0.0
    };
    hotspots.truncate(20);

    Report {
        root: result.root.clone(),
        files_analyzed: result.files_analyzed,
        files_measured: measured.len(),
        loc_total,
        loc: histogram(&locs, Some(&weights), &LOC_BINS),
        ccn: histogram(&ccn_values, None, &CCN_BINS),
        ccn_file: histogram(&ccn_file_values, Some(&ccn_file_weights), &CCN_BINS),
        health: health(&result.files),
        stats,
        top,
        git_available,
        commits_scanned,
        hotspot_debt_share,
        hotspots,
        churn,
        trend: None,
        bin_spec_version: BIN_SPEC_VERSION,
        health_experimental: HEALTH_EXPERIMENTAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{CcnSummary, FileCcn, FunctionCcn};

    fn func(ccn: u32, nloc: usize) -> FunctionCcn {
        FunctionCcn {
            name: "f".into(),
            line: 1,
            end_line: nloc,
            nloc,
            ccn,
            params: 0,
        }
    }

    fn file(path: &str, loc: usize, fns: Vec<FunctionCcn>) -> FileLoc {
        let summary = CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions: fns,
        });
        FileLoc {
            path: path.into(),
            lang: "TypeScript".into(),
            loc_total: loc,
            loc,
            comments: 0,
            blanks: 0,
            ccn: Some(summary),
            is_generated: false,
            is_vendored: false,
        }
    }

    /// Minimal `ScanResult` for report tests: the aggregates a real scan would
    /// produce, including the per-bucket exclusion counts.
    fn scan_result(files: Vec<FileLoc>) -> crate::model::ScanResult {
        let loc = files.iter().map(|f| f.loc).sum();
        let loc_total = files.iter().map(|f| f.loc_total).sum();
        let comments = files.iter().map(|f| f.comments).sum();
        let blanks = files.iter().map(|f| f.blanks).sum();
        let functions_total = files
            .iter()
            .filter_map(|f| f.ccn.as_ref())
            .filter(|s| s.tier != CcnTier::Heuristic)
            .map(|s| s.functions)
            .sum();
        let mut files_excluded = crate::model::ExcludedFiles::default();
        for f in &files {
            if f.is_vendored {
                files_excluded.vendored += 1;
            } else if f.is_generated {
                files_excluded.generated += 1;
            }
        }
        crate::model::ScanResult {
            root: "C:/repo".into(),
            files_analyzed: files.len(),
            loc_total,
            loc,
            comments,
            blanks,
            files_excluded,
            functions_total,
            ccn: None,
            languages: Vec::new(),
            files,
        }
    }

    #[test]
    fn generated_files_do_not_move_health_histograms_or_the_top_table() {
        let clean = file("src/a.ts", 100, vec![func(2, 100)]);
        // A 9000-line, CCN-200 bundle: exactly the file that used to own the
        // max, the open bin and the health score.
        let mut generated = file("src/bundle.min.js", 9000, vec![func(200, 9000)]);
        generated.is_generated = true;

        let report = build(&scan_result(vec![clean, generated]));

        assert_eq!(report.files_analyzed, 2, "both files were found");
        assert_eq!(report.files_measured, 1, "only one is the repo's code");
        assert_eq!(report.stats.files_excluded_generated, 1);
        assert_eq!(report.stats.files_excluded_vendored, 0);

        assert_eq!(report.stats.ccn_max, 2, "generated CCN 200 must not win");
        assert_eq!(report.ccn.total, 1);
        assert_eq!(report.ccn_file.total, 1);
        let h = report.health.as_ref().expect("one measured function");
        assert_eq!(h.functions, 1);
        assert_eq!(h.lines, 100);
        assert_eq!(h.score, 100.0);

        assert_eq!(report.loc.total, 1, "LOC histogram is measured-only");
        assert_eq!(report.loc.total_weight, 100);
        assert_eq!(report.stats.loc_max, 100);
        assert_eq!(report.loc_total, 100, "headline LOC follows the histogram");
        assert_eq!(report.stats.loc_share_top10pct, 100.0);

        assert_eq!(report.top.len(), 1);
        assert_eq!(report.top[0].path, "src/a.ts");
    }

    #[test]
    fn vendored_files_are_counted_in_their_own_bucket() {
        let clean = file("src/a.ts", 10, vec![func(1, 10)]);
        let mut generated = file("src/api.gen.ts", 20, vec![func(1, 20)]);
        generated.is_generated = true;
        let mut vendored = file("vendor/lib/x.js", 30, vec![func(1, 30)]);
        vendored.is_vendored = true;
        // Both flags: one bucket, vendored, so the counts still add up.
        let mut both = file("vendor/proto/x.pb.go", 40, vec![func(1, 40)]);
        both.is_generated = true;
        both.is_vendored = true;

        let report = build(&scan_result(vec![clean, generated, vendored, both]));

        assert_eq!(report.files_analyzed, 4);
        assert_eq!(report.files_measured, 1);
        assert_eq!(report.stats.files_excluded_generated, 1);
        assert_eq!(report.stats.files_excluded_vendored, 2);
        assert_eq!(
            report.files_analyzed - report.files_measured,
            report.stats.files_excluded_generated + report.stats.files_excluded_vendored,
            "every excluded file is in exactly one bucket"
        );
        assert_eq!(report.loc.total, 1);
        assert_eq!(report.stats.functions_total, 1);
    }

    #[test]
    fn health_skips_excluded_files() {
        let clean = file("src/a.ts", 100, vec![func(1, 100)]);
        let mut generated = file("gen/big.js", 1000, vec![func(100, 1000)]);
        generated.is_generated = true;

        let h = health(&[clean, generated]).expect("one measured function");
        assert_eq!(h.functions, 1);
        assert_eq!(h.lines, 100);
        assert_eq!(h.score, 100.0);
    }

    #[test]
    fn penalty_anchors_at_the_mccabe_limit() {
        assert_eq!(penalty(1), 0.0);
        assert_eq!(penalty(10), 0.0);
        assert!((penalty(20) - std::f64::consts::LOG10_2).abs() < 1e-4);
        assert!((penalty(50) - (5.0f64).log10()).abs() < 1e-4);
        assert_eq!(penalty(100), 1.0);
        assert_eq!(penalty(900), 1.0, "bounded: one monster cannot dominate");
    }

    #[test]
    fn health_is_100_when_everything_is_simple() {
        let files = vec![file("a.ts", 100, vec![func(1, 10), func(5, 20)])];
        let h = health(&files).unwrap();
        assert_eq!(h.score, 100.0);
        assert_eq!(h.grade, 'A');
        assert_eq!(h.debt, 0.0);
    }

    #[test]
    fn health_drops_to_zero_for_a_ccn_100_function() {
        let files = vec![file("a.ts", 100, vec![func(100, 100)])];
        let h = health(&files).unwrap();
        assert!(h.score < 1e-9);
        assert_eq!(h.grade, 'E');
    }

    #[test]
    fn health_is_size_weighted() {
        // One 10-line CCN-20 function and one 90-line CCN-1 function:
        // debt = (10*0.301 + 90*0) / 100
        let files = vec![file("a.ts", 100, vec![func(20, 10), func(1, 90)])];
        let h = health(&files).unwrap();
        assert!((h.debt - 0.030103).abs() < 1e-4);
        assert!((h.score - 96.99).abs() < 0.01);
    }

    #[test]
    fn heuristic_files_never_enter_health() {
        let mut f = file("vendor.php", 1000, vec![]);
        f.ccn = Some(CcnSummary::heuristic(200));
        let files = vec![f, file("a.ts", 100, vec![func(1, 100)])];
        let h = health(&files).unwrap();
        assert_eq!(h.functions, 1);
        assert_eq!(h.score, 100.0);
    }

    #[test]
    fn empty_function_set_has_no_health_rather_than_a_perfect_score() {
        assert!(health(&[]).is_none());
        let files = vec![file("a.ts", 100, vec![])];
        assert!(health(&files).is_none());
    }

    #[test]
    fn ccn_bins_use_the_published_edges() {
        // 1, 2, 4, 5, 10, 11, 20, 21, 50, 51
        let values = [1u32, 2, 4, 5, 10, 11, 20, 21, 50, 51];
        let h = histogram(&values, None, &CCN_BINS);
        let counts: Vec<usize> = h.bins.iter().map(|b| b.count).collect();
        assert_eq!(counts, vec![1, 2, 2, 2, 2, 1]);
        assert_eq!(h.total, 10);
    }

    #[test]
    fn loc_bins_capture_every_file_including_zero_and_open_end() {
        let values = [0u32, 49, 50, 9999, 10000, 250000];
        let h = histogram(&values, None, &LOC_BINS);
        assert_eq!(h.total, 6);
        assert_eq!(h.bins.iter().map(|b| b.count).sum::<usize>(), 6);
        assert_eq!(h.bins.last().unwrap().count, 2);
        assert_eq!(h.bins.last().unwrap().max, 250000);
    }

    #[test]
    fn weighted_histogram_reports_line_share() {
        let values = [10u32, 100];
        let weights = [10u32, 990];
        let h = histogram(&values, Some(&weights), &LOC_BINS);
        assert!(h.weighted);
        assert_eq!(h.total_weight, 1000);
        // 100 lines lands in the 100–199 bin, not the 50–99 one.
        let second = &h.bins[2];
        assert_eq!(second.count, 1);
        assert!((second.share_weight - 99.0).abs() < 1e-9);
    }

    #[test]
    fn shares_sum_to_100() {
        let values: Vec<u32> = (0..100).collect();
        let h = histogram(&values, None, &LOC_BINS);
        let sum: f64 = h.bins.iter().map(|b| b.share_count).sum();
        assert!((sum - 100.0).abs() < 1e-9, "got {sum}");
    }

    #[test]
    fn risk_ranks_bad_files_first() {
        let files = [
            file("small.ts", 10, vec![func(1, 10)]),
            file("bad.ts", 200, vec![func(60, 200)]),
        ];
        assert!((file_risk(&files[1]) - penalty(60)).abs() < 1e-9);
        assert_eq!(file_risk(&files[0]), 0.0);
    }

    #[test]
    fn grade_bands_match_the_calibrated_cut_points() {
        assert_eq!(grade_for(100.0), 'A');
        assert_eq!(grade_for(97.5), 'A');
        assert_eq!(grade_for(97.49), 'B');
        assert_eq!(grade_for(94.0), 'B');
        assert_eq!(grade_for(93.99), 'C');
        assert_eq!(grade_for(88.0), 'C');
        assert_eq!(grade_for(87.99), 'D');
        assert_eq!(grade_for(79.0), 'D');
        assert_eq!(grade_for(78.99), 'E');
        assert_eq!(grade_for(0.0), 'E');
    }

    #[test]
    fn a_genuinely_complex_repo_reaches_the_bottom_of_the_scale() {
        // Twenty CCN-30 functions: p(30) = log10(3) = 0.477, so debt ~0.48.
        let fns: Vec<_> = (0..20).map(|_| func(30, 20)).collect();
        let h = health(&[file("a.ts", 400, fns)]).unwrap();
        assert!(h.score < 60.0, "score was {}", h.score);
        assert_eq!(h.grade, 'E');
    }

    #[test]
    fn calibration_note_is_present_when_grades_are_no_longer_experimental() {
        // Compile-time assertions: flipping the flag without updating the note
        // (or the note without the flag) is a review error, not a runtime one.
        const { assert!(!HEALTH_EXPERIMENTAL) };
        const { assert!(!HEALTH_CALIBRATION.is_empty()) };
        assert!(HEALTH_CALIBRATION.contains("11 repositories"));
    }

    fn hotspot_row(path: &str, loc: u32, risk: f64, commits: u32, changed: u32) -> HotspotRow {
        HotspotRow {
            path: path.into(),
            loc,
            ccn_max: 10,
            risk,
            commits,
            lines_changed: changed,
            hotspot: 0.0,
            top_author: String::new(),
            authors: 0,
        }
    }

    #[test]
    fn average_ranks_share_ties() {
        // Ranks 1, then the tie spans 2 and 3, then 4, over a population of 4.
        let ranks = average_ranks(&[0u32, 2, 2, 5]);
        assert!((ranks[0] - 0.25).abs() < 1e-9, "got {}", ranks[0]);
        assert!((ranks[1] - 0.625).abs() < 1e-9, "got {}", ranks[1]);
        assert_eq!(ranks[1], ranks[2], "a tie is one rank, not an input order");
        assert!((ranks[3] - 1.0).abs() < 1e-9, "got {}", ranks[3]);

        assert_eq!(average_ranks(&[7u32]), vec![1.0]);
        assert!(average_ranks::<u32>(&[]).is_empty());
    }

    #[test]
    fn a_file_without_commits_always_scores_zero() {
        let mut rows = vec![
            // Highest risk, zero churn: the intersection is empty.
            hotspot_row("dormant.rs", 100, 0.9, 0, 0),
            hotspot_row("hot.rs", 100, 0.9, 4, 20),
        ];
        score_hotspots(&mut rows);

        assert_eq!(rows[1].path, "dormant.rs");
        assert_eq!(rows[1].hotspot, 0.0);
        assert_eq!(rows[0].path, "hot.rs");
        assert!(rows[0].hotspot > 0.0 && rows[0].hotspot <= 1.0);
    }

    #[test]
    fn hotspot_is_the_product_of_the_two_rank_percentiles() {
        let mut rows = vec![
            hotspot_row("a.rs", 100, 0.0, 0, 0),
            hotspot_row("b.rs", 100, 0.0, 3, 3),
            hotspot_row("c.rs", 100, 0.5, 3, 3),
        ];
        score_hotspots(&mut rows);

        // Commits: a is rank 1/3 (then clamped to 0), b and c tie at 2.5/3.
        // Risk: a and b tie at 1.5/3, c is highest at 3/3.
        let by_path = |p: &str| rows.iter().find(|r| r.path == p).unwrap().hotspot;
        assert_eq!(by_path("a.rs"), 0.0);
        assert!((by_path("b.rs") - (2.5 / 3.0) * (1.5 / 3.0)).abs() < 1e-9);
        assert!((by_path("c.rs") - (2.5 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn hotspot_decile_share_uses_measured_lines_as_the_denominator() {
        // Ten ranked files of 100 lines: the top decile is one file = 10%.
        let mut rows: Vec<HotspotRow> = (0..10)
            .map(|i| hotspot_row(&format!("f{i}.rs"), 100, 0.1, i + 1, 10))
            .collect();
        score_hotspots(&mut rows);
        assert!((hotspot_decile_share(&rows, 1_000) - 10.0).abs() < 1e-9);

        // Files without CCN are not ranked but are still our lines.
        assert!((hotspot_decile_share(&rows, 2_000) - 5.0).abs() < 1e-9);

        assert_eq!(hotspot_decile_share(&[], 1_000), 0.0);
        assert_eq!(hotspot_decile_share(&rows, 0), 0.0);
    }

    #[test]
    fn build_without_a_repository_degrades_to_ccn_only() {
        let mut generated = file("gen/b.js", 900, vec![func(50, 900)]);
        generated.is_generated = true;
        let mut result = scan_result(vec![file("a.ts", 100, vec![func(1, 100)]), generated]);
        // A path that cannot be a work tree: `git -C` fails, so no history.
        result.root = "C:/ccn-no-such-repository-4f2a".into();

        let report = build(&result);

        assert!(!report.git_available);
        assert_eq!(report.commits_scanned, 0);
        assert_eq!(report.hotspot_debt_share, 0.0);
        assert!(report.churn.is_empty());
        assert_eq!(report.hotspots.len(), 1, "generated files never rank");
        assert_eq!(report.hotspots[0].path, "a.ts");
        assert_eq!(report.hotspots[0].commits, 0);
        assert_eq!(report.hotspots[0].hotspot, 0.0);
    }
}
