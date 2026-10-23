//! Collapsible directory tree for the split view's file pane.
//!
//! The tree is a pure view over the scan-relative file paths in
//! [`crate::model::ScanResult::files`]. It owns only navigation state
//! (selection, collapsed directories, scroll offset); the caller decides what
//! an `Open(path)` request means.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::status::FileChange;
use crate::theme::palette;

/// What a key press asked the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeAction {
    /// The key was navigation or did nothing; the caller keeps focus.
    None,
    /// `e` / `Enter` on a file: the caller decides whether to open a tab or
    /// just focus the workbench.
    Open(PathBuf),
}

#[derive(Debug)]
struct FileNode {
    name: String,
    path: String,
}

#[derive(Debug)]
struct Dir {
    name: String,
    path: String,
    dirs: Vec<Dir>,
    files: Vec<FileNode>,
    expanded: bool,
    /// Number of files below this directory, recursively.
    count: usize,
}

impl Dir {
    fn root() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            dirs: Vec::new(),
            files: Vec::new(),
            expanded: true,
            count: 0,
        }
    }

    fn new(name: String, path: String) -> Self {
        Self {
            name,
            path,
            dirs: Vec::new(),
            files: Vec::new(),
            expanded: true,
            count: 0,
        }
    }

    /// Insert one already-normalised path (`parts` is never empty).
    fn insert(&mut self, parts: &[&str], full: &str) {
        if parts.len() == 1 {
            self.files.push(FileNode {
                name: parts[0].to_string(),
                path: full.to_string(),
            });
            return;
        }

        let head = parts[0];
        let child_path = if self.path.is_empty() {
            head.to_string()
        } else {
            format!("{}/{}", self.path, head)
        };
        let index = match self.dirs.iter().position(|dir| dir.name == head) {
            Some(index) => index,
            None => {
                self.dirs
                    .push(Dir::new(head.to_string(), child_path.clone()));
                self.dirs.len() - 1
            }
        };
        self.dirs[index].insert(&parts[1..], full);
    }

    fn sort(&mut self) {
        self.dirs.sort_by(|a, b| a.name.cmp(&b.name));
        self.files.sort_by(|a, b| a.name.cmp(&b.name));
        for dir in &mut self.dirs {
            dir.sort();
        }
    }

    fn recount(&mut self) -> usize {
        let mut count = self.files.len();
        for dir in &mut self.dirs {
            count += dir.recount();
        }
        self.count = count;
        count
    }

    /// Apply the collapsed set. Paths are stable across rescans, so a
    /// collapsed directory stays collapsed when the tree is rebuilt.
    fn apply_collapsed(&mut self, collapsed: &BTreeSet<String>) {
        self.expanded = !collapsed.contains(&self.path);
        for dir in &mut self.dirs {
            dir.apply_collapsed(collapsed);
        }
    }
}

/// One visible row of the flattened tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Full scan-relative path, `/`-separated.
    pub path: String,
    /// Last path component (the label shown in the row).
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// Recursive file count for directories; 0 for files.
    pub file_count: usize,
    /// Working-tree status of a file row, when the backend reported one.
    pub change: Option<FileChange>,
}

/// Directory tree state that survives a rescan.
#[derive(Debug)]
pub struct Tree {
    root: Dir,
    rows: Vec<TreeRow>,
    selected: usize,
    offset: usize,
    /// Collapsed directories, keyed by their scan-relative path.
    collapsed: BTreeSet<String>,
    total_files: usize,
    /// Every scan-relative file, before the changed-only view is applied, so
    /// turning the filter off restores the complete tree.
    all_paths: Vec<String>,
    /// Working-tree status per scan-relative file.
    changes: HashMap<String, FileChange>,
    /// When true, only files present in `changes` are shown.
    changed_only: bool,
}

impl Tree {
    /// Build a tree from scan-relative paths. Backslashes are normalised so a
    /// Windows scan and a Unix scan produce the same hierarchy.
    pub fn new<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut tree = Self {
            root: Dir::root(),
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            collapsed: BTreeSet::new(),
            total_files: 0,
            all_paths: Vec::new(),
            changes: HashMap::new(),
            changed_only: false,
        };
        tree.rebuild(paths);
        tree
    }

    /// Rebuild from a fresh path list, preserving collapsed directories and
    /// the selected path when it still exists.
    ///
    /// The full list is kept internally so [`Tree::set_changed_filter`] can be
    /// turned back off without another scan.
    pub fn rebuild<I, S>(&mut self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut seen: HashSet<String> = HashSet::new();
        let mut all = Vec::new();

        for path in paths {
            let normalised: Vec<&str> = path
                .as_ref()
                .split(['/', '\\'])
                .filter(|part| !part.is_empty() && *part != ".")
                .collect();
            if normalised.is_empty() {
                continue;
            }
            let full = normalised.join("/");
            if seen.insert(full.clone()) {
                all.push(full);
            }
        }

        self.all_paths = all;
        self.rebuild_view();
    }

    /// Rebuild the visible hierarchy from `all_paths`, applying the changed-only
    /// filter, the collapsed set and the previous selection.
    fn rebuild_view(&mut self) {
        let previous = self.selected_path().map(str::to_string);
        let visible: Vec<String> = self
            .all_paths
            .iter()
            .filter(|path| !self.changed_only || self.changes.contains_key(*path))
            .cloned()
            .collect();

        let mut root = Dir::root();
        for path in &visible {
            let parts: Vec<&str> = path.split('/').collect();
            root.insert(&parts, path);
        }

        root.sort();
        root.recount();
        root.apply_collapsed(&self.collapsed);
        self.total_files = root.count;
        self.root = root;
        self.rebuild_rows();

        if let Some(path) = previous {
            self.select_path(&path);
        }
        self.clamp_selection();
    }

    /// Attach working-tree status to the tree.
    ///
    /// Markers update immediately; when the changed-only filter is on, the
    /// visible hierarchy is rebuilt from the new map (a file that is no longer
    /// changed disappears). The full path set is untouched.
    pub fn set_changes(&mut self, changes: &HashMap<String, FileChange>) {
        self.changes = changes.clone();
        self.rebuild_view();
    }

    /// Show only files with a change status (`enabled`) or every file.
    ///
    /// Directory counts and [`Tree::total_files`] reflect the filtered set;
    /// disabling the filter restores the full tree from the retained path list.
    pub fn set_changed_filter(&mut self, enabled: bool) {
        if self.changed_only == enabled {
            return;
        }
        self.changed_only = enabled;
        self.rebuild_view();
    }

    /// [`Tree::set_changed_filter`] under the name the callers asked for.
    pub fn changed_only(&mut self, enabled: bool) {
        self.set_changed_filter(enabled);
    }

    /// True when only changed files are visible.
    pub fn is_changed_only(&self) -> bool {
        self.changed_only
    }

    /// Working-tree status for `path`, if one was reported.
    pub fn change_of(&self, path: &str) -> Option<FileChange> {
        self.changes.get(path).copied()
    }

    /// Directories that are currently expanded, sorted.
    ///
    /// Derived from the full path set and the collapsed set, so directories
    /// hidden by the changed-only filter still round-trip through a session.
    pub fn expanded_paths(&self) -> Vec<String> {
        self.all_directories()
            .into_iter()
            .filter(|dir| !self.collapsed.contains(dir))
            .collect()
    }

    /// Restore expansion from [`Tree::expanded_paths`].
    ///
    /// Every listed directory is expanded together with all of its ancestors,
    /// and every known directory that is not listed is collapsed, so a saved
    /// list round-trips exactly. An empty list is a no-op: an old session that
    /// predates this field must keep the default "everything expanded" view
    /// rather than collapsing the whole tree.
    pub fn restore_expanded(&mut self, paths: &[String]) {
        let mut expanded = BTreeSet::new();
        for path in paths {
            let parts: Vec<&str> = path
                .split(['/', '\\'])
                .filter(|part| !part.is_empty() && *part != ".")
                .collect();
            if parts.is_empty() || parts.contains(&"..") {
                continue;
            }
            let mut prefix = String::new();
            for part in parts {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                expanded.insert(prefix.clone());
            }
        }
        if expanded.is_empty() {
            return;
        }

        let directories = self.all_directories();
        self.collapsed = directories.difference(&expanded).cloned().collect();
        self.root.apply_collapsed(&self.collapsed);
        self.rebuild_rows();
        self.clamp_selection();
    }

    /// Every directory path implied by the full file list, sorted.
    fn all_directories(&self) -> BTreeSet<String> {
        let mut directories = BTreeSet::new();
        for path in &self.all_paths {
            let mut parts = path.split('/');
            let mut prefix = String::new();
            // The last component is the file itself.
            let count = path.split('/').count().saturating_sub(1);
            for _ in 0..count {
                let Some(part) = parts.next() else {
                    break;
                };
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                directories.insert(prefix.clone());
            }
        }
        directories
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut rows = Vec::new();
        collect_rows(&self.root, 0, &self.changes, &mut rows);
        self.rows = rows;
    }

    /// Visible, flattened rows in display order.
    pub fn visible_rows(&self) -> &[TreeRow] {
        &self.rows
    }

    /// Number of visible rows.
    pub fn visible_len(&self) -> usize {
        self.rows.len()
    }

    /// Total number of files in the tree (independent of collapsed state).
    pub fn total_files(&self) -> usize {
        self.total_files
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Path of the selected row, for either a file or a directory.
    pub fn selected_path(&self) -> Option<&str> {
        self.rows.get(self.selected).map(|row| row.path.as_str())
    }

    /// Select the row with this exact path, if it is currently visible.
    pub fn select_path(&mut self, path: &str) -> bool {
        match self.rows.iter().position(|row| row.path == path) {
            Some(index) => {
                self.selected = index;
                true
            }
            None => false,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.rows.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.rows.len().saturating_sub(1);
    }

    /// Toggle the selected row when it is a directory. Returns `true` when a
    /// directory was toggled.
    pub fn toggle(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return false;
        };
        if !row.is_dir {
            return false;
        }
        if row.expanded {
            self.collapsed.insert(row.path.clone());
        } else {
            self.collapsed.remove(&row.path);
        }
        self.root.apply_collapsed(&self.collapsed);
        self.rebuild_rows();
        // The toggled directory itself always stays visible.
        self.select_path(&row.path);
        self.clamp_selection();
        true
    }

    /// Directory paths that are currently collapsed.
    pub fn collapsed(&self) -> &BTreeSet<String> {
        &self.collapsed
    }

    /// Apply one key press and report what the caller should do.
    pub fn handle_key(&mut self, key: KeyEvent) -> TreeAction {
        if key.kind == KeyEventKind::Release {
            return TreeAction::None;
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.toggle() {
                    // A directory was toggled; nothing else to do.
                } else if let Some(row) = self.rows.get(self.selected) {
                    if !row.is_dir {
                        return TreeAction::Open(PathBuf::from(&row.path));
                    }
                }
            }
            KeyCode::Char('e') => {
                if let Some(row) = self.rows.get(self.selected) {
                    if !row.is_dir {
                        return TreeAction::Open(PathBuf::from(&row.path));
                    }
                }
            }
            _ => {}
        }
        TreeAction::None
    }

    fn clamp_selection(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            self.offset = 0;
        } else if self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }
    }

    /// Hit-test a mouse click inside the rendered tree. Single-clicking a
    /// directory toggles it; clicking a file returns `Open`. `area` is the
    /// same rectangle passed to `render`, and the current offset is used.
    pub fn click(&mut self, column: u16, row: u16, area: Rect) -> TreeAction {
        let inner_x = area.x.saturating_add(1);
        let inner_y = area.y.saturating_add(1);
        let inner_w = area.width.saturating_sub(2);
        let inner_h = area.height.saturating_sub(2);
        if column < inner_x
            || column >= inner_x.saturating_add(inner_w)
            || row < inner_y
            || row >= inner_y.saturating_add(inner_h)
        {
            return TreeAction::None;
        }
        let header = u16::from(inner_h >= 2);
        let row_y = row.saturating_sub(inner_y);
        if row_y < header {
            return TreeAction::None;
        }
        let index = self.offset + usize::from(row_y - header);
        let Some(tree_row) = self.rows.get(index) else {
            return TreeAction::None;
        };
        self.selected = index;
        if tree_row.is_dir {
            self.toggle();
            TreeAction::None
        } else {
            TreeAction::Open(PathBuf::from(&tree_row.path))
        }
    }

    /// Draw the tree into `area`. `focused` swaps the border and adds the same
    /// accent marker the dashboard's file table uses.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let title = Line::from(vec![
            Span::styled(
                if focused { "▌" } else { " " },
                Style::default().fg(palette().accent),
            ),
            Span::styled(
                format!(" Files ({}) ", self.total_files),
                Style::default().fg(palette().muted),
            ),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(palette().card))
            .border_style(Style::default().fg(if focused {
                palette().border_focus
            } else {
                palette().border
            }))
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "no files",
                    Style::default().fg(palette().muted),
                ))
                .style(Style::default().bg(palette().card)),
                inner,
            );
            return;
        }

        let width = inner.width as usize;
        let header_height = usize::from(inner.height >= 2);
        let rows_height = (inner.height as usize).saturating_sub(header_height);
        self.clamp_selection();

        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + rows_height {
            self.offset = self.selected + 1 - rows_height;
        }
        let max_offset = self.rows.len().saturating_sub(rows_height);
        if self.offset > max_offset {
            self.offset = max_offset;
        }

        let mut lines: Vec<Line> = Vec::with_capacity(rows_height + header_height);

        if header_height == 1 {
            let path = self.selected_path().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                truncate_right(path, width),
                Style::default()
                    .fg(palette().muted)
                    .add_modifier(Modifier::DIM),
            )));
        }

        let end = (self.offset + rows_height).min(self.rows.len());
        for position in self.offset..end {
            let row = &self.rows[position];
            let selected = position == self.selected;

            let indent = "  ".repeat(row.depth);
            let (marker, marker_color) = if row.is_dir {
                (
                    if row.expanded {
                        "▾ ".to_string()
                    } else {
                        "▸ ".to_string()
                    },
                    None,
                )
            } else {
                match row.change {
                    Some(change) => (format!("{} ", change.marker()), Some(change_color(change))),
                    None => ("  ".to_string(), None),
                }
            };
            let prefix = format!("{indent}{marker}");
            let suffix = if row.is_dir {
                format!(" ({})", row.file_count)
            } else {
                String::new()
            };

            let prefix_width = prefix.chars().count();
            let suffix_width = suffix.chars().count();
            let name_width = width
                .saturating_sub(prefix_width)
                .saturating_sub(suffix_width);
            let name = truncate_right(&row.name, name_width);

            let base = if selected {
                Style::default().fg(palette().text).bg(palette().pill)
            } else if row.is_dir {
                Style::default()
                    .fg(palette().muted)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette().text)
            };
            let marker_style = match marker_color {
                Some(color) if selected => Style::default().fg(color).bg(palette().pill),
                Some(color) => Style::default().fg(color),
                None => base,
            };

            let used = prefix_width + name.chars().count() + suffix_width;
            let fill = width.saturating_sub(used);
            let mut spans = vec![
                Span::styled(prefix, marker_style),
                Span::styled(format!("{name}{suffix}"), base),
            ];
            if selected && fill > 0 {
                // The pill must cover the whole row, not just the text.
                spans.push(Span::styled(" ".repeat(fill), base));
            }
            lines.push(Line::from(spans));
        }

        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(palette().card)),
            inner,
        );
    }
}

fn collect_rows(
    dir: &Dir,
    depth: usize,
    changes: &HashMap<String, FileChange>,
    out: &mut Vec<TreeRow>,
) {
    for child in &dir.dirs {
        out.push(TreeRow {
            path: child.path.clone(),
            name: child.name.clone(),
            depth,
            is_dir: true,
            expanded: child.expanded,
            file_count: child.count,
            change: None,
        });
        if child.expanded {
            collect_rows(child, depth + 1, changes, out);
        }
    }
    for file in &dir.files {
        out.push(TreeRow {
            path: file.path.clone(),
            name: file.name.clone(),
            depth,
            is_dir: false,
            expanded: false,
            file_count: 0,
            change: changes.get(&file.path).copied(),
        });
    }
}

/// Colour for a status marker: additions green, removals red, everything else
/// (a modification, rename or untracked file) amber.
fn change_color(change: FileChange) -> Color {
    match change {
        FileChange::Added => palette().ok,
        FileChange::Deleted | FileChange::Conflicted => palette().danger,
        FileChange::Modified | FileChange::Untracked | FileChange::Renamed => palette().warn,
    }
}

/// Keep the head of a string, adding an ellipsis when it has to be cut.
fn truncate_right(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn tree() -> Tree {
        Tree::new(["src/a.cpp", "src/sub/b.c", "top.c"])
    }

    fn press(tree: &mut Tree, code: KeyCode) -> TreeAction {
        tree.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn paths(tree: &Tree) -> Vec<&str> {
        tree.visible_rows()
            .iter()
            .map(|row| row.path.as_str())
            .collect()
    }

    #[test]
    fn builds_hierarchy_with_directories_first() {
        let tree = tree();
        assert_eq!(tree.total_files(), 3);
        assert_eq!(
            paths(&tree),
            vec!["src", "src/sub", "src/sub/b.c", "src/a.cpp", "top.c"]
        );

        let rows = tree.visible_rows();
        assert!(rows[0].is_dir && rows[0].expanded);
        assert_eq!(rows[0].file_count, 2);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].name, "sub");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].name, "b.c");
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[3].name, "a.cpp");
        assert_eq!(rows[3].depth, 1);
        assert!(!rows[4].is_dir);
    }

    #[test]
    fn collapse_hides_children_and_expand_restores_them() {
        let mut tree = tree();
        assert!(tree.select_path("src"));

        assert!(tree.toggle());
        assert_eq!(paths(&tree), vec!["src", "top.c"]);
        assert!(!tree.visible_rows()[0].expanded);
        assert_eq!(tree.selected_path(), Some("src"));

        press(&mut tree, KeyCode::Char(' '));
        assert_eq!(
            paths(&tree),
            vec!["src", "src/sub", "src/sub/b.c", "src/a.cpp", "top.c"]
        );
    }

    #[test]
    fn selection_skips_hidden_rows_and_clamps() {
        let mut tree = tree();
        assert!(tree.select_path("src/sub/b.c"));
        assert_eq!(tree.selected_index(), 2);

        // Collapse the parent while a child is selected; selection lands on the
        // visible parent instead of pointing at a hidden row.
        assert!(tree.select_path("src/sub"));
        tree.toggle();
        assert_eq!(paths(&tree), vec!["src", "src/sub", "src/a.cpp", "top.c"]);
        assert_eq!(tree.selected_path(), Some("src/sub"));

        tree.toggle();
        tree.select_last();
        assert_eq!(tree.selected_path(), Some("top.c"));
        tree.move_selection(1);
        assert_eq!(tree.selected_path(), Some("top.c"));
        tree.move_selection(-100);
        assert_eq!(tree.selected_path(), Some("src"));
    }

    #[test]
    fn path_round_trip_and_rebuild_preserve_state() {
        let mut tree = tree();
        assert!(tree.select_path("src/a.cpp"));
        let selected = tree.selected_path().unwrap().to_string();

        let collapsed: BTreeSet<String> = ["src/sub".to_string()].into_iter().collect();
        tree.collapsed = collapsed;
        tree.rebuild(["src/a.cpp", "src/sub/b.c", "top.c", "new/file.rs"]);

        assert_eq!(tree.total_files(), 4);
        assert_eq!(tree.selected_path(), Some(selected.as_str()));
        assert!(tree.collapsed().contains("src/sub"));
        assert!(!paths(&tree).contains(&"src/sub/b.c"));
        assert!(paths(&tree).contains(&"new/file.rs"));
    }

    #[test]
    fn mouse_click_selects_a_file_and_opens_it() {
        let mut tree = tree();
        tree.offset = 0;
        let area = Rect::new(0, 0, 30, 10);
        assert_eq!(
            tree.click(5, 4, area),
            TreeAction::Open(PathBuf::from("src/sub/b.c"))
        );
        assert_eq!(tree.selected_path(), Some("src/sub/b.c"));
    }

    #[test]
    fn keyboard_navigation_and_file_open() {
        let mut tree = tree();
        assert_eq!(press(&mut tree, KeyCode::Char('j')), TreeAction::None);
        assert_eq!(tree.selected_path(), Some("src/sub"));
        press(&mut tree, KeyCode::Char('j'));
        assert_eq!(tree.selected_path(), Some("src/sub/b.c"));

        assert_eq!(
            press(&mut tree, KeyCode::Char('e')),
            TreeAction::Open(PathBuf::from("src/sub/b.c"))
        );
        assert_eq!(
            press(&mut tree, KeyCode::Enter),
            TreeAction::Open(PathBuf::from("src/sub/b.c"))
        );
        // Space on a file is also Enter, not a toggle.
        assert_eq!(
            press(&mut tree, KeyCode::Char(' ')),
            TreeAction::Open(PathBuf::from("src/sub/b.c"))
        );

        press(&mut tree, KeyCode::Char('G'));
        assert_eq!(tree.selected_path(), Some("top.c"));
        press(&mut tree, KeyCode::Char('g'));
        assert_eq!(tree.selected_path(), Some("src"));
        press(&mut tree, KeyCode::End);
        assert_eq!(tree.selected_path(), Some("top.c"));
        press(&mut tree, KeyCode::Home);
        assert_eq!(tree.selected_path(), Some("src"));

        // `e` on a directory does not open anything.
        assert_eq!(press(&mut tree, KeyCode::Char('e')), TreeAction::None);
        // Enter on the directory collapses it instead.
        assert_eq!(press(&mut tree, KeyCode::Enter), TreeAction::None);
        assert!(!tree.visible_rows()[0].expanded);
    }

    #[test]
    fn empty_tree_is_safe() {
        let mut tree = Tree::new(Vec::<String>::new());
        assert!(tree.visible_rows().is_empty());
        assert_eq!(tree.selected_path(), None);
        assert!(!tree.toggle());
        assert_eq!(press(&mut tree, KeyCode::Char('j')), TreeAction::None);
        assert_eq!(tree.selected_index(), 0);

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| tree.render(frame, frame.area(), true))
            .expect("draw empty tree");
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Files (0)"), "{text}");
        assert!(text.contains("no files"), "{text}");
    }

    #[test]
    fn renders_in_a_narrow_pane_without_overflow() {
        let mut tree = tree();
        tree.select_path("src/sub/b.c");

        let backend = TestBackend::new(14, 8);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| tree.render(frame, frame.area(), true))
            .expect("draw narrow tree");
        let buffer = terminal.backend().buffer();
        // `buffer_text` writes exactly one symbol per cell; a panic or a
        // wrapped line would show up as a different row count.
        let text = buffer_text(buffer);
        assert_eq!(text.lines().count(), 8);
        assert!(text.contains("Files"), "{text}");
        assert!(text.contains("▾"), "{text}");
    }

    #[test]
    fn selected_row_uses_the_aura_pill() {
        let mut tree = tree();
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| tree.render(frame, frame.area(), true))
            .expect("draw tree");
        let buffer = terminal.backend().buffer();

        let mut selected_y = None;
        for y in 0..buffer.area.height {
            let mut text = String::new();
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            if text.contains("▾ src") {
                selected_y = Some(y);
                break;
            }
        }
        let y = selected_y.expect("selected directory row");
        assert_eq!(
            buffer[(2, y)].bg,
            palette().pill,
            "selected row needs the pill"
        );
    }

    #[test]
    fn status_markers_render_before_file_names() {
        let mut tree = tree();
        let changes = HashMap::from([
            ("src/a.cpp".to_string(), FileChange::Modified),
            ("top.c".to_string(), FileChange::Added),
        ]);
        tree.set_changes(&changes);

        assert_eq!(
            tree.change_of("src/a.cpp"),
            Some(FileChange::Modified),
            "the map is attached to the tree"
        );
        let file_row = tree
            .visible_rows()
            .iter()
            .find(|row| row.path == "src/a.cpp")
            .expect("file row");
        assert_eq!(file_row.change, Some(FileChange::Modified));
        assert_eq!(
            tree.visible_rows()
                .iter()
                .find(|row| row.path == "top.c")
                .and_then(|row| row.change),
            Some(FileChange::Added)
        );
        assert_eq!(
            tree.visible_rows()
                .iter()
                .find(|row| row.path == "src/sub/b.c")
                .and_then(|row| row.change),
            None,
            "an unchanged file has no marker"
        );

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| tree.render(frame, frame.area(), true))
            .expect("draw tree");
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("M a.cpp"), "{text}");
        assert!(text.contains("A top.c"), "{text}");
    }

    #[test]
    fn changed_only_filter_hides_unchanged_files_and_restores_them() {
        let mut tree = tree();
        let changes = HashMap::from([
            ("src/sub/b.c".to_string(), FileChange::Modified),
            ("top.c".to_string(), FileChange::Untracked),
        ]);
        tree.set_changes(&changes);

        tree.set_changed_filter(true);
        assert!(tree.is_changed_only());
        assert_eq!(tree.total_files(), 2);
        assert_eq!(paths(&tree), vec!["src", "src/sub", "src/sub/b.c", "top.c"]);
        assert_eq!(
            tree.visible_rows()[0].file_count,
            1,
            "directory counts reflect the filtered files"
        );

        tree.set_changed_filter(false);
        assert!(!tree.is_changed_only());
        assert_eq!(tree.total_files(), 3);
        assert_eq!(
            paths(&tree),
            vec!["src", "src/sub", "src/sub/b.c", "src/a.cpp", "top.c"],
            "the full path set was retained"
        );
        assert_eq!(tree.visible_rows()[0].file_count, 2);
    }

    #[test]
    fn expanded_paths_round_trip_through_restore() {
        let mut original = tree();
        assert!(original.select_path("src/sub"));
        assert!(original.toggle(), "collapse src/sub");
        let expanded = original.expanded_paths();
        assert!(expanded.contains(&"src".to_string()), "{expanded:?}");
        assert!(!expanded.contains(&"src/sub".to_string()), "{expanded:?}");

        let mut restored = tree();
        restored.restore_expanded(&expanded);
        assert_eq!(
            restored.collapsed(),
            original.collapsed(),
            "the collapsed set is reproduced from the expanded list"
        );
        assert_eq!(paths(&restored), paths(&original));

        // Listing a nested directory expands its ancestors too.
        let mut nested = tree();
        nested.collapsed = ["src".to_string(), "src/sub".to_string()]
            .into_iter()
            .collect();
        nested.restore_expanded(&["src/sub".to_string()]);
        assert!(nested.collapsed().is_empty());
        assert_eq!(
            paths(&nested),
            vec!["src", "src/sub", "src/sub/b.c", "src/a.cpp", "top.c"]
        );
    }

    #[test]
    fn an_empty_expanded_list_keeps_the_current_state() {
        let mut tree = tree();
        tree.restore_expanded(&[]);
        assert!(tree.collapsed().is_empty());
        assert!(!paths(&tree).is_empty());
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }
}
