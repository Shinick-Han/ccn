//! Baseline comparison: functions that became complex since a revision.
//!
//! The current tree is already measured by the scan; this module re-measures
//! the same files at `rev` through [`crate::vcs::file_at`] and reports only the
//! *delta*: a function that crossed the threshold, an already-complex function
//! that got worse, or a brand-new function that is already over it.
//!
//! Three rules keep the result honest:
//!
//! - A file that is [`crate::vcs::FileAt::Absent`] at `rev` is treated as
//!   **new**: its baseline is empty, so its complex functions are reported as
//!   added.
//! - A file that is [`crate::vcs::FileAt::Unavailable`] (permission, timeout,
//!   unknown revision, VCS error) is **not** treated as new. It is counted in
//!   [`Baseline::files_unavailable`], stays out of `files_compared`, and
//!   produces no findings: unreadable history must never fabricate `Added`.
//! - `files_skipped` only counts files whose baseline copy *was* read but could
//!   not be analysed.
//!
//! Matching is by **name**, consuming duplicates in source order, because a
//! `FunctionRef` has no stable identity across revisions.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::ccn::types::{CcnSummary, CcnTier, FunctionCcn};
use crate::model::{FileLoc, ScanResult};
use crate::vcs::{Backend, FileAt};

/// One function in a snapshot, kept minimal so the comparison is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRef {
    pub name: String,
    pub ccn: u32,
    pub line: usize,
    pub nloc: usize,
    pub max_nesting: u32,
    /// Parse confidence of the measurement that produced this reference.
    pub tier: CcnTier,
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
    pub nloc: usize,
    pub base_nloc: Option<usize>,
    pub max_nesting: u32,
    pub base_max_nesting: Option<u32>,
    pub kind: Kind,
    /// Human-readable reasons, e.g. `["ccn +3", "nesting +1", "nloc +42"]`.
    pub reasons: Vec<String>,
    /// Confidence of the current measurement. `best-effort` means the parser
    /// recovered from syntax errors; the finding is advisory.
    pub tier: CcnTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Baseline {
    pub rev: String,
    pub threshold: u32,
    pub files_compared: usize,
    pub files_skipped: usize,
    /// Files whose baseline copy could not be read at all (permission, timeout,
    /// unknown revision, VCS/transport error). They are excluded from
    /// `files_compared` and produce no findings: an unreadable file is not a
    /// new file.
    #[serde(default)]
    pub files_unavailable: usize,
    /// Sorted by `ccn` descending.
    pub changes: Vec<Change>,
}

/// Extra lines a function may grow by before it counts as worsened on its own.
const NLOC_GROWTH: usize = 20;

/// Compare one file's functions against its baseline.
///
/// Functions are matched by name, consuming duplicates in source order: the
/// first `foo` in `base` is the baseline of the first `foo` in `current`, and so
/// on. A name can therefore match at most as many times as it occurs in the
/// baseline; every further occurrence counts as [`Kind::Added`].
///
/// A finding needs the current function to be above the threshold, plus one of:
///
/// - no baseline function with that name -> [`Kind::Added`];
/// - more branches than the baseline (`ccn` grew) -> [`Kind::Worsened`];
/// - deeper nesting than the baseline -> [`Kind::Worsened`];
/// - at least [`NLOC_GROWTH`] more lines than the baseline -> [`Kind::Worsened`].
///
/// This reports regressions, not the existing backlog: an already-complex
/// function that stays the same or improves is not a finding, but an
/// already-complex function that gets worse is.
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
                if function.ccn <= threshold {
                    continue;
                }

                let mut reasons = Vec::new();
                if function.ccn > matched.ccn {
                    reasons.push(format!("ccn +{}", function.ccn - matched.ccn));
                }
                if function.max_nesting > matched.max_nesting {
                    reasons.push(format!(
                        "nesting +{}",
                        function.max_nesting - matched.max_nesting
                    ));
                }
                if function.nloc >= matched.nloc.saturating_add(NLOC_GROWTH) {
                    reasons.push(format!("nloc +{}", function.nloc - matched.nloc));
                }
                if reasons.is_empty() {
                    continue;
                }

                changes.push(Change {
                    path: path.to_string(),
                    name: function.name.clone(),
                    line: function.line,
                    ccn: function.ccn,
                    base_ccn: Some(matched.ccn),
                    nloc: function.nloc,
                    base_nloc: Some(matched.nloc),
                    max_nesting: function.max_nesting,
                    base_max_nesting: Some(matched.max_nesting),
                    kind: Kind::Worsened,
                    reasons,
                    tier: function.tier,
                });
            }
            None => {
                if function.ccn > threshold {
                    changes.push(Change {
                        path: path.to_string(),
                        name: function.name.clone(),
                        line: function.line,
                        ccn: function.ccn,
                        base_ccn: None,
                        nloc: function.nloc,
                        base_nloc: None,
                        max_nesting: function.max_nesting,
                        base_max_nesting: None,
                        kind: Kind::Added,
                        reasons: vec!["new function".to_string()],
                        tier: function.tier,
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
/// against. A file absent at `rev` is compared against an empty baseline (it is
/// new). A file whose baseline copy is **unavailable** is counted in
/// [`Baseline::files_unavailable`] and skipped entirely, so a VCS read failure
/// can never be reported as added functions.
///
/// The VCS reads dominate the cost and are independent per file, so they run on
/// rayon's pool. The result is sorted afterwards, which makes the output
/// independent of worker scheduling.
pub fn collect(result: &ScanResult, backend: Backend, rev: &str, threshold: u32) -> Baseline {
    let root = Path::new(&result.root);
    // One extra VCS command, before the parallel reads: rename detection is not
    // per-file work. An empty map (P4, or no renames) is the old behaviour.
    let renamed = crate::vcs::renamed_from(root, backend, rev);

    let outcomes: Vec<FileOutcome> = result
        .files
        .par_iter()
        .filter_map(|file| {
            let summary = file.ccn.as_ref()?;
            if file.is_excluded() || summary.tier == CcnTier::Heuristic || summary.detail.is_empty()
            {
                return None;
            }
            Some(measure_baseline(
                root, backend, rev, file, summary, &renamed,
            ))
        })
        .collect();

    let mut files_compared = 0usize;
    let mut files_skipped = 0usize;
    let mut files_unavailable = 0usize;
    let mut changes = Vec::new();

    for outcome in outcomes {
        match outcome {
            FileOutcome::Skipped => files_skipped += 1,
            FileOutcome::Unavailable => files_unavailable += 1,
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
        files_unavailable,
        changes,
    }
}

/// Convenience for the CLI: true when the baseline found anything to report.
pub fn has_changes(baseline: &Baseline) -> bool {
    !baseline.changes.is_empty()
}

/// Per-file result of the (I/O bound) baseline read.
enum FileOutcome {
    /// The baseline copy was read but cannot be analysed (no grammar).
    Skipped,
    /// The baseline read could not be answered; never invented as `Added`.
    Unavailable,
    Compared {
        path: String,
        base: Vec<FunctionRef>,
        current: Vec<FunctionRef>,
    },
}

/// Read and analyse one file at `rev`.
///
/// Every failure mode is kept distinct: a file that does not exist at `rev` is
/// new (empty baseline), a read the backend could not answer is
/// [`FileOutcome::Unavailable`], and an unparseable baseline copy is
/// [`FileOutcome::Skipped`]. None of them panics or fabricates `Added` from a
/// failed read.
fn measure_baseline(
    root: &Path,
    backend: Backend,
    rev: &str,
    file: &FileLoc,
    summary: &CcnSummary,
    renamed: &HashMap<String, String>,
) -> FileOutcome {
    let baseline = read_baseline(root, backend, rev, &file.path, renamed);

    let bytes = match baseline {
        FileAt::Found(bytes) => bytes,
        FileAt::Absent => {
            // The path genuinely has no revision at `rev`: the file is new, so
            // its complex functions are reported as added against empty.
            return FileOutcome::Compared {
                path: file.path.clone(),
                base: Vec::new(),
                current: summary.detail.iter().map(to_ref(summary.tier)).collect(),
            };
        }
        FileAt::Unavailable => return FileOutcome::Unavailable,
    };

    let base = crate::ccn::analyze_file(Path::new(&file.path), &bytes, &file.lang);
    if base.tier == CcnTier::Heuristic {
        // No grammar for the *baseline copy*: there are no per-function numbers
        // to match against, so every current function would look newly added.
        return FileOutcome::Skipped;
    }

    FileOutcome::Compared {
        path: file.path.clone(),
        base: base.detail.iter().map(to_ref(base.tier)).collect(),
        current: summary.detail.iter().map(to_ref(summary.tier)).collect(),
    }
}

/// Read the baseline copy of `path`, following a rename when the path is new.
///
/// The rename fallback is only tried for [`FileAt::Absent`]: an unavailable read
/// says nothing about whether the old path would have answered, and retrying
/// there would only risk turning a failure into a fabricated baseline. An
/// unavailable read of the old path stays unavailable rather than defaulting to
/// "new".
fn read_baseline(
    root: &Path,
    backend: Backend,
    rev: &str,
    path: &str,
    renamed: &HashMap<String, String>,
) -> FileAt {
    let found = crate::vcs::file_at(root, backend, rev, path);
    if found != FileAt::Absent {
        return found;
    }
    match renamed.get(path) {
        Some(old) => crate::vcs::file_at(root, backend, rev, old),
        None => found,
    }
}

fn to_ref(tier: CcnTier) -> impl Fn(&FunctionCcn) -> FunctionRef {
    move |function| FunctionRef {
        name: function.name.clone(),
        ccn: function.ccn,
        line: function.line,
        nloc: function.nloc,
        max_nesting: function.max_nesting,
        tier,
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
        func_full(name, line, ccn, 1, 0)
    }

    fn func_full(name: &str, line: usize, ccn: u32, nloc: usize, max_nesting: u32) -> FunctionRef {
        FunctionRef {
            name: name.to_string(),
            ccn,
            line,
            nloc,
            max_nesting,
            tier: CcnTier::Exact,
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
                max_nesting: 0,
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

    /// A root that does not exist, so every VCS read must answer
    /// [`FileAt::Unavailable`] regardless of which backend implementation is
    /// wired in.
    const MISSING_ROOT: &str = "ccn-baseline-root-that-does-not-exist";

    fn git_works() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("ccn-baseline-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &std::path::Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "CCN Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn commit(dir: &std::path::Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    /// Rust source for one `foo` whose CCN is `1 + ifs` (an `if` is a decision).
    fn rust_source(ifs: usize) -> String {
        let mut source = String::from("fn foo(x: i32) {\n");
        for _ in 0..ifs {
            source.push_str("    if x > 0 { }\n");
        }
        source.push_str("}\n");
        source
    }

    /// A `FileLoc` whose per-function detail comes from the real analyser, so
    /// the baseline side and the current side use the same parser.
    fn scanned(root: &std::path::Path, rel: &str) -> FileLoc {
        let bytes = std::fs::read(root.join(rel)).expect("read scanned file");
        let summary = crate::ccn::analyze_file(std::path::Path::new(rel), &bytes, "Rust");
        FileLoc {
            ccn: Some(summary),
            ..file(rel, &[])
        }
    }

    fn reasons(change: &Change) -> Vec<&str> {
        change.reasons.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_moved_file_with_identical_functions_has_no_changes() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("rename-same");
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("a.rs"), rust_source(11)).expect("write a.rs");
        commit(repo, "first");

        git(repo, &["mv", "a.rs", "b.rs"]);
        commit(repo, "rename");

        let result = scan_result(
            repo.to_str().expect("utf-8 root"),
            vec![scanned(repo, "b.rs")],
        );
        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(
            baseline.files_compared, 1,
            "a renamed file still has a baseline to compare against"
        );
        assert_eq!(baseline.files_skipped, 0);
        assert!(
            baseline.changes.is_empty(),
            "the old file's complexity is not a new function: {:?}",
            baseline.changes
        );
    }

    #[test]
    fn a_moved_file_whose_function_grew_is_worsened_against_the_old_path() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("rename-grew");
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("a.rs"), rust_source(8)).expect("write a.rs");
        commit(repo, "first");

        git(repo, &["mv", "a.rs", "b.rs"]);
        commit(repo, "rename");

        // 1 + 8 ifs = 9 at HEAD~1; 1 + 11 ifs = 12 now.
        std::fs::write(repo.join("b.rs"), rust_source(11)).expect("grow b.rs");

        let result = scan_result(
            repo.to_str().expect("utf-8 root"),
            vec![scanned(repo, "b.rs")],
        );
        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(baseline.files_compared, 1);
        assert_eq!(baseline.changes.len(), 1);
        let change = &baseline.changes[0];
        assert_eq!(change.path, "b.rs");
        assert_eq!(change.name, "foo");
        assert_eq!(change.ccn, 12);
        assert_eq!(change.base_ccn, Some(9));
        assert_eq!(change.kind, Kind::Worsened);
        assert_eq!(reasons(change), vec!["ccn +3"]);
    }

    #[test]
    fn an_actually_new_file_is_still_added() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("really-new");
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("a.rs"), rust_source(2)).expect("write a.rs");
        commit(repo, "first");

        std::fs::write(repo.join("new.rs"), rust_source(11)).expect("write new.rs");

        let result = scan_result(
            repo.to_str().expect("utf-8 root"),
            vec![scanned(repo, "new.rs")],
        );
        let baseline = collect(&result, Backend::Git, "HEAD", 10);

        assert_eq!(baseline.files_compared, 1);
        assert_eq!(baseline.changes.len(), 1);
        let change = &baseline.changes[0];
        assert_eq!(change.path, "new.rs");
        assert_eq!(change.name, "foo");
        assert_eq!(change.ccn, 12);
        assert_eq!(change.base_ccn, None);
        assert_eq!(change.base_nloc, None);
        assert_eq!(change.base_max_nesting, None);
        assert_eq!(change.kind, Kind::Added);
        assert_eq!(reasons(change), vec!["new function"]);
    }

    #[test]
    fn already_over_the_threshold_and_worse_is_reported() {
        // 30 was already over the limit; 33 is still a regression.
        let changes = compare_file("a.rs", &[func("f", 1, 30)], &[func("f", 2, 33)], 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, Kind::Worsened);
        assert_eq!(changes[0].base_ccn, Some(30));
        assert_eq!(changes[0].ccn, 33);
        assert_eq!(reasons(&changes[0]), vec!["ccn +3"]);
    }

    #[test]
    fn crossing_the_threshold_is_worsened() {
        let changes = compare_file("a.rs", &[func("f", 1, 9)], &[func("f", 2, 11)], 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, Kind::Worsened);
        assert_eq!(changes[0].base_ccn, Some(9));
        assert_eq!(reasons(&changes[0]), vec!["ccn +2"]);
    }

    #[test]
    fn an_unchanged_over_threshold_function_is_not_reported() {
        assert!(
            compare_file("a.rs", &[func("f", 1, 11)], &[func("f", 2, 11)], 10).is_empty(),
            "no delta, no finding"
        );
    }

    #[test]
    fn an_improved_over_threshold_function_is_not_reported() {
        assert!(
            compare_file("a.rs", &[func("f", 1, 30)], &[func("f", 2, 28)], 10).is_empty(),
            "getting simpler is never a regression"
        );
    }

    #[test]
    fn a_growth_still_under_the_threshold_is_not_reported() {
        assert!(
            compare_file("a.rs", &[func("f", 1, 5)], &[func("f", 2, 6)], 10).is_empty(),
            "the function is still below the limit"
        );
    }

    #[test]
    fn deeper_nesting_is_worsened_even_with_the_same_ccn() {
        let base = vec![func_full("f", 1, 20, 40, 2)];
        let current = vec![func_full("f", 2, 20, 40, 4)];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, Kind::Worsened);
        assert_eq!(changes[0].base_max_nesting, Some(2));
        assert_eq!(changes[0].max_nesting, 4);
        assert_eq!(reasons(&changes[0]), vec!["nesting +2"]);
    }

    #[test]
    fn large_nloc_growth_is_worsened_but_nineteen_lines_is_not() {
        let base = vec![func_full("f", 1, 20, 40, 0)];
        let grown = vec![func_full("f", 2, 20, 61, 0)];

        let changes = compare_file("a.rs", &base, &grown, 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, Kind::Worsened);
        assert_eq!(changes[0].base_nloc, Some(40));
        assert_eq!(changes[0].nloc, 61);
        assert_eq!(reasons(&changes[0]), vec!["nloc +21"]);

        let just_under = vec![func_full("f", 2, 20, 59, 0)];
        assert!(
            compare_file("a.rs", &base, &just_under, 10).is_empty(),
            "+19 lines is below the growth threshold"
        );
    }

    #[test]
    fn a_new_function_carries_no_baseline_metrics() {
        let current = vec![func_full("f", 7, 12, 50, 3)];

        let changes = compare_file("a.rs", &[], &current, 10);

        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        assert_eq!(change.kind, Kind::Added);
        assert_eq!(change.base_ccn, None);
        assert_eq!(change.base_nloc, None);
        assert_eq!(change.base_max_nesting, None);
        assert_eq!(change.nloc, 50);
        assert_eq!(change.max_nesting, 3);
        assert_eq!(reasons(change), vec!["new function"]);
    }

    #[test]
    fn every_reason_is_reported_when_several_metrics_grow() {
        let base = vec![func_full("f", 1, 30, 40, 2)];
        let current = vec![func_full("f", 2, 34, 61, 4)];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(
            reasons(&changes[0]),
            vec!["ccn +4", "nesting +2", "nloc +21"]
        );
    }

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
            changes
                .iter()
                .map(|c| (c.name.as_str(), c.line, c.ccn, c.base_ccn, c.kind))
                .collect::<Vec<_>>(),
            vec![
                ("foo", 41, 40, Some(30), Kind::Worsened),
                ("bar", 61, 40, Some(20), Kind::Worsened),
                // 3 -> 25 crosses the limit.
                ("foo", 2, 25, Some(3), Kind::Worsened),
                // Absent from the baseline and already over the limit.
                ("added", 95, 15, None, Kind::Added),
            ],
            "unchanged (baz, qux) and simple new (simple) functions stay quiet"
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

        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].line, changes[0].base_ccn, changes[0].kind),
            (20, None, Kind::Added),
            "the baseline's single foo is spent by the first current foo"
        );
        assert_eq!(reasons(&changes[0]), vec!["new function"]);
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
            compare_file("a", &base(30), &current(31), 10).len() == 1,
            "already over the limit, but this is still a regression"
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
        // The revision is validated by the caller; inside `collect` a file that
        // is genuinely absent at `rev` (here: never committed) is new, so all of
        // its complex functions are reported as added.
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("absent-new");
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("kept.rs"), rust_source(1)).expect("write kept.rs");
        commit(repo, "first");
        std::fs::write(repo.join("a.rs"), rust_source(11)).expect("write a.rs");
        std::fs::write(repo.join("b.rs"), rust_source(12)).expect("write b.rs");

        let result = scan_result(
            repo.to_str().expect("utf-8 root"),
            vec![scanned(repo, "a.rs"), scanned(repo, "b.rs")],
        );
        let baseline = collect(&result, Backend::Git, "HEAD", 10);

        assert_eq!(baseline.files_compared, 2, "both files are genuinely new");
        assert_eq!(baseline.files_skipped, 0);
        assert_eq!(baseline.files_unavailable, 0);
        assert_eq!(baseline.changes.len(), 2, "foo in a.rs and foo in b.rs");
        assert!(baseline.changes.iter().all(|c| c.kind == Kind::Added));
        assert!(baseline
            .changes
            .iter()
            .all(|c| c.base_nloc.is_none() && c.base_max_nesting.is_none()));
        assert!(baseline
            .changes
            .iter()
            .all(|c| reasons(c) == vec!["new function"]));
    }

    #[test]
    fn unreadable_baseline_files_never_become_added_findings() {
        // Every read fails: the result must be an explicitly incomplete
        // baseline, not a pile of fabricated `Added` functions.
        let files = vec![
            file("a.rs", &[("foo", 30)]),
            file("b.rs", &[("bar", 40), ("baz", 12)]),
        ];
        let result = scan_result(MISSING_ROOT, files);

        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert!(
            baseline.changes.is_empty(),
            "an unavailable read is not a new function: {:?}",
            baseline.changes
        );
        assert_eq!(baseline.files_unavailable, 2);
        assert_eq!(
            baseline.files_compared, 0,
            "unreadable files are not compared"
        );
        assert_eq!(
            baseline.files_skipped, 0,
            "skipped means read but unparseable"
        );
        assert!(!has_changes(&baseline));
    }

    #[test]
    fn an_unknown_revision_is_unavailable_not_a_new_file() {
        // A real repository, but a revision git cannot resolve: nothing can be
        // said about the baseline, so no `Added` findings are invented.
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("bad-rev");
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("a.rs"), rust_source(11)).expect("write a.rs");
        commit(repo, "first");

        let result = scan_result(
            repo.to_str().expect("utf-8 root"),
            vec![scanned(repo, "a.rs")],
        );
        let baseline = collect(&result, Backend::Git, "no-such-rev-77bd", 10);

        assert!(baseline.changes.is_empty());
        assert_eq!(baseline.files_unavailable, 1);
        assert_eq!(baseline.files_compared, 0);
    }

    #[test]
    fn has_changes_tracks_the_change_vector() {
        let empty = Baseline {
            rev: "HEAD".to_string(),
            threshold: 10,
            files_compared: 0,
            files_skipped: 0,
            files_unavailable: 0,
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
