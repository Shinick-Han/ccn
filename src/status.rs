//! Status-bar rendering for the scanned workspace.
//!
//! [`crate::vcs::identity`] collects the backend's identity as data; this module
//! turns it into the single line the dashboard/workbench shows. Rendering stays
//! separate from probing so the formatting rules are testable without a live
//! repository, Perforce client or terminal.

use std::collections::HashMap;
use std::path::Path;

use crate::vcs::Identity;

/// One working-tree change reported by the backend.
///
/// The variants are intentionally backend-neutral: git porcelain codes and
/// Perforce opened actions are folded into the same small set so the tree view
/// can render one marker per file without knowing which VCS answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    Modified,
    Added,
    Deleted,
    Untracked,
    Renamed,
    Conflicted,
}

impl FileChange {
    /// Single-character marker shown before a file name in the tree.
    pub fn marker(self) -> char {
        match self {
            FileChange::Modified => 'M',
            FileChange::Added => 'A',
            FileChange::Deleted => 'D',
            FileChange::Untracked => '?',
            FileChange::Renamed => 'R',
            FileChange::Conflicted => '!',
        }
    }
}

/// Working-tree changes for `root`, keyed by scan-root-relative `/` paths.
///
/// Git is tried first (a repository checked out inside a Perforce workspace is
/// still primarily a git checkout, matching [`crate::vcs::detect`]). When git
/// cannot answer — missing binary, not a work tree, timeout — Perforce is
/// consulted. Anything the backends cannot determine yields an empty map rather
/// than an error, so the tree simply renders without markers.
pub fn changes(root: &Path) -> HashMap<String, FileChange> {
    if let Some(changes) = crate::git::status_changes(root) {
        return changes;
    }
    crate::p4::status_changes(root)
}

/// Normalise a backend path to a scan-root-relative `/` key.
///
/// Empty and `.` segments are dropped, a backslash separator becomes `/`, and
/// anything absolute (POSIX, Windows drive or UNC), containing `..` or a NUL is
/// rejected. A backend path outside the scan root is stripped before this is
/// called, so a `None` here means the entry cannot be represented at all.
pub(crate) fn relative_key(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.contains('\0') || path.starts_with('/') || path.starts_with('\\') {
        return None;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Format a workspace identity for the status bar.
///
/// ```text
/// git main
/// p4 ws-main (CL 12345)
/// p4 ws-main (default CL)
/// no VCS
/// ```
///
/// This delegates to [`Identity::label`] so every caller (status line, future
/// JSON field, log line) shares exactly one formatting rule.
pub fn identity_line(id: &Identity) -> String {
    id.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(backend: &'static str, branch: Option<&str>, changelist: Option<&str>) -> String {
        identity_line(&Identity {
            backend,
            branch: branch.map(str::to_string),
            changelist: changelist.map(str::to_string),
        })
    }

    #[test]
    fn renders_git_branch_identity() {
        assert_eq!(line("git", Some("main"), None), "git main");
        assert_eq!(
            line("git", Some("feature/status-line"), None),
            "git feature/status-line"
        );
    }

    #[test]
    fn renders_p4_client_and_changelist_identity() {
        assert_eq!(
            line("p4", Some("ws-main"), Some("CL 12345")),
            "p4 ws-main (CL 12345)"
        );
        assert_eq!(
            line("p4", Some("ws-main"), Some("default CL")),
            "p4 ws-main (default CL)"
        );
    }

    #[test]
    fn a_partial_identity_degrades_to_the_known_part() {
        assert_eq!(line("git", None, None), "git");
        assert_eq!(line("p4", Some("ws-main"), None), "p4 ws-main");
        assert_eq!(line("p4", None, Some("CL 12345")), "p4 (CL 12345)");
        assert_eq!(line("p4", None, None), "p4");
    }

    #[test]
    fn empty_components_are_not_rendered_as_separators() {
        assert_eq!(line("git", Some(""), None), "git");
        assert_eq!(line("p4", Some("  "), Some("")), "p4");
    }

    #[test]
    fn no_backend_renders_as_no_vcs() {
        assert_eq!(Identity::default().label(), "no VCS");
        assert_eq!(line("", None, None), "no VCS");
        assert_eq!(line("svn", Some("trunk"), None), "no VCS");
    }

    #[test]
    fn every_change_has_a_distinct_single_character_marker() {
        let changes = [
            (FileChange::Modified, 'M'),
            (FileChange::Added, 'A'),
            (FileChange::Deleted, 'D'),
            (FileChange::Untracked, '?'),
            (FileChange::Renamed, 'R'),
            (FileChange::Conflicted, '!'),
        ];
        let markers: std::collections::HashSet<char> =
            changes.iter().map(|(_, marker)| *marker).collect();
        assert_eq!(markers.len(), changes.len());
        for (change, marker) in changes {
            assert_eq!(change.marker(), marker);
        }
    }

    #[test]
    fn relative_keys_are_normalised_and_stay_inside_the_root() {
        assert_eq!(
            relative_key("src\\workbench\\mod.rs").as_deref(),
            Some("src/workbench/mod.rs")
        );
        assert_eq!(relative_key("./src/./a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(relative_key("src/a.rs"), Some("src/a.rs".to_string()));

        assert_eq!(relative_key(""), None);
        assert_eq!(relative_key("."), None);
        assert_eq!(relative_key("../escape.rs"), None);
        assert_eq!(relative_key("/etc/passwd"), None);
        assert_eq!(relative_key("\\\\server\\share\\a.rs"), None);
        assert_eq!(relative_key("C:\\abs\\a.rs"), None);
        assert_eq!(relative_key("a\0b.rs"), None);
    }
}
