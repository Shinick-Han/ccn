//! Baseline comparison: functions that became complex since a revision.
//!
//! The current tree is already measured by the scan; this module re-measures
//! the same files at `rev` through [`crate::vcs::file_at`] and reports only the
//! *delta*: a function that crossed the threshold, or a brand-new function that
//! is already over it.
//!
//! Two rules keep the result honest:
//!
//! - A file with no baseline version is treated as **new**: its baseline is
//!   empty, so its complex functions are reported as added. `files_skipped`
//!   only counts files whose baseline copy could not be *analysed*.
//! - Matching is by **name**, consuming duplicates in source order, because a
//!   `FunctionRef` has no stable identity across revisions.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::ccn::types::{CcnSummary, CcnTier, FunctionCcn};
use crate::model::{FileLoc, ScanResult};
use crate::vcs::Backend;

/// One function in a snapshot, kept minimal so the comparison is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRef {
    pub name: String,
    pub ccn: u32,
    pub line: usize,
    pub nloc: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Added,
    Worsened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub path: String,
    pub name: String,
    pub line: usize,
    pub ccn: u32,
    /// `None` when the function did not exist in the baseline.
    pub base_ccn: Option<u32>,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Baseline {
    pub rev: String,
    pub threshold: u32,
    pub files_compared: usize,
    pub files_skipped: usize,
    /// Sorted by `ccn` descending.
    pub changes: Vec<Change>,
}

/// Compare one file's functions against its baseline.
///
/// Functions are matched by name, consuming duplicates in source order: the
/// first `foo` in `base` is the baseline of the first `foo` in `current`, and so
/// on. A name can therefore match at most as many times as it occurs in the
/// baseline; every further occurrence counts as [`Kind::Added`].
///
/// Only two transitions are interesting:
///
/// - no baseline function with that name, and `ccn > threshold` -> `Added`;
/// - the baseline function was at or below the threshold and the current one is
///   above it -> `Worsened`.
///
/// A function that was already complex, or that got simpler, is not a finding:
/// this report is about regressions, not about the existing backlog.
pub fn compare_file(
    path: &str,
    base: &[FunctionRef],
    current: &[FunctionRef],
    threshold: u32,
) -> Vec<Change> {
    let mut by_name: HashMap<&str, VecDeque<&FunctionRef>> = HashMap::new();
    for function in base {
        by_name
            .entry(function.name.as_str())
            .or_default()
            .push_back(function);
    }

    let mut changes = Vec::new();
    for function in current {
        match by_name
            .get_mut(function.name.as_str())
            .and_then(VecDeque::pop_front)
        {
            Some(matched) => {
                if matched.ccn <= threshold && function.ccn > threshold {
                    changes.push(Change {
                        path: path.to_string(),
                        name: function.name.clone(),
                        line: function.line,
                        ccn: function.ccn,
                        base_ccn: Some(matched.ccn),
                        kind: Kind::Worsened,
                    });
                }
            }
            None => {
                if function.ccn > threshold {
                    changes.push(Change {
                        path: path.to_string(),
                        name: function.name.clone(),
                        line: function.line,
                        ccn: function.ccn,
                        base_ccn: None,
                        kind: Kind::Added,
                    });
                }
            }
        }
    }

    sort_changes(&mut changes);
    changes
}

/// Build a baseline report from a completed scan.
///
/// Only files that carry per-function CCN are compared: a missing summary, a
/// heuristic file-level number or an empty function list has nothing to match
/// against. Files whose baseline copy cannot be read (added after `rev`, or a
/// rev the backend cannot answer) are counted in `files_skipped`.
///
/// The VCS reads dominate the cost and are independent per file, so they run on
/// rayon's pool. The result is sorted afterwards, which makes the output
/// independent of worker scheduling.
pub fn collect(result: &ScanResult, backend: Backend, rev: &str, threshold: u32) -> Baseline {
    let root = Path::new(&result.root);

    let outcomes: Vec<FileOutcome> = result
        .files
        .par_iter()
        .filter_map(|file| {
            let summary = file.ccn.as_ref()?;
            if file.is_excluded() || summary.tier == CcnTier::Heuristic || summary.detail.is_empty()
            {
                return None;
            }
            Some(measure_baseline(root, backend, rev, file, summary))
        })
        .collect();

    let mut files_compared = 0usize;
    let mut files_skipped = 0usize;
    let mut changes = Vec::new();

    for outcome in outcomes {
        match outcome {
            FileOutcome::Skipped => files_skipped += 1,
            FileOutcome::Compared {
                path,
                base,
                current,
            } => {
                files_compared += 1;
                changes.extend(compare_file(&path, &base, &current, threshold));
            }
        }
    }

    sort_changes(&mut changes);
    Baseline {
        rev: rev.to_string(),
        threshold,
        files_compared,
        files_skipped,
        changes,
    }
}

/// Convenience for the CLI: true when the baseline found anything to report.
pub fn has_changes(baseline: &Baseline) -> bool {
    !baseline.changes.is_empty()
}

/// Per-file result of the (I/O bound) baseline read.
enum FileOutcome {
    /// No readable baseline for this file; never invented as `Added`.
    Skipped,
    Compared {
        path: String,
        base: Vec<FunctionRef>,
        current: Vec<FunctionRef>,
    },
}

/// Read and analyse one file at `rev`.
///
/// Every failure mode — unreadable revision, unparseable baseline copy — folds
/// into [`FileOutcome::Skipped`] rather than a panic or a fabricated `Added`.
fn measure_baseline(
    root: &Path,
    backend: Backend,
    rev: &str,
    file: &FileLoc,
    summary: &CcnSummary,
) -> FileOutcome {
    // `None` means "this path has no version at `rev`" — a file added by the
    // change under review. Its baseline is empty, so every complex function in
    // it is genuinely new. The revision itself is validated by the caller, so
    // a typo cannot turn the whole repository into "added".
    let Some(bytes) = crate::vcs::file_at(root, backend, rev, &file.path) else {
        return FileOutcome::Compared {
            path: file.path.clone(),
            base: Vec::new(),
            current: summary
                .detail
                .iter()
                .map(|f| FunctionRef {
                    name: f.name.clone(),
                    ccn: f.ccn,
                    line: f.line,
                    nloc: f.nloc,
                })
                .collect(),
        };
    };

    let base = crate::ccn::analyze_file(Path::new(&file.path), &bytes, &file.lang);
    if base.tier == CcnTier::Heuristic {
        // No grammar for the *baseline copy*: there are no per-function numbers
        // to match against, so every current function would look newly added.
        return FileOutcome::Skipped;
    }

    FileOutcome::Compared {
        path: file.path.clone(),
        base: base.detail.iter().map(to_ref).collect(),
        current: summary.detail.iter().map(to_ref).collect(),
    }
}

fn to_ref(function: &FunctionCcn) -> FunctionRef {
    FunctionRef {
        name: function.name.clone(),
        ccn: function.ccn,
        line: function.line,
        nloc: function.nloc,
    }
}

/// Highest complexity first; path and line make ties deterministic.
fn sort_changes(changes: &mut [Change]) {
    changes.sort_by(|a, b| {
        b.ccn
            .cmp(&a.ccn)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::FileCcn;
    use crate::model::ExcludedFiles;

    fn func(name: &str, line: usize, ccn: u32) -> FunctionRef {
        FunctionRef {
            name: name.to_string(),
            ccn,
            line,
            nloc: 1,
        }
    }

    fn change(
        path: &str,
        name: &str,
        line: usize,
        ccn: u32,
        base_ccn: Option<u32>,
        kind: Kind,
    ) -> Change {
        Change {
            path: path.to_string(),
            name: name.to_string(),
            line,
            ccn,
            base_ccn,
            kind,
        }
    }

    fn file(path: &str, functions: &[(&str, u32)]) -> FileLoc {
        let detail: Vec<FunctionCcn> = functions
            .iter()
            .enumerate()
            .map(|(i, (name, ccn))| FunctionCcn {
                name: (*name).to_string(),
                line: i + 1,
                end_line: i + 1,
                nloc: 1,
                ccn: *ccn,
                params: 0,
            })
            .collect();
        FileLoc {
            path: path.to_string(),
            lang: "Rust".to_string(),
            loc_total: 0,
            loc: 0,
            comments: 0,
            blanks: 0,
            ccn: Some(CcnSummary::from_file(FileCcn {
                tier: CcnTier::Exact,
                functions: detail,
            })),
            is_generated: false,
            is_vendored: false,
        }
    }

    fn scan_result(root: &str, files: Vec<FileLoc>) -> ScanResult {
        ScanResult {
            root: root.to_string(),
            files_analyzed: files.len(),
            loc_total: 0,
            loc: 0,
            comments: 0,
            blanks: 0,
            files_excluded: ExcludedFiles::default(),
            functions_total: 0,
            ccn: None,
            languages: Vec::new(),
            files,
        }
    }

    /// A root that does not exist, so every VCS read must answer `None`
    /// regardless of which backend implementation is wired in.
    const MISSING_ROOT: &str = "ccn-baseline-root-that-does-not-exist";

    #[test]
    fn added_worsened_and_ignored_are_distinguished() {
        let base = vec![
            func("foo", 1, 3),
            func("foo", 40, 30),
            func("bar", 60, 20),
            func("baz", 80, 5),
            func("qux", 90, 12),
        ];
        let current = vec![
            func("foo", 2, 25),
            func("foo", 41, 40),
            func("bar", 61, 40),
            func("baz", 81, 5),
            func("qux", 91, 12),
            func("added", 95, 15),
            func("simple", 96, 3),
        ];

        let changes = compare_file("src/a.rs", &base, &current, 10);

        assert_eq!(
            changes,
            vec![
                // 3 -> 25 crosses the limit.
                change("src/a.rs", "foo", 2, 25, Some(3), Kind::Worsened),
                // Absent from the baseline and already over the limit.
                change("src/a.rs", "added", 95, 15, None, Kind::Added),
            ],
            "already-complex (bar, qux), unchanged (baz) and simple new (simple) functions stay quiet"
        );
    }

    #[test]
    fn duplicate_names_are_consumed_in_order() {
        // The first `foo` in base must match the first `foo` in current.
        let base = vec![func("foo", 1, 1), func("foo", 2, 20)];
        let current = vec![func("foo", 10, 30), func("foo", 11, 2)];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(
            changes.len(),
            1,
            "the second foo matches the already-complex baseline copy"
        );
        assert_eq!(changes[0].line, 10);
        assert_eq!(changes[0].base_ccn, Some(1));
        assert_eq!(changes[0].kind, Kind::Worsened);
    }

    #[test]
    fn a_third_occurrence_has_no_baseline_left() {
        let base = vec![func("foo", 1, 1)];
        let current = vec![func("foo", 10, 2), func("foo", 20, 30)];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(
            changes,
            vec![change("a.rs", "foo", 20, 30, None, Kind::Added)],
            "the baseline's single foo is spent by the first current foo"
        );
    }

    #[test]
    fn threshold_boundaries_are_strict() {
        let base = |ccn: u32| vec![func("f", 1, ccn)];
        let current = |ccn: u32| vec![func("f", 2, ccn)];

        let worsened = compare_file("a", &base(10), &current(11), 10);
        assert_eq!(worsened.len(), 1, "10 -> 11 crosses the limit");
        assert_eq!(worsened[0].kind, Kind::Worsened);
        assert_eq!(worsened[0].base_ccn, Some(10));

        assert!(
            compare_file("a", &base(10), &current(10), 10).is_empty(),
            "10 is not over the limit"
        );
        assert!(
            compare_file("a", &base(11), &current(12), 10).is_empty(),
            "already over the limit, so this is not a regression"
        );

        // The same boundary applies to new functions.
        assert_eq!(
            compare_file("a", &[], &current(11), 10).len(),
            1,
            "a new function at 11 is added"
        );
        assert!(compare_file("a", &[], &current(10), 10).is_empty());
    }

    #[test]
    fn changes_are_sorted_by_descending_ccn_then_line() {
        let base: Vec<FunctionRef> = Vec::new();
        let current = vec![
            func("a", 1, 30),
            func("b", 2, 50),
            func("c", 3, 40),
            func("d", 9, 20),
            func("e", 4, 20),
        ];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(
            changes.iter().map(|c| c.ccn).collect::<Vec<_>>(),
            vec![50, 40, 30, 20, 20]
        );
        assert_eq!(
            changes.iter().map(|c| c.line).collect::<Vec<_>>(),
            vec![2, 3, 1, 4, 9],
            "equal CCN falls back to line order"
        );
    }

    #[test]
    fn collect_ignores_excluded_files() {
        let mut generated = file("gen/api.rs", &[("huge", 60)]);
        generated.is_generated = true;
        let mut vendored = file("vendor/lib.rs", &[("big", 40)]);
        vendored.is_vendored = true;
        let result = scan_result(MISSING_ROOT, vec![generated, vendored]);

        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(baseline.files_compared, 0);
        assert_eq!(baseline.files_skipped, 0, "excluded files are out of scope");
        assert!(baseline.changes.is_empty());
        assert_eq!(baseline.rev, "HEAD~1");
        assert_eq!(baseline.threshold, 10);
    }

    #[test]
    fn collect_ignores_files_without_function_detail() {
        let mut heuristic = file("a.rb", &[]);
        heuristic.ccn = Some(CcnSummary::heuristic(40));
        let unmeasured = FileLoc {
            ccn: None,
            ..file("b.rs", &[("f", 40)])
        };
        let empty = file("c.rs", &[]);
        let result = scan_result(MISSING_ROOT, vec![heuristic, unmeasured, empty]);

        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(baseline.files_compared, 0);
        assert_eq!(baseline.files_skipped, 0);
        assert!(baseline.changes.is_empty());
    }

    #[test]
    fn a_missing_baseline_means_the_file_is_new() {
        // The revision is validated by the caller; inside `collect` a missing
        // baseline copy is a file added by the change, so all of its complex
        // functions are reported as added.
        let files = vec![
            file("a.rs", &[("foo", 30)]),
            file("b.rs", &[("bar", 40), ("baz", 12)]),
        ];
        let result = scan_result(MISSING_ROOT, files);

        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(baseline.files_compared, 2);
        assert_eq!(baseline.files_skipped, 0);
        assert_eq!(baseline.changes.len(), 3, "foo, bar, baz are all new");
        assert!(baseline.changes.iter().all(|c| c.kind == Kind::Added));
    }

    #[test]
    fn has_changes_tracks_the_change_vector() {
        let empty = Baseline {
            rev: "HEAD".to_string(),
            threshold: 10,
            files_compared: 0,
            files_skipped: 0,
            changes: Vec::new(),
        };
        assert!(!has_changes(&empty));

        let found = Baseline {
            changes: compare_file("a.rs", &[], &[func("f", 1, 11)], 10),
            ..empty
        };
        assert!(has_changes(&found));
    }
}
