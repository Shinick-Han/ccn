//! Version-control abstraction: one API over git and Perforce.
//!
//! Metrics and report code should not care which VCS produced a history. This
//! module hides that choice behind [`Backend`], [`collect_history`] and
//! [`changed_since`], and — importantly — normalises every path key to the
//! **scan root**, so a scan of `repo/sub` joins history with
//! [`crate::loc::FileLoc::path`] without the caller rebasing anything.
//!
//! Path normalisation:
//!
//! - git (`crate::git`) reports keys relative to the *work tree*. The scan root
//!   may be a subdirectory of that tree, so keys are filtered to those under
//!   [`crate::git::prefix_relative_to`] and that prefix is stripped:
//!   `sub/a.rs` becomes `a.rs` for scan root `repo/sub`. Files outside the scan
//!   root are dropped, never rewritten into `../outside.rs` or an absolute path.
//! - Perforce (`crate::p4`) already reports scan-root-relative keys, so they
//!   pass through unchanged.
//!
//! Every entry point returns `None` rather than an error when the backend
//! cannot answer: history is an optional enrichment, and the scan degrades to
//! the CCN/LOC-only view.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::git::{self, Churn};

/// Which version-control system a scan is reading history from.
/// Default window for history collection: the last 300 commits/changelists.
///
/// Git can afford a year; Perforce spends one server round-trip per changelist,
/// so the cap is what keeps `--hotspots` from becoming a coffee break.
pub const DEFAULT_HISTORY_LIMIT: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Git,
    P4,
}

impl Backend {
    /// Short label for the report (`git`, `p4`).
    pub fn name(self) -> &'static str {
        match self {
            Backend::Git => "git",
            Backend::P4 => "p4",
        }
    }
}

/// Backend-neutral history, keyed by **scan-root-relative**, `/`-separated paths.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// Commits (git) or submitted changelists (Perforce) seen in the window.
    pub commits: u32,
    /// Churn per scan-root-relative path.
    pub churn: HashMap<String, Churn>,
    /// Commits per path per author.
    pub authors: HashMap<String, HashMap<String, u32>>,
}

/// Detect the backend for `scan_root`.
///
/// A `.git` entry in the scan root or any ancestor wins, because a repository
/// checked out inside a Perforce workspace is still primarily a git checkout.
/// Otherwise Perforce is used when `p4` recognises the directory. `None` means
/// neither backend can supply history.
pub fn detect(scan_root: &Path) -> Option<Backend> {
    if has_git_entry(scan_root) {
        return Some(Backend::Git);
    }
    if crate::p4::detect(scan_root) {
        return Some(Backend::P4);
    }
    None
}

/// History for `scan_root`, with every path key rebased onto the scan root.
///
/// `max_changes` bounds the Perforce changelist window and is ignored by git,
/// whose window is fixed by [`crate::git::collect_history`].
pub fn collect_history(scan_root: &Path, backend: Backend, max_changes: u32) -> Option<History> {
    match backend {
        Backend::Git => {
            let history = git::collect_history(scan_root)?;
            let prefix = git_prefix(scan_root, &history.work_tree)?;
            Some(History {
                commits: history.commits,
                churn: rebase(history.churn, &prefix),
                authors: rebase(history.authors, &prefix),
            })
        }
        Backend::P4 => {
            let history = crate::p4::collect_history(scan_root, max_changes)?;
            Some(History {
                commits: history.changelists,
                churn: history.churn,
                authors: history.authors,
            })
        }
    }
}

/// True when `rev` exists in the backend (a commit or a changelist).
pub fn rev_exists(scan_root: &Path, backend: Backend, rev: &str) -> bool {
    match backend {
        Backend::Git => git::rev_exists(scan_root, rev),
        Backend::P4 => crate::p4::rev_exists(scan_root, rev),
    }
}

/// File contents at `rev`, keyed by a scan-root-relative path.
///
/// `None` means "no baseline version" (a file added after `rev`, or a rev the
/// backend cannot read), which the baseline comparison treats as an empty file.
pub fn file_at(scan_root: &Path, backend: Backend, rev: &str, path: &str) -> Option<Vec<u8>> {
    match backend {
        Backend::Git => {
            let work_tree = git::work_tree(scan_root)?;
            let prefix = git_prefix(scan_root, &work_tree)?;
            git::file_at(scan_root, rev, &format!("{prefix}{path}"))
        }
        // P4 keys are already scan-root-relative.
        Backend::P4 => crate::p4::file_at(scan_root, rev, path),
    }
}

/// Paths changed since `rev`, relative to the scan root.
///
/// Files outside the scan root are dropped: a scan of `repo/sub` must never see
/// `../outside.rs`, because nothing else in the report uses such a key.
pub fn changed_since(scan_root: &Path, backend: Backend, rev: &str) -> Option<HashSet<String>> {
    match backend {
        Backend::Git => {
            let work_tree = git::work_tree(scan_root)?;
            let prefix = git_prefix(scan_root, &work_tree)?;
            let changed = git::changed_since(scan_root, rev)?;
            Some(rebase_set(changed, &prefix))
        }
        Backend::P4 => crate::p4::changed_since(scan_root, rev),
    }
}

/// True when `dir` or one of its ancestors contains a `.git` entry.
///
/// Both directories and files count: a linked worktree (and a submodule) uses a
/// `.git` *file* pointing at the real git directory, and `git` commands work
/// there exactly as they do in a normal checkout.
fn has_git_entry(dir: &Path) -> bool {
    dir.ancestors().any(|candidate| {
        let dot_git = candidate.join(".git");
        dot_git.is_dir() || dot_git.is_file()
    })
}

/// `scan_root`'s prefix inside `work_tree`, or `None` when it is not inside.
///
/// [`git::prefix_relative_to`] returns `""` both for "root is the work tree" and
/// for "root is not under the work tree", so the containment check is made here
/// on canonical paths — a textual comparison would fail because git prints
/// forward slashes while Windows canonicalisation adds a verbatim prefix.
fn git_prefix(scan_root: &Path, work_tree: &Path) -> Option<String> {
    let root = std::fs::canonicalize(scan_root).ok()?;
    let top = std::fs::canonicalize(work_tree).ok()?;
    if !root.starts_with(&top) {
        return None;
    }
    Some(git::prefix_relative_to(scan_root, work_tree))
}

/// Keep only keys under `prefix` and strip it.
///
/// `prefix` is `""` when the scan root *is* the work tree (everything passes
/// through) and otherwise ends in `/`, so `sub/a.rs` -> `a.rs`.
fn rebase<T>(map: HashMap<String, T>, prefix: &str) -> HashMap<String, T> {
    if prefix.is_empty() {
        return map;
    }
    map.into_iter()
        .filter_map(|(path, value)| {
            path.strip_prefix(prefix)
                .map(|relative| (relative.to_string(), value))
        })
        .collect()
}

/// [`rebase`] for a set of changed paths.
fn rebase_set(paths: HashSet<String>, prefix: &str) -> HashSet<String> {
    if prefix.is_empty() {
        return paths;
    }
    paths
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(prefix)
                .map(std::string::ToString::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_works() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Hides the git-specific tests from a machine without git.
    macro_rules! require_git {
        () => {
            if !git_works() {
                return;
            }
        };
    }

    /// True when a Perforce client is installed *and* responsive. Only used to
    /// skip an assertion: the P4 stub may be implemented concurrently.
    fn p4_available() -> bool {
        Command::new("p4")
            .arg("info")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("ccn-vcs-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "CCN Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    /// Repository at `<tmp>/repo` with a file under `sub/`, committed once.
    fn repo_with_sub_file(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo");
        init_repo(&repo);
        write(&repo, "sub/a.rs", "1\n");
        commit(&repo, "first");
        tmp
    }

    #[test]
    fn detects_git_from_the_root_and_an_ancestor() {
        require_git!();
        let tmp = TempDir::new("detect-git");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo");
        init_repo(&repo);

        assert_eq!(detect(&repo), Some(Backend::Git), "scan root has .git");

        let nested = repo.join("sub").join("deeper");
        fs::create_dir_all(&nested).expect("create nested");
        assert_eq!(
            detect(&nested),
            Some(Backend::Git),
            "an ancestor's .git is enough"
        );
    }

    #[test]
    fn an_empty_directory_has_no_backend_when_p4_is_unavailable() {
        let tmp = TempDir::new("detect-none");
        assert!(detect(tmp.path()).is_none() || p4_available());
    }

    #[test]
    fn collect_history_rebases_keys_onto_a_subdirectory_scan_root() {
        require_git!();
        let tmp = repo_with_sub_file("history-sub");
        let scan_root = tmp.path().join("repo").join("sub");

        let history =
            collect_history(&scan_root, Backend::Git, 50).expect("git history from subdirectory");
        assert_eq!(history.commits, 1);
        assert_eq!(
            history.churn.keys().collect::<Vec<_>>(),
            vec!["a.rs"],
            "the sub/ prefix is stripped, and only the scan-root subtree is kept"
        );
        assert_eq!(
            history.authors.get("a.rs").and_then(|a| a.get("CCN Test")),
            Some(&1),
            "authors are rebased exactly like churn"
        );
        assert!(!history.churn.contains_key("sub/a.rs"));
    }

    #[test]
    fn changed_since_rebases_onto_a_subdirectory_scan_root() {
        require_git!();
        let tmp = repo_with_sub_file("changed-sub");
        let repo = tmp.path().join("repo");
        let scan_root = repo.join("sub");

        write(&repo, "sub/a.rs", "1\n2\n");
        let changed =
            changed_since(&scan_root, Backend::Git, "HEAD").expect("diff from subdirectory");
        assert_eq!(changed, HashSet::from(["a.rs".to_string()]));
    }

    #[test]
    fn files_outside_the_scan_root_are_dropped() {
        require_git!();
        let tmp = repo_with_sub_file("outside");
        let repo = tmp.path().join("repo");
        let scan_root = repo.join("sub");

        write(&repo, "outside.rs", "1\n");
        commit(&repo, "add outside");
        write(&repo, "outside.rs", "1\n2\n");
        write(&repo, "sub/a.rs", "1\n2\n");

        let changed =
            changed_since(&scan_root, Backend::Git, "HEAD~1").expect("diff from subdirectory");
        assert_eq!(
            changed,
            HashSet::from(["a.rs".to_string()]),
            "outside.rs is not under the scan root"
        );
        assert!(
            changed
                .iter()
                .all(|p| !p.starts_with("../") && !Path::new(p).is_absolute()),
            "keys are never escaped or absolute"
        );

        let history =
            collect_history(&scan_root, Backend::Git, 50).expect("git history from subdirectory");
        assert!(history.churn.contains_key("a.rs"));
        assert!(
            !history.churn.contains_key("outside.rs"),
            "history has the same scan-root contract as changed_since"
        );
    }

    #[test]
    fn a_non_git_directory_has_no_git_history() {
        let tmp = TempDir::new("not-a-repo");
        let repo = tmp.path().join("plain");
        fs::create_dir_all(&repo).expect("create dir");
        // The root itself is not a repository. `detect` may still find Perforce
        // (stub: false); the git calls must be `None` regardless.
        assert!(git::collect_history(&repo).is_none());
        assert!(changed_since(&repo, Backend::Git, "HEAD").is_none());
        assert!(collect_history(&repo, Backend::Git, 50).is_none());
    }
}
