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
//! Commands (each with a 20 s wall-clock deadline, see [`TIMEOUT`]):
//!
//! ```text
//! git -C <dir> rev-parse --show-toplevel
//! git -c core.quotepath=false -C <top> log --since=365.days --no-merges \
//!     --no-renames --numstat --format=%x01%an
//! git -c core.quotepath=false -C <top> diff --name-only --diff-filter=ACMR <rev> --
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
//! `%x01%an` starts each commit record with `\x01` and appends the author name
//! (raw UTF-8, no quoting) on the same line. The name is attached to every path
//! in that commit's numstat block, so the single `git log` pass yields both
//! churn and ownership — [`History::authors`] is never a second command.
//!
//! Work is bounded twice: `--since=365.days` limits the window, and parsing
//! stops after [`MAX_NUMSTAT_LINES`] numstat lines (~a 200k-file-change year),
//! so a pathological monorepo cannot turn this into an unbounded parse. The
//! per-path author map is additionally capped at
//! [`MAX_AUTHORS_PER_PATH`] entries.

use std::collections::{HashMap, HashSet};
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
    /// Commits per path per author, for the "who owns this hotspot" column.
    ///
    /// Keys match [`History::churn`] exactly, and each path's counts sum to at
    /// most that path's `commits` (fewer only when the path has more than
    /// [`MAX_AUTHORS_PER_PATH`] distinct authors and the tail was trimmed).
    /// The collapsed unknown identity is [`UNKNOWN_AUTHOR`].
    pub authors: HashMap<String, HashMap<String, u32>>,
}

/// Wall-clock budget for a single git command. Past this the child is killed
/// and the metric is reported as unavailable rather than stalling the scan.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Hard cap on parsed `--numstat` lines. A year of history on a large monorepo
/// can be hundreds of thousands of file changes; the hotspot ranking only needs
/// the shape of the distribution, so the tail is dropped deterministically.
pub const MAX_NUMSTAT_LINES: usize = 200_000;

/// Distinct authors kept per path. Kept authors are the highest-count ones
/// (ties broken by name, ascending) so the map's shape — and therefore
/// `top_author` and the author count — is stable across runs.
///
/// The cap exists for monorepos with generated files touched by thousands of
/// bots/AI identities: `paths × authors` would otherwise grow unbounded. It is
/// applied when parsing finishes, because counts must be complete before
/// "highest" means anything; the intermediate map is bounded by the number of
/// distinct author names in the window, which git itself already materialised.
pub const MAX_AUTHORS_PER_PATH: usize = 20;

/// Author recorded when `%an` is empty (a commit made with an empty ident, or a
/// truncated capture). Never drop the path: churn and authorship stay aligned.
pub const UNKNOWN_AUTHOR: &str = "(unknown)";

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
            // One record separator + the author name; parsed below.
            "--format=%x01%an",
        ]);
    let out = run(cmd)?;

    let (commits, churn, authors) = parse_log(&out);
    if commits == 0 {
        // An empty log means an unborn branch, or a repository whose last
        // commit is older than the window. Either way there is nothing to rank.
        return None;
    }

    Some(History {
        work_tree,
        commits,
        churn,
        authors,
    })
}

/// Work-tree-relative paths that differ between `rev` and the working tree.
///
/// `git diff --name-only <rev>` compares the commit to the working tree, so it
/// covers committed, staged and unstaged edits in one command. Untracked files
/// are **not** included: git has no revision to diff them against, and a
/// brand-new file has by definition not churned within the window.
///
/// `--diff-filter=ACMR` keeps added, copied, modified and renamed paths and
/// drops deletions (`D`): a file that no longer exists has no lines to measure,
/// so reporting it would only add a path the LOC pass can never join. A rename
/// contributes only its new name, which is the half that exists on disk.
///
/// Keys are work-tree relative and `/`-separated — exactly the keys in
/// [`History::churn`] — so a caller can intersect the two sets directly.
///
/// Returns `None` when `rev` is empty or looks like an option, `repo` is not a
/// work tree, git is missing, the revision is unknown, the command fails, or it
/// exceeds [`TIMEOUT`]. A successful diff with no changes is `Some(empty)`,
/// which is meaningfully different from "could not ask".
pub fn changed_since(repo: &Path, rev: &str) -> Option<HashSet<String>> {
    if rev.is_empty() || rev.starts_with('-') {
        return None;
    }
    let work_tree = work_tree(repo)?;

    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(&work_tree)
        .args(["diff", "--name-only", "--diff-filter=ACMR", rev, "--"]);
    let out = run(cmd)?;

    let text = String::from_utf8_lossy(&out);
    let mut changed = HashSet::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
        if line.is_empty() {
            continue;
        }
        changed.insert(line.replace('\\', "/"));
    }
    Some(changed)
}

/// Renames between `rev` and the working tree: new path -> old path,
/// work-tree-relative and `/`-separated.
///
/// `git diff -M` uses rename detection, so a path that only moved appears as a
/// single `R<score>` record instead of a delete plus an add. Only renames are
/// kept: `A`, `M` and `D` entries carry no old path and are ignored.
///
/// Returns `None` when `rev` is empty, starts with `-`, contains whitespace,
/// `repo` is not a work tree, git is missing, the revision is unknown, the
/// command fails, or it exceeds [`TIMEOUT`]. A successful diff with no renames
/// is `Some(empty)`, which is meaningfully different from "could not ask".
pub fn renames(repo: &Path, rev: &str) -> Option<HashMap<String, String>> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) {
        return None;
    }
    let work_tree = work_tree(repo)?;

    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(&work_tree)
        .args(["diff", "-M", "--name-status", rev, "--"]);
    let out = run(cmd)?;

    let text = String::from_utf8_lossy(&out);
    let mut renames = HashMap::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        // `R<score>\told\tnew`; the first field never contains a tab, so
        // splitting the rest into exactly two paths is safe.
        let mut fields = line.splitn(3, '\t');
        let (Some(status), Some(old), Some(new)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !status.starts_with('R') {
            continue;
        }
        let (old, new) = (old.trim(), new.trim());
        if old.is_empty() || new.is_empty() {
            continue;
        }
        renames.insert(new.replace('\\', "/"), old.replace('\\', "/"));
    }
    Some(renames)
}

/// True when `rev` resolves to a commit in `repo`.
pub fn rev_exists(repo: &Path, rev: &str) -> bool {
    if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) {
        return false;
    }
    let Some(top) = work_tree(repo) else {
        return false;
    };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(top)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"));
    run(cmd).is_some()
}

/// Raw contents of `path` (work-tree-relative, `/`-separated) at `rev`.
///
/// Returns `None` when the rev/path does not exist or git fails.
///
/// The bytes are returned **verbatim** — no `String` round trip, no lossy
/// decoding — because baseline files may be non-UTF-8 (a Latin-1 source file, or
/// any binary asset a language's "source" glob happens to match) and a baseline
/// has to reproduce the old file exactly to be compared against the new one.
///
/// A file that was added after `rev` is legitimately absent there, and an unknown
/// revision is indistinguishable from a missing path at this layer: both are
/// `None`, the same "could not ask" degrade the rest of this module uses.
///
/// `rev` is rejected before spawning when it is empty, starts with `-` (an
/// option smuggled into the argument list) or contains whitespace (which could
/// split the `rev:path` operand). `path` is rejected when empty or containing a
/// NUL, which `Command` could not pass to the child at all.
pub fn file_at(repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) {
        return None;
    }
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    let work_tree = work_tree(repo)?;

    // One operand, not two: `git show <rev>:<path>` is the whole specification.
    // The path is not option-parsed after the colon, and `rev` was checked above.
    let spec = format!("{rev}:{path}");
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&work_tree).args(["show", &spec]);
    // `run` returns stdout bytes as-is and is already `None` on a non-zero exit
    // or a timeout, which is exactly the missing-file contract here.
    run(cmd)
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

/// Parse `git log --format=%x01%an --numstat` output.
///
/// Every `\x01` starts a new commit record: the rest of that line is the author
/// name, and the numstat table follows it. Only lines with exactly three
/// tab-separated fields are changes — a blank line separates the format from the
/// table and is ignored.
///
/// `splitn(3, '\t')` is what makes non-ASCII safe: a path may contain a tab
/// (quoted by git) or any UTF-8 sequence, but the first two fields never do, so
/// the third is taken verbatim rather than re-split or ASCII-decoded.
///
/// Both counters advance once per numstat line, so churn (`commits`,
/// `lines_changed`) and attribution (`authors[path][author]`) can never drift:
/// a path's author counts sum to the same number as its commit count in one
/// commit, and each commit contributes to exactly one author bucket per path.
fn parse_log(bytes: &[u8]) -> (u32, HashMap<String, Churn>, Authors) {
    let text = String::from_utf8_lossy(bytes);
    let mut commits = 0u32;
    let mut parsed = 0usize;
    let mut churn: HashMap<String, Churn> = HashMap::new();
    let mut authors: Authors = HashMap::new();
    // A numstat line before any record has no author to credit.
    let mut author = UNKNOWN_AUTHOR.to_string();

    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix('\u{1}') {
            commits = commits.saturating_add(1);
            let name = rest.trim();
            author = if name.is_empty() {
                UNKNOWN_AUTHOR.to_string()
            } else {
                name.to_string()
            };
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

        let key = path.replace('\\', "/");
        // `-` marks a binary file: it changes, but its line count is meaningless.
        let lines = count(added).saturating_add(count(deleted));
        let entry = churn.entry(key.clone()).or_default();
        entry.commits = entry.commits.saturating_add(1);
        entry.lines_changed = entry.lines_changed.saturating_add(lines);

        let per_author = authors.entry(key).or_default();
        let count = per_author.entry(author.clone()).or_insert(0);
        *count = count.saturating_add(1);
    }

    trim_authors(&mut authors);
    (commits, churn, authors)
}

/// Commits per path per author.
type Authors = HashMap<String, HashMap<String, u32>>;

/// Keep only the [`MAX_AUTHORS_PER_PATH`] most frequent authors of each path.
///
/// Highest count first, ties broken by name ascending, so the surviving set is
/// deterministic rather than dependent on hash iteration order. Paths at or
/// under the cap are untouched (the common case, and the only one a normal
/// repository ever hits).
fn trim_authors(authors: &mut Authors) {
    for per_author in authors.values_mut() {
        if per_author.len() <= MAX_AUTHORS_PER_PATH {
            continue;
        }
        let mut ranked: Vec<(String, u32)> =
            per_author.iter().map(|(n, c)| (n.clone(), *c)).collect();
        ranked.sort_by(|(an, ac), (bn, bc)| bc.cmp(ac).then_with(|| an.cmp(bn)));
        ranked.truncate(MAX_AUTHORS_PER_PATH);
        per_author.clear();
        per_author.extend(ranked);
    }
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
        write_bytes(dir, name, body.as_bytes());
    }

    /// Byte-exact writer: `write` would force the body through UTF-8.
    fn write_bytes(dir: &Path, name: &str, body: &[u8]) {
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

    /// Commit as `author`, without touching the repository config, so one test
    /// repository can carry several identities. `-c` before the subcommand is
    /// how git takes a one-shot config value.
    fn commit_as(dir: &Path, message: &str, author: &str) {
        git(dir, &["add", "-A"]);
        let name = format!("user.name={author}");
        git(dir, &["-c", name.as_str(), "commit", "-q", "-m", message]);
    }

    #[test]
    fn parses_commit_records_ties_and_binary_entries() {
        let out = "\u{1}Ann\n2\t1\tsrc/a.rs\n-\t-\tassets/logo.png\n\n\u{1}Zoë\n0\t3\tsrc\\b.rs\n";
        let (commits, churn, authors) = parse_log(out.as_bytes());

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

        // Attribution rides the same records, keyed exactly like churn.
        assert_eq!(authors.get("src/a.rs").unwrap().get("Ann"), Some(&1));
        assert_eq!(authors.get("src/b.rs").unwrap().get("Zoë"), Some(&1));
    }

    #[test]
    fn an_empty_author_name_falls_back_to_unknown() {
        let out = "\u{1}\n1\t0\ta.rs\n";
        let (commits, churn, authors) = parse_log(out.as_bytes());
        assert_eq!(commits, 1);
        assert_eq!(churn.get("a.rs").unwrap().commits, 1);
        assert_eq!(
            authors.get("a.rs").unwrap().get(UNKNOWN_AUTHOR),
            Some(&1),
            "a record without %an still attributes the work"
        );
    }

    #[test]
    fn the_author_map_keeps_only_the_top_twenty_per_path() {
        // Twenty-five authors on one path: author i gets i + 1 commits, so the
        // cap must drop the five *least* frequent, not the first five seen.
        let mut out = String::new();
        for i in 0..25u32 {
            for _ in 0..=i {
                out.push('\u{1}');
                out.push_str(&format!("author-{i:02}"));
                out.push('\n');
                out.push_str("1\t0\tsrc/hot.rs\n");
            }
        }

        let (commits, churn, authors) = parse_log(out.as_bytes());
        assert_eq!(commits, 325);
        let per_author = authors.get("src/hot.rs").expect("path is kept");
        assert_eq!(per_author.len(), MAX_AUTHORS_PER_PATH);
        assert_eq!(per_author.get("author-24"), Some(&25));
        assert_eq!(per_author.get("author-05"), Some(&6));
        assert_eq!(per_author.get("author-04"), None, "the tail is dropped");
        assert_eq!(per_author.get("author-00"), None);
        // The kept counts are 6..=25; the dropped tail (1..=5) is the price of
        // the cap, and `History::authors` documents it as "at most" the commits.
        assert_eq!(per_author.values().sum::<u32>(), 310);
        assert_eq!(churn.get("src/hot.rs").unwrap().commits, 325);
    }

    #[test]
    fn attributes_each_path_to_the_committing_author() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("authors");
        init_repo(tmp.path());

        write(tmp.path(), "a.rs", "1\n");
        write(tmp.path(), "b.rs", "1\n");
        commit_as(tmp.path(), "ann adds a and b", "Ann");

        write(tmp.path(), "a.rs", "1\n2\n");
        commit_as(tmp.path(), "bob edits a", "Bob");

        write(tmp.path(), "a.rs", "1\n2\n3\n");
        commit_as(tmp.path(), "ann edits a again", "Ann");

        // Non-ASCII identity: `%an` is emitted as raw UTF-8.
        write(tmp.path(), "c.rs", "1\n");
        commit_as(tmp.path(), "zoë adds c", "Zoë Ünïcode");

        // One commit each on c.rs, so `top_author` has to break a tie.
        write(tmp.path(), "c.rs", "1\n2\n");
        commit_as(tmp.path(), "bob edits c", "Bob");

        let history = collect_history(tmp.path()).expect("git history");
        assert_eq!(history.commits, 5);

        let a = history.authors.get("a.rs").expect("a.rs is attributed");
        assert_eq!(a.get("Ann"), Some(&2));
        assert_eq!(a.get("Bob"), Some(&1));
        assert_eq!(a.len(), 2);
        assert_eq!(
            a.values().sum::<u32>(),
            history.churn.get("a.rs").unwrap().commits,
            "authorship does not add extra commits"
        );
        assert_eq!(history.churn.get("a.rs").unwrap().commits, 3);

        assert_eq!(
            history.authors.get("b.rs").unwrap().get("Ann"),
            Some(&1),
            "b.rs was only touched by Ann"
        );
        let c = history.authors.get("c.rs").expect("c.rs is attributed");
        assert_eq!(c.get("Zoë Ünïcode"), Some(&1));
        assert_eq!(c.get("Bob"), Some(&1));

        // Exactly the rule the report uses: most commits, ties by name.
        let top_author = |path: &str| {
            history.authors.get(path).and_then(|per_author| {
                per_author
                    .iter()
                    .max_by(|(an, ac), (bn, bc)| ac.cmp(bc).then_with(|| bn.cmp(an)))
                    .map(|(name, _)| name.clone())
            })
        };
        assert_eq!(top_author("a.rs").as_deref(), Some("Ann"));
        assert_eq!(top_author("b.rs").as_deref(), Some("Ann"));
        assert_eq!(top_author("c.rs").as_deref(), Some("Bob"), "tie goes to B");
        assert_eq!(top_author("nope.rs"), None);
    }

    #[test]
    fn changed_since_lists_modified_paths_and_excludes_deletions() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("changed");
        init_repo(tmp.path());

        write(tmp.path(), "a.rs", "1\n");
        write(tmp.path(), "b.rs", "1\n");
        commit(tmp.path(), "first");

        write(tmp.path(), "a.rs", "1\n2\n");
        commit(tmp.path(), "second");

        // An unstaged edit on top of HEAD; b.rs still matches HEAD~1.
        write(tmp.path(), "a.rs", "1\n2\n3\n");
        let changed = changed_since(tmp.path(), "HEAD~1").expect("diff against HEAD~1");
        assert_eq!(changed, HashSet::from(["a.rs".to_string()]));

        // Deleting b.rs must not surface it: a deleted file has no LOC.
        fs::remove_file(tmp.path().join("b.rs")).expect("delete b.rs");
        let changed = changed_since(tmp.path(), "HEAD~1").expect("diff against HEAD~1");
        assert_eq!(changed, HashSet::from(["a.rs".to_string()]));
        assert!(
            !changed.contains("b.rs"),
            "--diff-filter=ACMR drops deletions"
        );

        // A revision that does not exist is "could not ask", not "nothing changed".
        assert!(changed_since(tmp.path(), "no-such-rev-9f3a").is_none());
    }

    #[test]
    fn renames_maps_the_new_path_to_the_old_one() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("renames");
        init_repo(tmp.path());

        write(tmp.path(), "a.rs", "fn a() {}\n");
        commit(tmp.path(), "first");

        git(tmp.path(), &["mv", "a.rs", "b.rs"]);
        commit(tmp.path(), "rename");

        let renames = renames(tmp.path(), "HEAD~1").expect("diff against HEAD~1");
        assert_eq!(
            renames,
            HashMap::from([("b.rs".to_string(), "a.rs".to_string())]),
            "a pure rename is one R100 record, not a delete plus an add"
        );
    }

    #[test]
    fn a_plain_modification_has_no_renames() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("renames-mod");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "1\n");
        commit(tmp.path(), "first");
        write(tmp.path(), "a.rs", "1\n2\n");
        commit(tmp.path(), "modify");

        let renames = renames(tmp.path(), "HEAD~1").expect("diff against HEAD~1");
        assert!(renames.is_empty(), "A/M entries carry no old path");
    }

    #[test]
    fn no_renames_is_some_empty_not_none() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("renames-none");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "1\n");
        commit(tmp.path(), "first");

        assert_eq!(
            renames(tmp.path(), "HEAD"),
            Some(HashMap::new()),
            "a successful diff with nothing renamed is a known-empty answer"
        );
    }

    #[test]
    fn renames_rejects_bad_revisions_before_spawning() {
        let tmp = TempDir::new("renames-bad-rev");
        assert!(renames(tmp.path(), "").is_none());
        assert!(renames(tmp.path(), "--output=/tmp/x").is_none());
        assert!(renames(tmp.path(), "HE AD").is_none());
    }

    #[test]
    fn changed_since_accepts_working_tree_relative_keys_from_a_subdirectory() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("changed-sub");
        init_repo(tmp.path());
        write(tmp.path(), "core/a.rs", "1\n");
        commit(tmp.path(), "first");
        write(tmp.path(), "core/a.rs", "1\n2\n");

        let core = tmp.path().join("core");
        let changed = changed_since(&core, "HEAD").expect("diff from a subdirectory");
        assert_eq!(
            changed,
            HashSet::from(["core/a.rs".to_string()]),
            "keys are work-tree relative, same as History::churn"
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
        // Diffing there cannot mean anything: None, not an empty set.
        assert!(changed_since(tmp.path(), "HEAD").is_none());
    }

    #[test]
    fn a_missing_directory_has_no_history() {
        let tmp = TempDir::new("missing");
        let gone = tmp.path().join("does-not-exist");
        assert!(!gone.exists());
        assert!(collect(&gone).is_none());
        assert!(work_tree(&gone).is_none());
        assert!(changed_since(&gone, "HEAD").is_none());
    }

    #[test]
    fn changed_since_rejects_revisions_that_look_like_options() {
        let tmp = TempDir::new("rev-option");
        // Rejected before git is even spawned: no way to smuggle a flag in.
        assert!(changed_since(tmp.path(), "--output=/tmp/x").is_none());
        assert!(changed_since(tmp.path(), "").is_none());
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

    #[test]
    fn file_at_returns_the_exact_bytes_including_non_utf8() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-bytes");
        init_repo(tmp.path());

        // Korean text proves the UTF-8 path is untouched; the `0xFF 0xFE` pair
        // is not valid UTF-8 at all, so any `String` round trip would lose it.
        let mut body = "// 한국어 주석\nfn main() {}\n".as_bytes().to_vec();
        body.extend_from_slice(&[0xFF, 0xFE]);
        body.extend_from_slice(b"\n");
        write_bytes(tmp.path(), "a.rs", &body);
        commit(tmp.path(), "first");

        let got = file_at(tmp.path(), "HEAD", "a.rs").expect("a.rs at HEAD");
        assert_eq!(got, body, "bytes must round-trip exactly");
        assert!(
            String::from_utf8(got.clone()).is_err(),
            "the 0xFF 0xFE pair is not valid UTF-8"
        );
        assert!(got.windows(9).any(|w| w == "한국어".as_bytes()));
    }

    #[test]
    fn file_at_is_none_for_a_missing_path_or_revision() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-missing");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "1\n");
        commit(tmp.path(), "first");

        assert!(file_at(tmp.path(), "HEAD", "missing.rs").is_none());
        assert!(file_at(tmp.path(), "no-such-rev", "a.rs").is_none());
        assert!(file_at(tmp.path(), "HEAD", "a.rs").is_some());
    }

    #[test]
    fn file_at_rejects_bad_arguments_before_spawning() {
        let tmp = TempDir::new("file-at-bad-args");
        // No repository and possibly no git: the rejection happens on the
        // arguments alone, so these are `None` regardless.
        assert!(file_at(tmp.path(), "-pwn", "a.rs").is_none());
        assert!(file_at(tmp.path(), "", "a.rs").is_none());
        assert!(file_at(tmp.path(), "HEAD", "").is_none());
        assert!(file_at(tmp.path(), "HEAD", "a\0b.rs").is_none());
        assert!(file_at(tmp.path(), "HE AD", "a.rs").is_none());
    }

    #[test]
    fn file_at_reads_a_path_in_a_subdirectory() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-sub");
        init_repo(tmp.path());
        write(tmp.path(), "sub/a.rs", "sub\n");
        commit(tmp.path(), "first");

        assert_eq!(
            file_at(tmp.path(), "HEAD", "sub/a.rs").as_deref(),
            Some(b"sub\n".as_slice())
        );
        // The path is work-tree relative, not relative to some invocation dir.
        assert!(file_at(tmp.path(), "HEAD", "a.rs").is_none());
    }
}
