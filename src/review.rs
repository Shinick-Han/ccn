//! Baseline review queue.
//!
//! [`ReviewQueue`] is the model behind the workbench review panel: it holds the
//! baseline [`Change`]s that matter for the active file (or all files), keeps a
//! valid selection, and summarizes the delta. It performs no I/O and never
//! mutates the incoming changes; the caller owns the [`Baseline`].
//!
//! The incoming order is preserved. [`Baseline::changes`] is already sorted by
//! CCN descending, so the queue shows the most complex regressions first.
//!
//! [`Baseline`]: crate::baseline::Baseline
//! [`Baseline::changes`]: crate::baseline::Baseline::changes
//! [`Change`]: crate::baseline::Change

use crate::baseline::{Change, Kind};

/// A selectable list of baseline changes, optionally restricted to one path.
#[derive(Debug, Clone)]
pub struct ReviewQueue {
    /// Every change from the baseline, in the incoming order. Kept so
    /// [`ReviewQueue::set_path_filter`] can widen the filter again.
    source: Vec<Change>,
    /// The changes matching the current path filter, in source order.
    changes: Vec<Change>,
    /// Index into `changes`. Always `0` when `changes` is empty, otherwise
    /// guaranteed to be in `0..changes.len()`.
    selected: usize,
    /// Normalized path filter (separators are `/`).
    path_filter: Option<String>,
}

impl ReviewQueue {
    /// Build a queue over `changes`, optionally restricted to one exact path.
    ///
    /// Paths are compared after normalizing `\` to `/`, so a Windows path and a
    /// Unix path naming the same file match. The comparison is otherwise
    /// case-sensitive and exact: no prefix or substring matching.
    pub fn new(changes: &[Change], path_filter: Option<&str>) -> Self {
        let mut queue = Self {
            source: changes.to_vec(),
            changes: Vec::new(),
            selected: 0,
            path_filter: path_filter.map(normalize_path),
        };
        queue.refilter();
        queue
    }

    /// `true` when no change matches the current filter.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Number of changes matching the current filter.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Index of the selected change. `0` when the queue is empty.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The selected change, or `None` when the queue is empty.
    pub fn selected_change(&self) -> Option<&Change> {
        self.changes.get(self.selected)
    }

    /// The changes matching the current filter, in the incoming order.
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Move the selection down by one.
    ///
    /// Clamps at the last change; it does **not** wrap.
    pub fn next(&mut self) {
        if !self.changes.is_empty() {
            self.selected = (self.selected + 1).min(self.changes.len() - 1);
        }
    }

    /// Move the selection up by one.
    ///
    /// Clamps at the first change; it does **not** wrap.
    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Select `index`, clamped to the last change.
    ///
    /// A no-op on an empty queue (the selection stays `0`).
    pub fn select(&mut self, index: usize) {
        if !self.changes.is_empty() {
            self.selected = index.min(self.changes.len() - 1);
        }
    }

    /// Replace the path filter and reset the selection to the first match.
    ///
    /// `None` shows all files again. The filter is normalized the same way as
    /// in [`ReviewQueue::new`].
    pub fn set_path_filter(&mut self, path: Option<&str>) {
        self.path_filter = path.map(normalize_path);
        self.selected = 0;
        self.refilter();
    }

    /// The normalized path filter, if one is active.
    pub fn path_filter(&self) -> Option<&str> {
        self.path_filter.as_deref()
    }

    /// Number of [`Kind::Added`] changes matching the filter.
    pub fn added(&self) -> usize {
        self.count_kind(Kind::Added)
    }

    /// Number of [`Kind::Worsened`] changes matching the filter.
    pub fn worsened(&self) -> usize {
        self.count_kind(Kind::Worsened)
    }

    /// Sum of `ccn - base_ccn`, using `ccn` for an added function.
    ///
    /// Ties (a change that worsened only by nesting or NLOC) contribute zero.
    pub fn delta_sum(&self) -> i64 {
        self.changes
            .iter()
            .map(|change| match change.base_ccn {
                Some(base) => i64::from(change.ccn) - i64::from(base),
                None => i64::from(change.ccn),
            })
            .sum()
    }

    /// A stable, ASCII-only one-line summary of the filtered queue.
    ///
    /// For example `3 changes - +2 added - +14 CCN`. An empty queue is
    /// `no changes`.
    pub fn summary(&self) -> String {
        if self.changes.is_empty() {
            return "no changes".to_string();
        }
        format!(
            "{} changes - +{} added - {:+} CCN",
            self.changes.len(),
            self.added(),
            self.delta_sum()
        )
    }

    /// Rebuild `changes` from `source` and keep the selection valid.
    fn refilter(&mut self) {
        self.changes = match self.path_filter.as_deref() {
            // `normalize_path` has already replaced separators in both sides.
            Some(filter) => self
                .source
                .iter()
                .filter(|change| normalize_path(&change.path) == filter)
                .cloned()
                .collect(),
            None => self.source.clone(),
        };
        if self.changes.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.changes.len() - 1);
        }
    }

    fn count_kind(&self, kind: Kind) -> usize {
        self.changes
            .iter()
            .filter(|change| change.kind == kind)
            .count()
    }
}

/// Normalize path separators to `/` so Windows and Unix spellings of the same
/// path compare equal. Case and every other character are preserved.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::Kind;
    use crate::ccn::types::CcnTier;

    fn change(path: &str, name: &str, line: usize, ccn: u32, base: Option<u32>) -> Change {
        let kind = if base.is_some() {
            Kind::Worsened
        } else {
            Kind::Added
        };
        Change {
            path: path.to_string(),
            name: name.to_string(),
            identity: format!("{name}()"),
            line,
            ccn,
            base_ccn: base,
            nloc: 10,
            base_nloc: base.map(|_| 8),
            max_nesting: 2,
            base_max_nesting: base.map(|_| 1),
            kind,
            reasons: vec!["ccn +1".to_string()],
            tier: CcnTier::Exact,
        }
    }

    fn sample() -> Vec<Change> {
        vec![
            change("src/a.rs", "big", 10, 30, Some(20)),
            change("src/a.rs", "new", 40, 14, None),
            change("src/b.rs", "worse", 5, 18, Some(12)),
            change("src/b.rs", "other", 9, 11, Some(11)),
        ]
    }

    #[test]
    fn all_files_mode_keeps_every_change() {
        let queue = ReviewQueue::new(&sample(), None);

        assert_eq!(queue.len(), 4);
        assert_eq!(queue.changes().len(), 4);
        assert!(!queue.is_empty());
        assert_eq!(queue.path_filter(), None);
    }

    #[test]
    fn filters_by_exact_path() {
        let queue = ReviewQueue::new(&sample(), Some("src/b.rs"));

        assert_eq!(queue.len(), 2);
        let names: Vec<&str> = queue.changes().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["worse", "other"]);
        assert_eq!(queue.path_filter(), Some("src/b.rs"));
    }

    #[test]
    fn a_path_filter_with_no_matches_is_empty() {
        let queue = ReviewQueue::new(&sample(), Some("src/zzz.rs"));

        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.selected(), 0);
        assert!(queue.selected_change().is_none());
    }

    #[test]
    fn windows_and_unix_separators_are_equivalent() {
        let windows = vec![change("src\\a.rs", "f", 1, 12, None)];

        let by_unix = ReviewQueue::new(&windows, Some("src/a.rs"));
        assert_eq!(by_unix.len(), 1, "`\\` in the change path matches `/`");
        assert_eq!(by_unix.path_filter(), Some("src/a.rs"));

        let by_windows = ReviewQueue::new(&sample(), Some("src\\b.rs"));
        assert_eq!(by_windows.len(), 2, "`\\` in the filter matches `/`");
        assert_eq!(by_windows.path_filter(), Some("src/b.rs"));

        assert_eq!(normalize_path("a\\b\\c.rs"), "a/b/c.rs");
        assert_eq!(normalize_path("a/b/c.rs"), "a/b/c.rs");
    }

    #[test]
    fn path_matching_is_case_sensitive() {
        let queue = ReviewQueue::new(&sample(), Some("SRC/a.rs"));
        assert!(queue.is_empty());
    }

    #[test]
    fn added_worsened_and_delta_sum() {
        let queue = ReviewQueue::new(&sample(), None);

        assert_eq!(queue.added(), 1);
        assert_eq!(queue.worsened(), 3);
        // (30-20) + 14 (added) + (18-12) + (11-11)
        assert_eq!(queue.delta_sum(), 10 + 14 + 6);
    }

    #[test]
    fn counts_and_delta_follow_the_filter() {
        let mut queue = ReviewQueue::new(&sample(), Some("src/b.rs"));

        assert_eq!(queue.added(), 0);
        assert_eq!(queue.worsened(), 2);
        assert_eq!(queue.delta_sum(), 6);

        queue.set_path_filter(None);
        assert_eq!(queue.added(), 1);
        assert_eq!(queue.delta_sum(), 30);
    }

    #[test]
    fn next_prev_and_select_clamp_without_wrapping() {
        let mut queue = ReviewQueue::new(&sample(), None);

        assert_eq!(queue.selected(), 0);
        queue.prev();
        assert_eq!(queue.selected(), 0, "prev clamps at the first change");

        queue.next();
        queue.next();
        assert_eq!(queue.selected(), 2);
        queue.next();
        queue.next();
        queue.next();
        assert_eq!(queue.selected(), 3, "next clamps at the last change");

        queue.select(99);
        assert_eq!(queue.selected(), 3, "select clamps to the last change");

        queue.select(1);
        assert_eq!(queue.selected(), 1);
        assert_eq!(
            queue.selected_change().map(|c| c.name.as_str()),
            Some("new")
        );
    }

    #[test]
    fn set_path_filter_resets_and_clamps_selection() {
        let mut queue = ReviewQueue::new(&sample(), None);
        queue.select(3);

        queue.set_path_filter(Some("src/a.rs"));
        assert_eq!(queue.selected(), 0);
        assert_eq!(queue.len(), 2);

        queue.select(1);
        queue.set_path_filter(Some("src/zzz.rs"));
        assert_eq!(queue.selected(), 0, "an empty result resets the selection");
        assert!(queue.selected_change().is_none());

        queue.set_path_filter(None);
        assert_eq!(queue.selected(), 0);
        assert_eq!(queue.len(), 4);
    }

    #[test]
    fn empty_queue_is_safe() {
        let mut queue = ReviewQueue::new(&[], None);

        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.selected(), 0);
        assert!(queue.selected_change().is_none());
        assert_eq!(queue.added(), 0);
        assert_eq!(queue.worsened(), 0);
        assert_eq!(queue.delta_sum(), 0);
        assert_eq!(queue.summary(), "no changes");

        queue.next();
        queue.prev();
        queue.select(7);
        assert_eq!(queue.selected(), 0);
    }

    #[test]
    fn order_is_preserved() {
        let changes = vec![
            change("src/a.rs", "first", 1, 30, None),
            change("src/a.rs", "second", 2, 20, Some(10)),
            change("src/a.rs", "third", 3, 10, None),
        ];
        let queue = ReviewQueue::new(&changes, None);

        let ccn: Vec<u32> = queue.changes().iter().map(|c| c.ccn).collect();
        assert_eq!(ccn, vec![30, 20, 10], "incoming order, not re-sorted");
        assert_eq!(
            queue.selected_change().map(|c| c.name.as_str()),
            Some("first")
        );
    }

    #[test]
    fn summary_is_stable_and_ascii_only() {
        let queue = ReviewQueue::new(&sample(), None);
        assert_eq!(queue.summary(), "4 changes - +1 added - +30 CCN");
        assert!(queue.summary().is_ascii());

        let filtered = ReviewQueue::new(&sample(), Some("src/b.rs"));
        assert_eq!(filtered.summary(), "2 changes - +0 added - +6 CCN");
        assert!(filtered.summary().is_ascii());
    }
}
