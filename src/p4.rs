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
//! p4 client -o <client>                         # the client view lines
//! p4 where <dir>                                # fallback: one depot/client/local triple
//! p4 files <view-spec>@>N                       # revisions submitted after CL N
//! p4 opened                                     # pending local edits
//! p4 changes -m <n> -s submitted -ztag <view-spec>
//! p4 describe -s -ztag <cl>                     # one per changelist, in parallel
//! p4 files <depot>@<cl>                         # revision in effect at <cl>?
//! p4 print -q <depot>@<cl>                      # raw contents as of <cl>
//! ```
//!
//! # The client view, not one `p4 where` triple
//!
//! An EDA workspace rarely maps one depot directory onto one local directory.
//! Its client view has several lines, remapped directories, exclusions and
//! overlay lines, e.g.
//!
//! ```text
//! View:
//!     //depot/prj/main/... //ws/main/...
//!     //depot/prj/lib/...  //ws/third_party/lib/...
//!     -//depot/prj/main/secret/...
//!     +//depot/prj/hotfix/... //ws/main/...
//! ```
//!
//! [`mapping_for`] therefore reads the client spec and builds one [`Mapping`]
//! of the view lines anchored at the scan root; `p4 where` is only the
//! fallback when the spec cannot be read, and that fallback is marked
//! incomplete. See [`Mapping`] for the ordered matching precedence, and
//! [`FileAt::Unmapped`] / [`FileAt::Unknown`] for a path that the view excludes
//! versus a path that an incomplete mapping may still cover.
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::git::{Churn, UNKNOWN_AUTHOR};
use crate::status::FileChange;
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

/// Identity fields available without slowing the scan down.
///
/// Returns `(client, changelist summary)`. The client name comes from the
/// already-cheap `p4 info`; one further bounded command (`p4 opened`) supplies
/// the open changelist summary. No changelist is invented when `p4 opened`
/// cannot answer or lists only files with no changelist.
///
/// Every failure path is an empty side, never an error: a missing `p4`, a
/// directory outside a workspace, an expired ticket or the 20 s [`TIMEOUT`]
/// simply leaves the status line without that field.
pub(crate) fn identity(dir: &Path) -> (Option<String>, Option<String>) {
    if !dir.is_dir() {
        return (None, None);
    }
    let Some(info) = run_p4(dir, &["info"]) else {
        return (None, None);
    };
    let Some(client) = parse_client_name(&info) else {
        // `p4 info` can succeed with "Client unknown."; without a client there
        // is no workspace identity to show.
        return (None, None);
    };
    let changelist = run_p4(dir, &["opened"])
        .as_deref()
        .and_then(parse_open_changelists);
    (Some(client), changelist)
}

/// Extract `Client name:` from `p4 info` output.
///
/// Empty and `unknown` values are rejected so a machine without a workspace
/// does not render `p4 unknown`. Matching is case-insensitive on the word only;
/// a real client name is preserved verbatim.
pub(crate) fn parse_client_name(info: &str) -> Option<String> {
    let value = info.lines().find_map(|raw| {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        line.strip_prefix("Client name: ")
    })?;
    let client = value.trim();
    if client.is_empty() || client.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(client.to_string())
    }
}

/// Summarise the changelists in `p4 opened` output.
///
/// A line looks like `//depot/a.rs#3 - edit change 12345 (text)` or
/// `//depot/a.rs#3 - add default change (text)`. Only the token around `change`
/// is trusted: a numeric changelist is collected (deduplicated), `default`
/// marks the default changelist, and anything else is ignored rather than
/// guessed.
///
/// The result is a short label such as `CL 12345`, `CLs 123, 456 +1 more` or
/// `default CL`, or `None` when the output has no usable changelist. At most
/// three numbered changelists are spelled out so a workspace with many open
/// files cannot grow the status line without bound.
fn parse_open_changelists(opened: &str) -> Option<String> {
    let mut changes = std::collections::BTreeSet::new();
    let mut has_default = false;

    for raw in opened.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let mut words = line.split_whitespace();
        let mut previous = None;
        while let Some(word) = words.next() {
            if word != "change" {
                previous = Some(word);
                continue;
            }
            match words.next() {
                Some("default") => has_default = true,
                Some(number) => {
                    if let Ok(cl) = number.parse::<u32>() {
                        changes.insert(cl);
                    } else if previous == Some("default") {
                        // `default change (text)`: the marker precedes the
                        // keyword and the trailing parenthesis follows it.
                        has_default = true;
                    }
                }
                None if previous == Some("default") => has_default = true,
                _ => {}
            }
            break;
        }
    }

    let mut parts = Vec::new();
    if !changes.is_empty() {
        let shown: Vec<String> = changes.iter().take(3).map(u32::to_string).collect();
        let label = if changes.len() == 1 { "CL" } else { "CLs" };
        let mut summary = format!("{label} {}", shown.join(", "));
        let hidden = changes.len().saturating_sub(shown.len());
        if hidden > 0 {
            summary.push_str(&format!(" +{hidden} more"));
        }
        parts.push(summary);
    }
    if has_default {
        parts.push("default CL".to_string());
    }

    (!parts.is_empty()).then(|| parts.join(" + "))
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
    let mapping = mapping_cached(dir)?;
    let mut changed = HashSet::new();

    if let Rev::Since(cl) = rev {
        // `@>N` is "revisions after changelist N". `@=N,@now` includes the
        // files as they were *at* N, which would report untouched files too.
        // Every include view line is queried: a remapped view can put changed
        // files under several depot roots, and querying only one would silently
        // drop the others.
        for spec in mapping.depot_specs() {
            let filespec = format!("{spec}@>{cl}");
            let out = run_p4(dir, &["files", &filespec])?;
            for depot in parse_files(&out) {
                if let Some(path) = mapping.relative(&depot) {
                    changed.insert(path);
                }
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

/// Working-tree changes for `dir`, keyed by scan-root-relative `/` paths.
///
/// Perforce has no working-tree status command, so `p4 opened -ztag` is the
/// source: it lists every file open on this client with an action. The scan
/// root's client view ([`mapping_cached`]) turns each depot path into the same
/// key the tree uses; a tagged `clientFile` is the fallback when the view cannot
/// be read, and anything that still does not map under `root` is dropped.
///
/// Best effort throughout: a missing `p4`, an expired ticket, a timeout or
/// unparseable output yields an empty map, never an error or a panic.
pub(crate) fn status_changes(dir: &Path) -> HashMap<String, FileChange> {
    if !dir.is_dir() {
        return HashMap::new();
    }
    let Some(tagged) = run_p4(dir, &["opened", "-ztag"]) else {
        return HashMap::new();
    };
    let mut opened = parse_opened_status(&tagged);
    if opened.is_empty() {
        // An old server/client may not understand `-ztag`; the plain form still
        // carries the depot path and action.
        if let Some(plain) = run_p4(dir, &["opened"]) {
            opened = parse_opened_status(&plain);
        }
    }
    if opened.is_empty() {
        return HashMap::new();
    }

    let mapping = mapping_cached(dir);
    let mut changes = HashMap::new();
    for file in opened {
        let relative = mapping
            .as_ref()
            .and_then(|mapping| mapping.relative(&file.depot))
            .or_else(|| {
                file.local
                    .as_deref()
                    .and_then(|local| relative_under_root(dir, local))
            });
        if let Some(relative) = relative {
            if let Some(key) = crate::status::relative_key(&relative) {
                changes.insert(key, action_change(&file.action));
            }
        }
    }
    changes
}

/// One `p4 opened` record, reduced to what the tree needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenedFile {
    /// Depot path without the `#rev` suffix.
    depot: String,
    /// Local path from `-ztag` (`clientFile`), when present.
    local: Option<String>,
    /// Perforce action (`edit`, `add`, `delete`, `move/add`, ...).
    action: String,
}

/// Grouped `-ztag` fields for one opened file, in index order.
#[derive(Debug, Default)]
struct OpenedFields {
    depot: Option<String>,
    local: Option<String>,
    action: Option<String>,
}

/// Parse `p4 opened` output in either `-ztag` or the plain form.
///
/// Tagged output is a stream of `... key<N> value` fields; the numeric suffix
/// groups one file, so the result is ordered by that index rather than hash
/// order. Plain output is `//depot/path#rev - action change CL (type)` per line.
fn parse_opened_status(text: &str) -> Vec<OpenedFile> {
    let mut grouped: BTreeMap<u32, OpenedFields> = BTreeMap::new();
    for (key, value) in ztag(text) {
        let Some((name, index)) = split_tag_index(key) else {
            continue;
        };
        let slot = grouped.entry(index).or_default();
        match name {
            "depotFile" => slot.depot = Some(value.to_string()),
            "clientFile" => slot.local = Some(value.to_string()),
            "action" => slot.action = Some(value.to_string()),
            _ => {}
        }
    }

    let mut files = Vec::new();
    for (_, fields) in grouped {
        if let Some(depot) = fields.depot {
            files.push(OpenedFile {
                depot,
                local: fields.local,
                action: fields.action.unwrap_or_else(|| "edit".to_string()),
            });
        }
    }
    if !files.is_empty() {
        return files;
    }

    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let Some(depot) = depot_path(line) else {
            continue;
        };
        let action = line
            .split_once(" - ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or("edit")
            .to_string();
        files.push(OpenedFile {
            depot,
            local: None,
            action,
        });
    }
    files
}

/// Split a `-ztag` key such as `depotFile12` into `("depotFile", 12)`.
fn split_tag_index(key: &str) -> Option<(&str, u32)> {
    let name = key.trim_end_matches(|c: char| c.is_ascii_digit());
    if name.len() == key.len() {
        return None;
    }
    let index = key[name.len()..].parse().ok()?;
    Some((name, index))
}

/// Map a tagged local path onto the scan root, relative and `/`-separated.
fn relative_under_root(root: &Path, local: &str) -> Option<String> {
    let root = normalize(root);
    let local = normalize(Path::new(local.trim_start_matches(r"\\?\")));
    let relative = local.strip_prefix(&root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Fold a Perforce action into the shared [`FileChange`] vocabulary.
///
/// A `move` shows up as two records (`move/add` + `move/delete`), so the new
/// side is `Added`; the old side is `Deleted`. That is the honest reading of
/// Perforce's model (it does not track renames as a pair the way git does).
fn action_change(action: &str) -> FileChange {
    match action.trim().to_ascii_lowercase().as_str() {
        "add" | "branch" | "import" | "move/add" => FileChange::Added,
        "delete" | "purge" | "move/delete" => FileChange::Deleted,
        _ => FileChange::Modified,
    }
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
/// A path that no include view line maps (or that an exclusion line removes)
/// is [`FileAt::Unmapped`] when the mapping is complete: it has no depot
/// history at all, which is neither a new file nor a read failure. When the
/// mapping is incomplete (an unsupported `*`/`%%n` line, an unparseable view
/// line, or the `p4 where` fallback used after `client -o` failed), the same
/// path is [`FileAt::Unknown`]: the view may cover it, so a baseline must not
/// pretend the comparison was complete. A workspace whose client spec and
/// `p4 where` mapping both fail is still `Unavailable`, because there the view
/// is unknown rather than merely incomplete.
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
    let Some(mapping) = mapping_cached(dir) else {
        return FileAt::Unavailable;
    };
    let Some(depot) = mapping.depot_file(&rel) else {
        // Complete view: the path genuinely has no depot history, so a baseline
        // must skip it. Incomplete view: an unsupported pattern may cover it, so
        // the comparison is incomplete rather than ordinary out-of-view.
        return unmapped_file_at(&mapping);
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

/// Outcome for a path the mapping does not resolve.
///
/// A complete mapping proves the path is out of view ([`FileAt::Unmapped`]).
/// An incomplete one (unsupported wildcard, parse failure, `p4 where` fallback)
/// means the path may still be in view, so the baseline must report the
/// comparison as incomplete ([`FileAt::Unknown`]) instead of "no history".
fn unmapped_file_at(mapping: &Mapping) -> FileAt {
    if mapping.complete() {
        FileAt::Unmapped
    } else {
        FileAt::Unknown
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
    let mapping = mapping_cached(dir)?;
    let max = max_changes.max(1);
    let max_arg = max.to_string();

    // One query per include view line: a remapped client view spans more than
    // one depot root, and a changelist that touches two of them is returned
    // twice. It is deduplicated below by changelist number, so the window stays
    // "the last `max` changelists", not "the last `max` per view line".
    let mut changes: Vec<Change> = Vec::new();
    for spec in mapping.depot_specs() {
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
                &spec,
            ],
        )?;
        changes.extend(parse_changes(&out));
    }

    let mut seen_changes = HashSet::new();
    changes.retain(|change| seen_changes.insert(change.change));
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

/// One parsed line of a client spec's `View:` block, with both patterns kept
/// as written (normalised to `/`, quotes and trailing `...` removed).
///
/// Every line keeps its client-spec **order** (the index in the parsed list):
/// Perforce evaluates overlapping mappings in order, so the order is part of
/// the semantics, not a parsing detail. A line with an unsupported pattern is
/// not returned at all, and its presence makes the whole mapping incomplete
/// (see [`ParsedView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// Plain include (`//depot/a/... //client/a/...`).
    Include,
    /// Overlay include (`+//depot/a/... //client/a/...`).
    Overlay,
    /// Exclusion (`-//depot/a/...`), optionally with a client side.
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewLine {
    kind: LineKind,
    /// Depot side (without the trailing `...`). Always present.
    depot: String,
    /// Directory subtree (`//a/b/...`) vs single file (`//a/b.c`).
    is_dir: bool,
    /// Client side; `None` only for an exclusion without one.
    client: Option<String>,
}

/// One include view line, anchored at the scan root.
///
/// The `client` pattern names a path below the client root, so a remapped line
/// (`//depot/prj/lib/... //ws/third_party/lib/...`) is modelled exactly: the
/// depot prefix and the local base are the two ends of the same line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IncludeLine {
    /// Position in the client spec; later lines override earlier ones.
    order: usize,
    /// Depot directory prefix without a trailing `...` or separator
    /// (`//depot/prj/main`), or the whole depot path for a single-file line.
    depot: String,
    /// `false` for a single-file line (`//depot/a.c //client/a.c`), which
    /// matches only that exact path instead of a subtree.
    is_dir: bool,
    /// A `+` (overlay) line: it participates in include matching with priority
    /// over a plain include for the same client path. See [`Mapping`].
    overlay: bool,
    /// Scan root relative to the line's local base, `/`-separated with a
    /// trailing `/` (`"src/"`), or empty when they coincide.
    scan_prefix: String,
    /// The line's local base relative to the scan root, `/`-separated with a
    /// trailing `/` (`"main/"`), or empty. Non-empty only when `scan_prefix` is
    /// empty: the scan root is then *above* this line's local base (the normal
    /// case when scanning the client root of a remapped view).
    base_prefix: String,
    /// Scan-root-relative path of the single file a file view line maps.
    single_file: Option<String>,
}

impl IncludeLine {
    /// Depot path for the scan-root-relative `rel`, if this line maps it.
    fn depot_for(&self, rel: &str) -> Option<String> {
        if let Some(name) = &self.single_file {
            return (name == rel).then(|| self.depot.clone());
        }
        let rest = if self.base_prefix.is_empty() {
            // The scan root is inside (or is) this line's local base, so the key
            // is already relative to the base once `scan_prefix` is re-applied.
            rel
        } else {
            rel.strip_prefix(self.base_prefix.as_str())?
        };
        if rest.is_empty() {
            return None;
        }
        Some(format!("{}/{}{}", self.depot, self.scan_prefix, rest))
    }

    /// Scan-root-relative path for `depot_path`, if this line maps it.
    fn scan_relative(&self, depot_path: &str) -> Option<String> {
        if let Some(name) = &self.single_file {
            return (depot_path == self.depot).then(|| name.clone());
        }
        let rest = depot_path.strip_prefix(&self.depot)?;
        // The prefix must end at a path boundary: `//depot/proj2/x` is not under
        // `//depot/proj`.
        let rest = rest.strip_prefix('/')?;
        if rest.is_empty() {
            return None;
        }
        if !self.base_prefix.is_empty() {
            Some(format!("{}{}", self.base_prefix, rest))
        } else if self.scan_prefix.is_empty() {
            Some(rest.to_string())
        } else {
            rest.strip_prefix(self.scan_prefix.as_str())
                .map(str::to_string)
        }
    }
}

/// One exclusion (`-`) line, kept in client-spec order so that a later include
/// can re-add what an earlier exclusion removed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Exclusion {
    /// Position in the client spec.
    order: usize,
    /// Excluded depot path (without a trailing `...`).
    depot: String,
    /// A `...` exclusion covers the subtree; otherwise only the exact file.
    is_dir: bool,
}

impl Exclusion {
    fn matches(&self, depot_path: &str) -> bool {
        if !self.is_dir {
            return depot_path == self.depot;
        }
        depot_path
            .strip_prefix(self.depot.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
    }
}

/// Depot→local mapping of one workspace, plus the scan root's position inside
/// that local root.
///
/// A mapping is the client **view** (not a single `p4 where` triple): every
/// view line is kept **in client-spec order**, so remapped directories,
/// exclusions and overlays are modelled with Perforce's real precedence:
///
/// 1. **Ordered overrides.** Later lines override earlier overlapping lines.
///    An exclusion removes a mapping until a later include/overlay re-adds it;
///    a later broad line therefore wins over an earlier specific one (and the
///    later line's depot/client pair is the one used). An exclusion wins only
///    against includes that precede it.
/// 2. **Overlay preference.** When a client path is covered by both a `+`
///    overlay and a plain include, the overlay is preferred (Perforce's `+`
///    lines overlay a depot tree on top of another). Order still decides
///    between two plain includes and gives an exclusion after the last include
///    its effect.
///
/// Only subtree (`...`) and exact-file patterns are modelled. A `*`/`%%n`
/// wildcard line (or any line that cannot be parsed) is not interpreted: it is
/// dropped **and** leaves [`Mapping::complete`] false, so a file that only that
/// line could cover is reported as [`FileAt::Unknown`] rather than silently
/// dropped as out of view.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    /// Include view lines, in client-spec order (carrying their original
    /// position).
    includes: Vec<IncludeLine>,
    /// Exclusions, in client-spec order.
    exclusions: Vec<Exclusion>,
    /// False when the view is only partially understood: an unsupported
    /// wildcard, an unparseable view line, or the single `p4 where` fallback.
    complete: bool,
}

impl Mapping {
    /// True when every view line was understood (so "not mapped" really means
    /// "out of view").
    fn complete(&self) -> bool {
        self.complete
    }

    /// Scan-root-relative path for a depot file, or `None` when no include line
    /// maps it, or when the last applicable line is an exclusion.
    fn relative(&self, depot_path: &str) -> Option<String> {
        let depot_path = depot_path.trim();
        if depot_path.is_empty() {
            return None;
        }
        let (order, rel) = self
            .includes
            .iter()
            .filter_map(|line| line.scan_relative(depot_path).map(|rel| (line.order, rel)))
            .max_by_key(|(order, _)| *order)?;
        // Ordered semantics: an exclusion only removes mappings that precede
        // it. A later include/overlay re-adds the path.
        if self.excluded_after(depot_path, order) {
            return None;
        }
        Some(rel.replace('\\', "/"))
    }

    fn excluded_after(&self, depot_path: &str, include_order: usize) -> bool {
        self.exclusions
            .iter()
            .any(|exclusion| exclusion.order > include_order && exclusion.matches(depot_path))
    }

    /// Depot path for the scan-root-relative `path`, or `None` when the path is
    /// not a plain relative path (see [`relative_file_path`]), no include line
    /// maps it, or the winning line is overridden by a later exclusion.
    ///
    /// Candidates are tried with overlays first, then the later client-spec
    /// line, and each is confirmed through [`Mapping::relative`] so an exclusion
    /// that follows that line still removes it (and a later re-inclusion can
    /// win).
    fn depot_file(&self, path: &str) -> Option<String> {
        let rel = relative_file_path(path)?;
        let mut candidates: Vec<&IncludeLine> = self
            .includes
            .iter()
            .filter(|line| line.depot_for(&rel).is_some())
            .collect();
        candidates.sort_by_key(|line| (line.overlay, line.order));
        for line in candidates.into_iter().rev() {
            let depot = line.depot_for(&rel)?;
            if self.relative(&depot).as_deref() == Some(rel.as_str()) {
                return Some(depot);
            }
        }
        None
    }

    /// Every include depot prefix, deduplicated and sorted.
    fn depot_prefixes(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut prefixes: Vec<String> = self
            .includes
            .iter()
            .filter(|line| seen.insert(line.depot.clone()))
            .map(|line| line.depot.clone())
            .collect();
        prefixes.sort();
        prefixes
    }

    /// Every include view line as a `p4 files`/`p4 changes` file spec: a
    /// directory subtree as `//depot/x/...`, a single file as its exact path.
    ///
    /// Deduplicated and sorted, like [`Mapping::depot_prefixes`]. A remapped
    /// view needs all of them: querying only the first would drop an entire
    /// depot root from history and from the changed-file list.
    fn depot_specs(&self) -> Vec<String> {
        self.depot_prefixes()
            .into_iter()
            .map(|depot| {
                let is_dir = self
                    .includes
                    .iter()
                    .any(|line| line.depot == depot && line.is_dir);
                if is_dir {
                    format!("{}...", depot_wildcard(&depot))
                } else {
                    depot
                }
            })
            .collect()
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

/// Resolve `dir` from the client spec, falling back to `p4 where`.
///
/// The client spec is the only place that knows the whole view: multiple
/// include lines, remapped directories, exclusions and overlays. `p4 where`
/// answers for one path only, so it is kept as the fallback for a client spec
/// that cannot be read (an old server, a denied `client -o`, an unparseable
/// spec) and as the shape its tests already pin.
/// Resolve the client view once per process for a given workspace and
/// Perforce environment.
///
/// `file_at` is called once per file in a baseline comparison. Without this,
/// every file pays `p4 info` plus `p4 client -o`; at 20k files that is 40k
/// process spawns before the actual baseline read. The key includes the
/// environment that selects the server/client, and the cache lives for the
/// process (one CLI invocation, or one test binary).
fn mapping_cached(dir: &Path) -> Option<Arc<Mapping>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Mapping>>>>> = OnceLock::new();

    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let key = format!(
        "{}|{}|{}|{}|{}",
        canonical.to_string_lossy(),
        env("P4PORT"),
        env("P4CLIENT"),
        env("P4USER"),
        env("P4CONFIG")
    );

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hit) = guard.get(&key) {
        return hit.clone();
    }
    // Hold the lock across the two `p4` calls: the first thread resolves the
    // view while the others wait, instead of each spawning its own pair.
    let value = mapping_for(dir).map(Arc::new);
    guard.insert(key, value.clone());
    value
}

fn mapping_for(dir: &Path) -> Option<Mapping> {
    if !dir.is_dir() {
        return None;
    }
    // The scanner canonicalises its root, which yields `\\?\C:\...` on Windows;
    // p4 does not understand the verbatim prefix.
    let plain = dir.to_string_lossy();
    let dir = Path::new(plain.trim_start_matches(r"\\?\"));
    mapping_from_client_spec(dir).or_else(|| mapping_from_where(dir))
}

/// Build the mapping from `p4 client -o <client>`, or `None` when the spec
/// cannot be read or contains no usable view line.
///
/// `p4 info` supplies the client name and root; `p4 client -o` only prints the
/// spec (no editor, no mutation), so this costs one extra round-trip per scan
/// and never blocks on input.
fn mapping_from_client_spec(dir: &Path) -> Option<Mapping> {
    let info = run_p4(dir, &["info"])?;
    let client = info
        .lines()
        .find_map(|line| line.strip_prefix("Client name: "))
        .map(str::trim)
        .unwrap_or("");
    if client.is_empty() || client.eq_ignore_ascii_case("unknown") {
        return None;
    }
    let root = info
        .lines()
        .find_map(|line| line.strip_prefix("Client root: "))
        .map(str::trim)
        .filter(|root| !root.is_empty() && !root.eq_ignore_ascii_case("unknown"))?;

    let spec = run_p4(dir, &["client", "-o", client])?;
    let view = parse_client_view(&spec);
    if view.lines.is_empty() {
        if view.complete {
            // No view block at all: fall back to `p4 where`, which is
            // necessarily incomplete (it cannot see the rest of the view).
            return None;
        }
        // The view had lines, but none could be understood (e.g. only `*`
        // patterns). Keep an explicitly incomplete, empty mapping so its files
        // are reported as Unknown instead of being hidden by the fallback.
        return Some(Mapping {
            includes: Vec::new(),
            exclusions: Vec::new(),
            complete: false,
        });
    }
    Some(mapping_from_view(
        &view.lines,
        Path::new(root),
        dir,
        view.complete,
    ))
}

/// Result of parsing a client spec's `View:` block.
///
/// `complete` is false when any indented view line could not be understood
/// (an unsupported `*`/`%%n` pattern, a malformed line, a comment the parser
/// does not model). Dropping such a line is fine; pretending the remaining
/// lines are the whole view is not, because files only that line covers would
/// look out of view.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedView {
    lines: Vec<ViewLine>,
    complete: bool,
}

/// Parse the `View:` block of `p4 client -o` output.
///
/// The block starts at the `View:` field and continues while the following
/// lines are indented (P4 writes a tab before every view line) or blank; the
/// first non-indented field ends it. A line that is not a valid view line is
/// skipped **and marks the view incomplete**, so one comment or unexpected
/// field cannot hide the rest of the view or make it look complete.
fn parse_client_view(text: &str) -> ParsedView {
    let mut lines = Vec::new();
    let mut complete = true;
    let mut in_view = false;
    for line in text
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
    {
        if !in_view {
            if line.trim_end() == "View:" {
                in_view = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        // A view line is indented; the next field (`SubmitOptions:`, ...) is not.
        if !(line.starts_with(' ') || line.starts_with('\t')) {
            break;
        }
        match parse_view_line(line) {
            Some(view_line) => lines.push(view_line),
            None => complete = false,
        }
    }
    ParsedView { lines, complete }
}

/// Parse one view line into a [`ViewLine`].
///
/// Grammar: `[+|-] <depot-pattern> [<client-pattern>]`, with either side
/// optionally wrapped in double quotes (paths with spaces). An exclusion may
/// carry a client side; `+` lines are overlay includes and always need one.
/// Separators are normalised to `/`, and a trailing `...` marks a directory
/// pattern (a pattern without it maps one file). A line whose pattern is
/// unsupported (`*`, `%%n`) or malformed is rejected, which makes the whole
/// view incomplete.
fn parse_view_line(line: &str) -> Option<ViewLine> {
    let tokens = split_view_tokens(line);
    let first = tokens.first()?;

    // Tolerate the spaced marker form (`- //depot/x`), which P4 itself writes
    // without the space but which is otherwise unambiguous.
    let spaced = first == "-" || first == "+";
    let depot_index = usize::from(spaced);
    let depot_token = tokens.get(depot_index)?;

    let (kind, depot_token) = if first == "+" {
        (LineKind::Overlay, depot_token.as_str())
    } else if first == "-" {
        (LineKind::Exclude, depot_token.as_str())
    } else if let Some(path) = first.strip_prefix('+') {
        (LineKind::Overlay, path)
    } else if let Some(path) = first.strip_prefix('-') {
        (LineKind::Exclude, path)
    } else {
        (LineKind::Include, first.as_str())
    };

    let depot = normalize_pattern(depot_token)?;
    let is_dir = is_directory_pattern(depot_token);

    // An exclusion's client side is optional; an include/overlay needs one.
    let client = match tokens.get(depot_index + 1) {
        Some(token) => Some(normalize_pattern(token)?),
        None => None,
    };
    if kind != LineKind::Exclude && client.is_none() {
        return None;
    }

    Some(ViewLine {
        kind,
        depot,
        is_dir,
        client,
    })
}

/// Split a view line into tokens, treating a double-quoted span as one token
/// (P4 quotes paths that contain spaces) and dropping the quotes.
fn split_view_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in line.trim().chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

/// Normalise one side of a view line: `\` becomes `/`, a trailing `...` and
/// trailing separators are dropped (the caller tracks "directory" separately).
///
/// The result must be a depot/client path (`//a/b`); anything else is not a
/// view path and the line is rejected. Only a trailing `...` subtree is
/// modelled: `*`/`%%n` wildcards cannot be reduced to a prefix, so a line that
/// uses them is rejected — [`parse_client_view`] then marks the mapping
/// incomplete instead of pretending the view was fully understood.
fn normalize_pattern(raw: &str) -> Option<String> {
    let path = raw.replace('\\', "/");
    let path = path.trim_end_matches("...");
    let path = path.trim_end_matches('/');
    if !path.starts_with("//") || path.len() <= 2 {
        return None;
    }
    if path.contains('*') || path.contains('%') {
        return None;
    }
    Some(path.to_string())
}

/// True when a view pattern maps a directory subtree (`...` or a trailing `/`).
fn is_directory_pattern(raw: &str) -> bool {
    let path = raw.replace('\\', "/");
    path.ends_with("...") || path.ends_with('/')
}

/// Build a [`Mapping`] from parsed view lines, the client root and the scan
/// root.
///
/// Each line keeps its index in `view` as its client-spec order, so ordered
/// override/re-inclusion semantics survive anchoring. Each include line
/// contributes a local base (`client_root` + the client pattern below
/// `//<client>/`) and the scan-relative prefix of that base. A line whose local
/// base is neither above nor below the scan root is dropped: it cannot name
/// anything under the scan root in either direction (the line is still
/// understood, so it does not make the mapping incomplete).
fn mapping_from_view(
    view: &[ViewLine],
    client_root: &Path,
    scan_root: &Path,
    complete: bool,
) -> Mapping {
    let mut includes = Vec::new();
    let mut exclusions = Vec::new();

    for (order, line) in view.iter().enumerate() {
        if line.kind == LineKind::Exclude {
            exclusions.push(Exclusion {
                order,
                depot: line.depot.clone(),
                is_dir: line.is_dir,
            });
            continue;
        }
        let Some(client_pattern) = &line.client else {
            continue;
        };
        let Some(local_base) = client_local_base(client_root, client_pattern) else {
            continue;
        };

        if line.is_dir {
            let Some((scan_prefix, base_prefix)) = anchor(scan_root, &local_base) else {
                continue;
            };
            includes.push(IncludeLine {
                order,
                depot: line.depot.clone(),
                is_dir: true,
                overlay: line.kind == LineKind::Overlay,
                scan_prefix,
                base_prefix,
                single_file: None,
            });
        } else {
            // A single-file line maps that one file, not a subtree: its name is
            // the path of the local file relative to the scan root.
            let Some(parts) = relative_prefix(&local_base, scan_root) else {
                continue;
            };
            if parts.is_empty() {
                continue; // the "file" is the scan root itself
            }
            let name = parts.join("/");
            includes.push(IncludeLine {
                order,
                depot: line.depot.clone(),
                is_dir: false,
                overlay: line.kind == LineKind::Overlay,
                scan_prefix: String::new(),
                base_prefix: String::new(),
                single_file: Some(name),
            });
        }
    }

    Mapping {
        includes,
        exclusions,
        complete,
    }
}

/// Local path of a client-side view pattern: the client root plus everything
/// below the pattern's leading `//<client>/` component.
///
/// P4 requires that component to be the client name; it is not re-checked
/// because the remainder under the client root is what defines the local base,
/// and a mismatch would still produce the path P4 itself would use.
fn client_local_base(client_root: &Path, pattern: &str) -> Option<PathBuf> {
    let rest = pattern.strip_prefix("//")?;
    let (_client, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let mut base = client_root.to_path_buf();
    for part in tail.split('/') {
        if !part.is_empty() && part != "." {
            base.push(part);
        }
    }
    Some(base)
}

/// The scan root's position relative to a local base, as
/// `(scan_prefix, base_prefix)`.
///
/// - scan root inside the base: `("src/", "")` — the key is below the base, so
///   the depot path re-applies `src/`;
/// - base inside the scan root: `("", "main/")` — only keys below `main/` are
///   mapped, and the depot path drops that prefix;
/// - equal: `("", "")`;
/// - unrelated (different drive, sibling trees): `None`, so the line is not
///   visible from this scan root.
fn anchor(scan_root: &Path, local_base: &Path) -> Option<(String, String)> {
    if let Some(parts) = relative_prefix(scan_root, local_base) {
        let prefix = if parts.is_empty() {
            String::new()
        } else {
            format!("{}/", parts.join("/"))
        };
        return Some((prefix, String::new()));
    }
    let parts = relative_prefix(local_base, scan_root)?;
    if parts.is_empty() {
        return Some((String::new(), String::new()));
    }
    Some((String::new(), format!("{}/", parts.join("/"))))
}

/// Fallback: resolve `dir` through `p4 where`, when the client spec could not
/// be read.
fn mapping_from_where(dir: &Path) -> Option<Mapping> {
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

/// Parse `p4 where <dir>` into a one-line [`Mapping`] anchored at `scan_root`.
///
/// The normal form is one line with three whitespace-separated columns
/// (depot / client / local). Some server/client combinations print the three
/// values on separate lines, so both shapes are accepted. The local column may
/// contain spaces; everything after the client column is taken verbatim.
///
/// This is the fallback path: it cannot see exclusions or other view lines, so
/// the resulting mapping is a single include covering that one triple and is
/// always [`Mapping::complete`] `false` (the full view was never established).
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
    if depot_prefix.is_empty() {
        return None;
    }

    let local_root = local.trim_end_matches("...").trim_end_matches(['/', '\\']);
    if local_root.is_empty() {
        return None;
    }
    let local_prefix = PathBuf::from(local_root);

    // The scan root must be inside the local root; otherwise the depot files
    // have no scan-relative name.
    let (scan_prefix, base_prefix) = anchor(scan_root, &local_prefix)?;

    Some(Mapping {
        includes: vec![IncludeLine {
            order: 0,
            depot: depot_prefix,
            is_dir: true,
            overlay: false,
            scan_prefix,
            base_prefix,
            single_file: None,
        }],
        exclusions: Vec::new(),
        complete: false,
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
    fn parse_client_name_rejects_unknown_and_keeps_the_real_name() {
        let info = "User name: alice\r\nClient name: ws-main\r\nClient root: C:\\ws\r\n";
        assert_eq!(parse_client_name(info).as_deref(), Some("ws-main"));

        assert_eq!(parse_client_name("Client name: unknown\n"), None);
        assert_eq!(parse_client_name("Client name: \n"), None);
        assert_eq!(parse_client_name("User name: alice\n"), None);
    }

    #[test]
    fn parse_open_changelists_deduplicates_and_summarises() {
        let opened = "\
//depot/proj/a.rs#3 - edit change 12345 (text)
//depot/proj/b.rs#1 - edit change 12345 (text)
//depot/proj/c.rs#2 - add change 12344 (text)
";
        assert_eq!(
            parse_open_changelists(opened).as_deref(),
            Some("CLs 12344, 12345")
        );

        let default = "\
//depot/proj/a.rs#3 - edit default change (text)
//depot/proj/b.rs#1 - add change 12345 (text)
";
        assert_eq!(
            parse_open_changelists(default).as_deref(),
            Some("CL 12345 + default CL")
        );
        assert_eq!(
            parse_open_changelists("//depot/proj/a.rs#3 - edit default change (text)\n").as_deref(),
            Some("default CL")
        );
        assert_eq!(
            parse_open_changelists("File(s) not opened on this client.\n"),
            None
        );
        assert_eq!(
            parse_open_changelists("//depot/proj/a.rs#3 - edit change not-a-number (text)\n"),
            None,
            "an unrecognised changelist is not guessed"
        );
    }

    #[test]
    fn parse_open_changelists_bounds_a_long_list() {
        let mut opened = String::new();
        for cl in 101..=105 {
            opened.push_str(&format!("//depot/a.rs#1 - edit change {cl} (text)\n"));
        }
        assert_eq!(
            parse_open_changelists(&opened).as_deref(),
            Some("CLs 101, 102, 103 +2 more")
        );
    }

    #[test]
    fn identity_does_not_spawn_for_a_missing_directory() {
        let missing = std::env::temp_dir().join("ccn-p4-identity-does-not-exist");
        assert_eq!(identity(&missing), (None, None));
    }

    #[test]
    fn where_output_maps_depot_paths_onto_the_scan_root() {
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping = parse_where(out, Path::new("C:\\ws\\proj")).expect("mapping");

        assert_eq!(mapping.depot_prefixes(), vec!["//depot/proj".to_string()]);
        assert_eq!(mapping.depot_specs(), vec!["//depot/proj/...".to_string()]);
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
    #[cfg(windows)]
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
    #[cfg(unix)]
    fn a_subdirectory_scan_root_is_stripped_from_the_key() {
        let out = "//depot/proj/... //client/proj/... /ws/proj/...\n";
        let mapping =
            parse_where(out, Path::new("/ws/proj/src")).expect("mapping for a subdirectory");

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

    /// Platform-neutral workspace path: `C:\ws\...` on Windows, `/ws/...`
    /// elsewhere, so the view tests exercise the same view text on both.
    #[cfg(windows)]
    fn ws_path(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::from(r"C:\ws");
        for part in parts {
            path.push(part);
        }
        path
    }

    #[cfg(unix)]
    fn ws_path(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::from("/ws");
        for part in parts {
            path.push(part);
        }
        path
    }

    /// Build a mapping from a `View:` body, exactly as `mapping_for` does after
    /// `p4 info` + `p4 client -o`.
    fn view_mapping(body: &str, scan_root: &Path) -> Option<Mapping> {
        let spec = format!(
            "# A Perforce Client Specification.\n\
             \n\
             Client:\tccn_complex\n\
             \n\
             Root:\tC:\\ws\\ccn_complex\n\
             \n\
             View:\n\
             {body}\n\
             SubmitOptions:\tsubmitunchanged\n"
        );
        let client_root = ws_path(&["ccn_complex"]);
        let parsed = parse_client_view(&spec);
        if parsed.lines.is_empty() {
            if parsed.complete {
                return None;
            }
            return Some(Mapping {
                includes: Vec::new(),
                exclusions: Vec::new(),
                complete: false,
            });
        }
        Some(mapping_from_view(
            &parsed.lines,
            &client_root,
            scan_root,
            parsed.complete,
        ))
    }

    /// A view body from the review's reproductions: a complete client view with
    /// ordered include/exclude lines.
    fn parsed_view(body: &str) -> ParsedView {
        let spec = format!("View:\n{body}\nSubmitOptions:\tsubmitunchanged\n");
        parse_client_view(&spec)
    }

    #[test]
    fn a_multi_line_view_maps_every_include_and_honours_exclusions() {
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...\n\
\t//depot/prj/lib/... //ccn_complex/third_party/lib/...\n\
\t-//depot/prj/main/secret/...\n\
\t+//depot/prj/hotfix/... //ccn_complex/main/hotfix/...\n\
\t\"//depot/prj/my tools/...\" \"//ccn_complex/tools dir/...\"";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping from the client view");

        // Plain include, remapped directory, overlay and a quoted path with
        // spaces each map through their own line.
        assert_eq!(
            mapping.depot_file("main/a.c").as_deref(),
            Some("//depot/prj/main/a.c")
        );
        assert_eq!(
            mapping.depot_file("third_party/lib/x.c").as_deref(),
            Some("//depot/prj/lib/x.c"),
            "the remapped directory is not fabricated as //depot/prj/main/..."
        );
        assert_eq!(
            mapping.depot_file("main/hotfix/h.c").as_deref(),
            Some("//depot/prj/hotfix/h.c"),
            "the overlay line wins over the shallower plain include"
        );
        assert_eq!(
            mapping.depot_file("tools dir/notes.txt").as_deref(),
            Some("//depot/prj/my tools/notes.txt"),
            "quoted paths keep their spaces"
        );

        // The depot→scan-relative direction round-trips.
        for (rel, depot) in [
            ("main/a.c", "//depot/prj/main/a.c"),
            ("third_party/lib/x.c", "//depot/prj/lib/x.c"),
            ("main/hotfix/h.c", "//depot/prj/hotfix/h.c"),
            ("tools dir/notes.txt", "//depot/prj/my tools/notes.txt"),
        ] {
            assert_eq!(mapping.relative(depot).as_deref(), Some(rel), "{depot}");
        }

        // The exclusion is depot-side only and beats the include that covers it.
        assert_eq!(mapping.depot_file("main/secret/s.c"), None);
        assert_eq!(mapping.relative("//depot/prj/main/secret/s.c"), None);

        // A path outside every line is simply not mapped.
        assert_eq!(mapping.depot_file("other/z.c"), None);
    }

    #[test]
    fn an_overlay_line_wins_a_tie_against_a_plain_include() {
        // Both lines have the same local base. Perforce gives the `+` line
        // precedence, so the overlay's depot path is the one used; the plain
        // include still answers the reverse direction for its own depot tree.
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...\n\
\t+//depot/prj/hotfix/... //ccn_complex/main/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(
            mapping.depot_file("main/a.c").as_deref(),
            Some("//depot/prj/hotfix/a.c"),
            "the overlay line wins the tie"
        );
        assert_eq!(
            mapping.relative("//depot/prj/main/a.c").as_deref(),
            Some("main/a.c")
        );
        assert_eq!(
            mapping.relative("//depot/prj/hotfix/a.c").as_deref(),
            Some("main/a.c")
        );
    }

    #[test]
    fn the_longest_local_base_wins() {
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...\n\
\t//depot/prj/vendor/... //ccn_complex/main/vendor/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(
            mapping.depot_file("main/vendor/v.c").as_deref(),
            Some("//depot/prj/vendor/v.c"),
            "the deeper client directory wins over the enclosing one"
        );
        assert_eq!(
            mapping.depot_file("main/m.c").as_deref(),
            Some("//depot/prj/main/m.c"),
            "the enclosing line still maps everything else"
        );
        assert_eq!(
            mapping.relative("//depot/prj/vendor/v.c").as_deref(),
            Some("main/vendor/v.c")
        );
    }

    #[test]
    fn a_single_file_view_line_maps_only_that_file() {
        let body = "\
\t//depot/a.c //ccn_complex/a.c\n\
\t//depot/prj/dir/... //ccn_complex/dir/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(mapping.depot_file("a.c").as_deref(), Some("//depot/a.c"));
        assert_eq!(mapping.relative("//depot/a.c").as_deref(), Some("a.c"));
        assert_eq!(
            mapping.depot_file("dir/b.c").as_deref(),
            Some("//depot/prj/dir/b.c")
        );
        assert_eq!(mapping.relative("//depot/a.h"), None);

        assert_eq!(
            mapping.depot_specs(),
            vec!["//depot/a.c".to_string(), "//depot/prj/dir/...".to_string()],
            "a single-file line is queried as its exact path, not as a subtree"
        );
    }

    #[test]
    fn wildcard_view_lines_are_not_guessed_at_but_make_the_mapping_incomplete() {
        // `*`/`%%n` cannot be reduced to a prefix. The line is not used to
        // fabricate a literal depot path, and the mapping records that the view
        // was only partially understood, so its files cannot be called
        // definitively out of view.
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(
            "\t//depot/good/... //ccn_complex/good/...\n\
             \t//depot/prj/*.c //ccn_complex/src/*.c",
            &root,
        )
        .expect("the supported lines still form a mapping");

        assert!(!mapping.complete(), "the wildcard line is unsupported");
        assert_eq!(
            mapping.depot_file("good/a.c").as_deref(),
            Some("//depot/good/a.c"),
            "the supported line still works"
        );
        assert_eq!(
            mapping.depot_file("src/a.c"),
            None,
            "the wildcard line is never reduced to a literal //depot/prj/*.c"
        );
        assert_eq!(
            unmapped_file_at(&mapping),
            FileAt::Unknown,
            "an unmapped path under an incomplete view is Unknown, not Unmapped"
        );
    }

    #[test]
    fn a_wildcard_only_view_is_still_an_incomplete_mapping() {
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping("\t//depot/prj/*.c //ccn_complex/src/*.c", &root)
            .expect("an incomplete mapping, not None");
        assert!(!mapping.complete());
        assert!(mapping.depot_specs().is_empty());
        assert_eq!(unmapped_file_at(&mapping), FileAt::Unknown);
    }

    #[test]
    fn a_later_include_re_adds_an_earlier_exclusion() {
        // The reviewer's reproduction: broad include, exclusion, re-inclusion.
        let body = "\
             \t//depot/... //ccn_complex/...\n\
             \t-//depot/src/... //ccn_complex/src/...\n\
             \t//depot/src/... //ccn_complex/src/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");
        assert!(mapping.complete());

        assert_eq!(
            mapping.depot_file("src/a.cpp").as_deref(),
            Some("//depot/src/a.cpp"),
            "the final include re-adds the path the exclusion removed"
        );
        assert_eq!(
            mapping.relative("//depot/src/a.cpp").as_deref(),
            Some("src/a.cpp")
        );
        assert_eq!(
            mapping.depot_file("good/control.c").as_deref(),
            Some("//depot/good/control.c")
        );
    }

    #[test]
    fn an_exclusion_without_a_later_include_still_excludes() {
        let body = "\
             \t//depot/... //ccn_complex/...\n\
             \t-//depot/src/... //ccn_complex/src/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(mapping.depot_file("src/a.cpp"), None);
        assert_eq!(mapping.relative("//depot/src/a.cpp"), None);
        assert_eq!(
            unmapped_file_at(&mapping),
            FileAt::Unmapped,
            "a complete view makes the exclusion ordinary out-of-view"
        );
    }

    #[test]
    fn a_later_line_overrides_an_earlier_specific_mapping() {
        // The reviewer's reproduction: `//depot/specific/... //review/src/...`
        // then `//depot/broad/... //review/...`. The later line wins, so the
        // request must use the later depot/client pair.
        let body = "\
             \t//depot/specific/... //ccn_complex/src/...\n\
             \t//depot/broad/... //ccn_complex/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(
            mapping.depot_file("src/a.cpp").as_deref(),
            Some("//depot/broad/src/a.cpp"),
            "the later broad mapping overrides the earlier specific one"
        );
        assert_eq!(
            mapping.relative("//depot/broad/src/a.cpp").as_deref(),
            Some("src/a.cpp")
        );

        // Order matters: the reverse view keeps the specific mapping.
        let reversed = "\
             \t//depot/broad/... //ccn_complex/...\n\
             \t//depot/specific/... //ccn_complex/src/...";
        let mapping = view_mapping(reversed, &root).expect("mapping");
        assert_eq!(
            mapping.depot_file("src/a.cpp").as_deref(),
            Some("//depot/specific/a.cpp")
        );
    }

    #[test]
    fn an_unparseable_view_line_makes_the_view_incomplete() {
        let parsed = parsed_view(
            "\t//depot/good/... //ccn_complex/good/...\n\
             \tnot a view line",
        );
        assert!(!parsed.complete, "a dropped line must be recorded");
        assert_eq!(parsed.lines.len(), 1);

        let root = ws_path(&["ccn_complex"]);
        let mapping = mapping_from_view(&parsed.lines, &root, &root, parsed.complete);
        assert_eq!(unmapped_file_at(&mapping), FileAt::Unknown);
    }

    #[test]
    fn the_where_fallback_is_incomplete() {
        // `client -o` failed (or had no view), so one `p4 where` triple is all
        // that is known. The full view was never established.
        let out = "//depot/proj/... //client/proj/... C:\\ws\\proj\\...\n";
        let mapping = parse_where(out, Path::new("C:\\ws\\proj")).expect("mapping");
        assert!(!mapping.complete());
        assert_eq!(
            unmapped_file_at(&mapping),
            FileAt::Unknown,
            "another file may be mapped by the unread view"
        );
        // The triple itself still resolves normally.
        assert_eq!(
            mapping.depot_file("src/a.rs").as_deref(),
            Some("//depot/proj/src/a.rs")
        );
    }

    #[test]
    fn a_complete_view_reports_ordinary_out_of_view_as_unmapped() {
        let body = "\t//depot/prj/main/... //ccn_complex/main/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");
        assert!(mapping.complete());
        assert_eq!(unmapped_file_at(&mapping), FileAt::Unmapped);
    }

    #[test]
    fn a_path_outside_every_view_line_is_unmapped() {
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(mapping.depot_file("outside.c"), None);
        assert_eq!(mapping.depot_file("main/../outside.c"), None);
        assert!(!mapping.depot_prefixes().is_empty());
    }

    #[test]
    fn depot_prefixes_are_deduped_and_include_overlay_lines() {
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...\n\
\t//depot/prj/main/... //ccn_complex/main/dup/...\n\
\t+//depot/prj/hotfix/... //ccn_complex/main/hotfix/...\n\
\t-//depot/prj/main/secret/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("mapping");

        assert_eq!(
            mapping.depot_prefixes(),
            vec![
                "//depot/prj/hotfix".to_string(),
                "//depot/prj/main".to_string(),
            ],
            "duplicates collapse, overlays are included and exclusions are not"
        );
        assert_eq!(
            mapping.depot_specs(),
            vec![
                "//depot/prj/hotfix/...".to_string(),
                "//depot/prj/main/...".to_string(),
            ]
        );
    }

    #[test]
    fn a_view_with_only_exclusions_or_nothing_visible_maps_nothing() {
        let body = "\
\t-//depot/prj/main/secret/...";
        let root = ws_path(&["ccn_complex"]);
        let mapping = view_mapping(body, &root).expect("the view was still read");
        assert!(mapping.complete(), "a parsed exclusion is understood");
        assert!(mapping.depot_specs().is_empty());

        // An include whose local base is a sibling of the scan root cannot name
        // anything under it, but the view itself is complete.
        let sibling = "\
\t//depot/prj/main/... //ccn_complex/main/...";
        let mapping = view_mapping(sibling, &ws_path(&["elsewhere"])).expect("mapping");
        assert!(mapping.complete());
        assert_eq!(mapping.depot_file("main/a.c"), None);
        assert_eq!(unmapped_file_at(&mapping), FileAt::Unmapped);
    }

    #[test]
    fn view_lines_with_a_scan_root_inside_the_local_base_reapply_the_offset() {
        // A scan of `<client>/main` still needs the `main/` segment in the depot
        // path, exactly like the `p4 where` fallback's subdirectory case.
        let body = "\
\t//depot/prj/main/... //ccn_complex/main/...";
        let mapping = view_mapping(body, &ws_path(&["ccn_complex", "main"])).expect("mapping");

        assert_eq!(
            mapping.depot_file("src/a.c").as_deref(),
            Some("//depot/prj/main/src/a.c")
        );
        assert_eq!(
            mapping.relative("//depot/prj/main/src/a.c").as_deref(),
            Some("src/a.c")
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
    #[cfg(unix)]
    fn depot_file_reapplies_the_scan_root_offset() {
        let out = "//depot/proj/... //client/proj/... /ws/proj/...\n";
        let mapping =
            parse_where(out, Path::new("/ws/proj/src")).expect("mapping for a subdirectory");

        assert_eq!(
            mapping.depot_file("a.rs").as_deref(),
            Some("//depot/proj/src/a.rs")
        );
        assert_eq!(
            mapping.relative("//depot/proj/src/a.rs").as_deref(),
            Some("a.rs")
        );
    }

    #[test]
    #[cfg(windows)]
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

    /// Spawn `p4` for the live evidence test with a fixed client/root.
    fn live_p4(
        dir: &Path,
        client: &str,
        args: &[&str],
        stdin: Option<&str>,
    ) -> std::process::Output {
        let mut cmd = Command::new("p4");
        cmd.current_dir(dir)
            .env("P4CLIENT", client)
            .env("P4CONFIG", "")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn p4");
        if let Some(input) = stdin {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write client spec");
        }
        drop(child.stdin.take());
        child.wait_with_output().expect("p4 output")
    }

    fn live_p4_ok(dir: &Path, client: &str, args: &[&str], stdin: Option<&str>) -> String {
        let out = live_p4(dir, client, args, stdin);
        assert!(
            out.status.success(),
            "p4 {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn submitted_change(text: &str) -> u64 {
        text.split_whitespace()
            .skip_while(|word| *word != "Change")
            .nth(1)
            .and_then(|number| number.trim_end_matches('.').parse().ok())
            .unwrap_or_else(|| panic!("no changelist number in {text:?}"))
    }

    /// Text of a [`FileAt::Found`] with server newline translation undone.
    fn found_text(file: FileAt) -> Option<String> {
        match file {
            FileAt::Found(bytes) => Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n")),
            _ => None,
        }
    }

    /// Live p4d evidence for the ordered view semantics and completeness rules.
    ///
    /// Skipped unless `CCN_P4_LIVE=1` is set. Requires a real server: start
    /// `p4d -r <dir> -p 1667`, put `p4` on `PATH`, set `P4PORT=1667`,
    /// `P4USER=probe` and `CCN_P4_LIVE=1`, then run
    /// `cargo test --release --locked --lib p4::tests::live_ordered_view_evidence -- --test-threads=1`.
    /// It creates (and deletes) its own clients and depot files under
    /// `//depot/ccnprobe_<pid>/`.
    #[test]
    fn live_ordered_view_evidence() {
        if std::env::var("CCN_P4_LIVE").is_err() {
            return;
        }
        let tmp = TempDir::new("live");
        let root = tmp.path().join("ws");
        fs::create_dir_all(&root).expect("create client root");
        let pid = std::process::id();
        let client = format!("ccn_probe_{pid}");
        let complete_client = format!("ccn_probe_complete_{pid}");
        let depot = format!("//depot/ccnprobe_{pid}");

        let view = format!(
            "\t{depot}/base/... //{client}/base/...\n\
             \t-{depot}/src/... //{client}/src/...\n\
             \t{depot}/src/... //{client}/src/...\n\
             \t{depot}/specific/... //{client}/over/...\n\
             \t{depot}/broad/... //{client}/over/...\n\
             \t{depot}/wild/*.c //{client}/wild/*.c"
        );
        let spec = format!(
            "Client: {client}\nOwner: probe\nRoot: {}\nView:\n{view}\n",
            root.display()
        );
        live_p4_ok(&root, &client, &["client", "-i"], Some(&spec));

        // CL1: the re-included file at v1, the override's later depot path, and
        // a file that only the wildcard line can map.
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("wild")).expect("wild dir");
        fs::create_dir_all(root.join("over")).expect("over dir");
        fs::write(root.join("src/re.cpp"), "reinclude v1\n").expect("write re");
        fs::write(root.join("wild/x.c"), "wild only\n").expect("write wild");
        fs::write(root.join("over/a.cpp"), "broad override\n").expect("write over");
        live_p4_ok(
            &root,
            &client,
            &["add", "src/re.cpp", "wild/x.c", "over/a.cpp"],
            None,
        );
        let cl1 = submitted_change(&live_p4_ok(
            &root,
            &client,
            &["submit", "-d", "probe base"],
            None,
        ));

        // CL2: only src/re.cpp changes, so the as-of read at CL1 must still see
        // v1 while CL2 sees v2.
        std::env::set_var("P4CLIENT", &client);
        live_p4_ok(&root, &client, &["edit", "src/re.cpp"], None);
        fs::write(root.join("src/re.cpp"), "reinclude v2\n").expect("edit re");
        let cl2 = submitted_change(&live_p4_ok(
            &root,
            &client,
            &["submit", "-d", "probe edit"],
            None,
        ));
        assert!(cl2 > cl1);

        assert_eq!(
            found_text(file_at(&root, &cl1.to_string(), "src/re.cpp")).as_deref(),
            Some("reinclude v1\n"),
            "the re-included file maps and reads its as-of revision"
        );
        assert_eq!(
            found_text(file_at(&root, &cl2.to_string(), "src/re.cpp")).as_deref(),
            Some("reinclude v2\n"),
            "the later changelist sees the newer revision"
        );
        assert_eq!(
            found_text(file_at(&root, &cl1.to_string(), "over/a.cpp")).as_deref(),
            Some("broad override\n"),
            "the later broad line overrides the earlier specific line; \
             //depot/specific/a.cpp was never submitted, so a specific read \
             would have been Absent"
        );
        assert_eq!(
            file_at(&root, &cl1.to_string(), "wild/x.c"),
            FileAt::Unknown,
            "a file only the unsupported wildcard line covers is Unknown, not Unmapped"
        );
        assert_eq!(
            file_at(&root, &cl1.to_string(), "other/out.c"),
            FileAt::Unknown,
            "the whole view is incomplete because of the wildcard line"
        );

        // A complete view: the same out-of-view path is ordinary Unmapped.
        let complete_root = tmp.path().join("ws_complete");
        fs::create_dir_all(&complete_root).expect("create second root");
        let complete_view = format!(
            "\t{depot}/base/... //{complete_client}/base/...\n\
             \t-{depot}/src/... //{complete_client}/src/...\n\
             \t{depot}/src/... //{complete_client}/src/...\n\
             \t{depot}/specific/... //{complete_client}/over/...\n\
             \t{depot}/broad/... //{complete_client}/over/..."
        );
        let complete_spec = format!(
            "Client: {complete_client}\nOwner: probe\nRoot: {}\nView:\n{complete_view}\n",
            complete_root.display()
        );
        live_p4_ok(
            &complete_root,
            &complete_client,
            &["client", "-i"],
            Some(&complete_spec),
        );
        std::env::set_var("P4CLIENT", &complete_client);
        assert_eq!(
            file_at(&complete_root, &cl1.to_string(), "other/out.c"),
            FileAt::Unmapped,
            "with every view line understood, out-of-view is Unmapped"
        );

        // Clean up both clients (the server and its depot are temporary).
        let _ = live_p4(&root, &client, &["client", "-d", "-f", &client], None);
        let _ = live_p4(
            &complete_root,
            &complete_client,
            &["client", "-d", "-f", &complete_client],
            None,
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

    #[test]
    fn parse_opened_groups_ztag_fields_by_index() {
        let text = "... depotFile0 //depot/proj/a.rs\n\
                    ... clientFile0 C:\\ws\\proj\\a.rs\n\
                    ... action0 edit\n\
                    ... depotFile1 //depot/proj/b.rs\n\
                    ... clientFile1 C:\\ws\\proj\\b.rs\n\
                    ... action1 add\n";
        let files = parse_opened_status(text);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].depot, "//depot/proj/a.rs");
        assert_eq!(files[0].local.as_deref(), Some("C:\\ws\\proj\\a.rs"));
        assert_eq!(files[0].action, "edit");
        assert_eq!(files[1].depot, "//depot/proj/b.rs");
        assert_eq!(files[1].action, "add");
    }

    #[test]
    fn parse_opened_reads_the_plain_form() {
        let text = "//depot/proj/a.rs#3 - edit change 12345 (text)\n\
                    //depot/proj/b.rs#1 - add default change (text)\n\
                    //depot/proj/c.rs#2 - move/delete change 9 (text)\n\
                    File(s) not opened on this client.\n";
        let files = parse_opened_status(text);

        assert_eq!(files.len(), 3, "diagnostics are skipped");
        assert_eq!(files[0].depot, "//depot/proj/a.rs");
        assert_eq!(files[0].action, "edit");
        assert_eq!(files[2].action, "move/delete");
        assert!(files.iter().all(|file| file.local.is_none()));
    }

    #[test]
    fn opened_actions_fold_into_file_changes() {
        assert_eq!(action_change("edit"), FileChange::Modified);
        assert_eq!(action_change("integrate"), FileChange::Modified);
        assert_eq!(action_change("add"), FileChange::Added);
        assert_eq!(action_change("branch"), FileChange::Added);
        assert_eq!(action_change("import"), FileChange::Added);
        assert_eq!(action_change("move/add"), FileChange::Added);
        assert_eq!(action_change("delete"), FileChange::Deleted);
        assert_eq!(action_change("purge"), FileChange::Deleted);
        assert_eq!(action_change("move/delete"), FileChange::Deleted);
        assert_eq!(action_change(" EDIT "), FileChange::Modified);
    }

    #[test]
    fn status_changes_is_empty_for_a_missing_directory() {
        let missing = Path::new("ccn-p4-status-definitely-missing");
        assert!(status_changes(missing).is_empty());
    }
}
