//! Core data model for a scan.
//!
//! The JSON emitted here is the public contract of the tool: report
//! generation, caching and any future consumer all read this shape.
//! See `docs/PLAN.md` §5.3/§5.4.

use serde::Serialize;

use crate::ccn::types::{CcnSummary, CcnTier};

/// One file's line statistics.
///
/// `loc` is non-blank, non-comment physical lines (what tokei calls `code`);
/// `loc_total` includes blanks and comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileLoc {
    pub path: String,
    pub lang: String,
    pub loc_total: usize,
    pub loc: usize,
    pub comments: usize,
    pub blanks: usize,
    /// Per-file CCN roll-up. `None` means the CCN pass never ran for this file
    /// (`--no-ccn`), or the file could not be read / was skipped by the size
    /// guard. The tier inside the summary says how much to trust it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ccn: Option<CcnSummary>,
    /// Produced by a generator (marker in the file head, or a generated
    /// filename). Excluded from every metric in [`crate::metrics::Report`] but
    /// kept in the JSON so a consumer can see what was dropped.
    ///
    /// Omitted from JSON when false: a healthy repository must not grow a
    /// `"is_generated": false` on every one of its files.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_generated: bool,
    /// Part of a checked-in third-party tree (`node_modules/`, `vendor/`, ...).
    /// Same exclusion and serialisation rules as [`Self::is_generated`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_vendored: bool,
}

impl FileLoc {
    /// `true` when this file must not feed any score, histogram or ranking.
    pub fn is_excluded(&self) -> bool {
        self.is_generated || self.is_vendored
    }
}

/// How many files were dropped from the measured metrics, by reason.
///
/// A file that is both generated and vendored is counted once, as vendored —
/// the same precedence [`crate::generated::classify`] applies to its reason.
/// Therefore `files_analyzed - files_measured == total()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExcludedFiles {
    pub generated: usize,
    pub vendored: usize,
}

impl ExcludedFiles {
    pub fn total(&self) -> usize {
        self.generated + self.vendored
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Per-language roll-up. `loc` is always the sum of `code` lines, never
/// "lines" — the two differ by blanks and comments and mixing them is the
/// classic cloc/tokei reporting bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LangSummary {
    pub lang: String,
    pub files: usize,
    pub loc: usize,
    pub comments: usize,
    pub blanks: usize,
}

/// Repository-level CCN aggregates.
///
/// **Every field except `files_heuristic` is computed from non-heuristic files
/// only.** A heuristic file reports one keyword-derived number for the whole
/// file and no functions; letting that number into a function histogram would
/// mix two different measurements into one distribution.
///
/// The distribution is over *functions*, not files: `median`, `p90`, `p99` and
/// `max` are all CCN values of individual functions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoCcn {
    /// Nearest-rank median CCN over functions.
    pub median: u32,
    /// Nearest-rank p90 CCN over functions.
    pub p90: u32,
    /// Nearest-rank p99 CCN over functions.
    pub p99: u32,
    /// Highest single-function CCN.
    pub max: u32,
    /// Functions with CCN strictly greater than 10.
    pub over_10: usize,
    /// Functions with CCN strictly greater than 20.
    pub over_20: usize,
    /// Functions with CCN strictly greater than 50.
    pub over_50: usize,
    /// `over_10` as a percentage of measured functions, `0.0` when there are
    /// none (never `NaN`).
    pub pct_over_10: f64,
    /// Measured complexity per 1,000 code lines: `sum(ccn) / (loc / 1000)`,
    /// `0.0` when `loc == 0`.
    pub per_kloc: f64,
    /// Files that carry function-level (exact or best-effort) CCN.
    pub files_exact: usize,
    /// Files that only produced a file-level heuristic number.
    pub files_heuristic: usize,
    /// Files the CCN pass could not measure (unreadable). Their function count
    /// is unknowable, so this is a count of *files*, not functions; files
    /// skipped by the size guard are out of scope here until M3 tracks them.
    pub functions_without_ccn: usize,
}

/// Result of phase 1 (LOC) plus phase 2 (CCN) aggregates. The line fields are
/// additive, so older consumers keep working when CCN shows up.
///
/// **These aggregates describe every file found, including generated and
/// vendored ones**, so `loc == sum(files[].loc)` still holds and the JSON says
/// what is in the tree. The measured view (what the score is computed from) is
/// [`crate::metrics::Report`]; `files_analyzed - files_measured` equals
/// `files_excluded.total()`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub root: String,
    pub files_analyzed: usize,
    pub loc_total: usize,
    pub loc: usize,
    pub comments: usize,
    pub blanks: usize,
    /// Files dropped from the measured metrics, by reason. Empty when nothing
    /// was excluded or when `--include-generated` was passed.
    pub files_excluded: ExcludedFiles,
    /// Functions across all files with function-level CCN. Heuristic files
    /// contribute nothing (they have none).
    pub functions_total: usize,
    /// `None` when the CCN pass did not run at all (`--no-ccn`) or no file
    /// produced a summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ccn: Option<RepoCcn>,
    pub languages: Vec<LangSummary>,
    pub files: Vec<FileLoc>,
}

/// Nearest-rank percentile over an already sorted slice.
///
/// Nearest-rank (not interpolation) is deliberate: every reported percentile
/// is an actual file size, so "p90 = 312 lines" names a real file.
pub fn percentile(sorted: &[usize], p: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Distribution summary used by the terminal block and, later, the histogram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Distribution {
    pub count: usize,
    pub median: usize,
    pub p90: usize,
    pub p99: usize,
    pub max: usize,
}

impl Distribution {
    pub fn from_values(mut values: Vec<usize>) -> Self {
        values.sort_unstable();
        Self {
            count: values.len(),
            median: percentile(&values, 50.0),
            p90: percentile(&values, 90.0),
            p99: percentile(&values, 99.0),
            max: values.last().copied().unwrap_or(0),
        }
    }
}

impl RepoCcn {
    /// Fold per-file summaries into repository aggregates.
    ///
    /// `loc` is the repository's total code lines (the `per_kloc` denominator).
    /// `functions_without_ccn` is supplied by the scanner: only it knows which
    /// files were never opened (see `FileLoc.ccn`).
    pub fn from_files(files: &[FileLoc], loc: usize, functions_without_ccn: usize) -> Self {
        let mut ccns: Vec<usize> = Vec::new();
        let mut sum: u64 = 0;
        let mut files_exact = 0usize;
        let mut files_heuristic = 0usize;

        for file in files {
            let Some(summary) = file.ccn.as_ref() else {
                continue;
            };
            if summary.tier == CcnTier::Heuristic {
                // File-level number only. It is *not* a function, so it never
                // enters the histogram (PLAN §4.3: never mix tiers).
                files_heuristic += 1;
                continue;
            }
            files_exact += 1;
            for f in &summary.detail {
                sum += u64::from(f.ccn);
                ccns.push(f.ccn as usize);
            }
        }

        ccns.sort_unstable();
        let measured = ccns.len();
        let over_10 = ccns.iter().filter(|&&c| c > 10).count();
        let over_20 = ccns.iter().filter(|&&c| c > 20).count();
        let over_50 = ccns.iter().filter(|&&c| c > 50).count();

        Self {
            median: percentile(&ccns, 50.0) as u32,
            p90: percentile(&ccns, 90.0) as u32,
            p99: percentile(&ccns, 99.0) as u32,
            max: ccns.last().copied().unwrap_or(0) as u32,
            over_10,
            over_20,
            over_50,
            pct_over_10: if measured == 0 {
                0.0
            } else {
                over_10 as f64 * 100.0 / measured as f64
            },
            per_kloc: if loc == 0 {
                0.0
            } else {
                sum as f64 / (loc as f64 / 1000.0)
            },
            files_exact,
            files_heuristic,
            functions_without_ccn,
        }
    }
}

impl ScanResult {
    pub fn loc_distribution(&self) -> Distribution {
        Distribution::from_values(self.files.iter().map(|f| f.loc).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{FileCcn, FunctionCcn};

    #[test]
    fn percentile_uses_nearest_rank() {
        let v: Vec<usize> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 90.0), 90);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&v, 100.0), 100);
        assert_eq!(percentile(&[], 90.0), 0);
    }

    #[test]
    fn percentile_of_single_value_is_that_value() {
        assert_eq!(percentile(&[7], 90.0), 7);
    }

    #[test]
    fn distribution_is_sorted_before_ranking() {
        let d = Distribution::from_values(vec![500, 10, 90, 30]);
        assert_eq!(d.count, 4);
        assert_eq!(d.max, 500);
        assert_eq!(d.median, 30);
    }

    fn function(ccn: u32) -> FunctionCcn {
        FunctionCcn {
            name: "f".to_string(),
            line: 1,
            end_line: 1,
            nloc: 1,
            ccn,
            params: 0,
        }
    }

    fn file(path: &str, ccn: Option<CcnSummary>) -> FileLoc {
        FileLoc {
            path: path.to_string(),
            lang: "Rust".to_string(),
            loc_total: 10,
            loc: 10,
            comments: 0,
            blanks: 0,
            ccn,
            is_generated: false,
            is_vendored: false,
        }
    }

    fn exact(ccns: &[u32]) -> CcnSummary {
        CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions: ccns.iter().copied().map(function).collect(),
        })
    }

    #[test]
    fn aggregates_ignore_heuristic_files() {
        let files = vec![
            file("a.rs", Some(exact(&[1, 2, 30]))),
            // A wild file-level number must not move any function statistic.
            file("b.vb", Some(CcnSummary::heuristic(200))),
            file("c.bin", None),
        ];
        let repo = RepoCcn::from_files(&files, 100, 1);

        assert_eq!(repo.median, 2);
        assert_eq!(repo.p90, 30);
        assert_eq!(repo.max, 30, "heuristic 200 is never a function");
        assert_eq!(repo.over_10, 1);
        assert_eq!(repo.over_20, 1);
        assert_eq!(repo.over_50, 0);
        assert_eq!(repo.files_exact, 1);
        assert_eq!(repo.files_heuristic, 1);
        assert_eq!(repo.functions_without_ccn, 1);
        // 33 complexity over 0.1 kloc.
        assert!((repo.per_kloc - 330.0).abs() < 1e-9);
        assert!((repo.pct_over_10 - 100.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn empty_function_set_reports_zero_percent_not_nan() {
        let files = vec![file("a.rs", Some(exact(&[])))];
        let repo = RepoCcn::from_files(&files, 0, 0);

        assert_eq!(repo.median, 0);
        assert_eq!(repo.p90, 0);
        assert_eq!(repo.max, 0);
        assert_eq!(repo.pct_over_10, 0.0);
        assert!(!repo.pct_over_10.is_nan());
        assert_eq!(repo.per_kloc, 0.0, "zero loc cannot divide");
    }

    #[test]
    fn all_heuristic_has_no_exact_files() {
        let files = vec![
            file("a.vb", Some(CcnSummary::heuristic(12))),
            file("b.rb", Some(CcnSummary::heuristic(3))),
        ];
        let repo = RepoCcn::from_files(&files, 40, 0);

        assert_eq!(repo.files_exact, 0);
        assert_eq!(repo.files_heuristic, 2);
        assert_eq!(repo.max, 0, "no functions were measured");
    }

    #[test]
    fn exclusion_flags_stay_out_of_json_when_false() {
        // A healthy repo's JSON must not grow two `false`s per file.
        let json = serde_json::to_string(&file("a.rs", None)).expect("serialise");
        assert!(!json.contains("is_generated"), "{json}");
        assert!(!json.contains("is_vendored"), "{json}");

        let mut f = file("vendor/x.js", None);
        f.is_vendored = true;
        let json = serde_json::to_string(&f).expect("serialise");
        assert!(json.contains("\"is_vendored\":true"), "{json}");
        assert!(!json.contains("is_generated"), "{json}");
    }

    #[test]
    fn excluded_files_counts_add_up_and_ignore_clean_files() {
        let clean = file("src/a.rs", None);
        assert!(!clean.is_excluded());

        let mut generated = file("src/api.gen.ts", None);
        generated.is_generated = true;
        assert!(generated.is_excluded());

        let mut vendored = file("vendor/x.js", None);
        vendored.is_vendored = true;
        assert!(vendored.is_excluded());

        let counts = ExcludedFiles {
            generated: 1,
            vendored: 1,
        };
        assert_eq!(counts.total(), 2);
        assert!(!counts.is_empty());
        assert_eq!(ExcludedFiles::default().total(), 0);
        assert!(ExcludedFiles::default().is_empty());
    }
}
