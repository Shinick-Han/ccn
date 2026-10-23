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

use crate::status::FileChange;
use crate::vcs::FileAt;

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
/// The three [`FileAt`] states are kept distinct on purpose:
///
/// - `git show <rev>:<path>` succeeds -> [`FileAt::Found`] with the bytes as-is;
/// - it fails, the commit exists, and `git cat-file -e <rev>:<path>` reports
///   the object is absent -> [`FileAt::Absent`] (the file did not exist at
///   `rev`: added later, deleted, or never committed);
/// - anything else (`cat-file` says the object exists even though `show`
///   failed, the revision itself does not resolve, git is missing, a spawn
///   fails, or either command exceeds [`TIMEOUT`]) -> [`FileAt::Unavailable`].
///
/// The last rule is the point: a permission error, a corrupt object read or a
/// timeout must not be reported as "the file was new", because that turns
/// unreadable history into fabricated `Added` findings.
///
/// The bytes are returned **verbatim** — no `String` round trip, no lossy
/// decoding — because baseline files may be non-UTF-8 (a Latin-1 source file, or
/// any binary asset a language's "source" glob happens to match) and a baseline
/// has to reproduce the old file exactly to be compared against the new one.
///
/// `rev` is rejected when it is empty, starts with `-` (an option smuggled into
/// the argument list) or contains whitespace (which could split the `rev:path`
/// operand). `path` is rejected when it is not a plain existing-tree path: empty,
/// containing a NUL, absolute, a Windows separator, or a `..`/empty segment.
/// A rejected argument is `Unavailable`, never `Absent`, because no question was
/// actually asked of the repository.
pub fn file_at(repo: &Path, rev: &str, path: &str) -> FileAt {
    if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) {
        return FileAt::Unavailable;
    }
    if !is_tree_path(path) {
        return FileAt::Unavailable;
    }
    let Some(work_tree) = work_tree(repo) else {
        return FileAt::Unavailable;
    };

    // One operand, not two: `git show <rev>:<path>` is the whole specification.
    // The path is not option-parsed after the colon, and `rev` was checked above.
    let spec = format!("{rev}:{path}");
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&work_tree).args(["show", &spec]);
    match run_exit(cmd) {
        Some((true, bytes)) => FileAt::Found(bytes),
        // `show` ran and failed: absence or a read failure, decided below.
        Some((false, _)) => classify_failed_show(&work_tree, rev, &spec),
        // Spawn failure or timeout: we did not learn anything.
        None => FileAt::Unavailable,
    }
}

/// Decide whether a failed `git show <rev>:<path>` was absence or a failure.
///
/// Runs only after `show` exited non-zero:
///
/// 1. the revision must resolve to a commit, else nothing can be said
///    ([`FileAt::Unavailable`], not "the file did not exist yet");
/// 2. the commit's **tree** must be readable, which separates "the object store
///    answered and the path is not in that tree" from "the repository could not
///    be read at all";
/// 3. with both proven, `git cat-file -e <rev>:<path>` returning non-zero is the
///    tree saying the path has no object there -> [`FileAt::Absent`]. If it
///    returns zero the object exists and only `show` failed (e.g. an unreadable
///    blob), which is [`FileAt::Unavailable`].
///
/// Exit status only: no diagnostic text is parsed, so a localized or reworded
/// git message cannot flip a failure into "absent".
fn classify_failed_show(work_tree: &Path, rev: &str, spec: &str) -> FileAt {
    if !commit_exists(work_tree, rev) {
        return FileAt::Unavailable;
    }
    if !object_exists(work_tree, &format!("{rev}^{{tree}}")) {
        return FileAt::Unavailable;
    }
    match cat_file_exists(work_tree, spec) {
        Some(true) => FileAt::Unavailable,
        Some(false) => FileAt::Absent,
        None => FileAt::Unavailable,
    }
}

/// True when `rev` resolves to a commit in `work_tree`.
fn commit_exists(work_tree: &Path, rev: &str) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(work_tree)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"));
    matches!(run_exit(cmd), Some((true, _)))
}

/// `git cat-file -e <spec>`: `Some(true)` if the object is there,
/// `Some(false)` if git ran and said no, `None` if git could not run.
fn object_exists(work_tree: &Path, spec: &str) -> bool {
    cat_file_exists(work_tree, spec).unwrap_or(false)
}

fn cat_file_exists(work_tree: &Path, spec: &str) -> Option<bool> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(work_tree).args(["cat-file", "-e", spec]);
    run_exit(cmd).map(|(success, _)| success)
}

/// True when `path` is a plain relative path that can exist in a git tree.
///
/// Backslashes are rejected: every caller passes `/`-separated tree paths, and a
/// Windows-looking path would silently address a different (or no) object.
fn is_tree_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return false;
    }
    if path.starts_with('/') {
        return false;
    }
    path.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Current branch of `dir`, or the short commit id when HEAD is detached.
///
/// `git symbolic-ref --short -q HEAD` is the cheap answer and works from any
/// directory inside the work tree, so no separate `rev-parse --show-toplevel`
/// round trip is needed. A detached HEAD makes that command fail, and the short
/// commit id is then the only useful identity the repository has.
///
/// Best effort by design: a missing `git`, a non-repository directory, a timeout
/// or unreadable output all yield `None`, never an error or a fabricated name.
pub(crate) fn branch(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let mut symbolic = Command::new("git");
    symbolic
        .arg("-C")
        .arg(dir)
        .args(["symbolic-ref", "--short", "-q", "HEAD"]);
    if let Some(out) = run(symbolic) {
        if let Some(name) = parse_ref_output(&out) {
            return Some(name);
        }
    }

    let mut short = Command::new("git");
    short
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--short", "HEAD"]);
    run(short).and_then(|out| parse_ref_output(&out))
}

/// Parse a one-line git ref/commit response (`<name>\n`, possibly CRLF).
///
/// The first non-blank line is kept verbatim apart from surrounding whitespace.
/// A ref name containing internal spaces is legal on some filesystems, so only
/// the line terminator is removed, not interior whitespace.
pub(crate) fn parse_ref_output(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).trim())
        .find(|line| !line.is_empty())
        .map(str::to_string)
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

/// Working-tree status for `dir`, keyed by scan-root-relative `/` paths.
///
/// Runs `git status --porcelain=v1 -z` (the NUL-separated form, which cannot be
/// confused by a path containing a newline or ` -> `), falling back to the
/// line-oriented form for a git too old to know `-z`. Both use the module's
/// bounded [`run`] helper, so a hung repository degrades to `None` after
/// [`TIMEOUT`] instead of blocking the UI. `None` also means git could not
/// answer at all (not a work tree, missing binary); an empty map means "clean".
///
/// `--untracked-files=all` is deliberate: the default porcelain output collapses
/// a wholly untracked directory into one `dir/` entry, which would leave every
/// file inside it without a marker.
pub(crate) fn status_changes(dir: &Path) -> Option<HashMap<String, FileChange>> {
    let work_tree = work_tree(dir)?;
    let prefix = status_prefix(dir, &work_tree);

    let mut z = Command::new("git");
    z.arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let changes = match run(z) {
        Some(out) => parse_status_z(&out),
        None => {
            let mut plain = Command::new("git");
            plain
                .arg("-c")
                .arg("core.quotepath=false")
                .arg("-C")
                .arg(dir)
                .args(["status", "--porcelain=v1", "--untracked-files=all"]);
            let out = run(plain)?;
            parse_status_text(&String::from_utf8_lossy(&out))
        }
    };
    Some(rebase_status(changes, &prefix))
}

/// Prefix of `root` inside `top`, or `""` when the containment cannot be
/// established (the fallback then behaves like a scan of the whole work tree).
fn status_prefix(root: &Path, top: &Path) -> String {
    match (std::fs::canonicalize(root), std::fs::canonicalize(top)) {
        (Ok(root), Ok(top)) if root.starts_with(&top) => prefix_relative_to(&root, &top),
        _ => String::new(),
    }
}

/// Keep only work-tree-relative keys under `prefix` and strip it.
fn rebase_status(
    changes: HashMap<String, FileChange>,
    prefix: &str,
) -> HashMap<String, FileChange> {
    changes
        .into_iter()
        .filter_map(|(path, change)| {
            let relative = if prefix.is_empty() {
                path.as_str()
            } else {
                path.strip_prefix(prefix)?
            };
            crate::status::relative_key(relative).map(|key| (key, change))
        })
        .collect()
}

/// Parse `git status --porcelain=v1 -z`.
///
/// Each record is `XY <path>`, except that a rename/copy is followed by one
/// extra record holding the *original* path (no ` -> ` marker in the `-z`
/// form). The new path is the key, matching what the tree displays.
fn parse_status_z(bytes: &[u8]) -> HashMap<String, FileChange> {
    let text = String::from_utf8_lossy(bytes);
    let mut records = text.split('\0');
    let mut changes = HashMap::new();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let Some((status, path)) = split_status_record(record) else {
            continue;
        };
        if status.starts_with('R') || status.starts_with('C') {
            // Consume the original path that belongs to this rename/copy.
            let _ = records.next();
        }
        if let Some(key) = crate::status::relative_key(path) {
            changes.insert(key, classify_status(status));
        }
    }
    changes
}

/// Parse the line-oriented `git status --porcelain=v1` form.
///
/// Used only when the `-z` invocation failed. A rename line is
/// `R  old -> new`; the part after the final ` -> ` is the new path. Git quotes
/// paths containing unusual characters and `core.quotepath=false` keeps UTF-8
/// verbatim; surrounding quotes are stripped, interior escapes are left as git
/// wrote them rather than guessed at.
fn parse_status_text(text: &str) -> HashMap<String, FileChange> {
    let mut changes = HashMap::new();
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let Some(status) = line.get(..2) else {
            continue;
        };
        let Some(rest) = line.get(3..) else {
            continue;
        };
        let path = if status.starts_with('R') || status.starts_with('C') {
            rest.rsplit(" -> ").next().unwrap_or(rest)
        } else {
            rest
        };
        let path = unquote_path(path);
        if let Some(key) = crate::status::relative_key(path) {
            changes.insert(key, classify_status(status));
        }
    }
    changes
}

/// Split `XY <path>` into the two status characters and the path.
fn split_status_record(record: &str) -> Option<(&str, &str)> {
    if record.len() < 3 {
        return None;
    }
    let status = record.get(..2)?;
    let rest = record.get(2..)?;
    Some((status, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// Strip the quotes git puts around an unusual path (line-oriented form only).
fn unquote_path(path: &str) -> &str {
    let path = path.trim();
    if path.len() >= 2 && path.starts_with('"') && path.ends_with('"') {
        &path[1..path.len() - 1]
    } else {
        path
    }
}

/// Fold a porcelain `XY` pair into one [`FileChange`].
///
/// Conflict detection comes before add/delete because an unmerged pair such as
/// `UU`, `AA` or `DD` is a conflict, not two ordinary changes. A rename/copy is
/// recognised on either side so `RM` (rename staged, modified after) still keys
/// the new path as a rename.
fn classify_status(status: &str) -> FileChange {
    let mut chars = status.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');
    if x == '?' || x == '!' {
        return FileChange::Untracked;
    }
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return FileChange::Conflicted;
    }
    if x == 'R' || y == 'R' || x == 'C' || y == 'C' {
        return FileChange::Renamed;
    }
    if x == 'A' || y == 'A' {
        return FileChange::Added;
    }
    if x == 'D' || y == 'D' {
        return FileChange::Deleted;
    }
    FileChange::Modified
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
fn run(cmd: Command) -> Option<Vec<u8>> {
    let (success, out) = run_exit(cmd)?;
    success.then_some(out)
}

/// [`run`] that keeps the child's exit status. `None` still means the command
/// could not run at all (spawn failure, I/O error or [`TIMEOUT`]), which callers
/// must not confuse with "ran and failed".
fn run_exit(mut cmd: Command) -> Option<(bool, Vec<u8>)> {
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
                return Some((status.success(), out));
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

        let got = match file_at(tmp.path(), "HEAD", "a.rs") {
            FileAt::Found(bytes) => bytes,
            other => panic!("a.rs at HEAD must be Found, got {other:?}"),
        };
        assert_eq!(got, body, "bytes must round-trip exactly");
        assert!(
            String::from_utf8(got.clone()).is_err(),
            "the 0xFF 0xFE pair is not valid UTF-8"
        );
        assert!(got.windows(9).any(|w| w == "한국어".as_bytes()));
    }

    #[test]
    fn file_at_is_found_at_the_old_rev_and_absent_after_deletion() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-deleted");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "old\n");
        commit(tmp.path(), "first");
        fs::remove_file(tmp.path().join("a.rs")).expect("delete a.rs");
        commit(tmp.path(), "remove a.rs");

        assert_eq!(
            file_at(tmp.path(), "HEAD", "a.rs"),
            FileAt::Absent,
            "a deleted file has no revision at HEAD, which is absence, not a failure"
        );
        assert_eq!(
            file_at(tmp.path(), "HEAD~1", "a.rs"),
            FileAt::Found(b"old\n".to_vec()),
            "the deleted file is still readable at the revision that had it"
        );
    }

    #[test]
    fn file_at_is_absent_for_a_file_added_after_the_rev() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-added-later");
        init_repo(tmp.path());
        write(tmp.path(), "old.rs", "1\n");
        commit(tmp.path(), "first");
        write(tmp.path(), "new.rs", "2\n");
        commit(tmp.path(), "add new.rs");

        assert_eq!(
            file_at(tmp.path(), "HEAD~1", "new.rs"),
            FileAt::Absent,
            "added after HEAD~1: an empty baseline, not a read failure"
        );
        assert_eq!(
            file_at(tmp.path(), "HEAD", "new.rs"),
            FileAt::Found(b"2\n".to_vec())
        );
    }

    #[test]
    fn file_at_is_unavailable_for_an_unknown_revision_or_path_outside_the_tree() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("file-at-unavailable");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "1\n");
        commit(tmp.path(), "first");

        assert_eq!(
            file_at(tmp.path(), "no-such-rev-4c1f", "a.rs"),
            FileAt::Unavailable,
            "an unknown revision says nothing about the path, so it is not Absent"
        );
        for outside in ["../outside.rs", "/etc/passwd", "..\\outside.rs", "a//b.rs"] {
            assert_eq!(
                file_at(tmp.path(), "HEAD", outside),
                FileAt::Unavailable,
                "{outside:?} must not be answered as Absent"
            );
        }
        // A path that never existed in a resolvable commit is still genuine
        // absence: the question was well formed and the tree answered it.
        assert_eq!(file_at(tmp.path(), "HEAD", "missing.rs"), FileAt::Absent);
    }

    #[test]
    fn file_at_rejects_bad_arguments_before_spawning() {
        let tmp = TempDir::new("file-at-bad-args");
        // No repository and possibly no git: the rejection happens on the
        // arguments alone, so these are `Unavailable` regardless.
        assert_eq!(file_at(tmp.path(), "-pwn", "a.rs"), FileAt::Unavailable);
        assert_eq!(file_at(tmp.path(), "", "a.rs"), FileAt::Unavailable);
        assert_eq!(file_at(tmp.path(), "HEAD", ""), FileAt::Unavailable);
        assert_eq!(file_at(tmp.path(), "HEAD", "a\0b.rs"), FileAt::Unavailable);
        assert_eq!(file_at(tmp.path(), "HE AD", "a.rs"), FileAt::Unavailable);
    }

    #[test]
    fn parse_ref_output_keeps_the_first_non_blank_line() {
        assert_eq!(parse_ref_output(b"main\n"), Some("main".to_string()));
        assert_eq!(
            parse_ref_output(b"\r\nfeature/topic\r\n"),
            Some("feature/topic".to_string())
        );
        assert_eq!(
            parse_ref_output(b"release 1.2\n"),
            Some("release 1.2".to_string()),
            "interior whitespace belongs to the ref name"
        );
        assert_eq!(parse_ref_output(b""), None);
        assert_eq!(parse_ref_output(b" \r\n\t\n"), None);
    }

    #[test]
    fn branch_reports_the_branch_then_the_short_id_when_detached() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("identity");
        init_repo(tmp.path());
        write(tmp.path(), "a.rs", "1\n");
        commit(tmp.path(), "first");
        git(tmp.path(), &["checkout", "-q", "-b", "ccn-identity"]);

        assert_eq!(branch(tmp.path()).as_deref(), Some("ccn-identity"));

        git(tmp.path(), &["checkout", "-q", "--detach", "HEAD"]);
        let detached = branch(tmp.path()).expect("detached HEAD still has an identity");
        assert_ne!(detached, "ccn-identity");
        assert!(
            detached.len() >= 4 && detached.chars().all(|c| c.is_ascii_hexdigit()),
            "a detached HEAD falls back to the short commit id, got {detached:?}"
        );
    }

    #[test]
    fn branch_is_none_outside_a_repository() {
        let tmp = TempDir::new("identity-none");
        assert!(branch(tmp.path()).is_none());
        assert!(branch(&tmp.path().join("missing")).is_none());
    }

    #[test]
    fn parses_porcelain_z_with_rename_new_paths_and_conflicts() {
        let out = b" M src/a.rs\0?? new.c\0R  src/new.rs\0src/old.rs\0UU conflict.c\0A  added.rs\0D  gone.rs\0C  copy.rs\0source.rs\0";
        let changes = parse_status_z(out);

        assert_eq!(changes["src/a.rs"], FileChange::Modified);
        assert_eq!(changes["new.c"], FileChange::Untracked);
        assert_eq!(changes["src/new.rs"], FileChange::Renamed);
        assert!(
            !changes.contains_key("src/old.rs"),
            "the old rename path is consumed, not reported as a second entry"
        );
        assert_eq!(changes["conflict.c"], FileChange::Conflicted);
        assert_eq!(changes["added.rs"], FileChange::Added);
        assert_eq!(changes["gone.rs"], FileChange::Deleted);
        assert_eq!(changes["copy.rs"], FileChange::Renamed);
    }

    #[test]
    fn classifies_every_unmerged_porcelain_pair_as_a_conflict() {
        for status in ["UU", "AA", "DD", "AU", "UA", "DU", "UD"] {
            assert_eq!(
                classify_status(status),
                FileChange::Conflicted,
                "{status} is unmerged"
            );
        }
        assert_eq!(classify_status("RM"), FileChange::Renamed);
        assert_eq!(classify_status("AM"), FileChange::Added);
        assert_eq!(classify_status(" M"), FileChange::Modified);
        assert_eq!(classify_status("T "), FileChange::Modified);
        assert_eq!(classify_status("??"), FileChange::Untracked);
    }

    #[test]
    fn parses_line_oriented_porcelain_as_a_fallback() {
        let stdout = " M dir/a.rs\nR  old.rs -> dir/new.rs\n?? x y.c\nD  gone.rs\n";
        let changes = parse_status_text(stdout);

        assert_eq!(changes["dir/a.rs"], FileChange::Modified);
        assert_eq!(
            changes["dir/new.rs"],
            FileChange::Renamed,
            "the new path after the final -> is the key"
        );
        assert!(!changes.contains_key("old.rs"));
        assert_eq!(
            changes["x y.c"],
            FileChange::Untracked,
            "spaces in a path are preserved"
        );
        assert_eq!(changes["gone.rs"], FileChange::Deleted);
    }

    #[test]
    fn status_changes_rebases_onto_a_subdirectory_scan_root() {
        if !git_works() {
            return;
        }
        let tmp = TempDir::new("status-sub");
        let repo = tmp.path();
        init_repo(repo);
        write(repo, "sub/a.rs", "1\n");
        write(repo, "outside.rs", "1\n");
        commit(repo, "first");

        write(repo, "sub/a.rs", "1\n2\n");
        write(repo, "outside.rs", "1\n2\n");
        write(repo, "sub/new.rs", "new\n");

        let changes = status_changes(&repo.join("sub")).expect("status for a subdirectory");
        assert_eq!(changes["a.rs"], FileChange::Modified);
        assert_eq!(changes["new.rs"], FileChange::Untracked);
        assert!(
            !changes.contains_key("outside.rs") && !changes.contains_key("sub/a.rs"),
            "keys are scan-root relative and outside files are dropped: {changes:?}"
        );

        let clean = status_changes(repo).expect("status for the work tree");
        assert_eq!(clean["sub/a.rs"], FileChange::Modified);
        assert_eq!(clean["outside.rs"], FileChange::Modified);

        commit(repo, "second");
        assert!(status_changes(repo)
            .expect("status after commit")
            .is_empty());
    }

    #[test]
    fn status_changes_is_none_outside_a_work_tree() {
        let tmp = TempDir::new("status-none");
        assert!(status_changes(tmp.path()).is_none());
        assert!(status_changes(&tmp.path().join("missing")).is_none());
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
            file_at(tmp.path(), "HEAD", "sub/a.rs"),
            FileAt::Found(b"sub\n".to_vec())
        );
        // The path is work-tree relative, not relative to some invocation dir.
        assert_eq!(file_at(tmp.path(), "HEAD", "a.rs"), FileAt::Absent);
    }
}
