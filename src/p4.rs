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
//! ```
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
    if !dir.is_dir() || !has_workspace_hint(dir) {
        return false;
    }
    run_p4(dir, &["info"]).is_some()
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
        let spec = format!("{}...@={cl},@now", mapping.depot_prefix);
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
    let depot_spec = format!("{}...", mapping.depot_prefix);

    let out = run_p4(
        dir,
        &[
            "changes",
            "-m",
            &max_arg,
            "-s",
            "submitted",
            "-ztag",
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
            let out = run_p4(dir, &["describe", "-s", "-ztag", &cl])?;
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
fn mapping_for(dir: &Path) -> Option<Mapping> {
    if !dir.is_dir() {
        return None;
    }
    let local = dir.to_string_lossy().into_owned();
    let out = run_p4(dir, &["where", &local])?;
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

/// Is there a reason to believe `dir` belongs to a Perforce workspace?
///
/// Only a cheap filesystem/env check — the authoritative answer still comes from
/// `p4 info` in [`detect`].
fn has_workspace_hint(dir: &Path) -> bool {
    if std::env::var_os("P4CLIENT").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    let config_name = std::env::var("P4CONFIG")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| ".p4config".to_string());
    dir.ancestors()
        .any(|ancestor| ancestor.join(&config_name).is_file())
}

/// Run `p4 <args>` with `dir` as the working directory (so `P4CONFIG` discovery
/// matches the tree being scanned) and return stdout on success.
fn run_p4(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("p4");
    cmd.current_dir(dir).args(args);
    let out = run(cmd)?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Spawn `cmd`, capture stdout and stderr to EOF, and give up after [`TIMEOUT`].
///
/// `try_wait` polling (not `wait`) is what makes the deadline real: `p4` talks to
/// a server over the network, and a scan that blocks on an unreachable one is
/// worse than a scan without hotspots. Both pipes are drained on their own
/// threads so a chatty child can never block on a full buffer, and stdout is
/// capped at [`MAX_CAPTURE_BYTES`]. Stderr is captured (and discarded) for the
/// same reason; parsing only ever looks at stdout.
fn run(mut cmd: Command) -> Option<Vec<u8>> {
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
                let out = out_reader.join().ok()?;
                err_reader.join().ok()?;
                return status.success().then_some(out);
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
}
