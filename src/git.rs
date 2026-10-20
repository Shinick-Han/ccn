//! Git history metrics (hotspots).
//!
//! This module shells out to `git` instead of linking a history library: the
//! tool already depends on the user's Git for the working tree, and `git log
//! --numstat` is the one output format stable enough across versions to parse.
//! It is the **only** place in the crate that spawns a process for metrics.
//!
//! Contract (see `docs/PLAN.md` §5.4 and
//! `docs/research-metrics-and-viz.md` §2.2):
//!
//! - [`collect`] returns `None` — never an error, never a partial map — when git
//!   is missing, the path is not inside a work tree, the command fails, or the
//!   repository has no commit in the window. The report then silently degrades
//!   to the CCN-only view, exactly like `--no-ccn`.
//! - Keys are **relative to the work tree** and always `/`-separated. The
//!   caller ([`crate::metrics::build`]) rebases them onto the scan root with
//!   [`prefix_relative_to`], so a scan of `repo/crates/core` still lines up.
//!
//! Commands (both with a 20 s wall-clock deadline, see [`TIMEOUT`]):
//!
//! ```text
//! git -C <dir> rev-parse --show-toplevel
//! git -c core.quotepath=false -C <top> log --since=365.days --no-merges \
//!     --no-renames --numstat --format=%x01
//! ```
//!
//! `--no-renames` is deliberate: rename detection would print `old => new`
//! (or a `dir/{a => b}/f` abbreviation) in the path column, and guessing which
//! side is "the file" invents history. A rename therefore shows up as a delete
//! plus an add, which is what the diff literally is.
//!
//! `core.quotepath=false` keeps non-ASCII paths as UTF-8 instead of octal
//! escapes, so a path matches the one the LOC pass reported.
//!
//! Work is bounded twice: `--since=365.days` limits the window, and parsing
//! stops after [`MAX_NUMSTAT_LINES`] numstat lines (~a 200k-file-change year),
//! so a pathological monorepo cannot turn this into an unbounded parse.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Per-file change history, in the last 365 days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Churn {
    /// Commits in the window that touched this path.
    pub commits: u32,
    /// Added + deleted lines over those commits. Binary files (`-\t-`) count
    /// as 0 lines but still as one commit.
    pub lines_changed: u32,
}

/// Everything [`crate::metrics::build`] needs from one `git log` pass.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// Absolute work-tree root the keys are relative to.
    pub work_tree: PathBuf,
    /// Commit records seen in the window (merges excluded).
    pub commits: u32,
    /// Churn per work-tree-relative, `/`-separated path.
    pub churn: HashMap<String, Churn>,
}

/// Wall-clock budget for a single git command. Past this the child is killed
/// and the metric is reported as unavailable rather than stalling the scan.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Hard cap on parsed `--numstat` lines. A year of history on a large monorepo
/// can be hundreds of thousands of file changes; the hotspot ranking only needs
/// the shape of the distribution, so the tail is dropped deterministically.
pub const MAX_NUMSTAT_LINES: usize = 200_000;

/// Upper bound on captured stdout. Past this the reader keeps draining the pipe
/// (so git cannot block on a full buffer) but stops growing the buffer.
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

/// Collect churn for `repo`. Returns `None` when git is unavailable, `repo` is
/// not inside a work tree, the command fails, or the window contains no commit.
pub fn collect(repo: &Path) -> Option<HashMap<String, Churn>> {
    collect_history(repo).map(|history| history.churn)
}

/// [`collect`] plus the work-tree root and commit count the join layer needs.
///
/// `collect` is the public shape; this is the same command with the context
/// kept, so [`crate::metrics::build`] does not have to run `rev-parse` twice.
pub fn collect_history(repo: &Path) -> Option<History> {
    let work_tree = work_tree(repo)?;

    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(&work_tree)
        .args([
            "log",
            "--since=365.days",
            "--no-merges",
            "--no-renames",
            "--numstat",
            "--format=%x01",
        ]);
    let out = run(cmd)?;

    let (commits, churn) = parse_numstat(&out);
    if commits == 0 {
        // An empty log means an unborn branch, or a repository whose last
        // commit is older than the window. Either way there is nothing to rank.
        return None;
    }

    Some(History {
        work_tree,
        commits,
        churn,
    })
}

/// Work-tree root for `dir`, or `None` when git cannot answer.
pub fn work_tree(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"]);
    let out = run(cmd)?;
    let text = String::from_utf8_lossy(&out);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(PathBuf::from(line))
    }
}

/// `root`'s directory relative to the work tree, as a `/`-separated prefix with
/// a trailing slash (`""` when `root` *is* the work tree).
///
/// Both sides are canonicalised first: `rev-parse` prints forward slashes and
/// canonicalise adds a Windows verbatim prefix, so a textual comparison would
/// silently fail. When `root` is not under `top` the prefix is `""` — churn then
/// simply never matches, which is the same outcome as "no history for this
/// subtree" rather than an error.
pub fn prefix_relative_to(root: &Path, top: &Path) -> String {
    let root = normalize(root);
    let top = normalize(top);
    let Ok(rel) = root.strip_prefix(&top) else {
        return String::new();
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}/", parts.join("/"))
    }
}

fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Parse `git log --format=%x01 --numstat` output.
///
/// Every `\x01` starts a new commit record; the numstat table follows it. Only
/// lines with exactly three tab-separated fields are changes — a blank line
/// separates the format from the table and is ignored.
fn parse_numstat(bytes: &[u8]) -> (u32, HashMap<String, Churn>) {
    let text = String::from_utf8_lossy(bytes);
    let mut commits = 0u32;
    let mut parsed = 0usize;
    let mut churn: HashMap<String, Churn> = HashMap::new();

    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with('\u{1}') {
            commits = commits.saturating_add(1);
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if parsed >= MAX_NUMSTAT_LINES {
            break;
        }

        let mut fields = line.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        parsed += 1;

        // `-` marks a binary file: it changes, but its line count is meaningless.
        let lines = count(added).saturating_add(count(deleted));
        let entry = churn.entry(path.replace('\\', "/")).or_default();
        entry.commits = entry.commits.saturating_add(1);
        entry.lines_changed = entry.lines_changed.saturating_add(lines);
    }

    (commits, churn)
}

fn count(field: &str) -> u32 {
    field.trim().parse::<u32>().unwrap_or(0)
}

/// Spawn `cmd`, read stdout to EOF, and give up after [`TIMEOUT`].
///
/// `try_wait` polling (not `wait`) is what makes the deadline real: a `git log`
/// on a network drive can hang forever, and a scan that blocks on it is worse
/// than a scan without hotspots. The reader thread is joined in every path so
/// no thread is leaked, and stdout is drained past the capture cap so a killed
/// or oversized child never blocks on a full pipe.
fn run(mut cmd: Command) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;

    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE_BYTES {
                        let take = n.min(MAX_CAPTURE_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        buf
    });

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn git_works() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Unique scratch directory, removed on drop. Prefix includes the pid and
    /// the nanos so parallel test threads never share a repository.
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
                std::env::temp_dir().join(format!("ccn-git-{tag}-{}-{nanos}", std::process::id()));
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

    #[test]
    fn parses_commit_records_ties_and_binary_entries() {
        let out = "\u{1}\n2\t1\tsrc/a.rs\n-\t-\tassets/logo.png\n\n\u{1}\n0\t3\tsrc\\b.rs\n";
        let (commits, churn) = parse_numstat(out.as_bytes());

        assert_eq!(commits, 2);
        assert_eq!(churn.len(), 3);
        assert_eq!(
            churn.get("src/a.rs"),
            Some(&Churn {
                commits: 1,
                lines_changed: 3
            })
        );
        assert_eq!(
            churn.get("assets/logo.png"),
            Some(&Churn {
                commits: 1,
                lines_changed: 0
            }),
            "a binary file changed but contributed no lines"
        );
        // Backslashes are normalised, so a Windows-generated path still joins.
        assert_eq!(
            churn.get("src/b.rs"),
            Some(&Churn {
                commits: 1,
                lines_changed: 3
            })
        );
    }

    #[test]
    fn counts_commits_and_lines_in_a_real_repository() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("repo");
        init_repo(tmp.path());

        write(tmp.path(), "a.rs", "1\n2\n");
        write(tmp.path(), "b.rs", "1\n");
        commit(tmp.path(), "first");

        write(tmp.path(), "a.rs", "1\n2\n3\n");
        commit(tmp.path(), "second");

        write(tmp.path(), "b.rs", "9\n");
        commit(tmp.path(), "third");

        let history = collect_history(tmp.path()).expect("git history");
        assert_eq!(history.commits, 3);
        assert_eq!(
            history.churn.get("a.rs"),
            Some(&Churn {
                commits: 2,
                lines_changed: 3,
            }),
            "2 lines added, then 1"
        );
        assert_eq!(
            history.churn.get("b.rs"),
            Some(&Churn {
                commits: 2,
                lines_changed: 3,
            }),
            "1 added, then 1 added + 1 deleted"
        );
        assert!(!history.churn.contains_key(".gitignore"));
    }

    #[test]
    fn a_subdirectory_scan_is_rebased_onto_the_work_tree() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("subdir");
        init_repo(tmp.path());
        write(tmp.path(), "core/src/a.rs", "1\n2\n");
        write(tmp.path(), "top.rs", "1\n");
        commit(tmp.path(), "first");

        let core = tmp.path().join("core");
        let history = collect_history(&core).expect("git history from a subdirectory");
        assert_eq!(history.commits, 1);
        // Keys stay work-tree relative: the join layer adds the prefix.
        assert!(history.churn.contains_key("core/src/a.rs"));
        assert_eq!(
            prefix_relative_to(&core, &history.work_tree),
            "core/",
            "scan root prefix"
        );
        assert_eq!(prefix_relative_to(tmp.path(), &history.work_tree), "");
    }

    #[test]
    fn a_directory_that_is_not_a_repository_has_no_history() {
        let tmp = TempDir::new("plain");
        assert!(collect(tmp.path()).is_none());
        assert!(work_tree(tmp.path()).is_none());
    }

    #[test]
    fn a_missing_directory_has_no_history() {
        let tmp = TempDir::new("missing");
        let gone = tmp.path().join("does-not-exist");
        assert!(!gone.exists());
        assert!(collect(&gone).is_none());
        assert!(work_tree(&gone).is_none());
    }

    #[test]
    fn an_empty_repository_has_no_history() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("empty");
        init_repo(tmp.path());
        // No commit yet: `git log` fails, which is `None`, not an error.
        assert!(collect(tmp.path()).is_none());
    }
}
