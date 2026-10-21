//! Snapshot history and trend comparison.
//!
//! Every recorded scan appends one small [`Snapshot`] to
//! `<root>/.ccn/history.json`, which lives **inside the repository** on purpose:
//! the trend is part of the code's history, not of the machine that measured it.
//!
//! Conventions (see `docs/PLAN.md` §19):
//! - **Atomic replace**: write `.tmp`, then rename over the destination, so a
//!   killed process can never leave a half-written history behind.
//! - **Never fail the scan**: a missing, corrupt or wrong-version file loads as
//!   an empty history, and callers are expected to ignore [`record`]'s error.
//! - The file is committed and read by humans, so it is pretty-printed: one
//!   field per line means a changed number is a one-line diff and a new scan is
//!   a clean block, not a re-flowed single line.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bumped when the on-disk shape changes incompatibly. An unknown version reads
/// as "no history" rather than as a parse error, so an old binary cannot corrupt
/// a newer file (it rewrites it from scratch, which is the documented cost).
const HISTORY_VERSION: u32 = 1;

/// How many snapshots are kept. 200 weekly scans is ~4 years; anything older is
/// archaeology, and an unbounded file would eventually be a load on every scan.
const HISTORY_CAP: usize = 200;

/// One scan's headline numbers. Deliberately small: this file is committed and
/// read by humans, so it must stay reviewable in a diff.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds.
    pub at_unix: u64,
    pub health: Option<f64>,
    pub grade: Option<char>,
    pub loc: u64,
    pub functions: u64,
    pub pct_over_10: f64,
    pub ccn_max: u32,
    pub files_measured: usize,
    /// Version identity. A delta across a tool/band change is not a code
    /// change, so the trend must be able to say "not comparable".
    #[serde(default)]
    pub tool_version: String,
    #[serde(default)]
    pub bin_spec_version: u32,
    #[serde(default)]
    pub health_calibration: String,
}

impl Snapshot {
    /// Read the headline numbers out of a built report.
    ///
    /// `health`/`grade`/`pct_over_10` are `None`/`0.0` for a report with no
    /// measured functions; that is a real state (an empty repository, a
    /// `--no-ccn` run), not a missing measurement, so it is stored as such
    /// instead of being skipped.
    pub fn from_report(report: &crate::metrics::Report) -> Self {
        let at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);

        Self {
            at_unix,
            health: report.health.as_ref().map(|h| h.score),
            grade: report.health.as_ref().map(|h| h.grade),
            loc: report.loc_total as u64,
            functions: report.stats.functions_total as u64,
            pct_over_10: report.health.as_ref().map_or(0.0, |h| h.pct_over_10),
            ccn_max: report.stats.ccn_max,
            files_measured: report.files_measured,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            bin_spec_version: report.bin_spec_version,
            health_calibration: crate::metrics::HEALTH_CALIBRATION.to_string(),
        }
    }
}

/// Previous and current snapshot, plus the deltas a report shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trend {
    pub previous: Snapshot,
    pub current: Snapshot,
}

impl Trend {
    pub fn health_delta(&self) -> Option<f64> {
        match (self.previous.health, self.current.health) {
            (Some(a), Some(b)) => Some(b - a),
            _ => None,
        }
    }

    pub fn pct_over_10_delta(&self) -> f64 {
        self.current.pct_over_10 - self.previous.pct_over_10
    }

    /// `Some(reason)` when the two snapshots were produced by different tools,
    /// bin specs or grade calibrations.
    ///
    /// A delta across a version change is not a code change. The report still
    /// shows the numbers (they are real measurements), but it must say that the
    /// comparison is not apples-to-apples.
    pub fn compatibility_warning(&self) -> Option<String> {
        let previous = &self.previous;
        let current = &self.current;

        // Snapshots written before 0.3.1 have none of these fields.
        if previous.tool_version.is_empty()
            && previous.bin_spec_version == 0
            && previous.health_calibration.is_empty()
        {
            return Some(
                "previous snapshot predates versioning; deltas are not comparable".to_string(),
            );
        }

        let mut differences: Vec<String> = Vec::new();
        if previous.tool_version != current.tool_version {
            differences.push(format!(
                "ccn {} -> {}",
                known(&previous.tool_version),
                known(&current.tool_version)
            ));
        }
        if previous.bin_spec_version != current.bin_spec_version {
            differences.push(format!(
                "bin spec v{} -> v{}",
                previous.bin_spec_version, current.bin_spec_version
            ));
        }
        if previous.health_calibration != current.health_calibration {
            differences.push("grade calibration changed".to_string());
        }

        if differences.is_empty() {
            None
        } else {
            Some(format!("not comparable: {}", differences.join("; ")))
        }
    }
}

fn known(version: &str) -> &str {
    if version.is_empty() {
        "unknown"
    } else {
        version
    }
}

/// The file itself. `version` is checked on read, never assumed.
#[derive(Debug, Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    snapshots: Vec<Snapshot>,
}

impl HistoryFile {
    fn new(snapshots: Vec<Snapshot>) -> Self {
        Self {
            version: HISTORY_VERSION,
            snapshots,
        }
    }

    fn read(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let file: Self = serde_json::from_slice(&bytes).ok()?;
        (file.version == HISTORY_VERSION).then_some(file)
    }

    /// Atomic replace. Windows rename fails when the destination exists, so it
    /// is removed first; the window where neither file exists is handled by
    /// [`load`] degrading to an empty history.
    fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json)?;
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    }
}

/// Read the history for `root` (newest last). Missing/corrupt file -> empty.
///
/// Appends happen in [`record`], so the stored order already is chronological.
/// Entries sharing an `at_unix` are kept as-is: a rerun is a data point, not a
/// duplicate to collapse.
pub fn load(root: &str) -> Vec<Snapshot> {
    read_paths(root)
        .iter()
        .find_map(|path| HistoryFile::read(path))
        .map(|file| file.snapshots)
        .unwrap_or_default()
}

/// Where to look, most specific first: the repository, then the user-cache
/// sidecar used when the workspace is read-only (Perforce syncs files
/// read-only until they are opened for edit).
fn read_paths(root: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = history_path(root) {
        if path.exists() {
            paths.push(path);
        }
    }
    if let Some(path) = crate::cache::sidecar_path(root, "history") {
        if path.exists() {
            paths.push(path);
        }
    }
    paths
}

/// Append `snap` to the history for `root` and return the comparison with the
/// previous entry (the first run has no trend).
///
/// Errors are the caller's to ignore: a scan must never fail because history
/// could not be written.
pub fn record(root: &str, snap: Snapshot) -> std::io::Result<Option<Trend>> {
    // Nowhere to write (an empty root) is not a write failure.
    let Some(path) = history_path(root) else {
        return Ok(None);
    };

    let mut snapshots = load(root);
    let previous = snapshots.last().cloned();
    snapshots.push(snap.clone());
    // Ring-buffer semantics: the oldest entry goes, so the newest 200 survive.
    if snapshots.len() > HISTORY_CAP {
        let excess = snapshots.len() - HISTORY_CAP;
        snapshots.drain(..excess);
    }

    let file = HistoryFile::new(snapshots);
    // Perforce workspaces reject the write (read-only files). Falling back to
    // the user cache keeps `--trend` working there instead of failing.
    if file.write(&path).is_err() {
        if let Some(fallback) = crate::cache::sidecar_path(root, "history") {
            file.write(&fallback)?;
        }
    }
    Ok(previous.map(|previous| Trend {
        previous,
        current: snap,
    }))
}

/// `<root>/.ccn/history.json`.
pub fn history_path(root: &str) -> Option<PathBuf> {
    if root.trim().is_empty() {
        return None;
    }
    Some(Path::new(root).join(".ccn").join("history.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A clean directory per test. The tag keeps tests from sharing state, and
    /// the pid keeps two `cargo test` processes from sharing it either.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccn-hist-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    fn root_str(dir: &Path) -> &str {
        dir.to_str().expect("temp dir is valid UTF-8")
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn snap(at: u64, health: f64, pct_over_10: f64) -> Snapshot {
        Snapshot {
            at_unix: at,
            health: Some(health),
            grade: Some(crate::metrics::grade_for(health)),
            loc: 1000,
            functions: 10,
            pct_over_10,
            ccn_max: 20,
            files_measured: 4,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            bin_spec_version: crate::metrics::BIN_SPEC_VERSION,
            health_calibration: crate::metrics::HEALTH_CALIBRATION.to_string(),
        }
    }

    #[test]
    fn first_record_creates_the_file_and_has_no_trend() {
        let dir = temp_root("first");
        let root = root_str(&dir);

        let trend = record(root, snap(100, 90.0, 5.0)).expect("record succeeds");
        assert!(trend.is_none(), "the first run has nothing to compare to");

        let path = history_path(root).expect("root is not empty");
        assert!(path.is_file(), "record creates {path:?}");

        let history = load(root);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0], snap(100, 90.0, 5.0));

        // The on-disk contract is versioned, so a reader can refuse a shape it
        // does not understand.
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read history")).expect("valid JSON");
        assert_eq!(raw["version"], 1);
        assert!(raw["snapshots"].is_array());

        cleanup(&dir);
    }

    #[test]
    fn second_record_compares_against_the_previous_scan() {
        let dir = temp_root("second");
        let root = root_str(&dir);

        record(root, snap(100, 90.0, 5.0)).expect("first record");
        let trend = record(root, snap(200, 85.5, 7.5))
            .expect("second record")
            .expect("a previous snapshot exists");

        assert_eq!(trend.previous.at_unix, 100);
        assert_eq!(trend.current.at_unix, 200);
        assert_eq!(trend.previous.health, Some(90.0));
        assert_eq!(trend.health_delta(), Some(-4.5));
        assert!((trend.pct_over_10_delta() - 2.5).abs() < 1e-9);

        assert_eq!(load(root).len(), 2, "both scans are kept");
        cleanup(&dir);
    }

    #[test]
    fn history_is_capped_at_200_and_drops_the_oldest() {
        let dir = temp_root("cap");
        let root = root_str(&dir);

        for i in 0..205u64 {
            record(root, snap(i, 90.0, 1.0)).expect("record survives the cap");
        }

        let history = load(root);
        assert_eq!(history.len(), 200);
        assert_eq!(history[0].at_unix, 5, "snapshots 0-4 were dropped");
        assert_eq!(history[199].at_unix, 204, "the newest scan is last");
        cleanup(&dir);
    }

    #[test]
    fn corrupt_history_loads_empty_and_can_be_replaced() {
        let dir = temp_root("corrupt");
        let root = root_str(&dir);
        let path = history_path(root).expect("root is not empty");
        fs::create_dir_all(path.parent().expect("has parent")).expect("create .ccn");
        fs::write(&path, b"{\"version\": 1, \"snapshots\": [ oops").expect("write corrupt file");

        assert!(load(root).is_empty(), "corrupt input is not an error");

        let trend = record(root, snap(300, 95.0, 2.0)).expect("record overwrites the debris");
        assert!(trend.is_none(), "no readable previous entry");
        assert_eq!(load(root), vec![snap(300, 95.0, 2.0)]);

        // A wrong version is treated the same way as garbage.
        fs::write(&path, b"{\"version\": 99, \"snapshots\": []}").expect("write future version");
        assert!(load(root).is_empty());
        assert!(record(root, snap(400, 96.0, 1.5)).is_ok());

        cleanup(&dir);
    }

    #[test]
    fn same_timestamp_snapshots_are_both_kept() {
        let dir = temp_root("same-second");
        let root = root_str(&dir);

        record(root, snap(500, 90.0, 5.0)).expect("first");
        record(root, snap(500, 91.0, 4.0)).expect("rerun in the same second");

        let history = load(root);
        assert_eq!(history.len(), 2, "a rerun is a data point, not a duplicate");
        cleanup(&dir);
    }

    #[test]
    fn history_path_is_inside_the_dot_ccn_directory() {
        assert_eq!(
            history_path("C:/repo"),
            Some(PathBuf::from("C:/repo").join(".ccn").join("history.json"))
        );
        assert_eq!(
            history_path("/home/me/project"),
            Some(
                PathBuf::from("/home/me/project")
                    .join(".ccn")
                    .join("history.json")
            )
        );
        assert_eq!(history_path(""), None, "an empty root has no history");
        assert_eq!(history_path("   "), None);
    }

    #[test]
    fn trend_deltas_are_current_minus_previous() {
        let trend = Trend {
            previous: snap(1, 90.0, 5.0),
            current: snap(2, 92.5, 3.25),
        };
        assert_eq!(trend.health_delta(), Some(2.5));
        assert!((trend.pct_over_10_delta() - (-1.75)).abs() < 1e-9);

        // One side missing a score (e.g. a `--no-ccn` run) means no delta, but
        // the function-share arithmetic still works on the stored numbers.
        let trend = Trend {
            previous: snap(1, 90.0, 5.0),
            current: Snapshot {
                health: None,
                grade: None,
                pct_over_10: 4.0,
                ..snap(2, 0.0, 0.0)
            },
        };
        assert_eq!(trend.health_delta(), None);
        assert!((trend.pct_over_10_delta() - (-1.0)).abs() < 1e-9);
    }

    #[test]
    fn same_versions_have_no_compatibility_warning() {
        let trend = Trend {
            previous: snap(1, 90.0, 5.0),
            current: snap(2, 91.0, 4.0),
        };
        assert_eq!(trend.compatibility_warning(), None);
    }

    #[test]
    fn a_tool_version_change_is_reported() {
        let mut previous = snap(1, 90.0, 5.0);
        previous.tool_version = "0.2.0".to_string();
        let trend = Trend {
            previous,
            current: snap(2, 91.0, 4.0),
        };
        let warning = trend.compatibility_warning().expect("warning");
        assert!(warning.contains("ccn 0.2.0 -> "), "{warning}");
    }

    #[test]
    fn a_bin_spec_or_calibration_change_is_reported() {
        let mut previous = snap(1, 90.0, 5.0);
        previous.bin_spec_version = 0;
        let trend = Trend {
            previous,
            current: snap(2, 91.0, 4.0),
        };
        let warning = trend.compatibility_warning().expect("warning");
        assert!(warning.contains("bin spec v0 -> v1"), "{warning}");

        let mut previous = snap(1, 90.0, 5.0);
        previous.health_calibration = "older corpus".to_string();
        let trend = Trend {
            previous,
            current: snap(2, 91.0, 4.0),
        };
        let warning = trend.compatibility_warning().expect("warning");
        assert!(warning.contains("grade calibration changed"), "{warning}");
    }

    #[test]
    fn a_pre_versioning_snapshot_is_called_out() {
        let mut previous = snap(1, 90.0, 5.0);
        previous.tool_version.clear();
        previous.bin_spec_version = 0;
        previous.health_calibration.clear();
        let trend = Trend {
            previous,
            current: snap(2, 91.0, 4.0),
        };
        let warning = trend.compatibility_warning().expect("warning");
        assert!(warning.contains("predates versioning"), "{warning}");
    }

    #[test]
    fn from_report_copies_the_headline_numbers() {
        use crate::ccn::types::{CcnSummary, CcnTier, FileCcn, FunctionCcn};
        use crate::model::{ExcludedFiles, FileLoc, ScanResult};

        let function = FunctionCcn {
            name: "f".into(),
            identity: "f()".into(),
            line: 1,
            end_line: 120,
            nloc: 120,
            ccn: 12,
            params: 0,
            max_nesting: 0,
        };
        let file = FileLoc {
            path: "src/a.rs".into(),
            lang: "Rust".into(),
            loc_total: 140,
            loc: 120,
            comments: 15,
            blanks: 5,
            ccn: Some(CcnSummary::from_file(FileCcn {
                tier: CcnTier::Exact,
                functions: vec![function],
            })),
            is_generated: false,
            is_vendored: false,
        };
        let result = ScanResult {
            root: "C:/repo".into(),
            files_analyzed: 1,
            loc_total: 140,
            loc: 120,
            comments: 15,
            blanks: 5,
            files_excluded: ExcludedFiles::default(),
            functions_total: 1,
            ccn: None,
            languages: Vec::new(),
            files: vec![file],
        };
        let report = crate::metrics::build(&result);

        let snapshot = Snapshot::from_report(&report);
        let health = report.health.as_ref().expect("one measured function");

        assert!(snapshot.at_unix > 0, "the clock is past the epoch");
        assert_eq!(snapshot.health, Some(health.score));
        assert_eq!(snapshot.grade, Some(health.grade));
        assert_eq!(snapshot.loc, report.loc_total as u64);
        assert_eq!(snapshot.loc, 140);
        assert_eq!(snapshot.functions, 1);
        assert_eq!(snapshot.ccn_max, 12);
        assert_eq!(snapshot.files_measured, 1);
        assert!((snapshot.pct_over_10 - 100.0).abs() < 1e-9);
    }
}
