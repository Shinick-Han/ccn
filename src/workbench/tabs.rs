//! Ordered multi-buffer tab model for the workbench.
//!
//! This is deliberately pure state: it knows the files the session has opened,
//! which one is active, and whether each buffer has unsaved changes. It does not
//! talk to Neovim; `WorkbenchSession` translates `open`/`next`/`close` into RPC
//! calls and feeds dirty state back in.

use std::path::{Path, PathBuf};

/// One open file. `display` is the compact name shown in the tab bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tab {
    pub(crate) path: PathBuf,
    pub(crate) display: String,
    pub(crate) modified: bool,
}

impl Tab {
    fn new(path: PathBuf, display: String) -> Self {
        Self {
            path,
            display,
            modified: false,
        }
    }
}

/// Ordered open files plus the active index.
///
/// The index is always valid when `tabs` is non-empty and `0` when it is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
}

#[allow(dead_code)] // The complete tab model is exercised by tests and wired by the split UI.
impl Tabs {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Open the first file, or select it when a tab with the same path exists.
    pub(crate) fn open_or_select(&mut self, path: &Path) -> usize {
        self.open_or_select_named(path, default_display(path))
    }

    /// Open a tab with an explicit display name (the session passes its
    /// workspace-relative path).
    pub(crate) fn open_or_select_named(&mut self, path: &Path, display: String) -> usize {
        if let Some(index) = self.index_of(path) {
            self.active = index;
            return index;
        }
        self.tabs.push(Tab::new(path.to_path_buf(), display));
        self.active = self.tabs.len() - 1;
        self.active
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Add a tab without making it active. Used when a session restores the
    /// buffer list before the user switches to those buffers.
    pub(crate) fn add_inactive(&mut self, path: &Path, display: String) {
        if self.index_of(path).is_none() {
            self.tabs.push(Tab::new(path.to_path_buf(), display));
        }
    }

    /// Replace the tab list with exactly `paths`, preserving their order and
    /// display names.
    ///
    /// Duplicate paths collapse to their first occurrence, using the loose
    /// comparison from [`Tabs::index_of_loose`]. Restored tabs always start
    /// clean: dirty markers are not persisted, so an unsaved edit from a
    /// previous process is never shown as pending after a restart.
    ///
    /// The active tab is the one matching `active` (loose path match), and
    /// `true` is returned. When no tab matches -- including for an empty list
    /// -- the first restored tab is selected instead and `false` is returned.
    pub(crate) fn restore_ordered(&mut self, paths: &[(PathBuf, String)], active: &Path) -> bool {
        self.tabs.clear();
        for (path, display) in paths {
            let key = path_key(path);
            if self.tabs.iter().any(|tab| path_key(&tab.path) == key) {
                continue;
            }
            self.tabs.push(Tab::new(path.clone(), display.clone()));
        }

        self.active = 0;
        let active_key = path_key(active);
        match self
            .tabs
            .iter()
            .position(|tab| path_key(&tab.path) == active_key)
        {
            Some(index) => {
                self.active = index;
                true
            }
            None => false,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// True when any open buffer has unsaved changes. The dashboard uses this
    /// to refuse an implicit quit.
    pub(crate) fn any_modified(&self) -> bool {
        self.tabs.iter().any(|tab| tab.modified)
    }

    pub(crate) fn active_index(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some(self.active)
        }
    }

    pub(crate) fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub(crate) fn active_path(&self) -> Option<&Path> {
        self.active().map(|tab| tab.path.as_path())
    }

    pub(crate) fn tab(&self, index: usize) -> Option<&Tab> {
        self.tabs.get(index)
    }

    pub(crate) fn index_of(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.path == path)
    }

    /// Compare paths the way Neovim reports them: Windows drops the `\\?\`
    /// verbatim prefix and is case-insensitive; separators are normalised.
    pub(crate) fn index_of_loose(&self, path: &Path) -> Option<usize> {
        let key = path_key(path);
        self.tabs.iter().position(|tab| path_key(&tab.path) == key)
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.index_of(path).is_some()
    }

    /// Loose-path variant of [`Tabs::remove_path`].
    pub(crate) fn remove_path_loose(&mut self, path: &Path) -> Option<Tab> {
        let index = self.index_of_loose(path)?;
        if index < self.active {
            self.active -= 1;
        }
        let removed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(removed)
    }

    /// Loose-path variant of [`Tabs::set_modified`].
    pub(crate) fn set_modified_loose(&mut self, path: &Path, modified: bool) {
        if let Some(index) = self.index_of_loose(path) {
            self.tabs[index].modified = modified;
        }
    }

    /// Select a tab. Returns `false` for an out-of-range index.
    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Index of the next tab, wrapping at the end.
    pub(crate) fn next_index(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some((self.active + 1) % self.tabs.len())
        }
    }

    /// Index of the previous tab, wrapping at the start.
    pub(crate) fn prev_index(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some((self.active + self.tabs.len() - 1) % self.tabs.len())
        }
    }

    /// Move to the next tab (wrapping). Returns the new index.
    pub(crate) fn next(&mut self) -> Option<usize> {
        let index = self.next_index()?;
        self.active = index;
        Some(index)
    }

    /// Move to the previous tab (wrapping). Returns the new index.
    pub(crate) fn prev(&mut self) -> Option<usize> {
        let index = self.prev_index()?;
        self.active = index;
        Some(index)
    }

    /// Remove the active tab and select a neighbour (the tab that took its
    /// place, or the previous one when the last tab was closed).
    pub(crate) fn close_active(&mut self) -> Option<Tab> {
        if self.tabs.is_empty() {
            return None;
        }
        let removed = self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(removed)
    }

    /// Remove a tab by path, selecting the nearest neighbour. Used when the
    /// async `:bdelete` reply confirms the buffer is gone.
    pub(crate) fn remove_path(&mut self, path: &Path) -> Option<Tab> {
        let index = self.index_of(path)?;
        if index < self.active {
            self.active -= 1;
        }
        let removed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(removed)
    }

    /// Mark a tab dirty/clean. Unknown paths are ignored.
    pub(crate) fn set_modified(&mut self, path: &Path, modified: bool) {
        if let Some(index) = self.index_of(path) {
            self.tabs[index].modified = modified;
        }
    }

    /// Display titles, with `●` appended for unsaved buffers.
    pub(crate) fn titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| {
                if tab.modified {
                    format!("{}●", tab.display)
                } else {
                    tab.display.clone()
                }
            })
            .collect()
    }

    /// All open paths, for the periodic dirty query.
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|tab| tab.path.clone()).collect()
    }
}

fn default_display(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Stable comparison key for paths reported by different layers.
pub(crate) fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    #[cfg(windows)]
    {
        text.replace('/', r"\").to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/work/{name}"))
    }

    #[test]
    fn opens_new_tabs_and_selects_existing_ones() {
        let mut tabs = Tabs::new();
        assert!(tabs.is_empty());
        assert_eq!(tabs.open_or_select(&path("engine.cpp")), 0);
        assert_eq!(tabs.open_or_select(&path("util.c")), 1);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_path(), Some(path("util.c").as_path()));

        // Re-opening an existing file only moves the selection.
        assert_eq!(tabs.open_or_select(&path("engine.cpp")), 0);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_path(), Some(path("engine.cpp").as_path()));
        assert_eq!(tabs.titles(), vec!["engine.cpp", "util.c"]);
    }

    #[test]
    fn next_and_prev_wrap() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));
        tabs.open_or_select(&path("b.c"));
        tabs.open_or_select(&path("c.c"));
        assert_eq!(tabs.active_index(), Some(2));
        assert_eq!(tabs.next(), Some(0));
        assert_eq!(tabs.active_path(), Some(path("a.c").as_path()));
        assert_eq!(tabs.prev(), Some(2));
        assert_eq!(tabs.next_index(), Some(0));
        assert_eq!(tabs.prev_index(), Some(1));
    }

    #[test]
    fn close_selects_the_neighbour_and_can_empty_the_set() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));
        tabs.open_or_select(&path("b.c"));
        tabs.open_or_select(&path("c.c"));
        tabs.select(1);

        let removed = tabs.close_active().expect("a tab");
        assert_eq!(removed.path, path("b.c"));
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_path(), Some(path("c.c").as_path()));

        // Closing the last tab selects the previous one.
        assert!(tabs.close_active().is_some());
        assert_eq!(tabs.active_path(), Some(path("a.c").as_path()));
        assert!(tabs.close_active().is_some());
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_index(), None);
        assert!(tabs.close_active().is_none());
    }

    #[test]
    fn remove_path_keeps_the_active_tab_stable() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));
        tabs.open_or_select(&path("b.c"));
        tabs.open_or_select(&path("c.c"));
        tabs.select(1);

        assert!(tabs.remove_path(&path("a.c")).is_some());
        assert_eq!(tabs.active_path(), Some(path("b.c").as_path()));
        assert!(tabs.remove_path(&path("missing.c")).is_none());
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn dirty_state_is_per_tab_and_shown_in_titles() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));
        tabs.open_or_select(&path("b.c"));
        tabs.set_modified(&path("a.c"), true);
        assert_eq!(tabs.titles(), vec!["a.c●", "b.c"]);
        assert!(tabs.tab(0).is_some_and(|tab| tab.modified));
        assert!(tabs.tab(1).is_some_and(|tab| !tab.modified));

        tabs.set_modified(&path("a.c"), false);
        tabs.set_modified(&path("b.c"), true);
        assert_eq!(tabs.titles(), vec!["a.c", "b.c●"]);
        // Unknown paths must not create or corrupt tabs.
        tabs.set_modified(&path("missing.c"), true);
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn explicit_display_names_are_preserved() {
        let mut tabs = Tabs::new();
        tabs.open_or_select_named(Path::new("/work/src/engine.cpp"), "src/engine.cpp".into());
        assert_eq!(tabs.titles(), vec!["src/engine.cpp"]);
        assert_eq!(tabs.paths(), vec![PathBuf::from("/work/src/engine.cpp")]);
    }

    #[test]
    fn restore_ordered_replaces_the_list_in_exact_order() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("stale.c"));
        tabs.open_or_select(&path("older.c"));

        let restored = vec![
            (path("z.c"), "z.c".into()),
            (path("a.c"), "a.c".into()),
            (path("m.c"), "m.c".into()),
        ];
        assert!(tabs.restore_ordered(&restored, &path("a.c")));

        assert_eq!(tabs.paths(), vec![path("z.c"), path("a.c"), path("m.c")]);
        assert_eq!(tabs.titles(), vec!["z.c", "a.c", "m.c"]);
        assert_eq!(tabs.active_path(), Some(path("a.c").as_path()));
        assert_eq!(tabs.active_index(), Some(1));
    }

    #[test]
    fn restore_ordered_selects_the_active_tab_with_a_loose_match() {
        let mut tabs = Tabs::new();
        #[cfg(windows)]
        let active = PathBuf::from(r"\WORK\SRC\B.C");
        #[cfg(not(windows))]
        let active = PathBuf::from("/work/src/b.c");
        let stored = PathBuf::from("/work/src/b.c");

        let restored = vec![(path("a.c"), "a.c".into()), (stored, "b.c".into())];
        assert!(tabs.restore_ordered(&restored, &active));
        assert_eq!(tabs.active_index(), Some(1));
        assert_eq!(tabs.titles(), vec!["a.c", "b.c"]);
    }

    #[test]
    fn restore_ordered_falls_back_to_the_first_tab_for_a_missing_active() {
        let mut tabs = Tabs::new();
        let restored = vec![(path("a.c"), "a.c".into()), (path("b.c"), "b.c".into())];
        assert!(!tabs.restore_ordered(&restored, &path("missing.c")));
        assert_eq!(tabs.active_path(), Some(path("a.c").as_path()));
    }

    #[test]
    fn restore_ordered_deduplicates_paths_and_starts_clean() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));
        tabs.set_modified(&path("a.c"), true);

        let restored = vec![
            (path("a.c"), "first/a.c".into()),
            (path("b.c"), "b.c".into()),
            (path("a.c"), "second/a.c".into()),
        ];
        assert!(tabs.restore_ordered(&restored, &path("b.c")));

        assert_eq!(tabs.paths(), vec![path("a.c"), path("b.c")]);
        assert_eq!(tabs.titles(), vec!["first/a.c", "b.c"]);
        assert!(!tabs.any_modified());
    }

    #[test]
    fn restore_ordered_with_an_empty_list_clears_the_tabs() {
        let mut tabs = Tabs::new();
        tabs.open_or_select(&path("a.c"));

        assert!(!tabs.restore_ordered(&[], &path("a.c")));
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_index(), None);
        assert_eq!(tabs.active_path(), None);
    }
}
