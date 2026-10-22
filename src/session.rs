//! Persisted workspace state for the dashboard and split/workbench views.
//!
//! The scanned checkout is often read-only (Perforce, NFS), so the session
//! lives beside the scan cache under the OS user cache directory, one file per
//! repository root, never inside the tree being scanned.
//!
//! Loading is deliberately forgiving: a missing file, corrupt JSON or a file
//! written by a different schema version yields [`Session::default`]. The same
//! rule applies to individual values — an out-of-range split ratio is clamped
//! and unknown sort/tab names fall back to their defaults. Paths are
//! scan-relative; anything absolute, containing `..`, or containing a NUL byte
//! is dropped rather than allowing a stale session to point outside the root.
//!
//! Writes use the same atomic-ish replace as the cache (`.tmp` then rename), so
//! a killed process cannot leave a half-written JSON document in place.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::sidecar_path;

/// Bump when the on-disk shape or field meaning changes. A file with any other
/// version is treated as absent.
pub const SESSION_VERSION: u32 = 1;

pub const DEFAULT_SPLIT_RATIO: u16 = 38;
pub const MIN_SPLIT_RATIO: u16 = 20;
pub const MAX_SPLIT_RATIO: u16 = 70;

const DEFAULT_SORT: &str = "risk";
const DEFAULT_TAB: &str = "ccn";
const SORTS: [&str; 5] = ["risk", "loc", "max-ccn", "over-10", "path"];
const TABS: [&str; 2] = ["ccn", "loc"];

/// Workspace state restored at startup. All paths are relative to the scan
/// root; `open_files` preserves tab order and `tree_expanded` is a set of
/// directories that were open in the tree view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub version: u32,
    pub split_ratio: u16,
    pub selected_path: Option<String>,
    pub sort: String,
    pub sort_desc: bool,
    pub filter: Option<String>,
    pub left_tab: String,
    pub open_files: Vec<String>,
    pub active_file: Option<String>,
    pub tree_expanded: Vec<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            split_ratio: DEFAULT_SPLIT_RATIO,
            selected_path: None,
            sort: DEFAULT_SORT.to_string(),
            sort_desc: true,
            filter: None,
            left_tab: DEFAULT_TAB.to_string(),
            open_files: Vec::new(),
            active_file: None,
            tree_expanded: Vec::new(),
        }
    }
}

impl Session {
    /// Clamp and discard anything that would make the state unsafe or
    /// meaningless. Called for both loaded and saved values, so the file on
    /// disk is always something a future version can consume.
    fn normalize(&mut self) {
        self.version = SESSION_VERSION;
        self.split_ratio = self.split_ratio.clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);

        if !SORTS.contains(&self.sort.as_str()) {
            self.sort = DEFAULT_SORT.to_string();
            self.sort_desc = true;
        }
        if !TABS.contains(&self.left_tab.as_str()) {
            self.left_tab = DEFAULT_TAB.to_string();
        }

        self.selected_path = self.selected_path.take().filter(|p| safe_relative(p));
        self.active_file = self.active_file.take().filter(|p| safe_relative(p));
        self.open_files.retain(|p| safe_relative(p));
        self.tree_expanded.retain(|p| safe_relative(p));
    }
}

/// Load the session for `root`. Never fails: missing, unreadable, corrupt or
/// stale data is simply a fresh session.
pub fn load(root: &str) -> Session {
    match sidecar_path(root, "session") {
        Some(path) => load_from(&path),
        None => Session::default(),
    }
}

/// Replace the session for `root`. Callers should treat errors as non-fatal.
pub fn save(root: &str, session: &Session) -> io::Result<()> {
    match sidecar_path(root, "session") {
        Some(path) => save_to(&path, session),
        None => Ok(()),
    }
}

/// Explicit-path load, for tests and callers that already know the location.
pub fn load_from(path: &Path) -> Session {
    let bytes = std::fs::read(path).or_else(|_| {
        // A crash between moving the old file aside and installing the new one
        // leaves the last good session as `.old`.
        std::fs::read(path.with_extension("old"))
    });
    let Ok(bytes) = bytes else {
        return Session::default();
    };
    let Ok(mut session) = serde_json::from_slice::<Session>(&bytes) else {
        return Session::default();
    };
    if session.version != SESSION_VERSION {
        return Session::default();
    }
    session.normalize();
    session
}

/// Explicit-path save. Creates parent directories and replaces the destination
/// via a temporary file so readers never see a partial document.
pub fn save_to(path: &Path, session: &Session) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut persisted = session.clone();
    persisted.normalize();
    let json = serde_json::to_vec(&persisted).map_err(io::Error::other)?;

    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    // Unix replaces the destination atomically. Windows refuses an existing
    // destination, so keep the old file as `.old` until the new one is in
    // place; a failure restores it.
    if std::fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let backup = path.with_extension("old");
    let _ = std::fs::remove_file(&backup);
    if path.exists() {
        std::fs::rename(path, &backup)?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::rename(&backup, path);
            let _ = std::fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// A path that stays inside the scan root and can be displayed safely.
///
/// Both separators are checked so a path recorded on Windows cannot escape
/// when the same cache directory is read on a Unix host.
fn safe_relative(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') {
        return false;
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    if Path::new(path).is_absolute() {
        return false;
    }
    !path.split(['/', '\\']).any(|component| component == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ccn-session-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn populated() -> Session {
        Session {
            split_ratio: 45,
            selected_path: Some("src/tui.rs".into()),
            sort: "max-ccn".into(),
            sort_desc: false,
            filter: Some("tui".into()),
            left_tab: "loc".into(),
            open_files: vec!["src/tui.rs".into(), "src/cache.rs".into()],
            active_file: Some("src/cache.rs".into()),
            tree_expanded: vec!["src".into(), "src/workbench".into()],
            ..Session::default()
        }
    }

    /// Write JSON directly, bypassing `save_to`'s normalization/clamping.
    fn write_raw(path: &Path, session: &Session) {
        std::fs::write(path, serde_json::to_vec(session).unwrap()).unwrap();
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("session.json");
        let session = populated();

        save_to(&path, &session).expect("save");
        let loaded = load_from(&path);

        assert_eq!(loaded, session);
        assert_eq!(loaded.version, SESSION_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_the_default_session() {
        let dir = temp_dir("missing");
        let loaded = load_from(&dir.join("nope.json"));
        assert_eq!(loaded, Session::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_json_is_the_default_session() {
        let dir = temp_dir("corrupt");
        let path = dir.join("session.json");
        std::fs::write(&path, b"{not json").unwrap();

        assert_eq!(load_from(&path), Session::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_version_is_the_default_session() {
        let dir = temp_dir("version");
        let path = dir.join("session.json");
        let mut session = populated();
        session.version = SESSION_VERSION + 1;
        write_raw(&path, &session);

        assert_eq!(load_from(&path), Session::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_ratio_is_clamped_on_load() {
        let dir = temp_dir("ratio");
        let path = dir.join("session.json");

        let mut low = populated();
        low.split_ratio = 0;
        write_raw(&path, &low);
        assert_eq!(load_from(&path).split_ratio, MIN_SPLIT_RATIO);

        let mut high = populated();
        high.split_ratio = u16::MAX;
        write_raw(&path, &high);
        assert_eq!(load_from(&path).split_ratio, MAX_SPLIT_RATIO);

        let mut inside = populated();
        inside.split_ratio = 60;
        write_raw(&path, &inside);
        assert_eq!(load_from(&path).split_ratio, 60);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsafe_paths_are_dropped_on_load() {
        let dir = temp_dir("paths");
        let path = dir.join("session.json");
        let session = Session {
            selected_path: Some("C:/Windows/System32".into()),
            sort: "risk".into(),
            open_files: vec![
                "../secret.txt".into(),
                "/etc/passwd".into(),
                "src/ok.rs".into(),
                "a\0b.rs".into(),
                "\\\\server\\share".into(),
                "C:\\abs\\file.rs".into(),
                "src/../../escape.rs".into(),
            ],
            active_file: Some("../outside.rs".into()),
            tree_expanded: vec!["..".into(), "src/good".into(), "sub\\..\\bad".into()],
            ..Session::default()
        };
        write_raw(&path, &session);

        let loaded = load_from(&path);
        assert_eq!(loaded.selected_path, None);
        assert_eq!(loaded.active_file, None);
        assert_eq!(loaded.open_files, vec!["src/ok.rs".to_string()]);
        assert_eq!(loaded.tree_expanded, vec!["src/good".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_sort_and_tab_fall_back_to_defaults() {
        let dir = temp_dir("unknown");
        let path = dir.join("session.json");
        let session = Session {
            sort: "nonsense".into(),
            sort_desc: false,
            left_tab: "graphs".into(),
            ..Session::default()
        };
        write_raw(&path, &session);

        let loaded = load_from(&path);
        assert_eq!(loaded.sort, DEFAULT_SORT);
        assert!(loaded.sort_desc);
        assert_eq!(loaded.left_tab, DEFAULT_TAB);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = temp_dir("parents");
        let path = dir.join("a").join("b").join("session.json");
        assert!(!path.exists());

        save_to(&path, &Session::default()).expect("save creates parents");

        assert!(path.exists());
        assert_eq!(load_from(&path), Session::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_writes_valid_values_for_an_invalid_session() {
        let dir = temp_dir("sanitize-save");
        let path = dir.join("session.json");
        let session = Session {
            split_ratio: 999,
            sort: "bogus".into(),
            left_tab: "bogus".into(),
            open_files: vec!["../escape".into(), "src/ok.rs".into()],
            ..Session::default()
        };

        save_to(&path, &session).expect("save");
        let loaded = load_from(&path);

        assert_eq!(loaded.split_ratio, MAX_SPLIT_RATIO);
        assert_eq!(loaded.sort, DEFAULT_SORT);
        assert_eq!(loaded.left_tab, DEFAULT_TAB);
        assert_eq!(loaded.open_files, vec!["src/ok.rs".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
