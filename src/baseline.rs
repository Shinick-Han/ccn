//! Baseline comparison: functions that became complex since a revision.
//!
//! The current tree is already measured by the scan; this module re-measures
//! the same files at `rev` through [`crate::vcs::file_at`] and reports only the
//! *delta*: a function that crossed the threshold, an already-complex function
//! that got worse, or a brand-new function that is already over it.
//!
//! ## Every relevant file is accounted for
//!
//! A comparison is only meaningful if every file that *should* have been
//! compared is reported in exactly one bucket. [`Baseline`] therefore carries
//! an explicit outcome count per file:
//!
//! - [`Baseline::files_compared`] - a comparison was actually done (including
//!   a file absent at `rev`, which is compared against an empty baseline);
//! - [`Baseline::files_skipped`] - the baseline copy was read but produced no
//!   exact measurement (heuristic fallback or a partial parse);
//! - [`Baseline::files_unmeasured`] - the current file has no per-function CCN
//!   although its language has a grammar (parse or read degradation);
//! - [`Baseline::files_oversize`] - the current file was deliberately left to
//!   the size guard ([`crate::loc::MAX_CCN_BYTES`]-class skip);
//! - [`Baseline::files_unreadable`] - a read/stat failure on the current file;
//! - [`Baseline::files_unavailable`] - the VCS could not answer for the
//!   baseline copy (permission, timeout, unknown revision, transport error);
//! - [`Baseline::files_unmapped`] - the path is genuinely outside the client
//!   view: no depot history exists;
//! - [`Baseline::files_unknown`] - the view itself could not be established
//!   completely, so the path's status is unknown;
//! - [`Baseline::files_ambiguous`] - two or more candidate functions could not
//!   be matched without guessing.
//!
//! `oversize` and `unmapped` are explicit, accepted exclusions. Everything
//! else means a required comparison was not made, which makes
//! [`Baseline::complete`] false; `--fail-on-new` turns that into exit 2
//! instead of a silent pass.
//!
//! Matching is **identity-first** and globally reserved: every exact identity
//! match is reserved before any name fallback, and a fallback is used only when
//! it is unambiguous on both sides. See [`compare`].

use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::ccn::types::{CcnSummary, CcnTier, FunctionCcn};
use crate::model::{CcnSkip, FileLoc, ScanResult};
use crate::vcs::{Backend, FileAt};

/// One function in a snapshot, kept minimal so the comparison is cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRef {
    pub name: String,
    /// Stable match key from the engine (`scope::name(params)`). Empty means
    /// "unknown" (old data or a hand-built test) and matches by name only.
    pub identity: String,
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
    /// Scope-qualified identity used for matching, e.g. `Widget::draw(int)`.
    #[serde(default)]
    pub identity: String,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Baseline {
    pub rev: String,
    pub threshold: u32,
    /// Files that should have been compared: source files with a wired CCN
    /// grammar, after generated/vendored exclusions, when CCN is enabled.
    #[serde(default)]
    pub files_expected: usize,
    /// Exact or best-effort comparisons actually done.
    #[serde(default)]
    pub files_compared: usize,
    /// Baseline copy read but unparseable (heuristic tier).
    #[serde(default)]
    pub files_skipped: usize,
    /// Current file could not be measured although its language has a grammar
    /// (no summary, or a heuristic fallback after a parse failure).
    #[serde(default)]
    pub files_unmeasured: usize,
    /// Current file deliberately skipped by the CCN size guard. An accepted
    /// exclusion, not an incomplete comparison.
    #[serde(default)]
    pub files_oversize: usize,
    /// Read/stat failure on the current file.
    #[serde(default)]
    pub files_unreadable: usize,
    /// Files whose baseline copy could not be read at all (permission, timeout,
    /// unknown revision, VCS/transport error). They are excluded from
    /// `files_compared` and produce no findings: an unreadable file is not a
    /// new file.
    #[serde(default)]
    pub files_unavailable: usize,
    /// Files the version-control view does not contain (a Perforce exclusion or
    /// an unmapped path). They have no depot history, so they are excluded from
    /// `files_compared`, produce no findings, and are not an operational error:
    /// there is nothing to compare, not something that failed to be read.
    #[serde(default)]
    pub files_unmapped: usize,
    /// Files whose view status is unknown: the client view could not be
    /// established completely (unsupported wildcard, client-spec fallback), so
    /// "not mapped" cannot be asserted. Incomplete, not a skip.
    #[serde(default)]
    pub files_unknown: usize,
    /// Files where 2+ baseline and 2+ current functions shared a name/identity
    /// and could not be paired without guessing. Their findings are withheld
    /// and the comparison is incomplete.
    #[serde(default)]
    pub files_ambiguous: usize,
    /// `true` only when every required comparison was made. Oversize and
    /// unmapped files are accepted exclusions and do not make this false.
    #[serde(default)]
    pub complete: bool,
    /// Sorted by `ccn` descending.
    pub changes: Vec<Change>,
}

/// Extra lines a function may grow by before it counts as worsened on its own.
const NLOC_GROWTH: usize = 20;

/// The outcome of comparing one file: its findings, plus whether matching had
/// to be abandoned because two or more candidates were indistinguishable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    pub changes: Vec<Change>,
    /// `true` when at least one function could not be attributed without
    /// guessing. The findings are withheld in that case: an ambiguous match is
    /// not a finding.
    pub ambiguous: bool,
}

/// Compare one file's functions against its baseline.
///
/// Thin wrapper over [`compare`] for callers that only need the findings.
pub fn compare_file(
    path: &str,
    base: &[FunctionRef],
    current: &[FunctionRef],
    threshold: u32,
) -> Vec<Change> {
    compare(path, base, current, threshold).changes
}

/// Compare one file's functions against its baseline.
///
/// Matching is **identity-first and globally reserved**, never left-to-right
/// greedy:
///
/// 1. **Reserve exact identities.** Group base and current functions by
///    `identity`. A group with equal counts on both sides is paired before any
///    fallback runs. Inside a duplicate group, members with identical
///    `(ccn, nloc, max_nesting)` are paired first (that is the only evidence a
///    file gives for telling same-signature lambdas apart), then the remainder
///    in source order. A group with 2+ functions on both sides and unequal
///    counts is *ambiguous*: nothing can say which current function replaces
///    which baseline function.
/// 2. **Name fallback only when unambiguous on both sides.** Exactly one
///    unmatched base and exactly one unmatched current may share a name, and
///    their scopes must be compatible (if both identities carry a `::`, the
///    scope must match). This keeps a parameter-type edit attached to its old
///    function, while a new overload can never consume an existing baseline
///    function.
/// 3. **Ambiguity is not a finding.** A remaining current function that still
///    has an unmatched same-named baseline candidate marks the file ambiguous;
///    it is never reported as `Added` on a guess.
///
/// A finding needs the current function to be above the threshold, plus one of:
///
/// - no baseline function matched -> [`Kind::Added`];
/// - more branches than the baseline (`ccn` grew) -> [`Kind::Worsened`];
/// - deeper nesting than the baseline -> [`Kind::Worsened`];
/// - at least [`NLOC_GROWTH`] more lines than the baseline -> [`Kind::Worsened`].
///
/// This reports regressions, not the existing backlog: an already-complex
/// function that stays the same or improves is not a finding, but an
/// already-complex function that gets worse is.
pub fn compare(
    path: &str,
    base: &[FunctionRef],
    current: &[FunctionRef],
    threshold: u32,
) -> Comparison {
    let mut matched: Vec<Option<usize>> = vec![None; current.len()];
    let mut used_base = vec![false; base.len()];
    // An excluded function is part of an ambiguous group: it must not be
    // reported as `Added` and must not be offered to the name fallback.
    let mut excluded_base = vec![false; base.len()];
    let mut excluded_current = vec![false; current.len()];
    let mut ambiguous = false;

    let mut base_by_identity: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, function) in base.iter().enumerate() {
        if !function.identity.is_empty() {
            base_by_identity
                .entry(function.identity.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut current_by_identity: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, function) in current.iter().enumerate() {
        if !function.identity.is_empty() {
            current_by_identity
                .entry(function.identity.as_str())
                .or_default()
                .push(index);
        }
    }

    // Pass 1/2: identity groups, before any fallback.
    for (identity, currents) in &current_by_identity {
        let Some(bases) = base_by_identity.get(identity) else {
            continue;
        };
        if bases.len() == currents.len() {
            if let [base_index] = bases.as_slice() {
                pair(&mut matched, &mut used_base, currents[0], *base_index);
            } else {
                pair_duplicates(bases, currents, base, current, &mut matched, &mut used_base);
            }
        } else if bases.len() >= 2 && currents.len() >= 2 {
            // Two candidate sets that cannot be told apart.
            ambiguous = true;
            for &index in bases {
                excluded_base[index] = true;
            }
            for &index in currents {
                excluded_current[index] = true;
            }
        } else if let [base_index] = bases.as_slice() {
            // One baseline definition, several current definitions with the
            // same identity: prefer the metric-identical one, then source
            // order. The others are genuinely additional definitions.
            let pick = currents
                .iter()
                .copied()
                .find(|&current_index| metrics_equal(&current[current_index], &base[*base_index]))
                .unwrap_or(currents[0]);
            pair(&mut matched, &mut used_base, pick, *base_index);
        } else {
            // 2+ baseline definitions, one current: no way to attribute it.
            ambiguous = true;
            for &index in bases {
                excluded_base[index] = true;
            }
            for &index in currents {
                excluded_current[index] = true;
            }
        }
    }

    // Pass 3: name fallback, only when both sides are unambiguous.
    let mut unmatched_base_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, function) in base.iter().enumerate() {
        if !used_base[index] && !excluded_base[index] {
            unmatched_base_by_name
                .entry(function.name.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut unmatched_current_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, function) in current.iter().enumerate() {
        if matched[index].is_none() && !excluded_current[index] {
            unmatched_current_by_name
                .entry(function.name.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut fallback: Vec<(usize, usize)> = Vec::new();
    for (name, currents) in &unmatched_current_by_name {
        let Some(bases) = unmatched_base_by_name.get(name) else {
            continue;
        };
        if let ([base_index], [current_index]) = (bases.as_slice(), currents.as_slice()) {
            if scopes_compatible(&base[*base_index], &current[*current_index]) {
                fallback.push((*current_index, *base_index));
            }
        }
    }
    for (current_index, base_index) in fallback {
        pair(&mut matched, &mut used_base, current_index, base_index);
    }

    // Pass 4: anything still unmatched that has a same-named baseline
    // candidate left is ambiguous, not `Added`.
    for (index, function) in current.iter().enumerate() {
        if matched[index].is_some() || excluded_current[index] {
            continue;
        }
        let has_candidate = base.iter().enumerate().any(|(base_index, candidate)| {
            !used_base[base_index] && !excluded_base[base_index] && candidate.name == function.name
        });
        if has_candidate {
            ambiguous = true;
            excluded_current[index] = true;
        }
    }

    if ambiguous {
        // Never report a guessed pairing. The file is counted in
        // `files_ambiguous` and makes an enforced gate incomplete instead.
        return Comparison {
            changes: Vec::new(),
            ambiguous: true,
        };
    }

    let mut changes = Vec::new();
    for (index, function) in current.iter().enumerate() {
        if excluded_current[index] {
            continue;
        }
        let Some(base_index) = matched[index] else {
            if function.ccn > threshold {
                changes.push(Change {
                    path: path.to_string(),
                    name: function.name.clone(),
                    identity: function.identity.clone(),
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
            continue;
        };

        let matched = &base[base_index];
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
            identity: function.identity.clone(),
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

    sort_changes(&mut changes);
    Comparison {
        changes,
        ambiguous: false,
    }
}

fn pair(matched: &mut [Option<usize>], used_base: &mut [bool], current: usize, base: usize) {
    matched[current] = Some(base);
    used_base[base] = true;
}

/// Pair a duplicate identity group: identical metrics first (the only evidence
/// for telling same-signature lambdas apart), then source order.
fn pair_duplicates(
    bases: &[usize],
    currents: &[usize],
    base: &[FunctionRef],
    current: &[FunctionRef],
    matched: &mut [Option<usize>],
    used_base: &mut [bool],
) {
    let mut remaining: Vec<usize> = currents.to_vec();
    for &current_index in currents {
        let candidate = bases.iter().copied().find(|&base_index| {
            !used_base[base_index] && metrics_equal(&current[current_index], &base[base_index])
        });
        if let Some(base_index) = candidate {
            pair(matched, used_base, current_index, base_index);
            remaining.retain(|&index| index != current_index);
        }
    }
    let free_bases: Vec<usize> = bases
        .iter()
        .copied()
        .filter(|&index| !used_base[index])
        .collect();
    for (current_index, base_index) in remaining.into_iter().zip(free_bases) {
        pair(matched, used_base, current_index, base_index);
    }
}

fn metrics_equal(a: &FunctionRef, b: &FunctionRef) -> bool {
    a.ccn == b.ccn && a.nloc == b.nloc && a.max_nesting == b.max_nesting
}

/// The scope part of an identity (`Widget::draw(int)` -> `Widget`), if any.
fn scope(identity: &str) -> Option<&str> {
    let head = identity.split_once('(').map_or(identity, |(head, _)| head);
    head.rsplit_once("::").map(|(scope, _)| scope)
}

/// Scopes are compatible unless both identities name a scope and the scopes
/// differ. `A::f(int)` must never absorb `B::f(int)` through a name fallback.
fn scopes_compatible(base: &FunctionRef, current: &FunctionRef) -> bool {
    match (scope(&base.identity), scope(&current.identity)) {
        (Some(base_scope), Some(current_scope)) => base_scope == current_scope,
        _ => true,
    }
}

/// Build a baseline report from a completed scan.
///
/// Every source file whose language has a wired CCN grammar is assigned
/// exactly one outcome, whether or not the parse succeeded. A file absent at
/// `rev` is compared against an empty baseline (it is new). A file whose
/// baseline copy is **unavailable** is counted in
/// [`Baseline::files_unavailable`] and skipped entirely, so a VCS read failure
/// can never be reported as added functions. A file the version-control view
/// does not contain is counted in [`Baseline::files_unmapped`] - no history,
/// not a failure - while an unknown view is [`Baseline::files_unknown`] and
/// does make the comparison incomplete.
///
/// The VCS reads dominate the cost and are independent per file, so they run on
/// rayon's pool. The result is sorted afterwards, which makes the output
/// independent of worker scheduling.
pub fn collect(result: &ScanResult, backend: Backend, rev: &str, threshold: u32) -> Baseline {
    collect_with_manifest(result, backend, rev, threshold).0
}

/// [`collect`], plus one record per accounted file for `--manifest`.
///
/// The records are *not* part of [`Baseline`]: they would bloat report JSON.
/// The caller writes them to a separate, versioned document.
pub fn collect_with_manifest(
    result: &ScanResult,
    backend: Backend,
    rev: &str,
    threshold: u32,
) -> (Baseline, Vec<FileRecord>) {
    let root = Path::new(&result.root);
    // One extra VCS command, before the parallel reads: rename detection is not
    // per-file work. An empty map (P4, or no renames) is the old behaviour.
    let renamed = crate::vcs::renamed_from(root, backend, rev);

    let outcomes: Vec<FileOutcome> = result
        .files
        .par_iter()
        .filter_map(|file| classify_file(root, backend, rev, file, &renamed))
        .collect();

    summarize(outcomes, rev, threshold)
}

/// Decide what to do with one scanned file.
///
/// `None` means the file is out of scope for a CCN comparison: generated or
/// vendored, a language with no grammar, or CCN disabled for the whole scan.
/// Every other file becomes exactly one [`FileOutcome`].
fn classify_file(
    root: &Path,
    backend: Backend,
    rev: &str,
    file: &FileLoc,
    renamed: &HashMap<String, String>,
) -> Option<FileOutcome> {
    if file.is_excluded() {
        return None;
    }
    let path = file.path.clone();
    match (&file.ccn, file.ccn_skip) {
        // No grammar: deliberately out of scope, not an incomplete measurement.
        (_, Some(CcnSkip::NoGrammar)) => None,
        // A whole-scan `--no-ccn` leaves both fields empty; nothing to compare.
        (None, None) => None,
        (None, Some(CcnSkip::Oversize)) => {
            wired_for(file).then_some(FileOutcome::Oversize { path })
        }
        (None, Some(CcnSkip::ReadError)) => wired_for(file).then(|| FileOutcome::Unreadable {
            path,
            reason: "the current file could not be read".into(),
        }),
        (Some(summary), _) => {
            if !wired_for(file) {
                return None;
            }
            if summary.tier == CcnTier::Heuristic {
                // A grammar exists, so a keyword fallback means the parser gave
                // up: this is a parse failure, not "no grammar".
                return Some(FileOutcome::Unmeasured {
                    path,
                    reason: "heuristic fallback: the parser recovered no functions".into(),
                });
            }
            if summary.detail.is_empty() && summary.tier == CcnTier::BestEffort {
                return Some(FileOutcome::Unmeasured {
                    path,
                    reason: "best-effort parse recovered no functions".into(),
                });
            }
            if summary.detail.is_empty() {
                // A clean parse that found no functions is a complete
                // measurement with nothing to match; no VCS read is needed.
                return Some(FileOutcome::Compared {
                    path,
                    base: Vec::new(),
                    current: Vec::new(),
                    base_found: true,
                    base_absent: false,
                });
            }
            Some(measure_baseline(root, backend, rev, file, summary, renamed))
        }
    }
}

/// `true` when the file's language has a wired CCN grammar.
fn wired_for(file: &FileLoc) -> bool {
    crate::ccn::Lang::from_path(&file.path)
        .or_else(|| crate::ccn::Lang::from_tokei_name(&file.lang))
        .is_some()
}

/// Turn per-file outcomes into the reportable [`Baseline`] plus manifest rows.
///
/// Pulled out of [`collect`] so the counting rules are testable without a VCS:
/// an unavailable or unknown read must never appear as `Added`, and every
/// required-but-missing comparison must make `complete` false.
fn summarize(outcomes: Vec<FileOutcome>, rev: &str, threshold: u32) -> (Baseline, Vec<FileRecord>) {
    let mut files_compared = 0usize;
    let mut files_skipped = 0usize;
    let mut files_unmeasured = 0usize;
    let mut files_oversize = 0usize;
    let mut files_unreadable = 0usize;
    let mut files_unavailable = 0usize;
    let mut files_unmapped = 0usize;
    let mut files_unknown = 0usize;
    let mut files_ambiguous = 0usize;
    let mut changes = Vec::new();
    let mut records = Vec::new();

    let mut record = |path: &str, outcome: &'static str, reason: String, base_found: bool| {
        records.push(FileRecord {
            path: path.to_string(),
            outcome,
            reason,
            base_found,
        });
    };

    for outcome in outcomes {
        match outcome {
            FileOutcome::Skipped { path, reason } => {
                files_skipped += 1;
                record(&path, "skipped", reason, true);
            }
            FileOutcome::Unmeasured { path, reason } => {
                files_unmeasured += 1;
                record(&path, "unmeasured", reason, true);
            }
            FileOutcome::Oversize { path } => {
                files_oversize += 1;
                record(
                    &path,
                    "oversize",
                    "exceeds the CCN size guard".into(),
                    false,
                );
            }
            FileOutcome::Unreadable { path, reason } => {
                files_unreadable += 1;
                record(&path, "unreadable", reason, false);
            }
            FileOutcome::Unavailable { path, reason } => {
                files_unavailable += 1;
                record(&path, "unavailable", reason, false);
            }
            FileOutcome::Unmapped { path } => {
                files_unmapped += 1;
                record(
                    &path,
                    "unmapped",
                    "path is outside the version-control view".into(),
                    false,
                );
            }
            FileOutcome::Unknown { path, reason } => {
                files_unknown += 1;
                record(&path, "unknown", reason, false);
            }
            FileOutcome::Compared {
                path,
                base,
                current,
                base_found,
                base_absent,
            } => {
                files_compared += 1;
                let comparison = compare(&path, &base, &current, threshold);
                if comparison.ambiguous {
                    files_ambiguous += 1;
                    record(
                        &path,
                        "ambiguous",
                        format!(
                            "{} baseline and {} current function(s) could not be paired",
                            base.len(),
                            current.len()
                        ),
                        base_found,
                    );
                } else {
                    changes.extend(comparison.changes);
                    let outcome = if base_absent { "absent" } else { "compared" };
                    record(
                        &path,
                        outcome,
                        if base_absent {
                            "not present at the baseline revision; compared against empty".into()
                        } else if current.iter().any(|f| f.tier == CcnTier::BestEffort) {
                            "best-effort comparison (the parser recovered from syntax errors)"
                                .into()
                        } else {
                            "exact comparison".into()
                        },
                        base_found,
                    );
                }
            }
        }
    }

    sort_changes(&mut changes);
    records.sort_by(|a, b| a.path.cmp(&b.path));

    let files_expected = records.len();
    let mut baseline = Baseline {
        rev: rev.to_string(),
        threshold,
        files_expected,
        files_compared,
        files_skipped,
        files_unmeasured,
        files_oversize,
        files_unreadable,
        files_unavailable,
        files_unmapped,
        files_unknown,
        files_ambiguous,
        complete: false,
        changes,
    };
    baseline.complete = baseline.is_complete_counts();
    (baseline, records)
}

impl Baseline {
    /// `true` when every required comparison was made.
    ///
    /// Oversize and unmapped files are explicit, accepted exclusions, not
    /// missing comparisons. Everything else - a skipped or unmeasured file, a
    /// read/VCS/view failure, or an ambiguous match - means the delta cannot be
    /// trusted as a gate result.
    pub fn is_complete_counts(&self) -> bool {
        self.files_skipped == 0
            && self.files_unmeasured == 0
            && self.files_unreadable == 0
            && self.files_unknown == 0
            && self.files_unavailable == 0
            && self.files_ambiguous == 0
    }

    /// One human-readable line per reason the comparison is incomplete.
    pub fn incomplete_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        let mut push = |count: usize, what: &str| {
            if count > 0 {
                reasons.push(format!("{count} {what}"));
            }
        };
        push(
            self.files_skipped,
            "file(s) whose baseline copy could not be parsed",
        );
        push(
            self.files_unmeasured,
            "current file(s) could not be measured (parse failure)",
        );
        push(self.files_unreadable, "file(s) could not be read");
        push(
            self.files_unknown,
            "file(s) whose version-control view status is unknown",
        );
        push(
            self.files_unavailable,
            "baseline file(s) could not be read from the revision",
        );
        push(
            self.files_ambiguous,
            "file(s) whose functions could not be matched unambiguously",
        );
        reasons
    }

    /// `(N compared, N skipped, ...)`, omitting zero categories but always
    /// naming `compared` so an empty comparison is still visible.
    pub fn counts_line(&self) -> String {
        let mut parts = vec![format!("{} compared", self.files_compared)];
        let mut add = |count: usize, label: &str| {
            if count > 0 {
                parts.push(format!("{count} {label}"));
            }
        };
        add(self.files_skipped, "skipped");
        add(self.files_unmeasured, "unmeasured");
        add(self.files_oversize, "oversize");
        add(self.files_unreadable, "unreadable");
        add(self.files_unknown, "unknown");
        add(self.files_unavailable, "unavailable");
        add(self.files_unmapped, "unmapped");
        add(self.files_ambiguous, "ambiguous");
        format!("({})", parts.join(", "))
    }
}

/// Convenience for the CLI: true when the baseline found anything to report.
pub fn has_changes(baseline: &Baseline) -> bool {
    !baseline.changes.is_empty()
}

/// Per-file result of the (I/O bound) baseline read and the current-side
/// classification.
enum FileOutcome {
    /// The baseline copy was read but is not exact: an unparseable snapshot.
    Skipped { path: String, reason: String },
    /// The current file could not be measured although its language has a
    /// grammar.
    Unmeasured { path: String, reason: String },
    /// Deliberately out of the CCN pass because of its size.
    Oversize { path: String },
    /// The current file could not be read.
    Unreadable { path: String, reason: String },
    /// The baseline read could not be answered; never invented as `Added`.
    Unavailable { path: String, reason: String },
    /// The path is not in the client's view: no depot history to compare.
    Unmapped { path: String },
    /// The view itself could not be established, so "not mapped" is unknown.
    Unknown { path: String, reason: String },
    Compared {
        path: String,
        base: Vec<FunctionRef>,
        current: Vec<FunctionRef>,
        base_found: bool,
        base_absent: bool,
    },
}

/// Read and analyse one file at `rev`.
///
/// Every failure mode is kept distinct: a file that does not exist at `rev` is
/// new (empty baseline), a read the backend could not answer is
/// [`FileOutcome::Unavailable`], an unparseable baseline copy is
/// [`FileOutcome::Skipped`], and an incomplete view is
/// [`FileOutcome::Unknown`]. None of them panics or fabricates `Added` from a
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
                base_found: false,
                base_absent: true,
            };
        }
        FileAt::Unavailable => {
            return FileOutcome::Unavailable {
                path: file.path.clone(),
                reason: "the baseline copy could not be read (permission, timeout or VCS error)"
                    .into(),
            };
        }
        FileAt::Unmapped => {
            return FileOutcome::Unmapped {
                path: file.path.clone(),
            };
        }
        // The view itself could not be established completely: the path may be
        // in view, so this must never be mistaken for an ordinary skip.
        FileAt::Unknown => {
            return FileOutcome::Unknown {
                path: file.path.clone(),
                reason: "the version-control view could not be established completely".into(),
            };
        }
    };

    let base = crate::ccn::analyze_file(Path::new(&file.path), &bytes, &file.lang);
    if base.tier != CcnTier::Exact {
        // A degraded baseline copy (heuristic *or* partial parse) has no exact
        // per-function numbers to match against, so every current function
        // would look newly added or shifted.
        return FileOutcome::Skipped {
            path: file.path.clone(),
            reason: if base.tier == CcnTier::Heuristic {
                "the baseline copy is unparseable (heuristic fallback)".into()
            } else {
                "the baseline copy is only partially parseable (syntax errors)".into()
            },
        };
    }

    FileOutcome::Compared {
        path: file.path.clone(),
        base: base.detail.iter().map(to_ref(base.tier)).collect(),
        current: summary.detail.iter().map(to_ref(summary.tier)).collect(),
        base_found: true,
        base_absent: false,
    }
}

/// Read the baseline copy of `path`, following a rename when the path is new.
///
/// The rename fallback is only tried for [`FileAt::Absent`]: an unavailable read
/// says nothing about whether the old path would have answered, and retrying
/// there would only risk turning a failure into a fabricated baseline. An
/// unavailable read of the old path stays unavailable rather than defaulting to
/// "new". An unmapped path is not retried either: it has no history of its own,
/// and the view that hides it would hide its old name too. An unknown view is
/// left unknown for the same reason: a rename guess cannot repair a view that
/// was never established.
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
        identity: function.identity.clone(),
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

/// One accounted file, for the versioned `--manifest` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRecord {
    pub path: String,
    /// `compared | absent | skipped | unmeasured | oversize | unreadable |
    /// unmapped | unknown | unavailable | ambiguous`.
    pub outcome: &'static str,
    pub reason: String,
    /// Whether a baseline copy was found at the revision. `false` for a new
    /// file, an unreadable current file, or a file with no history.
    pub base_found: bool,
}

/// The machine-readable measurement manifest written by `--manifest`.
///
/// Versioned separately from the report: consumers must be able to tell when
/// the accounting contract changed. `counts` mirrors [`Baseline`] so a reader
/// can audit the delta without re-parsing the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub tool_version: String,
    pub rev: String,
    pub threshold: u32,
    pub complete: bool,
    pub counts: ManifestCounts,
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestCounts {
    pub expected: usize,
    pub compared: usize,
    pub skipped: usize,
    pub unmeasured: usize,
    pub oversize: usize,
    pub unreadable: usize,
    pub unmapped: usize,
    pub unknown: usize,
    pub unavailable: usize,
    pub ambiguous: usize,
}

pub const MANIFEST_VERSION: u32 = 1;

impl Manifest {
    pub fn new(baseline: &Baseline, files: Vec<FileRecord>) -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            rev: baseline.rev.clone(),
            threshold: baseline.threshold,
            complete: baseline.complete,
            counts: ManifestCounts {
                expected: baseline.files_expected,
                compared: baseline.files_compared,
                skipped: baseline.files_skipped,
                unmeasured: baseline.files_unmeasured,
                oversize: baseline.files_oversize,
                unreadable: baseline.files_unreadable,
                unmapped: baseline.files_unmapped,
                unknown: baseline.files_unknown,
                unavailable: baseline.files_unavailable,
                ambiguous: baseline.files_ambiguous,
            },
            files,
        }
    }

    /// The serialized document. Pretty-printed so a human reviewer can diff it.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
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
            identity: format!("{name}()"),
            ccn,
            line,
            nloc,
            max_nesting,
            tier: CcnTier::Exact,
        }
    }

    /// A reference with an explicit identity, as the engine would produce it.
    fn overload(identity: &str, name: &str, line: usize, ccn: u32) -> FunctionRef {
        FunctionRef {
            name: name.to_string(),
            identity: identity.to_string(),
            ccn,
            line,
            nloc: 1,
            max_nesting: 0,
            tier: CcnTier::Exact,
        }
    }

    /// A reference whose identity is unknown (old data / hand-built fixture).
    fn no_identity(name: &str, line: usize, ccn: u32) -> FunctionRef {
        FunctionRef {
            name: name.to_string(),
            identity: String::new(),
            ccn,
            line,
            nloc: 1,
            max_nesting: 0,
            tier: CcnTier::Exact,
        }
    }

    fn file(path: &str, functions: &[(&str, u32)]) -> FileLoc {
        let detail: Vec<FunctionCcn> = functions
            .iter()
            .enumerate()
            .map(|(i, (name, ccn))| FunctionCcn {
                name: (*name).to_string(),
                identity: format!("{name}()"),
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
            ccn_skip: None,
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
    fn reordered_overloads_with_identical_bodies_are_not_a_change() {
        // Same declarations, physically swapped. A name-only matcher would pair
        // `f(int)` in base with `f(double)` in current because they share the
        // name `f`; identities pair them with themselves.
        let base = vec![
            overload("f(int)", "f", 1, 1),
            overload("f(double)", "f", 10, 1),
        ];
        let current = vec![
            overload("f(double)", "f", 1, 1),
            overload("f(int)", "f", 10, 1),
        ];

        assert!(
            compare_file("a.rs", &base, &current, 10).is_empty(),
            "reordering overloads is not a regression"
        );
    }

    #[test]
    fn a_complex_body_move_stays_on_the_identity_it_belongs_to() {
        let base = vec![
            overload("f(int)", "f", 1, 1),
            overload("f(double)", "f", 10, 1),
        ];
        // `f(double)` moved above `f(int)`; only `f(int)` got complex.
        let current = vec![
            overload("f(double)", "f", 1, 1),
            overload("f(int)", "f", 10, 20),
        ];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(changes.len(), 1, "only the identity that grew is reported");
        assert_eq!(changes[0].name, "f");
        assert_eq!(changes[0].line, 10, "the complex body is `f(int)`");
        assert_eq!(changes[0].base_ccn, Some(1));
        assert_eq!(changes[0].ccn, 20);
        assert_eq!(changes[0].kind, Kind::Worsened);
        assert_eq!(reasons(&changes[0]), vec!["ccn +19"]);
    }

    #[test]
    fn a_parameter_type_edit_is_not_a_new_function() {
        // The identity changed (`int` -> `long`) but the name is unambiguous,
        // so this is the same function edited, not an addition.
        let base = vec![overload("f(int)", "f", 1, 20)];
        let current = vec![overload("f(long)", "f", 1, 20)];

        assert!(
            compare_file("a.rs", &base, &current, 10).is_empty(),
            "an unambiguous name fallback keeps the baseline attached"
        );
    }

    #[test]
    fn an_empty_base_identity_falls_back_to_the_unambiguous_name() {
        let base = vec![no_identity("f", 1, 20)];
        let current = vec![overload("f(long)", "f", 1, 20)];

        assert!(compare_file("a.rs", &base, &current, 10).is_empty());
    }

    #[test]
    fn a_genuinely_new_overload_is_added_once() {
        let base = vec![overload("f(int)", "f", 1, 20)];
        let current = vec![
            overload("f(int)", "f", 1, 20),
            overload("f(double)", "f", 2, 20),
        ];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(changes.len(), 1, "the existing overload is not a finding");
        assert_eq!(changes[0].name, "f");
        assert_eq!(changes[0].line, 2);
        assert_eq!(changes[0].base_ccn, None);
        assert_eq!(changes[0].kind, Kind::Added);
        assert_eq!(reasons(&changes[0]), vec!["new function"]);
    }

    #[test]
    fn reordered_const_and_non_const_overloads_are_not_a_change() {
        // The engine identity carries cv/ref qualifiers, so these are distinct
        // identities. Physically swapping them must not move complexity from
        // one to the other.
        let base = vec![
            overload("X::f(int) const", "f", 1, 12),
            overload("X::f(int)", "f", 20, 11),
        ];
        let current = vec![
            overload("X::f(int)", "f", 1, 11),
            overload("X::f(int) const", "f", 20, 12),
        ];

        assert!(
            compare_file("a.cpp", &base, &current, 10).is_empty(),
            "const and non-const overloads have distinct identities"
        );
    }

    #[test]
    fn reordered_anonymous_lambdas_are_not_a_change() {
        // Same-signature lambdas collide on identity. Their bodies are the only
        // evidence, and the metrics still pair exactly, so no finding is made.
        let base = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 12),
            overload("(anonymous)(int)", "(anonymous)", 20, 11),
        ];
        let current = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 11),
            overload("(anonymous)(int)", "(anonymous)", 20, 12),
        ];

        let comparison = compare("a.cpp", &base, &current, 10);
        assert!(
            comparison.changes.is_empty(),
            "swapping same-signature lambdas is not a regression: {:?}",
            comparison.changes
        );
        assert!(!comparison.ambiguous);
    }

    #[test]
    fn a_parameter_rename_does_not_fabricate_additions() {
        // The identity no longer contains parameter names, so a consistent
        // rename keeps every overload attached to its own baseline function.
        let base = vec![
            overload("f(int)", "f", 1, 12),
            overload("f(double)", "f", 20, 11),
        ];
        let current = vec![
            overload("f(int)", "f", 1, 12),
            overload("f(double)", "f", 20, 11),
        ];

        assert!(compare_file("a.cpp", &base, &current, 10).is_empty());
    }

    #[test]
    fn a_new_overload_inserted_before_an_unchanged_one_is_only_the_new_one() {
        // Baseline has one complex `f(int)`. A simple `f(double)` is inserted
        // *before* it. The old code's name fallback handed the baseline to the
        // new overload and reported the real function as `Added`.
        let base = vec![overload("f(int)", "f", 20, 12)];
        let current = vec![
            overload("f(double)", "f", 1, 1),
            overload("f(int)", "f", 20, 12),
        ];

        let changes = compare_file("a.cpp", &base, &current, 10);
        assert!(
            changes.is_empty(),
            "the unchanged overload must keep its baseline: {:?}",
            changes
        );
    }

    #[test]
    fn a_name_fallback_never_crosses_scopes() {
        // `A::f(int)` and `B::f(int)` share the short name but not the scope.
        // The name fallback must not move the baseline across scopes: the
        // comparison is ambiguous instead of a fabricated delta.
        let base = vec![overload("A::f(int)", "f", 1, 20)];
        let current = vec![overload("B::f(int)", "f", 1, 12)];

        let comparison = compare("a.cpp", &base, &current, 10);
        assert!(comparison.ambiguous, "different scopes cannot be paired");
        assert!(comparison.changes.is_empty());
    }

    #[test]
    fn two_ambiguous_current_functions_are_never_guessed() {
        let base = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 11),
            overload("(anonymous)(int)", "(anonymous)", 5, 12),
        ];
        let current = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 13),
            overload("(anonymous)(int)", "(anonymous)", 5, 30),
            overload("(anonymous)(int)", "(anonymous)", 9, 40),
        ];

        let comparison = compare("a.cpp", &base, &current, 10);
        assert!(comparison.ambiguous);
        assert!(
            comparison.changes.is_empty(),
            "ambiguity must not become a finding"
        );
    }

    #[test]
    fn a_rename_with_an_unchanged_body_is_still_added() {
        let base = vec![overload("old()", "old", 1, 20)];
        let current = vec![overload("renamed()", "renamed", 1, 20)];

        let changes = compare_file("a.rs", &base, &current, 10);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "renamed");
        assert_eq!(
            changes[0].kind,
            Kind::Added,
            "no rename detection is invented"
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
    fn files_without_a_function_are_still_accounted_for() {
        // Ruby has no CCN grammar: out of scope. A file with no CCN summary
        // (as `--no-ccn` leaves it) is out of scope too. An exact file with no
        // functions was measured - there is simply nothing to match - so it is
        // a completed comparison, not a skipped one.
        let mut heuristic = file("a.rb", &[]);
        heuristic.lang = "Ruby".to_string();
        heuristic.ccn = Some(CcnSummary::heuristic(40));
        let no_ccn = FileLoc {
            ccn: None,
            ..file("b.rs", &[("f", 40)])
        };
        let empty = file("c.rs", &[]);
        let result = scan_result(MISSING_ROOT, vec![heuristic, no_ccn, empty]);

        let baseline = collect(&result, Backend::Git, "HEAD~1", 10);

        assert_eq!(baseline.files_expected, 1, "only c.rs has a CCN grammar");
        assert_eq!(baseline.files_compared, 1);
        assert_eq!(baseline.files_skipped, 0);
        assert_eq!(baseline.files_unmeasured, 0);
        assert!(baseline.changes.is_empty());
        assert!(baseline.complete);
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
    fn ccn_skip_reasons_are_classified_and_counted() {
        let mut read_error = file("a.rs", &[]);
        read_error.ccn = None;
        read_error.ccn_skip = Some(CcnSkip::ReadError);
        let mut oversize = file("b.rs", &[]);
        oversize.ccn = None;
        oversize.ccn_skip = Some(CcnSkip::Oversize);
        let mut no_grammar = file("c.md", &[]);
        no_grammar.lang = "Markdown".to_string();
        no_grammar.ccn = Some(CcnSummary::heuristic(7));
        no_grammar.ccn_skip = Some(CcnSkip::NoGrammar);
        // An oversize Markdown file is still out of scope for CCN.
        let mut oversize_markdown = file("d.md", &[]);
        oversize_markdown.lang = "Markdown".to_string();
        oversize_markdown.ccn = None;
        oversize_markdown.ccn_skip = Some(CcnSkip::Oversize);

        let result = scan_result(
            MISSING_ROOT,
            vec![read_error, oversize, no_grammar, oversize_markdown],
        );
        let baseline = collect(&result, Backend::Git, "HEAD", 10);

        assert_eq!(baseline.files_unreadable, 1);
        assert_eq!(baseline.files_oversize, 1);
        assert_eq!(
            baseline.files_expected, 2,
            "only the CCN-wired files are expected"
        );
        assert!(!baseline.complete, "an unreadable file is incomplete");
    }

    #[test]
    fn an_unmapped_file_is_neither_new_nor_a_read_failure() {
        // A path the view does not contain has no depot history: it must not be
        // compared (which would have made its functions `Added`), and it must
        // not be counted as a read failure either.
        let outcomes = vec![
            FileOutcome::Unmapped {
                path: "gone.rs".to_string(),
            },
            FileOutcome::Unavailable {
                path: "unreadable.rs".to_string(),
                reason: "permission".to_string(),
            },
            FileOutcome::Compared {
                path: "kept.rs".to_string(),
                base: vec![func("foo", 1, 1)],
                current: vec![func("foo", 2, 20)],
                base_found: true,
                base_absent: false,
            },
        ];

        let (baseline, files) = summarize(outcomes, "123", 10);

        assert_eq!(baseline.files_unmapped, 1);
        assert_eq!(baseline.files_unavailable, 1);
        assert_eq!(baseline.files_compared, 1);
        assert_eq!(baseline.files_skipped, 0);
        assert_eq!(baseline.files_expected, 3);
        assert_eq!(
            baseline
                .changes
                .iter()
                .map(|change| (change.path.as_str(), change.kind))
                .collect::<Vec<_>>(),
            vec![("kept.rs", Kind::Worsened)],
            "the unmapped path produced no Added finding"
        );
        assert!(!baseline.complete, "an unavailable read is incomplete");
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn only_unmapped_and_oversize_files_leave_an_empty_but_complete_baseline() {
        let (baseline, _) = summarize(
            vec![
                FileOutcome::Unmapped {
                    path: "gone.rs".to_string(),
                },
                FileOutcome::Unmapped {
                    path: "also-gone.rs".to_string(),
                },
                FileOutcome::Oversize {
                    path: "bundle.js".to_string(),
                },
            ],
            "456",
            10,
        );

        assert_eq!(baseline.files_unmapped, 2);
        assert_eq!(baseline.files_oversize, 1);
        assert_eq!(baseline.files_unavailable, 0, "not a failure");
        assert_eq!(baseline.files_compared, 0);
        assert!(baseline.changes.is_empty());
        assert!(!has_changes(&baseline));
        assert!(
            baseline.complete,
            "unmapped and oversize are accepted exclusions"
        );
        assert!(baseline.incomplete_reasons().is_empty());
    }

    #[test]
    fn an_unknown_view_makes_the_comparison_incomplete() {
        let (baseline, files) = summarize(
            vec![FileOutcome::Unknown {
                path: "src/a.cpp".to_string(),
                reason: "the client view could not be established".to_string(),
            }],
            "789",
            10,
        );

        assert_eq!(baseline.files_unknown, 1);
        assert_eq!(baseline.files_expected, 1);
        assert_eq!(baseline.files_compared, 0);
        assert!(!baseline.complete, "an unknown view is not a skip");
        assert!(
            baseline
                .incomplete_reasons()
                .iter()
                .any(|reason| reason.contains("view status is unknown")),
            "the reason must name the unknown view: {:?}",
            baseline.incomplete_reasons()
        );
        assert_eq!(files[0].outcome, "unknown");
    }

    #[test]
    fn an_unmeasured_current_file_makes_the_comparison_incomplete() {
        let (baseline, files) = summarize(
            vec![FileOutcome::Unmeasured {
                path: "src/a.c".to_string(),
                reason: "heuristic fallback".to_string(),
            }],
            "HEAD",
            10,
        );

        assert_eq!(baseline.files_unmeasured, 1);
        assert!(!baseline.complete);
        assert_eq!(files[0].outcome, "unmeasured");
    }

    #[test]
    fn an_ambiguous_file_withholds_its_findings_and_is_incomplete() {
        // Two identical identities on both sides with unequal counts cannot be
        // attributed; the comparison must not guess.
        let base = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 11),
            overload("(anonymous)(int)", "(anonymous)", 5, 12),
        ];
        let current = vec![
            overload("(anonymous)(int)", "(anonymous)", 1, 12),
            overload("(anonymous)(int)", "(anonymous)", 5, 11),
            overload("(anonymous)(int)", "(anonymous)", 9, 30),
        ];

        let (baseline, files) = summarize(
            vec![FileOutcome::Compared {
                path: "a.cpp".to_string(),
                base,
                current,
                base_found: true,
                base_absent: false,
            }],
            "HEAD",
            10,
        );

        assert_eq!(baseline.files_ambiguous, 1);
        assert!(
            baseline.changes.is_empty(),
            "an ambiguous file must not produce guessed findings: {:?}",
            baseline.changes
        );
        assert!(!baseline.complete);
        assert_eq!(files[0].outcome, "ambiguous");
    }

    #[test]
    fn a_new_file_is_reported_as_absent_in_the_manifest() {
        let (baseline, files) = summarize(
            vec![FileOutcome::Compared {
                path: "new.rs".to_string(),
                base: Vec::new(),
                current: vec![func("f", 1, 11)],
                base_found: false,
                base_absent: true,
            }],
            "HEAD",
            10,
        );

        assert_eq!(baseline.files_compared, 1);
        assert!(baseline.complete);
        assert_eq!(files[0].outcome, "absent");
        assert!(!files[0].base_found);
    }

    #[test]
    fn the_counts_are_published_in_the_report_json() {
        // Report JSON must carry the counts so a reader can tell "no history"
        // apart from "nothing to compare". `serde(default)` keeps older
        // consumers that do not know the keys working.
        let (baseline, _) = summarize(Vec::new(), "123", 10);
        let json = serde_json::to_string(&baseline).expect("serialize the baseline");

        for key in [
            "\"files_expected\":0",
            "\"files_compared\":0",
            "\"files_skipped\":0",
            "\"files_unmeasured\":0",
            "\"files_oversize\":0",
            "\"files_unreadable\":0",
            "\"files_unavailable\":0",
            "\"files_unmapped\":0",
            "\"files_unknown\":0",
            "\"files_ambiguous\":0",
            "\"complete\":",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    #[test]
    fn the_manifest_is_versioned_and_lists_every_accounted_file() {
        let (baseline, files) = summarize(
            vec![
                FileOutcome::Oversize {
                    path: "bundle.js".to_string(),
                },
                FileOutcome::Compared {
                    path: "a.c".to_string(),
                    base: vec![func("f", 1, 2)],
                    current: vec![func("f", 2, 13)],
                    base_found: true,
                    base_absent: false,
                },
            ],
            "HEAD",
            10,
        );
        let manifest = Manifest::new(&baseline, files);
        let json = manifest.to_json().expect("serialize the manifest");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["manifest_version"], 1);
        assert_eq!(parsed["tool_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["rev"], "HEAD");
        assert_eq!(parsed["complete"], true);
        assert_eq!(parsed["counts"]["expected"], 2);
        assert_eq!(parsed["counts"]["oversize"], 1);
        assert_eq!(parsed["counts"]["compared"], 1);
        let rows = parsed["files"].as_array().expect("files array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["path"], "a.c");
        assert_eq!(rows[0]["outcome"], "compared");
        assert_eq!(rows[1]["path"], "bundle.js");
        assert_eq!(rows[1]["outcome"], "oversize");
        assert_eq!(manifest.manifest_version, MANIFEST_VERSION);
    }

    #[test]
    fn has_changes_tracks_the_change_vector() {
        let empty = Baseline {
            rev: "HEAD".to_string(),
            threshold: 10,
            ..Baseline::default()
        };
        assert!(!has_changes(&empty));

        let found = Baseline {
            changes: compare_file("a.rs", &[], &[func("f", 1, 11)], 10),
            ..empty
        };
        assert!(has_changes(&found));
    }

    #[test]
    fn the_counts_line_omits_zero_categories() {
        let baseline = Baseline {
            rev: "HEAD".to_string(),
            threshold: 10,
            files_compared: 4,
            files_unmeasured: 1,
            files_unmapped: 2,
            ..Baseline::default()
        };

        assert_eq!(
            baseline.counts_line(),
            "(4 compared, 1 unmeasured, 2 unmapped)"
        );

        let only_compared = Baseline {
            files_compared: 3,
            ..Baseline::default()
        };
        assert_eq!(only_compared.counts_line(), "(3 compared)");
    }
}
