//! Persistent per-file cache.
//!
//! The expensive parts of a scan are reading files and building ASTs. A cached
//! record is a pure function of `(bytes, parser version, rule version)`, so the
//! cache stores one entry per file and reuses it only when the file still looks
//! unchanged.
//!
//! ## What makes a hit
//!
//! The fast key is `(mtime, size)` from one `stat`. That is what keeps a warm
//! scan at a stat per file instead of a read and a parse, and it is also the
//! limit of the fast path: a tool that rewrites a file and restores the previous
//! nanosecond mtime (some formatters and build systems do) can leave the cache
//! pointing at older bytes. The cache therefore also stores a content digest
//! ([`Entry::content_hash`]) of the exact bytes that were parsed. A normal scan
//! ignores it — that is the documented fast path — while `--verify-cache` reads
//! each hit and recomputes the digest, replacing the entry on a mismatch. Gates
//! and NFS checkouts, where a stale number is worse than a read, want that mode.
//!
//! ## Identity
//!
//! Two fields must agree before anything is reused:
//!
//! - [`CACHE_VERSION`] for the shape of an entry and the CCN rule tables;
//! - [`Cache::tool_version`] for the binary that wrote it (`CARGO_PKG_VERSION`).
//!
//! A mismatch of either — or a different scan root, or unreadable JSON — is an
//! empty cache, never an error: a tool that refuses to run because its cache is
//! old is worse than a slow tool.
//!
//! ## Shape of a run
//!
//! Every measured file contributes exactly one entry to the cache written at the
//! end of a scan, whether it was a hit or a miss. A fully warm scan therefore
//! reproduces the loaded map exactly and is not rewritten; a scan that added,
//! changed or deleted a file differs and is written.
//!
//! Design constraints:
//! - **No cache file inside the scanned repository.** It would show up in the
//!   next scan and in `git status`. The cache lives under the OS user cache
//!   directory, one file per repository root.
//! - **Corrupt or old cache is a miss, never an error.**
//! - **Atomic-ish replace** (write `.tmp`, then rename) so a killed process
//!   cannot leave a half-written JSON file where the old one was.
//!
//! ## Digest stability
//!
//! `content_hash` uses [`std::collections::hash_map::DefaultHasher`], whose
//! algorithm is not guaranteed across Rust releases. That is acceptable here:
//! a different hash after a toolchain change is a digest mismatch, and a digest
//! mismatch only costs a re-parse.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ccn::types::CcnSummary;

/// Bump when a cached field changes meaning. `CARGO_PKG_VERSION` covers code
/// changes; this covers rule-table changes that do not move the crate version.
///
/// Version 3 added `tool_version` and `Entry::content_hash`. Version 4 added
/// `FunctionCcn::identity` (the serialized shape changed).
pub const CACHE_VERSION: u32 = 4;

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

/// A cheap digest of the bytes an entry was built from.
///
/// Used only by `--verify-cache` (and to detect an entry that predates hashing,
/// whose digest is 0 and therefore never matches nonzero content).
pub fn content_digest(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// One file's cached scan result. Mirrors the fields of `FileLoc` plus the CCN
/// summary, because a hit must be able to reproduce the whole record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub mtime_ns: u128,
    pub size: u64,
    #[serde(default)]
    pub content_hash: u64,
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
    /// `CARGO_PKG_VERSION` of the binary that wrote the file.
    #[serde(default)]
    pub tool_version: String,
    pub root: String,
    pub entries: HashMap<String, Entry>,
}

/// The on-disk shape. Written through this view so `save` always stamps the
/// current identity, even if the in-memory value was built by hand or by
/// `Default`.
#[derive(Serialize)]
struct Persisted<'a> {
    version: u32,
    tool_version: &'a str,
    root: &'a str,
    entries: &'a HashMap<String, Entry>,
}

impl Cache {
    /// An empty cache for `root`, stamped with this build's identity.
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            version: CACHE_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            root: root.into(),
            entries: HashMap::new(),
        }
    }

    /// Load the cache for `root`. Any failure (missing, corrupt, wrong version,
    /// wrong tool version, wrong root) yields an empty cache.
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
            Ok(cache)
                if cache.version == CACHE_VERSION
                    && cache.tool_version == env!("CARGO_PKG_VERSION")
                    && cache.root == root =>
            {
                cache
            }
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
        let persisted = Persisted {
            version: CACHE_VERSION,
            tool_version: env!("CARGO_PKG_VERSION"),
            root: &self.root,
            entries: &self.entries,
        };
        let json = serde_json::to_vec(&persisted).map_err(io::Error::other)?;
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

/// Test-only cache root, pinned once per test binary.
///
/// Unit tests share one process and run in parallel, so mutating
/// `CCN_CACHE_DIR` here would race with concurrent scans. A `OnceLock` gives
/// every test the same private directory without touching the environment;
/// per-root hashing keeps individual fixtures apart.
#[cfg(test)]
static TEST_CACHE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Pin the cache to a directory owned by this test run. Idempotent.
#[cfg(test)]
pub(crate) fn test_cache_dir() -> &'static Path {
    TEST_CACHE_DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ccn-test-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test cache dir");
        dir
    })
}

fn cache_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CACHE_DIR.get() {
        return Some(dir.clone());
    }
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
            content_hash: content_digest(b"hello"),
            lang: "Rust".into(),
            loc_total: 10,
            loc: 8,
            comments: 1,
            blanks: 1,
            ccn: Some(CcnSummary::from_file(crate::ccn::types::FileCcn {
                tier: CcnTier::Exact,
                functions: vec![FunctionCcn {
                    name: "f".into(),
                    identity: "f()".into(),
                    line: 1,
                    end_line: 3,
                    nloc: 3,
                    ccn: 2,
                    params: 0,
                    max_nesting: 0,
                }],
            })),
            is_generated: false,
            is_vendored: false,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ccn-cache-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        let mut cache = Cache::new("r");
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
        let dir = temp_dir("corrupt");
        let path = dir.join("cache.json");
        std::fs::write(&path, b"{not json").unwrap();

        let cache = Cache::load_from(&path, "C:/repo");
        assert!(cache.is_empty());
        assert_eq!(cache.version, 0, "default cache has version 0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("cache.json");

        let root = "C:/roundtrip";
        let mut cache = Cache::new(root);
        cache.insert("src/a.rs".into(), entry(1, 2));
        cache.save_to(&path).expect("save");

        let loaded = Cache::load_from(&path, root);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.tool_version, env!("CARGO_PKG_VERSION"));
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
        assert_eq!(got.content_hash, content_digest(b"hello"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An old shape, a different tool build and a different root are all
    /// "cold", not an error the caller has to handle.
    #[test]
    fn version_identity_mismatch_is_a_cold_cache() {
        let dir = temp_dir("identity");
        let path = dir.join("cache.json");
        let root = "C:/identity";

        let mut cache = Cache::new(root);
        cache.insert("src/a.rs".into(), entry(1, 2));
        cache.save_to(&path).expect("save");

        // A file written by another tool version: still v3 JSON, wrong build.
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json["tool_version"] = serde_json::Value::String("0.0.1-other".into());
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(Cache::load_from(&path, root).is_empty());

        // An older cache shape is rejected on the version field alone.
        json["tool_version"] = serde_json::Value::String(env!("CARGO_PKG_VERSION").into());
        json["version"] = serde_json::Value::from(CACHE_VERSION - 1);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(Cache::load_from(&path, root).is_empty());

        // Correct identity but a different scan root.
        json["version"] = serde_json::Value::from(CACHE_VERSION);
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert_eq!(Cache::load_from(&path, root).len(), 1);
        assert!(Cache::load_from(&path, "C:/elsewhere").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_digest_notices_a_same_length_rewrite() {
        // The stale-content probe: equal length, different bytes.
        let before = b"int f(void) { if(x) { return 1; } }\n";
        let after = b"int f(void) { /*x*/ { return 1; } }\n";
        assert_eq!(before.len(), after.len());
        let digest = content_digest(before);
        assert_ne!(
            digest,
            content_digest(after),
            "same length, different bytes"
        );
        assert_ne!(digest, 0, "a real file must not look like a pre-v3 entry");
    }
}
