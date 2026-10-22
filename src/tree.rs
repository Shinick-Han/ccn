//! Collapsible directory tree for the split view's file pane.
//!
//! The tree is a pure view over the scan-relative file paths in
//! [`crate::model::ScanResult::files`]. It owns only navigation state
//! (selection, collapsed directories, scroll offset); the caller decides what
//! an `Open(path)` request means.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::theme::AURA;

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
        };
        tree.rebuild(paths);
        tree
    }

    /// Rebuild from a fresh path list, preserving collapsed directories and
    /// the selected path when it still exists.
    pub fn rebuild<I, S>(&mut self, paths: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let previous = self.selected_path().map(str::to_string);
        let mut seen: HashSet<String> = HashSet::new();
        let mut root = Dir::root();

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
            if !seen.insert(full.clone()) {
                continue;
            }
            root.insert(&normalised, &full);
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

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut rows = Vec::new();
        collect_rows(&self.root, 0, &mut rows);
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
                Style::default().fg(AURA.accent),
            ),
            Span::styled(
                format!(" Files ({}) ", self.total_files),
                Style::default().fg(AURA.muted),
            ),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(AURA.card))
            .border_style(Style::default().fg(if focused {
                AURA.border_focus
            } else {
                AURA.border
            }))
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled("no files", Style::default().fg(AURA.muted)))
                    .style(Style::default().bg(AURA.card)),
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
                Style::default().fg(AURA.muted).add_modifier(Modifier::DIM),
            )));
        }

        let end = (self.offset + rows_height).min(self.rows.len());
        for position in self.offset..end {
            let row = &self.rows[position];
            let selected = position == self.selected;

            let marker = if row.is_dir {
                if row.expanded {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            let prefix = format!("{}{}", "  ".repeat(row.depth), marker);
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
            let text = truncate_right(&format!("{prefix}{name}{suffix}"), width);

            let base = if selected {
                Style::default().fg(AURA.text).bg(AURA.pill)
            } else if row.is_dir {
                Style::default().fg(AURA.muted).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(AURA.text)
            };

            let used = text.chars().count();
            let fill = width.saturating_sub(used);
            let mut spans = vec![Span::styled(text, base)];
            if selected && fill > 0 {
                // The pill must cover the whole row, not just the text.
                spans.push(Span::styled(" ".repeat(fill), base));
            }
            lines.push(Line::from(spans));
        }

        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(AURA.card)),
            inner,
        );
    }
}

fn collect_rows(dir: &Dir, depth: usize, out: &mut Vec<TreeRow>) {
    for child in &dir.dirs {
        out.push(TreeRow {
            path: child.path.clone(),
            name: child.name.clone(),
            depth,
            is_dir: true,
            expanded: child.expanded,
            file_count: child.count,
        });
        if child.expanded {
            collect_rows(child, depth + 1, out);
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
        });
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
        assert_eq!(buffer[(2, y)].bg, AURA.pill, "selected row needs the pill");
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
