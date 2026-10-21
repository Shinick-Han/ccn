//! Perforce (Helix Core) backend.
//!
//! Contract shared with the git backend: every key returned here is relative to
//! the **scan root** and uses `/` separators, so the metrics layer joins churn
//! and LOC without knowing which VCS produced them.
//!
//! Parsing is deliberately separate from command execution: [`parse_where`],
//! [`parse_changes`], [`parse_describe`] and [`parse_files`] are pure functions
//! over canned output, so the whole module is tested without a Perforce server.
//!
//! Commands (each with a 20 s wall-clock deadline, see [`TIMEOUT`]):
//!
//! ```text
//! p4 info                                       # detect
//! p4 where <dir>                                # depot/client/local mapping
//! p4 files <depot_prefix>...@=<cl>,@now         # revisions submitted after <cl>
//! p4 opened                                     # pending local edits
//! p4 changes -m <n> -s submitted -ztag <depot_prefix>...
//! p4 describe -s -ztag <cl>                     # one per changelist, in parallel
//! p4 files <depot>@<cl>                         # revision in effect at <cl>?
//! p4 print -q <depot>@<cl>                      # raw contents as of <cl>
//! ```
//!
//! # `@<cl>` is an as-of **tree**, `@=<cl>` is not
//!
//! Perforce file specifications (see the Helix Core file-spec reference):
//!
//! - `@<cl>` selects the revision **in effect at** that changelist: the newest
//!   revision at or before it. That is what a tree baseline means — a file last
//!   edited in CL10 still exists at CL20 when CL20 only touched another file;
//! - `@=<cl>` selects only the revision **created by** that exact change. A file
//!   whose last edit was CL10 therefore looks absent at CL20, which is how a
//!   baseline reported unchanged old functions as `Added` before this fix.
//!
//! [`file_at`] uses `@<cl>` for both the existence probe and the print, so it
//! answers "the submitted depot tree as of that changelist".
//!
//! # Line counts are unavailable
//!
//! `p4 describe -s` lists the affected files but suppresses the diff, and
//! `p4 describe` without `-s` is refused for changelists the user does not own
//! unless `-f` is passed, which can reprint arbitrarily large diffs. P4
//! therefore reports **no added/deleted line counts**: every [`Churn`] emitted
//! here has `lines_changed == 0` and only `commits` is meaningful.
//!
//! Consequences, documented so the report can be honest about them:
//!
//! - hot spot ranking uses `commits` (and, where available, CCN), so a P4 scan
//!   still ranks files by how often they change — the signal that matters;
//! - the "lines changed" column and any metric derived from it (churn ratio,
//!   lines-per-commit) are zero for P4 workspaces;
//! - `p4 files`/`p4 opened` are lists of revisions, not diffs, so
//!   [`changed_since`] is likewise path-only.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::git::{Churn, UNKNOWN_AUTHOR};
use crate::vcs::FileAt;

#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// Submitted changelists seen in the window.
    pub changelists: u32,
    /// Churn per scan-root-relative path.
    pub churn: HashMap<String, Churn>,
    /// Commits per path per author.
    pub authors: HashMap<String, HashMap<String, u32>>,
}

/// Wall-clock budget for a single `p4` command. Past this the child is killed
/// and the metric is reported as unavailable rather than stalling the scan.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on captured stdout (and, separately, stderr). Past this the
/// reader keeps draining the pipe so `p4` cannot block on a full buffer but
/// stops growing the buffer.
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

/// Distinct authors kept per path; the highest-count ones survive (ties by name,
/// ascending), exactly like the git backend, so the report's `top_author` is
/// stable across runs.
const MAX_AUTHORS_PER_PATH: usize = 20;

/// Commits per path per author.
type Authors = HashMap<String, HashMap<String, u32>>;

/// True when `dir` looks like a Perforce workspace (`p4 info` succeeds).
///
/// `p4 info` is the source of truth, but it is not spawned blindly: a `.p4config`
/// in `dir` or an ancestor (or a file named by `P4CONFIG`) or a non-empty
/// `P4CLIENT` is required as a hint first. Scanning an arbitrary tree must not
/// pay a server round-trip, or a 20 s timeout, in every directory that merely
/// has `p4` on `PATH`.
pub fn detect(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    // `p4 info` succeeds even outside a workspace ("Client unknown."), so the
    // client name and its root are the signals. This also covers users who
    // configure P4CLIENT/P4PORT with `p4 set` (registry) rather than env vars.
    let Some(info) = run_p4(dir, &["info"]) else {
        return false;
    };
    let client = info
        .lines()
        .find_map(|line| line.strip_prefix("Client name: "))
        .map(str::trim)
        .unwrap_or("");
    if client.is_empty() || client.eq_ignore_ascii_case("unknown") {
        return false;
    }
    let Some(root) = info
        .lines()
        .find_map(|line| line.strip_prefix("Client root: "))
        .map(str::trim)
        .filter(|root| !root.is_empty() && !root.eq_ignore_ascii_case("unknown"))
    else {
        return false;
    };

    // Only claim a directory that is actually inside the client view.
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let root = Path::new(root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(root));
    dir.starts_with(root)
}

/// Files changed since revision `rev` (a changelist number, or `"opened"` for
/// pending local changes), as scan-root-relative paths.
///
/// A numeric `rev` (optionally prefixed with `#` or `@`) means "submitted after
/// that changelist **plus** whatever is open locally", which matches the intent
/// of "what changed since I synced at CL N". `"opened"`, `"pending"` and
/// `"default"` mean "pending local edits only".
///
/// `None` when `rev` is empty, looks like an option or contains whitespace
/// (rejected before `p4` is spawned), when `dir` is not mapped by `p4 where`, or
/// when a command fails or exceeds [`TIMEOUT`]. A successful command with no
/// files is `Some(empty)`, which is different from "could not ask".
pub fn changed_since(dir: &Path, rev: &str) -> Option<HashSet<String>> {
    let rev = parse_rev(rev)?;
    let mapping = mapping_for(dir)?;
    let mut changed = HashSet::new();

    if let Rev::Since(cl) = rev {
        // `@>N` is "revisions after changelist N". `@=N,@now` includes the
        // files as they were *at* N, which would report untouched files too.
        let spec = format!("{}...@>{cl}", depot_wildcard(&mapping.depot_prefix));
        let out = run_p4(dir, &["files", &spec])?;
        for depot in parse_files(&out) {
            if let Some(path) = mapping.relative(&depot) {
                changed.insert(path);
            }
        }
    }

    // Pending edits are open on this client regardless of the submitted window;
    // they are what the user is about to submit, so they always count.
    let out = run_p4(dir, &["opened"])?;
    for depot in parse_files(&out) {
        if let Some(path) = mapping.relative(&depot) {
            changed.insert(path);
        }
    }

    Some(changed)
}

/// True when `rev` is a changelist the server knows about.
pub fn rev_exists(dir: &Path, rev: &str) -> bool {
    let Some(Rev::Since(cl)) = parse_rev(rev) else {
        return false;
    };
    // `p4 changes @=N` needs a file spec and prints nothing for a bare
    // revision; `describe` answers "does this changelist exist" directly.
    run_p4(dir, &["describe", "-s", &cl.to_string()])
        .is_some_and(|out| out.contains(&format!("Change {cl} ")))
}

/// Raw contents of the scan-root-relative `path` as of changelist `rev`.
///
/// Only submitted changelists are addressable: `123`, `#123` and `@123` are
/// accepted, while `opened`, `pending` and `default` return
/// [`FileAt::Unavailable`] because a pending edit has no stored revision the
/// server could print. A `path` that is empty, absolute, contains a NUL, or
/// contains a `..` segment is likewise `Unavailable` before `p4` is spawned, so
/// the request can never name a file outside the mapped workspace.
///
/// Selection is the **as-of tree** `@<cl>`, not `@=<cl>` (see the module docs):
/// the file's revision in effect at that changelist, whatever change created it.
///
/// Existence is decided first, with `p4 files <depot>@<cl>`:
///
/// - a revision line -> the file exists, so `print` supplies
///   [`FileAt::Found`];
/// - "no such file(s)" (the server's answer for a well-formed spec that matches
///   nothing) -> [`FileAt::Absent`], i.e. an empty baseline;
/// - anything else — an unreadable/expired ticket, a timeout, an unmapped
///   workspace, an unparseable response — -> [`FileAt::Unavailable`].
///
/// The bytes are returned exactly as `p4 print -q` wrote them to stdout, without
/// decoding or newline translation.
pub fn file_at(dir: &Path, rev: &str, path: &str) -> FileAt {
    let Some(Rev::Since(cl)) = parse_rev(rev) else {
        // Pending local edits are not revisions on the server.
        return FileAt::Unavailable;
    };
    // Validate the path before `p4 where`: a bad argument must not cost a spawn.
    let Some(rel) = relative_file_path(path) else {
        return FileAt::Unavailable;
    };
    let Some(mapping) = mapping_for(dir) else {
        return FileAt::Unavailable;
    };
    let Some(depot) = mapping.depot_file(&rel) else {
        return FileAt::Unavailable;
    };
    let spec = as_of_spec(&depot, cl);

    match run_p4_outcome(dir, &["files", &spec]) {
        Some(outcome) => {
            let stdout = String::from_utf8_lossy(&outcome.stdout);
            let stderr = String::from_utf8_lossy(&outcome.stderr);
            match classify_probe(outcome.success, &stdout, &stderr) {
                Existence::Absent => return FileAt::Absent,
                Existence::Unavailable => return FileAt::Unavailable,
                Existence::Present => {}
            }
        }
        None => return FileAt::Unavailable,
    }

    // `print -q` suppresses the `//depot/...#rev - ...` header line, which would
    // otherwise be prepended to the captured bytes.
    match run_p4_bytes(dir, &["print", "-q", &spec]) {
        Some(bytes) => FileAt::Found(bytes),
        // The probe said the file is there, so a failed print is a read
        // failure (permission, timeout, transport), never "the file is new".
        None => FileAt::Unavailable,
    }
}

/// File spec for "the revision of `depot` in effect at changelist `cl`".
///
/// `@<cl>`, never `@=<cl>`: `@=` selects only the revision *created by* that
/// exact change, so a file last edited in an earlier changelist would look
/// absent. `@<cl>` is the as-of tree a baseline means. Kept as a function so the
/// selector is asserted directly in tests instead of being inferred from a
/// server call.
fn as_of_spec(depot: &str, cl: u64) -> String {
    format!("{depot}@{cl}")
}

/// What an existence probe (`p4 files <depot>@<cl>`) established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Existence {
    Present,
    Absent,
    Unavailable,
}

/// Classify a `p4 files <depot>@<cl>` probe from its exit status and streams.
///
/// Kept pure so the absence/failure split is tested without a server. P4 answers
/// a valid spec that matches nothing with a diagnostic on **stderr** and — for a
/// single file spec — a **zero exit status**, so the message, not the status, is
/// what identifies absence:
///
/// ```text
/// //depot/x@=2 - no such file(s).
/// //depot/x@1 - no file(s) at that changelist number.
/// no files to display
/// ```
///
/// A successful probe with no revision line, or any other failure (expired
/// ticket, unreachable server, unparseable response), proves nothing and is
/// reported as [`Existence::Unavailable`] rather than inventing an empty
/// baseline.
fn classify_probe(success: bool, stdout: &str, stderr: &str) -> Existence {
    if is_absence_diagnostic(stdout) || is_absence_diagnostic(stderr) {
        return Existence::Absent;
    }
    if !success {
        return Existence::Unavailable;
    }
    if parse_files(stdout).is_empty() {
        // Success without a revision line is not evidence of absence.
        return Existence::Unavailable;
    }
    Existence::Present
}

/// True when `text` is one of P4's "this spec matched nothing" diagnostics.
///
/// These are negative answers, not failures: the file existed neither as of that
/// changelist nor at all. Everything else on stderr (authentication, `$P4PORT`,
/// permission) is a failure the caller must report as unavailable.
fn is_absence_diagnostic(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no such file")
        || lower.contains("no file(s) at that changelist")
        || lower.contains("no files to display")
}

/// Churn and authorship for the last `max_changes` submitted changelists.
///
/// `max_changes` is clamped to at least 1: asking `p4 changes -m 0` is an error,
/// not "no history".
///
/// Returns `None` when `dir` is not a mapped workspace, the `changes` command
/// fails, or no changelist could be described. Changelists whose `describe`
/// fails are skipped individually; [`History::changelists`] counts the ones that
/// succeeded.
pub fn collect_history(dir: &Path, max_changes: u32) -> Option<History> {
    let mapping = mapping_for(dir)?;
    let max = max_changes.max(1);
    let max_arg = max.to_string();
    let depot_spec = format!("{}...", depot_wildcard(&mapping.depot_prefix));

    let out = run_p4(
        dir,
        &[
            // `-ztag` is a global p4 flag: it must precede the command, or p4
            // rejects it with "Invalid option" and the whole history is lost.
            "-ztag",
            "changes",
            "-m",
            &max_arg,
            "-s",
            "submitted",
            &depot_spec,
        ],
    )?;

    let mut changes = parse_changes(&out);
    changes.truncate(max as usize);
    if changes.is_empty() {
        return None;
    }
    // `p4 changes` prints newest first; sorting defensively keeps accumulation
    // independent of server-side ordering. Counts are commutative, so this only
    // makes runs reproducible.
    changes.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.change.cmp(&a.change)));

    // One `p4 describe` per changelist: independent, and the dominant cost on a
    // history window, so they run in parallel on rayon's pool.
    let described: Vec<(String, Vec<String>)> = changes
        .par_iter()
        .filter_map(|change| {
            let cl = change.change.to_string();
            let out = run_p4(dir, &["-ztag", "describe", "-s", &cl])?;
            let describe = parse_describe(&out);
            let user = if describe.user.is_empty() {
                change.user.clone()
            } else {
                describe.user
            };
            Some((user, describe.files))
        })
        .collect();

    if described.is_empty() {
        // Every `describe` failed (or the window was empty): no history.
        return None;
    }

    let mut churn: HashMap<String, Churn> = HashMap::new();
    let mut authors: Authors = HashMap::new();

    for (user, files) in &described {
        // A changelist can mention the same path twice (integrate + edit); the
        // change is still one commit for that path.
        let mut seen = HashSet::new();
        for depot in files {
            let Some(path) = mapping.relative(depot) else {
                continue;
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            let entry = churn.entry(path.clone()).or_default();
            entry.commits = entry.commits.saturating_add(1);
            // `describe -s` carries no line counts; see the module docs.
            entry.lines_changed = entry.lines_changed.saturating_add(0);

            let author = if user.is_empty() {
                UNKNOWN_AUTHOR
            } else {
                user
            };
            let count = authors
                .entry(path)
                .or_default()
                .entry(author.to_string())
                .or_insert(0);
            *count = count.saturating_add(1);
        }
    }

    trim_authors(&mut authors);

    Some(History {
        changelists: described.len() as u32,
        churn,
        authors,
    })
}

/// Depot→local mapping of one workspace, plus the scan root's position inside
/// that local root.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    /// Depot path of the scan root's mapped directory, without a trailing `...`
    /// and without a trailing separator (`//depot/proj`).
    depot_prefix: String,
    /// Local root `p4 where` reported, normalised. Kept for diagnostics; the
    /// per-call work only needs the derived [`Mapping::scan_prefix`].
    #[allow(dead_code)]
    local_prefix: PathBuf,
    /// Scan root relative to `local_prefix`, `/`-separated and trailing-slashed
    /// (`"src/"`), or empty when the scan root *is* the local root.
    scan_prefix: String,
}

impl Mapping {
    /// Scan-root-relative path for a depot file, or `None` when the file is
    /// outside the mapped local root (or is the root itself).
    fn relative(&self, depot_path: &str) -> Option<String> {
        let depot_path = depot_path.trim();
        if self.depot_prefix.is_empty() {
            return None;
        }
        let rest = depot_path.strip_prefix(&self.depot_prefix)?;
        // The prefix must end at a path boundary: `//depot/proj2/x` is not under
        // `//depot/proj`.
        let rest = rest.strip_prefix('/')?;
        if rest.is_empty() {
            return None;
        }
        let rel = if self.scan_prefix.is_empty() {
            rest
        } else {
            rest.strip_prefix(self.scan_prefix.as_str())?
        };
        if rel.is_empty() {
            return None;
        }
        Some(rel.replace('\\', "/"))
    }

    /// Depot path for the scan-root-relative `path`, or `None` when the path is
    /// not a plain relative path inside the workspace (see
    /// [`relative_file_path`]).
    ///
    /// This is the inverse of [`Mapping::relative`]: the scan root's own offset
    /// inside the local root is re-applied, so a workspace whose scan root is
    /// `proj/src` turns `a.rs` back into `//depot/proj/src/a.rs`.
    fn depot_file(&self, path: &str) -> Option<String> {
        if self.depot_prefix.is_empty() {
            return None;
        }
        let rel = relative_file_path(path)?;
        Some(format!("{}/{}{}", self.depot_prefix, self.scan_prefix, rel))
    }
}

/// Normalise a scan-root-relative file argument: `\` becomes `/`, `.` and empty
/// segments are dropped, and `None` is returned for an empty or absolute path, a
/// `..` segment or an embedded NUL.
///
/// A leading separator is rejected rather than stripped because silently
/// reinterpreting `/etc/passwd` as a path under the workspace would answer a
/// different question than the caller asked.
fn relative_file_path(path: &str) -> Option<String> {
    if path.contains('\0') || path.starts_with('/') || path.starts_with('\\') {
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

/// Parsed `rev` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rev {
    /// Pending local edits only.
    Opened,
    /// Submitted after this changelist, plus pending edits.
    Since(u64),
}

/// Accepts a changelist number (`123`, `#123`, `@123`) or one of the pending
/// keywords. Rejects options, whitespace and anything else without spawning.
fn parse_rev(raw: &str) -> Option<Rev> {
    if raw.is_empty() || raw.starts_with('-') {
        return None;
    }
    if raw.chars().any(char::is_whitespace) {
        return None;
    }
    match raw.to_ascii_lowercase().as_str() {
        "opened" | "pending" | "default" => return Some(Rev::Opened),
        _ => {}
    }
    let digits = raw
        .strip_prefix('#')
        .or_else(|| raw.strip_prefix('@'))
        .unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().map(Rev::Since)
}

/// Resolve `dir` through `p4 where`, the only command that knows which depot
/// path the local tree corresponds to.
/// `//depot/proj` -> `//depot/proj/`, so a caller can append `...` and get a
/// valid wildcard. `p4 where` strips the trailing ellipsis, but the depot
/// separator is not optional: `//depot/proj...` is a different (invalid) path.
fn depot_wildcard(prefix: &str) -> String {
    if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

fn mapping_for(dir: &Path) -> Option<Mapping> {
    if !dir.is_dir() {
        return None;
    }
    // The scanner canonicalises its root, which yields `\\?\C:\...` on Windows;
    // `p4 where` does not understand the verbatim prefix.
    let plain = dir.to_string_lossy();
    let dir = Path::new(plain.trim_start_matches(r"\\?\"));
    // `p4 where` needs a file spec, not a bare directory: without the wildcard
    // it fails with "must refer to client ... or a depot".
    let local = dir.to_string_lossy();
    let spec = if cfg!(windows) {
        format!("{local}\\...")
    } else {
        format!("{local}/...")
    };
    let out = run_p4(dir, &["where", &spec])?;
    parse_where(&out, dir)
}

/// Parse `p4 where <dir>` into a [`Mapping`] anchored at `scan_root`.
///
/// The normal form is one line with three whitespace-separated columns
/// (depot / client / local). Some server/client combinations print the three
/// values on separate lines, so both shapes are accepted. The local column may
/// contain spaces; everything after the client column is taken verbatim.
fn parse_where(text: &str, scan_root: &Path) -> Option<Mapping> {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).trim())
        .filter(|line| !line.is_empty())
        .collect();

    let triple = lines
        .iter()
        .find_map(|line| {
            let (depot, client, local) = split_where_line(line)?;
            depot.starts_with("//").then_some((depot, client, local))
        })
        .or_else(|| {
            let depot = lines.first()?;
            let client = lines.get(1)?;
            let local = lines.get(2)?;
            depot
                .starts_with("//")
                .then(|| (depot.to_string(), client.to_string(), local.to_string()))
        })?;

    let (depot, _client, local) = triple;
    let depot_prefix = depot
        .trim_end_matches("...")
        .trim_end_matches(['/', '\\'])
        .to_string();

    let local_root = local.trim_end_matches("...").trim_end_matches(['/', '\\']);
    if local_root.is_empty() {
        return None;
    }
    let local_prefix = PathBuf::from(local_root);

    // The scan root must be inside the local root; otherwise the depot files
    // have no scan-relative name.
    let scan_parts = relative_prefix(scan_root, &local_prefix)?;
    let scan_prefix = if scan_parts.is_empty() {
        String::new()
    } else {
        format!("{}/", scan_parts.join("/"))
    };

    Some(Mapping {
        depot_prefix,
        local_prefix: normalize(&local_prefix),
        scan_prefix,
    })
}

/// Split `depot client local` into its three columns, keeping spaces that belong
/// to the local path.
fn split_where_line(line: &str) -> Option<(String, String, String)> {
    let (depot, rest) = line.trim().split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    let (client, local) = rest.split_once(char::is_whitespace)?;
    let local = local.trim();
    if depot.is_empty() || client.is_empty() || local.is_empty() {
        return None;
    }
    Some((depot.to_string(), client.to_string(), local.to_string()))
}

/// `root`'s components relative to `base`, or `None` when `root` is not under
/// `base` (so a scan root outside the workspace is rejected).
fn relative_prefix(root: &Path, base: &Path) -> Option<Vec<String>> {
    let root = path_parts(&normalize(root));
    let base = path_parts(&normalize(base));
    if root.len() < base.len() || root[..base.len()] != base[..] {
        return None;
    }
    Some(root[base.len()..].to_vec())
}

fn path_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
            Component::RootDir => Some("/".to_string()),
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        })
        .collect()
}

/// Canonicalise when the path exists (so drive-letter case and separators agree
/// with what `p4 where` printed), and otherwise keep it lexical: tests and
/// `p4 where` output must not require the file to be on disk.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A `p4 changes -ztag` record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    change: u32,
    user: String,
    time: i64,
}

/// Parse `p4 changes -ztag` output. A new `... change` line starts a record; the
/// `user` and `time` keys that follow belong to it.
fn parse_changes(text: &str) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut current: Option<Change> = None;

    for (key, value) in ztag(text) {
        match key {
            "change" => {
                if let Some(done) = current.take() {
                    changes.push(done);
                }
                if let Ok(number) = value.parse::<u32>() {
                    current = Some(Change {
                        change: number,
                        user: String::new(),
                        time: 0,
                    });
                }
            }
            "user" => {
                if let Some(change) = current.as_mut() {
                    change.user = value.to_string();
                }
            }
            "time" => {
                if let Some(change) = current.as_mut() {
                    change.time = value.parse().unwrap_or(0);
                }
            }
            _ => {}
        }
    }
    if let Some(done) = current {
        changes.push(done);
    }
    changes
}

/// Parsed `p4 describe -s -ztag` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Describe {
    user: String,
    files: Vec<String>,
}

/// Parse `p4 describe -s -ztag`: the changelist owner plus every affected depot
/// file. File keys are `depotFile`, or `depotFile0`, `depotFile1`, … as emitted
/// by `-ztag`; a digit suffix is required beyond the bare key, so unrelated keys
/// that merely start with `depotFile` are ignored.
fn parse_describe(text: &str) -> Describe {
    let mut describe = Describe::default();
    for (key, value) in ztag(text) {
        if key == "user" {
            describe.user = value.to_string();
        } else if let Some(suffix) = key.strip_prefix("depotFile") {
            if suffix.bytes().all(|b| b.is_ascii_digit()) {
                describe.files.push(value.to_string());
            }
        }
    }
    describe
}

/// Parse the depot paths out of `p4 files` (and, because the leading field is
/// identical, `p4 opened`) output. Annotations such as `#12 - edit change
/// 12345` are dropped, and "nothing matched" diagnostics are skipped.
fn parse_files(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let lower = line.to_ascii_lowercase();
        if lower.contains("no such file") || lower.contains("no files to display") {
            continue;
        }
        if let Some(depot) = depot_path(line) {
            paths.push(depot);
        }
    }
    paths
}

/// First `//depot/...` token on a line, with its `#rev` suffix removed.
fn depot_path(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    if !token.starts_with("//") {
        return None;
    }
    let path = token.split('#').next().unwrap_or(token);
    if path.is_empty() || path == "//" {
        None
    } else {
        Some(path.to_string())
    }
}

/// Iterate `p4 -ztag` fields as `(key, value)` pairs: every line begins with
/// `... `, the key runs to the first space, and the value keeps any further
/// spaces (paths and descriptions may contain them).
fn ztag(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|raw| {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let rest = line.strip_prefix("... ")?;
        let (key, value) = rest.split_once(char::is_whitespace)?;
        Some((key, value.trim()))
    })
}

/// Keep only the [`MAX_AUTHORS_PER_PATH`] most frequent authors of each path.
///
/// Highest count first, ties broken by name ascending, so the surviving set is
/// deterministic rather than dependent on hash iteration order.
fn trim_authors(authors: &mut Authors) {
    for per_author in authors.values_mut() {
        if per_author.len() <= MAX_AUTHORS_PER_PATH {
            continue;
        }
        let mut ranked: Vec<(String, u32)> = per_author
            .iter()
            .map(|(name, count)| (name.clone(), *count))
            .collect();
        ranked.sort_by(|(an, ac), (bn, bc)| bc.cmp(ac).then_with(|| an.cmp(bn)));
        ranked.truncate(MAX_AUTHORS_PER_PATH);
        per_author.clear();
        per_author.extend(ranked);
    }
}

/// Run `p4 <args>` with `dir` as the working directory (so `P4CONFIG` discovery
/// matches the tree being scanned) and return stdout on success.
fn run_p4(dir: &Path, args: &[&str]) -> Option<String> {
    let out = run_p4_bytes(dir, args)?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// [`run_p4`] including the exit status and stderr, for callers that must tell
/// "ran and answered negatively" from "could not ask" (see [`classify_probe`]).
fn run_p4_outcome(dir: &Path, args: &[&str]) -> Option<RunOutcome> {
    let mut cmd = Command::new("p4");
    cmd.current_dir(dir).args(args);
    run_outcome(cmd)
}

/// [`run_p4`] without the UTF-8 lossy conversion, for callers that need the
/// exact bytes (`p4 print` may return binary files).
fn run_p4_bytes(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("p4");
    cmd.current_dir(dir).args(args);
    run(cmd)
}

/// One finished `p4` invocation: whether it exited zero plus both streams.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunOutcome {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Spawn `cmd`, capture stdout and stderr to EOF, and give up after [`TIMEOUT`].
///
/// `try_wait` polling (not `wait`) is what makes the deadline real: `p4` talks to
/// a server over the network, and a scan that blocks on an unreachable one is
/// worse than a scan without hotspots. Both pipes are drained on their own
/// threads so a chatty child can never block on a full buffer, and stdout is
/// capped at [`MAX_CAPTURE_BYTES`]. Stderr is captured too: diagnostics such as
/// `no such file(s)` are what separate a negative answer from a failure.
fn run(cmd: Command) -> Option<Vec<u8>> {
    let outcome = run_outcome(cmd)?;
    outcome.success.then_some(outcome.stdout)
}

/// [`run`] that keeps the exit status and stderr. `None` means the command
/// could not run at all (spawn failure, I/O error or [`TIMEOUT`]).
fn run_outcome(mut cmd: Command) -> Option<RunOutcome> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;

    let out_reader = std::thread::spawn(move || read_capped(stdout));
    let err_reader = std::thread::spawn(move || read_capped(stderr));

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = out_reader.join().ok()?;
                let stderr = err_reader.join().ok()?;
                return Some(RunOutcome {
                    success: status.success(),
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return None;
            }
        }
    }
}

/// Read `stream` to EOF, keeping at most [`MAX_CAPTURE_BYTES`] but draining the
/// rest so the writer never blocks.
fn read_capped(mut stream: impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Unique scratch directory, removed on drop.
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
                std::env::temp_dir().join(format!("ccn-p4-{tag}-{}-{nanos}", std::process::id()));
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

    #[test]
    fn where_output_maps_depot_paths_onto_the_scan_root() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping = parse_where(out, Path::new("C:\\ws\\proj")).expect("mapping");

        assert_eq!(mapping.depot_prefix, "//depot/proj");
        assert_eq!(
            mapping.relative("//depot/proj/src/a.rs").as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            mapping.relative("//depot/proj/a.rs").as_deref(),
            Some("a.rs")
        );
        assert_eq!(
            mapping.relative("//depot/proj").as_deref(),
            None,
            "the mapped root itself is not a file"
        );
    }

    #[test]
    fn a_subdirectory_scan_root_is_stripped_from_the_key() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping =
            parse_where(out, Path::new("C:\\ws\\proj\\src")).expect("mapping for a subdirectory");

        assert_eq!(
            mapping.relative("//depot/proj/src/a.rs").as_deref(),
            Some("a.rs")
        );
        assert_eq!(
            mapping.relative("//depot/proj/top.rs"),
            None,
            "a file above the scan root is dropped"
        );
    }

    #[test]
    fn parse_changes_keeps_changelist_order_and_users() {
        let out = "\
... change 123
... time 1699999999
... user alice
... client ws
... desc fix the thing
... change 122
... time 1699999000
... user bob
... change 121
... time 1699998000
... user carol
";
        let changes = parse_changes(out);

        assert_eq!(
            changes.iter().map(|c| c.change).collect::<Vec<_>>(),
            vec![123, 122, 121]
        );
        assert_eq!(
            changes.iter().map(|c| c.user.as_str()).collect::<Vec<_>>(),
            vec!["alice", "bob", "carol"]
        );
        assert_eq!(changes[0].time, 1699999999);
    }

    #[test]
    fn parse_describe_yields_the_author_and_digit_keyed_depot_files() {
        let out = "\
Change 123 by alice@ws on 2023/11/14 22:13:19

Affected files ...

... user alice
... depotFile0 //depot/proj/src/a.rs
... rev0 3
... action0 edit
... depotFile1 //depot/proj/src/b.rs
... depotFileFoo //depot/proj/not-a-file.rs
";
        let describe = parse_describe(out);

        assert_eq!(describe.user, "alice");
        assert_eq!(
            describe.files,
            vec!["//depot/proj/src/a.rs", "//depot/proj/src/b.rs"],
            "only depotFile<digits> keys are file entries"
        );
    }

    #[test]
    fn parse_files_skips_the_no_such_file_diagnostic() {
        let out = "\
//depot/proj/src/a.rs#12 - edit change 12345 (text)
//depot/proj/src/b.rs#3 - add change 12346 (text)
//depot/proj/...@=122,@now - no such file(s).

no files to display
";
        assert_eq!(
            parse_files(out),
            vec!["//depot/proj/src/a.rs", "//depot/proj/src/b.rs"]
        );
    }

    #[test]
    fn revision_arguments_are_validated_before_p4_is_spawned() {
        assert_eq!(parse_rev("123"), Some(Rev::Since(123)));
        assert_eq!(parse_rev("#123"), Some(Rev::Since(123)));
        assert_eq!(parse_rev("@123"), Some(Rev::Since(123)));
        assert_eq!(parse_rev("opened"), Some(Rev::Opened));
        assert_eq!(parse_rev("OPENED"), Some(Rev::Opened));
        assert_eq!(parse_rev("pending"), Some(Rev::Opened));
        assert_eq!(parse_rev("default"), Some(Rev::Opened));

        assert_eq!(parse_rev("-x"), None);
        assert_eq!(parse_rev("12 3"), None);
        assert_eq!(parse_rev(""), None);
        assert_eq!(parse_rev("#"), None);
        assert_eq!(parse_rev("12a"), None);
        assert_eq!(parse_rev("--stream=1"), None);
    }

    #[test]
    fn changed_since_rejects_bad_revisions_without_a_server() {
        let tmp = TempDir::new("rev");
        for bad in ["", "-x", "12 3", "#", "@", "--output=x"] {
            assert!(
                changed_since(tmp.path(), bad).is_none(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn mapping_rejects_paths_outside_the_local_root() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping = parse_where(out, Path::new("C:\\ws\\proj")).expect("mapping");

        assert_eq!(mapping.relative("//depot/other/x.rs"), None);
        assert_eq!(
            mapping.relative("//depot/proj2/x.rs"),
            None,
            "the depot prefix must end at a path boundary"
        );
    }

    #[test]
    fn detect_is_false_without_a_server_and_never_panics() {
        let tmp = TempDir::new("detect");
        // Either answer is acceptable if `p4` is installed and happens to reach
        // a server, but it must never panic.
        let _ = detect(tmp.path());
        assert!(
            !detect(&tmp.path().join("missing")),
            "a missing directory is never a workspace"
        );
    }

    #[test]
    fn a_directory_without_a_workspace_has_no_history() {
        let tmp = TempDir::new("plain");
        assert!(collect_history(tmp.path(), 10).is_none());
        assert!(collect_history(&tmp.path().join("missing"), 10).is_none());
        assert!(changed_since(&tmp.path().join("missing"), "123").is_none());
    }

    #[test]
    fn file_at_accepts_only_numeric_changelists_without_a_server() {
        let tmp = TempDir::new("file-rev");
        // `opened`/`pending`/`default` name no server-side revision, and the
        // malformed values are rejected before any `p4` process exists.
        for bad in [
            "", "opened", "OPENED", "pending", "default", "-x", "12 3", "#", "@", "12a",
        ] {
            assert_eq!(
                file_at(tmp.path(), bad, "src/a.rs"),
                FileAt::Unavailable,
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn file_at_rejects_paths_that_leave_the_workspace() {
        let tmp = TempDir::new("file-path");
        for bad in [
            "../outside.rs",
            "src/../../outside.rs",
            "..\\outside.rs",
            "src/a\0.rs",
            "",
            "/etc/passwd",
        ] {
            assert_eq!(
                file_at(tmp.path(), "123", bad),
                FileAt::Unavailable,
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn depot_file_joins_the_mapping_with_the_relative_path() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping = parse_where(out, Path::new("C:\\ws\\proj")).expect("mapping");

        assert_eq!(
            mapping.depot_file("src/a.rs").as_deref(),
            Some("//depot/proj/src/a.rs")
        );
        assert_eq!(
            mapping.depot_file("a.rs").as_deref(),
            Some("//depot/proj/a.rs")
        );
        assert_eq!(
            mapping.depot_file("src\\a.rs").as_deref(),
            Some("//depot/proj/src/a.rs"),
            "Windows separators are normalised"
        );

        assert_eq!(mapping.depot_file("../outside.rs"), None);
        assert_eq!(mapping.depot_file("src/../../outside.rs"), None);
        assert_eq!(mapping.depot_file("/abs/outside.rs"), None);
        assert_eq!(mapping.depot_file(""), None);
    }

    #[test]
    fn depot_file_reapplies_the_scan_root_offset() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping =
            parse_where(out, Path::new("C:\\ws\\proj\\src")).expect("mapping for a subdirectory");

        assert_eq!(
            mapping.depot_file("a.rs").as_deref(),
            Some("//depot/proj/src/a.rs")
        );
        // Round-trip with the depot→scan-relative direction.
        assert_eq!(
            mapping.relative("//depot/proj/src/a.rs").as_deref(),
            Some("a.rs")
        );
    }

    #[test]
    fn file_at_is_unavailable_without_a_server_and_never_panics() {
        let tmp = TempDir::new("file-at");
        // With no workspace (and possibly no `p4` on PATH) the lookup fails; the
        // only hard requirement is that it does not panic or hang.
        assert_eq!(file_at(tmp.path(), "123", "src/a.rs"), FileAt::Unavailable);
        assert_eq!(
            file_at(&tmp.path().join("missing"), "123", "src/a.rs"),
            FileAt::Unavailable
        );
    }

    #[test]
    fn the_baseline_spec_is_as_of_the_changelist_not_at_it() {
        // `@<cl>` = the revision in effect at that changelist. `@=<cl>` would
        // only match a revision created by that exact change, so an unchanged
        // file last edited earlier would look absent.
        let spec = as_of_spec("//depot/proj/src/a.rs", 120);
        assert_eq!(spec, "//depot/proj/src/a.rs@120");
        assert!(
            !spec.contains("@="),
            "the exact-revision selector is not an as-of tree"
        );
    }

    #[test]
    fn probe_classification_separates_absence_from_failure() {
        // A well-formed spec that matched nothing: absent, not an error. These
        // strings are the exact stderr text of a live p4d 2024.2.
        assert_eq!(
            classify_probe(false, "", "//depot/proj/src/a.rs@120 - no such file(s).\n"),
            Existence::Absent
        );
        assert_eq!(
            classify_probe(
                // A single-file spec exits 0 even when nothing matched, so the
                // message must carry the answer.
                true,
                "",
                "//depot/proj/src/b.rs@119 - no file(s) at that changelist number.\n"
            ),
            Existence::Absent
        );
        assert_eq!(
            classify_probe(false, "no files to display\n", ""),
            Existence::Absent
        );
        // A real answer: the file exists as of that changelist.
        assert_eq!(
            classify_probe(
                true,
                "//depot/proj/src/a.rs#1 - add change 120 (text)\n",
                ""
            ),
            Existence::Present
        );
        // A deleted file still has a revision line as of the deletion; the
        // print then yields empty content, which is a valid empty baseline.
        assert_eq!(
            classify_probe(
                true,
                "//depot/proj/src/b.rs#2 - delete change 124 (text)\n",
                ""
            ),
            Existence::Present
        );
        // Everything else could not be determined and must never be "absent".
        assert_eq!(
            classify_probe(
                false,
                "",
                "Perforce password (P4PASSWD) invalid or unset.\n"
            ),
            Existence::Unavailable
        );
        assert_eq!(
            classify_probe(false, "", "Connect to server failed; check $P4PORT.\n"),
            Existence::Unavailable
        );
        assert_eq!(
            classify_probe(false, "", "//depot/proj/src/a.rs - access denied.\n"),
            Existence::Unavailable,
            "a permission error is not a missing file"
        );
        assert_eq!(
            classify_probe(true, "", ""),
            Existence::Unavailable,
            "success without a revision line proves nothing"
        );
    }
}
