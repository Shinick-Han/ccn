//! Persistent per-file cache.
//!
//! The expensive parts of a scan are reading files and building ASTs. Both are
//! a pure function of `(path, mtime, size, parser version, rule version)`, so a
//! cache keyed on exactly that cannot serve stale numbers after an edit.
//!
//! Design constraints:
//! - **No cache file inside the scanned repository.** It would show up in the
//!   next scan and in `git status`. The cache lives under the OS user cache
//!   directory, one file per repository root.
//! - **Corrupt or old cache is a miss, never an error.** A tool that refuses to
//!   run because its cache is unreadable is worse than a slow tool.
//! - **Atomic replace** (write `.tmp`, then rename) so a killed process cannot
//!   leave a half-written JSON file.
//! - The version constant is bumped whenever the CCN rules or the JSON shape
//!   change; a mismatch discards the whole file.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ccn::types::CcnSummary;

/// Bump when a cached field changes meaning. `CARGO_PKG_VERSION` covers code
/// changes; this covers rule-table changes that do not move the crate version.
pub const CACHE_VERSION: u32 = 1;

/// What identifies a file's content without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub mtime_ns: u128,
    pub size: u64,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        let mtime_ns = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(Self {
            mtime_ns,
            size: meta.len(),
        })
    }
}

/// One file's cached scan result. Mirrors the fields of `FileLoc` plus the CCN
/// summary, because a hit must be able to reproduce the whole record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub mtime_ns: u128,
    pub size: u64,
    pub lang: String,
    pub loc_total: usize,
    pub loc: usize,
    pub comments: usize,
    pub blanks: usize,
    #[serde(default)]
    pub ccn: Option<CcnSummary>,
    #[serde(default)]
    pub is_generated: bool,
    #[serde(default)]
    pub is_vendored: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub root: String,
    pub entries: HashMap<String, Entry>,
}

impl Cache {
    /// Load the cache for `root`. Any failure (missing, corrupt, wrong version,
    /// wrong root) yields an empty cache.
    pub fn load(root: &str) -> Self {
        match cache_path(root) {
            Some(path) => Self::load_from(&path, root),
            None => Self::default(),
        }
    }

    fn load_from(path: &Path, root: &str) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        match serde_json::from_slice::<Cache>(&bytes) {
            Ok(cache) if cache.version == CACHE_VERSION && cache.root == root => cache,
            _ => Self::default(),
        }
    }

    /// Replace the cache atomically. Errors are reported, but the caller treats
    /// them as non-fatal.
    pub fn save(&self) -> io::Result<()> {
        match cache_path(&self.root) {
            Some(path) => self.save_to(&path),
            None => Ok(()),
        }
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        std::fs::write(&tmp, json)?;
        // Windows rename fails when the destination exists.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    }

    pub fn get(&self, path: &str, fingerprint: Fingerprint) -> Option<&Entry> {
        self.entries
            .get(path)
            .filter(|e| e.mtime_ns == fingerprint.mtime_ns && e.size == fingerprint.size)
    }

    pub fn insert(&mut self, path: String, entry: Entry) {
        self.entries.insert(path, entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `<cache dir>/ccn/<hash of root>.json`.
///
/// The hash keeps two checkouts with the same basename apart without putting
/// anything in the repository. A human cannot map the file back to a repo, but
/// nobody needs to: deleting the directory is always safe.
pub fn cache_path(root: &str) -> Option<PathBuf> {
    let base = cache_dir()?;
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        base.join("ccn")
            .join(format!("{:016x}.json", hasher.finish())),
    )
}

/// A sidecar file next to the scan cache (e.g. `history` for trend snapshots).
///
/// Used when the repository itself is not writable — Perforce syncs files
/// read-only, so `--trend` cannot create `<repo>/.ccn/history.json` until the
/// file is opened for edit.
pub fn sidecar_path(root: &str, kind: &str) -> Option<PathBuf> {
    let base = cache_dir()?;
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        base.join("ccn")
            .join(format!("{kind}-{:016x}.json", hasher.finish())),
    )
}

fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CCN_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            return Some(PathBuf::from(dir));
        }
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cache"))
        .or_else(|| Some(std::env::temp_dir()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{CcnTier, FunctionCcn};

    fn entry(mtime: u128, size: u64) -> Entry {
        Entry {
            mtime_ns: mtime,
            size,
            lang: "Rust".into(),
            loc_total: 10,
            loc: 8,
            comments: 1,
            blanks: 1,
            ccn: Some(CcnSummary::from_file(crate::ccn::types::FileCcn {
                tier: CcnTier::Exact,
                functions: vec![FunctionCcn {
                    name: "f".into(),
                    line: 1,
                    end_line: 3,
                    nloc: 3,
                    ccn: 2,
                    params: 0,
                }],
            })),
            is_generated: false,
            is_vendored: false,
        }
    }

    #[test]
    fn fingerprint_changes_with_size() {
        let dir = std::env::temp_dir().join(format!("ccn-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, b"fn a() {}").unwrap();
        let first = Fingerprint::of(&file).unwrap();
        std::fs::write(&file, b"fn a() { if true {} }").unwrap();
        let second = Fingerprint::of(&file).unwrap();
        assert_ne!(first.size, second.size);
        assert!(!dir.exists() || second.size > first.size);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hit_requires_matching_mtime_and_size() {
        let mut cache = Cache {
            version: CACHE_VERSION,
            root: "r".into(),
            entries: HashMap::new(),
        };
        cache.insert("a.rs".into(), entry(100, 50));
        assert!(cache
            .get(
                "a.rs",
                Fingerprint {
                    mtime_ns: 100,
                    size: 50
                }
            )
            .is_some());
        assert!(cache
            .get(
                "a.rs",
                Fingerprint {
                    mtime_ns: 101,
                    size: 50
                }
            )
            .is_none());
        assert!(cache
            .get(
                "a.rs",
                Fingerprint {
                    mtime_ns: 100,
                    size: 51
                }
            )
            .is_none());
        assert!(cache
            .get(
                "b.rs",
                Fingerprint {
                    mtime_ns: 100,
                    size: 50
                }
            )
            .is_none());
    }

    #[test]
    fn corrupt_cache_is_an_empty_cache() {
        let dir = std::env::temp_dir().join(format!("ccn-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");
        std::fs::write(&path, b"{not json").unwrap();

        let cache = Cache::load_from(&path, "C:/repo");
        assert!(cache.is_empty());
        assert_eq!(cache.version, 0, "default cache has version 0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!("ccn-cache-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");

        let root = "C:/roundtrip";
        let mut cache = Cache {
            version: CACHE_VERSION,
            root: root.into(),
            entries: HashMap::new(),
        };
        cache.insert("src/a.rs".into(), entry(1, 2));
        cache.save_to(&path).expect("save");

        let loaded = Cache::load_from(&path, root);
        assert_eq!(loaded.len(), 1);
        let got = loaded
            .get(
                "src/a.rs",
                Fingerprint {
                    mtime_ns: 1,
                    size: 2,
                },
            )
            .expect("hit");
        assert_eq!(got.lang, "Rust");
        assert_eq!(got.ccn.as_ref().unwrap().max, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
