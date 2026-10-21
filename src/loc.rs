//! Phase 1: LOC collection via `tokei`, plus the phase-2 CCN pass.
//!
//! tokei already does the hard parts (gitignore handling, 150+ language
//! definitions, embedded-language blobs, rayon). We only normalise its output
//! into our own contract, then read each surviving file again for the
//! tree-sitter CCN engine. The two passes are separate on purpose: LOC is
//! cheap and cached by the OS, AST parsing is the expensive one, and fusing
//! them would re-parse files whose LOC already answered the question.
//!
//! See `docs/PLAN.md` §4.2.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::ccn::types::CcnTier;
use crate::generated;
use crate::model::{CcnSkip, ExcludedFiles, FileLoc, LangSummary, RepoCcn, ScanResult};

/// Files above this size are not opened by the CCN pass. Generated bundles and
/// checked-in vendored blobs live here and a 40 MB `min.js` would dominate both
/// the runtime and the histogram.
pub(crate) const MAX_CCN_BYTES: u64 = 8 * 1024 * 1024;

/// User-facing spellings for tokei's display names.
///
/// `--lang` is compared against [`tokei::LanguageType::name`], which is a
/// display name (`C++`, `C#`, `JavaScript`). Nobody types those on a command
/// line, so every alias resolves to the display name and *both* sides of the
/// comparison go through [`canonical_lang`]. Anything not listed here falls
/// back to the historical case-insensitive comparison, so a tokei name (or an
/// exact case variant) still works.
const LANG_ALIASES: &[(&str, &str)] = &[
    ("cpp", "C++"),
    ("c++", "C++"),
    ("cxx", "C++"),
    ("cc", "C++"),
    ("c", "C"),
    ("h", "C"),
    ("header", "C"),
    ("cs", "C#"),
    ("csharp", "C#"),
    ("c#", "C#"),
    ("js", "JavaScript"),
    ("javascript", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("typescript", "TypeScript"),
    ("tsx", "TSX"),
    ("py", "Python"),
    ("python", "Python"),
    ("rs", "Rust"),
    ("rust", "Rust"),
    ("go", "Go"),
    ("golang", "Go"),
    ("java", "Java"),
];

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Directory or file to scan.
    pub root: PathBuf,
    /// Include dotfiles / dot-directories.
    pub hidden: bool,
    /// Ignore every ignore file: `.gitignore`, `.ignore`, `.tokeignore` and
    /// `.p4ignore`.
    pub no_ignore: bool,
    /// Do not follow symbolic links.
    ///
    /// Links are followed by default: a workspace that symlinks its sources
    /// would otherwise silently lose them. The `ignore` crate stops a link
    /// cycle on its own.
    pub no_follow_links: bool,
    /// Skip the built-in build-output excludes.
    pub no_default_excludes: bool,
    /// Language allow-list. Empty means "everything". Aliases (see
    /// [`LANG_ALIASES`]) and tokei display names are both accepted.
    pub langs: Vec<String>,
    /// Skip the CCN pass entirely (LOC only).
    pub no_ccn: bool,
    /// Analyse generated and vendored files as first-party code.
    ///
    /// When `false` (the default) their `is_generated` / `is_vendored` flags are
    /// set and they are dropped from every metric in the report. When `true`,
    /// detection is skipped entirely: the flags stay `false`, the exclusion
    /// counters stay 0, and a checked-in bundle competes in the histogram.
    pub include_generated: bool,
    /// Disable the persistent per-file cache (always parse every file).
    pub no_cache: bool,
    /// Re-read every cache hit and recompute its content digest, re-parsing on
    /// a mismatch.
    ///
    /// The default warm path trusts `(mtime, size)`. A writer that preserves
    /// both can defeat that, so gates and NFS checkouts should pay for this
    /// full read instead (see `cache.rs`).
    pub verify_cache: bool,
    /// Only analyse files changed since this git revision.
    pub since: Option<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            hidden: false,
            no_ignore: false,
            no_follow_links: false,
            no_default_excludes: false,
            langs: Vec::new(),
            no_ccn: false,
            include_generated: false,
            no_cache: false,
            verify_cache: false,
            since: None,
        }
    }
}

/// Build-output directories that are never "our code".
///
/// This list exists because the tool must give a sane answer in a directory
/// that has no `.gitignore` yet — the alternative is counting `target/` and
/// generated sources, which is exactly the pollution `docs/PLAN.md` §10 warns
/// about. `.git` is always excluded; it is not source under any setting.
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
];

#[derive(Debug)]
pub enum ScanError {
    NotFound(PathBuf),
    NotADirectory(PathBuf),
    /// `--since <rev>` could not be resolved (not a git repo, unknown rev).
    InvalidSince(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::NotFound(p) => write!(f, "path does not exist: {}", p.display()),
            ScanError::NotADirectory(p) => write!(f, "not a directory: {}", p.display()),
            ScanError::InvalidSince(rev) => write!(
                f,
                "cannot resolve --since {rev}: no git or Perforce workspace, or unknown revision"
            ),
        }
    }
}

impl std::error::Error for ScanError {}

/// Never rely on the caller's cwd for ignore semantics; resolve once.
///
/// A file is a valid scan target (`ccn src/foo.c`); anything that is neither a
/// file nor a directory is still rejected.
fn canonical_root(root: &Path) -> Result<PathBuf, ScanError> {
    let joined = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(root)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|_| ScanError::NotFound(root.to_path_buf()))?;
    if !canonical.is_dir() && !canonical.is_file() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }
    Ok(canonical)
}

/// Resolve a `--lang` value to its tokei display name, if it is an alias.
fn canonical_lang(name: &str) -> Option<&'static str> {
    let key = name.trim().to_ascii_lowercase();
    LANG_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, canonical)| *canonical)
}

/// Does the requested language (alias or display name) mean `actual`?
fn lang_matches(wanted: &str, actual: &str) -> bool {
    match canonical_lang(wanted) {
        Some(canonical) => canonical.eq_ignore_ascii_case(actual),
        // Unknown spellings keep the case-insensitive fallback, so a tokei
        // display name (`C++`, `JavaScript`, ...) still works.
        None => wanted.trim().eq_ignore_ascii_case(actual),
    }
}

fn lang_allowed(opts: &ScanOptions, lang: tokei::LanguageType) -> bool {
    opts.langs.is_empty()
        || opts
            .langs
            .iter()
            .any(|wanted| lang_matches(wanted, lang.name()))
}

/// Scan `opts.root`. Never fails on unreadable individual files: a file that
/// cannot be read is reported with [`CcnSkip::ReadError`] and zero lines, so
/// the file count stays honest and a baseline comparison can tell "could not
/// measure" apart from "nothing to measure".
///
/// Two modes share one walk:
/// - **cold**: read the file once, count LOC and CCN from the same bytes, then
///   store the record in the cache;
/// - **warm**: a stat per file and one cache lookup. No read, no AST — except
///   with [`ScanOptions::verify_cache`], which reads each hit to check its
///   digest.
pub fn scan(opts: &ScanOptions) -> Result<ScanResult, ScanError> {
    let target = canonical_root(&opts.root)?;

    // Single-file mode (`ccn src/foo.c`). Everything downstream — cache keys,
    // VCS lookup, `metrics::build` -> `vcs::detect`, `--baseline`, `--hotspots`
    // — is written against a *directory* root, and a file path makes those
    // calls fail. So the scan root becomes the file's parent and the file stays
    // in `files[].path` relative to that parent (its VCS key, unchanged from a
    // directory scan of the same tree).
    //
    // Trade-off, accepted: the report's `root` shows the containing directory
    // rather than the file, and `ScanResult` needs no new field.
    let (root, single_file) = if target.is_file() {
        let parent = target
            .parent()
            .map_or_else(|| target.clone(), Path::to_path_buf);
        (parent, Some(target))
    } else {
        (target, None)
    };
    let root_display = display_root(&root);

    let config = tokei::Config {
        hidden: Some(opts.hidden),
        no_ignore: Some(opts.no_ignore),
        ..Default::default()
    };

    // `--no-ccn` sees no CCN to cache; `--no-cache` means "prove it"; a
    // language filter or `--include-generated` changes which files are measured
    // or how they are classified, and letting those runs rewrite the shared
    // cache would poison the next full scan. All four bypass it.
    let use_cache = !opts.no_cache
        && !opts.no_ccn
        && opts.langs.is_empty()
        && !opts.include_generated
        && opts.since.is_none();
    let load_started = std::time::Instant::now();
    let old = if use_cache {
        crate::cache::Cache::load(&root_display)
    } else {
        crate::cache::Cache::default()
    };
    let load_elapsed = load_started.elapsed();
    // Every measured file lands here, hit or miss, so a warm run reproduces
    // the loaded map exactly and can be left untouched.
    let mut next = crate::cache::Cache::new(root_display.clone());

    let classify_exclusions = !opts.include_generated;

    let walk_started = std::time::Instant::now();
    // A single file is never walked: the walker would descend the parent (or
    // return the file only when it is the root, mixing the two modes), and
    // "exactly one candidate" must hold even if the file is gitignored.
    let mut candidates = match &single_file {
        Some(file) => single_candidate(file, opts, &config).into_iter().collect(),
        None => walk_source_files(&root, opts, &config),
    };
    let walk_elapsed = walk_started.elapsed();

    // Diff mode: keep only files this change touched. Deleted files are not in
    // the set, which is correct — there is no LOC to measure for them.
    if let Some(rev) = &opts.since {
        let changed = crate::vcs::detect(&root)
            .and_then(|backend| crate::vcs::changed_since(&root, backend, rev))
            .ok_or_else(|| ScanError::InvalidSince(rev.clone()))?;
        candidates.retain(|(abs, _)| changed.contains(&relative_display(&root, abs)));
    }

    let resolve_started = std::time::Instant::now();
    let resolved: Vec<Option<(FileLoc, Option<crate::cache::Entry>)>> = candidates
        .par_iter()
        .map(|(abs, lang)| {
            resolve_file(abs, *lang, &root, &config, opts, &old, classify_exclusions)
        })
        .collect();

    if std::env::var_os("CCN_TIMING").is_some() {
        eprintln!(
            "ccn: cache load {:?} ({} entries) · walk {} in {:?} · resolve in {:?}",
            load_elapsed,
            old.len(),
            candidates.len(),
            walk_elapsed,
            resolve_started.elapsed()
        );
    }

    let mut files: Vec<FileLoc> = Vec::with_capacity(resolved.len());
    for item in resolved.into_iter().flatten() {
        let (loc, entry) = item;
        if let Some(entry) = entry {
            next.insert(loc.path.clone(), entry);
        }
        files.push(loc);
    }

    // Deterministic output: JSON diffs between runs should only move when the
    // code moved.
    files.sort_by(|a, b| a.path.cmp(&b.path));

    // Rewriting a multi-megabyte cache on a fully-cached run would cost more
    // than the parse it saves. The map comparison is the whole condition: a
    // warm run carries every hit forward unchanged, so `next == old`; an added,
    // edited or deleted file changes it and is persisted.
    if use_cache && next.entries != old.entries {
        // A failed cache write must never fail the scan; the next run is simply
        // cold again.
        let _ = next.save();
    }

    // Single-threaded fold: cheap, and it keeps the aggregate rules in one
    // readable place.
    let mut functions_total = 0usize;
    let mut files_without_ccn = 0usize;
    let mut files_excluded = ExcludedFiles::default();
    for file in &files {
        // One file, one bucket: generated-and-vendored counts as vendored, the
        // same precedence the classifier uses for its reason.
        if file.is_vendored {
            files_excluded.vendored += 1;
        } else if file.is_generated {
            files_excluded.generated += 1;
        }
        match file.ccn.as_ref() {
            // Only function-level tiers may contribute functions. A heuristic
            // file legitimately has none (types.rs keeps `functions == 0`).
            Some(ccn) if ccn.tier != CcnTier::Heuristic => functions_total += ccn.functions,
            Some(_) => {}
            None => files_without_ccn += 1,
        }
    }

    let mut summaries: Vec<LangSummary> = Vec::new();
    for file in &files {
        match summaries.iter_mut().find(|s| s.lang == file.lang) {
            Some(summary) => {
                summary.files += 1;
                summary.loc += file.loc;
                summary.comments += file.comments;
                summary.blanks += file.blanks;
            }
            None => summaries.push(LangSummary {
                lang: file.lang.clone(),
                files: 1,
                loc: file.loc,
                comments: file.comments,
                blanks: file.blanks,
            }),
        }
    }
    summaries.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.lang.cmp(&b.lang)));

    let mut result = ScanResult {
        root: root_display,
        files_analyzed: files.len(),
        loc_total: 0,
        loc: 0,
        comments: 0,
        blanks: 0,
        files_excluded,
        functions_total,
        ccn: None,
        languages: summaries,
        files,
    };
    // Sum from the per-file records, not from per-language totals, so `loc`
    // always equals `sum(files[].loc)` and cannot drift.
    for f in &result.files {
        result.loc_total += f.loc_total;
        result.loc += f.loc;
        result.comments += f.comments;
        result.blanks += f.blanks;
    }

    // No summaries at all (`--no-ccn`, or every file unreadable/oversize)
    // means there is nothing to aggregate; omit the block rather than emit a
    // row of zeroes that looks like a measurement.
    if result.files.iter().any(|f| f.ccn.is_some()) {
        result.ccn = Some(RepoCcn::from_files(
            &result.files,
            result.loc,
            files_without_ccn,
        ));
    }

    Ok(result)
}

/// The one candidate for a single-file scan, or `None` when tokei does not
/// recognise the path (or `--lang` excludes it).
fn single_candidate(
    file: &Path,
    opts: &ScanOptions,
    config: &tokei::Config,
) -> Option<(PathBuf, tokei::LanguageType)> {
    let lang = tokei::LanguageType::from_path(file, config)?;
    // tokei maps `.txt` to its `Text` language. A lone text file is not source
    // code and carries no CCN, so a single-file request for one measures
    // nothing rather than reporting a file with zero complexity.
    if lang == tokei::LanguageType::Text {
        return None;
    }
    lang_allowed(opts, lang).then(|| (file.to_path_buf(), lang))
}

/// Walk the tree the same way tokei's bulk scan did: gitignore-aware, hidden
/// files optional, build output excluded by name.
fn walk_source_files(
    root: &Path,
    opts: &ScanOptions,
    config: &tokei::Config,
) -> Vec<(PathBuf, tokei::LanguageType)> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(!opts.hidden)
        .git_ignore(!opts.no_ignore)
        .git_global(!opts.no_ignore)
        .git_exclude(!opts.no_ignore)
        .ignore(!opts.no_ignore)
        .parents(!opts.no_ignore)
        // Symlinked sources and symlinked workspace roots are ordinary layouts.
        // The walker keeps the link path (so relative keys and VCS keys stay
        // the ones a user sees) while measuring the target; it also detects
        // cycles itself. `--no-follow-links` restores the strict behaviour.
        .follow_links(!opts.no_follow_links);

    // Two extra ignore files, both with gitignore syntax and both honoured in
    // every directory the way `.ignore` is: `.tokeignore` is our own knob, and
    // `.p4ignore` is the Perforce equivalent (a P4 workspace usually has no
    // `.gitignore`). `--no-ignore` turns them off with the other sources, so
    // the flag means "no ignore rules at all" rather than "most of them".
    if !opts.no_ignore {
        builder
            .add_custom_ignore_filename(".tokeignore")
            .add_custom_ignore_filename(".p4ignore");
    }

    let allow_defaults = opts.no_default_excludes;
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        if name == ".git" {
            return false;
        }
        !(!allow_defaults && DEFAULT_EXCLUDES.contains(&name.as_ref()))
    });

    let mut out = Vec::new();
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(lang) = tokei::LanguageType::from_path(entry.path(), config) {
            if lang_allowed(opts, lang) {
                out.push((entry.into_path(), lang));
            }
        }
    }
    out
}

/// Resolve one file: cache hit -> cached record; miss -> read, measure and
/// build a new cache entry.
///
/// The returned entry is the record that belongs in the next cache in *both*
/// cases. A hit returns a clone of the cached entry, which is what lets a warm
/// scan reproduce the loaded cache instead of emptying it.
fn resolve_file(
    abs: &Path,
    lang: tokei::LanguageType,
    root: &Path,
    config: &tokei::Config,
    opts: &ScanOptions,
    old: &crate::cache::Cache,
    classify_exclusions: bool,
) -> Option<(FileLoc, Option<crate::cache::Entry>)> {
    let path = relative_display(root, abs);

    // One stat up front: this is the entire warm path.
    let before = crate::cache::Fingerprint::of(abs);
    let mut verified_bytes: Option<Vec<u8>> = None;

    if let Some(fp) = before {
        if let Some(hit) = old.get(&path, fp) {
            if !opts.verify_cache {
                return Some((file_loc_from_entry(path, hit), Some(hit.clone())));
            }
            // Slow but correct: mtime+size said "unchanged", the bytes decide.
            match std::fs::read(abs) {
                Ok(bytes) if crate::cache::content_digest(&bytes) == hit.content_hash => {
                    return Some((file_loc_from_entry(path, hit), Some(hit.clone())));
                }
                // Digest mismatch: re-measure from the bytes we already hold.
                Ok(bytes) => verified_bytes = Some(bytes),
                // Unreadable now: measuring nothing is honest, and the stale
                // entry must not survive into the next cache.
                Err(_) => return Some((unreadable_loc(path, lang), None)),
            }
        }
    }

    // One read serves LOC, generated-file markers and CCN. Reusing the bytes
    // also means the digest stored with the entry describes exactly what was
    // parsed, not a second read that may have seen something else.
    let bytes = match verified_bytes {
        Some(bytes) => bytes,
        None => match std::fs::read(abs) {
            Ok(bytes) => bytes,
            // A read failure is recorded, not silently dropped: dropping it is
            // what let an unmeasured file disappear from a baseline gate.
            Err(_) => return Some((unreadable_loc(path, lang), None)),
        },
    };
    let digest = crate::cache::content_digest(&bytes);

    // `LanguageType::parse` decodes BOM-marked UTF-16/UTF-8 before counting;
    // `tokei_slice` reproduces that for the bytes we already read.
    let stats = lang
        .parse_from_slice(tokei_slice(&bytes), config)
        .summarise();
    let loc_total = stats.lines();
    if loc_total == 0 {
        return None;
    }

    let class = if classify_exclusions {
        // Path rules plus generator markers in the head of the same bytes.
        generated::classify(&path, &bytes)
    } else {
        generated::Classification::SOURCE
    };

    // The size guard keeps AST parsing off minified bundles; the bytes are
    // already in memory because LOC needed them. The reason is recorded on the
    // file, because "no CCN" alone cannot distinguish a deliberate exclusion
    // from a measurement that never happened.
    let (ccn, ccn_skip) = if opts.no_ccn {
        (None, None)
    } else if bytes.len() as u64 > MAX_CCN_BYTES {
        (None, Some(CcnSkip::Oversize))
    } else {
        let summary = crate::ccn::analyze_file(Path::new(&path), &bytes, lang.name());
        let wired = has_ccn_grammar(&path, lang.name());
        let skip = if wired {
            None
        } else {
            Some(CcnSkip::NoGrammar)
        };
        (Some(summary), skip)
    };

    // Fingerprint *after* the read. If the file changed between the stat and
    // the read, the record is still reported but not cached: the next run is a
    // miss instead of a hit on bytes we did not measure. Best effort — without
    // the pre-read stat there is nothing to compare against.
    let entry = crate::cache::Fingerprint::of(abs).and_then(|after| {
        if before.is_some_and(|before| before != after) {
            return None;
        }
        Some(crate::cache::Entry {
            mtime_ns: after.mtime_ns,
            size: after.size,
            content_hash: digest,
            lang: lang.name().to_string(),
            loc_total,
            loc: stats.code,
            comments: stats.comments,
            blanks: stats.blanks,
            ccn: ccn.clone(),
            ccn_skip,
            is_generated: class.is_generated,
            is_vendored: class.is_vendored,
        })
    });

    Some((
        FileLoc {
            path,
            lang: lang.name().to_string(),
            loc_total,
            loc: stats.code,
            comments: stats.comments,
            blanks: stats.blanks,
            ccn,
            ccn_skip,
            is_generated: class.is_generated,
            is_vendored: class.is_vendored,
        },
        entry,
    ))
}

/// The record for a file whose bytes could not be read.
///
/// `loc_total` stays 0 because no line count was established, and the skip
/// reason is recorded so the baseline accounting can say "unreadable" instead
/// of silently losing the file.
fn unreadable_loc(path: String, lang: tokei::LanguageType) -> FileLoc {
    FileLoc {
        path,
        lang: lang.name().to_string(),
        loc_total: 0,
        loc: 0,
        comments: 0,
        blanks: 0,
        ccn: None,
        ccn_skip: Some(CcnSkip::ReadError),
        is_generated: false,
        is_vendored: false,
    }
}

/// `true` when the language has a tier-1 tree-sitter grammar wired in.
///
/// The extension wins over tokei's name, exactly like
/// [`crate::ccn::analyze_file`], so the accounting names the same language the
/// parser picked.
fn has_ccn_grammar(path: &str, tokei_name: &str) -> bool {
    crate::ccn::Lang::from_path(path)
        .or_else(|| crate::ccn::Lang::from_tokei_name(tokei_name))
        .is_some()
}

/// Reproduce a cache hit as the report record it stood for.
fn file_loc_from_entry(path: String, entry: &crate::cache::Entry) -> FileLoc {
    FileLoc {
        path,
        lang: entry.lang.clone(),
        loc_total: entry.loc_total,
        loc: entry.loc,
        comments: entry.comments,
        blanks: entry.blanks,
        ccn: entry.ccn.clone(),
        ccn_skip: entry.ccn_skip,
        is_generated: entry.is_generated,
        is_vendored: entry.is_vendored,
    }
}

/// The byte view tokei's own `LanguageType::parse` would have counted.
///
/// `parse` streams a file through BOM-sniffing decoding; the scanner now reads
/// once and calls `parse_from_slice`, so the two encodings that can be
/// recognised without a guess are handled here: a UTF-8 BOM is stripped and a
/// UTF-16 BOM is transcoded to UTF-8. Unmarked bytes are passed through exactly
/// as tree-sitter will see them; legacy single-byte encodings remain a
/// documented limit of the scanner, not something this function guesses at.
fn tokei_slice(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;

    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Cow::Borrowed(rest);
    }
    let (rest, little_endian) = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (rest, true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (rest, false)
    } else {
        return Cow::Borrowed(bytes);
    };
    let units: Vec<u16> = rest
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes(*pair)
            } else {
                u16::from_be_bytes(*pair)
            }
        })
        .collect();
    Cow::Owned(String::from_utf16_lossy(&units).into_bytes())
}

/// Render the scan root with forward slashes and without Windows' verbatim
/// (`\\?\`) prefix that `canonicalize` adds. The prefix is a real path but
/// noise in every report and JSON consumer.
fn display_root(root: &Path) -> String {
    let s = root.display().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.replace('\\', "/")
}

/// Render a report path relative to the scan root with forward slashes, so a
/// Windows report and a Linux CI report are textually comparable.
fn relative_display(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn relative_paths_use_forward_slashes() {
        let root = Path::new("C:\\repo");
        let path = Path::new("C:\\repo\\src\\main.rs");
        assert_eq!(relative_display(root, path), "src/main.rs");
    }

    #[test]
    #[cfg(unix)]
    fn relative_paths_use_forward_slashes() {
        let root = Path::new("/repo");
        let path = Path::new("/repo/src/main.rs");
        assert_eq!(relative_display(root, path), "src/main.rs");
    }

    #[test]
    fn display_root_strips_verbatim_prefix() {
        let root = Path::new(r"\\?\C:\repo");
        assert_eq!(display_root(root), "C:/repo");
    }

    #[test]
    fn paths_outside_root_are_kept_absolute() {
        let root = Path::new("C:\\repo");
        let path = Path::new("D:\\other\\x.rs");
        assert!(!relative_display(root, path).is_empty());
    }

    /// Tiny on-disk fixture. `tempfile` is not a dependency and this is the
    /// whole surface we need from it: a unique directory and best-effort
    /// cleanup.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            // Process id + tag keeps parallel tests and repeated runs apart.
            let dir = std::env::temp_dir().join(format!("ccn-loc-{}-{}", std::process::id(), tag));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            std::fs::write(
                dir.join("sample.rs"),
                concat!(
                    "// fixture\n",
                    "fn simple() {\n",
                    "    if true {\n",
                    "        println!(\"x\");\n",
                    "    }\n",
                    "}\n",
                ),
            )
            .expect("write fixture");
            Self { dir }
        }

        fn options(&self, no_ccn: bool) -> ScanOptions {
            ScanOptions {
                root: self.dir.clone(),
                no_ccn,
                ..Default::default()
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The scan root as [`scan`] reports it.
    fn root_of(fixture: &Fixture) -> String {
        display_root(&fixture.dir.canonicalize().expect("canonical fixture root"))
    }

    /// This test run's cache file for `root`, with the cache directory pinned.
    fn cache_file(root: &str) -> PathBuf {
        crate::cache::test_cache_dir();
        crate::cache::cache_path(root).expect("cache path")
    }

    /// Start every cache test from a cold cache, whatever earlier runs left.
    fn forget_cache(root: &str) -> PathBuf {
        let path = cache_file(root);
        let _ = std::fs::remove_file(&path);
        path
    }

    fn cached_entries(root: &str) -> usize {
        crate::cache::Cache::load(root).len()
    }

    fn file_ccn_max(result: &ScanResult, name: &str) -> u32 {
        result
            .files
            .iter()
            .find(|f| f.path == name)
            .unwrap_or_else(|| panic!("missing {name} in {:?}", result.files))
            .ccn
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has no CCN summary"))
            .max
    }

    /// The reported 2/0/2/0 oscillation: a hit used to return no entry, so a
    /// warm scan wrote an empty cache. Entry count must now be stable, and a
    /// fully warm scan must not rewrite the file at all.
    #[test]
    fn four_identical_scans_keep_every_cache_entry() {
        let fixture = Fixture::new("carry-forward");
        std::fs::write(
            fixture.dir.join("second.rs"),
            "fn second() {\n    if true {\n        return;\n    }\n}\n",
        )
        .expect("write second fixture");

        let opts = fixture.options(false);
        let root = root_of(&fixture);
        let path = forget_cache(&root);

        let first = scan(&opts).expect("cold scan");
        assert_eq!(first.files_analyzed, 2);
        assert_eq!(cached_entries(&root), 2, "a cold scan caches every file");
        let written = std::fs::metadata(&path)
            .expect("cache file exists")
            .modified()
            .expect("cache mtime");

        for run in 1..4 {
            let result = scan(&opts).expect("warm scan");
            assert_eq!(result.files_analyzed, 2);
            assert_eq!(
                cached_entries(&root),
                2,
                "warm run {run} must carry both hits forward, not empty the cache"
            );
        }

        let after = std::fs::metadata(&path)
            .expect("cache file exists")
            .modified()
            .expect("cache mtime");
        assert_eq!(
            written, after,
            "a fully warm scan must not rewrite the cache"
        );
    }

    /// The cache describes the files that were just measured, so a deletion
    /// removes its entry instead of leaving a ghost behind.
    #[test]
    fn deleting_a_file_drops_its_cache_entry() {
        let fixture = Fixture::new("delete-entry");
        std::fs::write(fixture.dir.join("second.rs"), "fn gone() {}\n")
            .expect("write second fixture");

        let opts = fixture.options(false);
        let root = root_of(&fixture);
        forget_cache(&root);

        assert_eq!(scan(&opts).expect("cold scan").files_analyzed, 2);
        assert_eq!(cached_entries(&root), 2);

        std::fs::remove_file(fixture.dir.join("second.rs")).expect("delete fixture");
        assert_eq!(scan(&opts).expect("scan after delete").files_analyzed, 1);

        let cache = crate::cache::Cache::load(&root);
        assert_eq!(cache.len(), 1);
        assert!(
            !cache.entries.contains_key("second.rs"),
            "deleted files must not keep an entry: {:?}",
            cache.entries
        );
    }

    /// The stale-content probe: same byte length, restored nanosecond mtime.
    /// Without `--verify-cache` the fast key is unchanged and the old number may
    /// be reused (documented); with it, the file is re-parsed.
    #[test]
    fn verify_cache_detects_a_same_size_same_mtime_rewrite() {
        let fixture = Fixture::new("verify-cache");
        let file = fixture.dir.join("unit.c");
        let before = "int f(void) { if(x) { return 1; } return 0; }\n";
        let after = "int f(void) { /*x*/ { return 1; } return 0; }\n";
        assert_eq!(before.len(), after.len(), "the probe needs equal sizes");
        std::fs::write(&file, before).expect("write C fixture");

        let opts = fixture.options(false);
        let root = root_of(&fixture);
        forget_cache(&root);

        let cold = scan(&opts).expect("cold scan");
        assert_eq!(file_ccn_max(&cold, "unit.c"), 2);

        let original = crate::cache::Fingerprint::of(&file).expect("fingerprint");
        let original_mtime = std::fs::metadata(&file)
            .expect("stat fixture")
            .modified()
            .expect("fixture mtime");
        std::fs::write(&file, after).expect("rewrite C fixture");
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open to restore mtime")
            .set_modified(original_mtime)
            .expect("restore original mtime");
        assert_eq!(
            crate::cache::Fingerprint::of(&file),
            Some(original),
            "the fast key must look unchanged for this to be a stale-cache probe"
        );

        // Control: the default fast path may reuse the cached record.
        let fast = scan(&opts).expect("warm scan");
        assert_eq!(
            file_ccn_max(&fast, "unit.c"),
            2,
            "without --verify-cache, mtime+size is still a hit (documented)"
        );

        // Verified: the digest decides, and the entry is replaced.
        let verified_opts = ScanOptions {
            verify_cache: true,
            ..fixture.options(false)
        };
        let verified = scan(&verified_opts).expect("verified scan");
        assert_eq!(file_ccn_max(&verified, "unit.c"), 1);

        // The corrected entry is cached, so the fast path agrees afterwards.
        let afterwards = scan(&opts).expect("warm scan after verification");
        assert_eq!(file_ccn_max(&afterwards, "unit.c"), 1);
    }

    /// `--no-cache` proves the measurement and must not write (or reuse) the
    /// persistent cache.
    #[test]
    fn no_cache_writes_no_cache_file() {
        let fixture = Fixture::new("no-cache-writes");
        let opts = ScanOptions {
            no_cache: true,
            ..fixture.options(false)
        };
        let root = root_of(&fixture);
        let path = forget_cache(&root);

        let result = scan(&opts).expect("scan without cache");
        assert_eq!(result.files_analyzed, 1);
        assert!(result.files[0].ccn.is_some(), "measurement still happens");
        assert!(
            !path.exists(),
            "--no-cache must not create {}",
            path.display()
        );
        assert_eq!(cached_entries(&root), 0);
    }

    #[test]
    fn scan_attaches_a_ccn_summary_per_file() {
        let fixture = Fixture::new("with-ccn");
        let result = scan(&fixture.options(false)).expect("scan fixture");

        assert_eq!(result.files_analyzed, 1);
        let file = &result.files[0];
        assert_eq!(file.path, "sample.rs");
        let ccn = file.ccn.as_ref().expect("CCN must be attached");
        // The engine may or may not have landed, so the tier is not asserted;
        // we only require a summary that respects the contract.
        if ccn.tier == CcnTier::Heuristic {
            assert_eq!(ccn.functions, 0);
            assert!(ccn.file_ccn.is_some());
        }
        assert!(
            result.ccn.is_some(),
            "repo aggregates follow the file records"
        );
        assert_eq!(result.functions_total, ccn.functions);
        assert!(result.loc_total == result.loc + result.comments + result.blanks);
    }

    #[test]
    fn no_ccn_skips_the_second_pass() {
        let fixture = Fixture::new("no-ccn");
        let result = scan(&fixture.options(true)).expect("scan fixture");

        assert_eq!(result.files_analyzed, 1);
        assert!(result.files[0].ccn.is_none());
        assert!(result.ccn.is_none());
        assert_eq!(result.functions_total, 0);
        // LOC contract unchanged by the flag.
        assert_eq!(result.loc, 5);
        assert_eq!(result.comments, 1);
        assert_eq!(result.loc_total, 6);
    }

    /// `Fixture` plus one marked generated file and one vendored file. `vendor`
    /// is not in `DEFAULT_EXCLUDES`, so both reach phase 1/2; `node_modules`
    /// would be filtered by tokei before we ever see it.
    fn fixture_with_exclusions(tag: &str) -> Fixture {
        let fixture = Fixture::new(tag);
        std::fs::write(
            fixture.dir.join("codegen.js"),
            concat!(
                "// @generated by codegen, do not edit\n",
                "export function generated() {\n",
                "  if (true) { return 1; }\n",
                "  return 2;\n",
                "}\n",
            ),
        )
        .expect("write generated fixture");
        let vendor = fixture.dir.join("vendor").join("lib");
        std::fs::create_dir_all(&vendor).expect("create vendor dir");
        std::fs::write(
            vendor.join("bundle.js"),
            "function vendored(){if(true){return 1;}return 2;}\n",
        )
        .expect("write vendored fixture");
        fixture
    }

    #[test]
    fn generated_and_vendored_files_are_flagged_and_counted() {
        let fixture = fixture_with_exclusions("exclusions");
        let result = scan(&fixture.options(false)).expect("scan fixture");

        assert_eq!(result.files_analyzed, 3, "excluded files stay in the JSON");
        assert_eq!(result.files_excluded.generated, 1);
        assert_eq!(result.files_excluded.vendored, 1);

        let find = |name: &str| {
            result
                .files
                .iter()
                .find(|f| f.path == name)
                .unwrap_or_else(|| panic!("missing {name} in {:?}", result.files))
        };
        assert!(find("codegen.js").is_generated);
        assert!(!find("codegen.js").is_vendored);
        assert!(find("vendor/lib/bundle.js").is_vendored);
        assert!(!find("sample.rs").is_excluded());

        let report = crate::metrics::build(&result);
        assert_eq!(report.files_analyzed, 3);
        assert_eq!(report.files_measured, 1, "only sample.rs is measured");
        assert_eq!(report.stats.files_excluded_generated, 1);
        assert_eq!(report.stats.files_excluded_vendored, 1);
        assert_eq!(report.loc.total, 1, "the LOC histogram is measured-only");
    }

    #[test]
    fn include_generated_disables_detection_entirely() {
        let fixture = fixture_with_exclusions("include-generated");
        let opts = ScanOptions {
            include_generated: true,
            ..fixture.options(false)
        };
        let result = scan(&opts).expect("scan fixture");

        assert_eq!(result.files_analyzed, 3);
        assert_eq!(result.files_excluded.generated, 0);
        assert_eq!(result.files_excluded.vendored, 0);
        assert!(
            result.files.iter().all(|f| !f.is_excluded()),
            "--include-generated keeps every file first-party"
        );

        let report = crate::metrics::build(&result);
        assert_eq!(report.files_measured, 3);
        assert_eq!(report.stats.files_excluded_generated, 0);
        assert_eq!(report.stats.files_excluded_vendored, 0);
        assert_eq!(report.loc.total, 3);
    }

    #[test]
    fn no_ccn_excludes_generated_and_vendored_files_the_same_way() {
        // LOC needs the bytes anyway, so `--no-ccn` sees generator markers too:
        // the flag must not change which files the histogram describes.
        let fixture = fixture_with_exclusions("no-ccn-exclusions");
        let result = scan(&fixture.options(true)).expect("scan fixture");

        assert_eq!(result.files_excluded.vendored, 1);
        assert_eq!(
            result.files_excluded.generated, 1,
            "the single read exposes the marker with or without the CCN pass"
        );
        let report = crate::metrics::build(&result);
        assert_eq!(
            report.files_measured, 1,
            "only sample.rs is first-party source"
        );
    }

    #[test]
    fn lang_alias_cpp_keeps_cpp_and_drops_c() {
        let fixture = Fixture::new("lang-alias-cpp");
        std::fs::write(fixture.dir.join("unit.c"), "int f(void) { return 0; }\n")
            .expect("write C fixture");
        std::fs::write(fixture.dir.join("unit.cpp"), "int g() { return 0; }\n")
            .expect("write C++ fixture");

        let opts = ScanOptions {
            langs: vec!["cpp".to_string()],
            ..fixture.options(true)
        };
        let result = scan(&opts).expect("scan fixture");

        assert_eq!(result.files_analyzed, 1, "{:?}", result.files);
        assert_eq!(result.files[0].path, "unit.cpp");
        assert_eq!(result.files[0].lang, "C++");
        assert_eq!(result.languages.len(), 1);
        assert_eq!(result.languages[0].lang, "C++");

        // The canonical spelling and the other aliases agree with the alias.
        for wanted in ["C++", "cxx", "cc"] {
            let opts = ScanOptions {
                langs: vec![wanted.to_string()],
                ..fixture.options(true)
            };
            let result = scan(&opts).expect("scan fixture");
            assert_eq!(result.files_analyzed, 1, "--lang {wanted}");
        }
        // `c` must not match C++.
        let opts = ScanOptions {
            langs: vec!["c".to_string()],
            ..fixture.options(true)
        };
        let result = scan(&opts).expect("scan fixture");
        assert_eq!(result.files_analyzed, 1);
        assert_eq!(result.files[0].path, "unit.c");
    }

    /// The reported paths, sorted: [`scan`] already sorts by path, so this is
    /// exactly the order a caller sees.
    fn scanned_paths(result: &ScanResult) -> Vec<String> {
        result.files.iter().map(|f| f.path.clone()).collect()
    }

    fn write_c(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "int f(void) { return 0; }\n").expect("write C fixture");
    }

    /// `.tokeignore` is advertised by the CLI, so it must actually be one of
    /// the walker's ignore sources. The file itself is a dotfile with no known
    /// extension and must never show up as measured source, not even with
    /// `--hidden` (which makes the walker see it).
    #[test]
    fn tokeignore_excludes_a_matching_file() {
        let fixture = Fixture::new("tokeignore-root");
        write_c(&fixture.dir, "ignored.c");
        std::fs::write(fixture.dir.join(".tokeignore"), "ignored.c\n").expect("write .tokeignore");

        let result = scan(&fixture.options(false)).expect("scan fixture");
        assert_eq!(result.files_analyzed, 1, "{:?}", result.files);
        assert_eq!(scanned_paths(&result), vec!["sample.rs".to_string()]);

        let opts = ScanOptions {
            hidden: true,
            ..fixture.options(false)
        };
        let hidden = scan(&opts).expect("scan fixture with hidden files");
        assert_eq!(
            scanned_paths(&hidden),
            vec!["sample.rs".to_string()],
            "the .tokeignore file is a rule source, not source code"
        );
    }

    /// A nested `.tokeignore` owns its directory: it must not leak upward and
    /// hide a sibling at the root.
    #[test]
    fn nested_tokeignore_applies_to_its_own_directory() {
        let fixture = Fixture::new("tokeignore-nested");
        let sub = fixture.dir.join("sub");
        std::fs::create_dir_all(&sub).expect("create sub dir");
        write_c(&sub, "nested.c");
        write_c(&fixture.dir, "other.c");
        std::fs::write(sub.join(".tokeignore"), "nested.c\n").expect("write nested .tokeignore");

        let result = scan(&fixture.options(false)).expect("scan fixture");
        assert_eq!(
            scanned_paths(&result),
            vec!["other.c".to_string(), "sample.rs".to_string()],
            "only sub/nested.c is excluded"
        );
    }

    /// `--no-ignore` is all-or-nothing: both custom sources (`.tokeignore` and
    /// `.p4ignore`) come back with the standard ones.
    #[test]
    fn no_ignore_disables_both_custom_ignore_files() {
        let fixture = Fixture::new("no-ignore-custom");
        write_c(&fixture.dir, "toke.c");
        write_c(&fixture.dir, "p4.c");
        std::fs::write(fixture.dir.join(".tokeignore"), "toke.c\n").expect("write .tokeignore");
        std::fs::write(fixture.dir.join(".p4ignore"), "p4.c\n").expect("write .p4ignore");

        let filtered = scan(&fixture.options(false)).expect("scan fixture");
        assert_eq!(scanned_paths(&filtered), vec!["sample.rs".to_string()]);

        let opts = ScanOptions {
            no_ignore: true,
            ..fixture.options(false)
        };
        let everything = scan(&opts).expect("scan fixture with --no-ignore");
        assert_eq!(
            scanned_paths(&everything),
            vec![
                "p4.c".to_string(),
                "sample.rs".to_string(),
                "toke.c".to_string()
            ]
        );
    }

    /// The negation syntax is gitignore's, which is what `.tokeignore` uses.
    #[test]
    fn tokeignore_negation_keeps_a_file() {
        let fixture = Fixture::new("tokeignore-negation");
        write_c(&fixture.dir, "keep.c");
        write_c(&fixture.dir, "drop.c");
        std::fs::write(fixture.dir.join(".tokeignore"), "*.c\n!keep.c\n")
            .expect("write .tokeignore");

        let result = scan(&fixture.options(false)).expect("scan fixture");
        assert_eq!(
            scanned_paths(&result),
            vec!["keep.c".to_string(), "sample.rs".to_string()]
        );
    }

    #[test]
    fn scanning_a_single_file_measures_only_that_file() {
        let fixture = Fixture::new("single-file");
        std::fs::write(fixture.dir.join("other.rs"), "fn other() {}\n")
            .expect("write second fixture");

        let file = fixture.dir.join("sample.rs");
        let canonical = file.canonicalize().expect("canonical file");
        let parent = canonical.parent().expect("parent").to_path_buf();

        let opts = ScanOptions {
            root: file,
            ..Default::default()
        };
        let result = scan(&opts).expect("scan a single file");

        assert_eq!(result.files_analyzed, 1, "{:?}", result.files);
        assert_eq!(result.files[0].path, "sample.rs");
        assert!(
            result.files[0].ccn.is_some(),
            "the CCN pass runs for a single file"
        );
        assert!(result.ccn.is_some(), "the repo summary is still produced");
        assert!(result.loc_total > 0);
        // Documented choice: the scan root is the file's parent so every
        // downstream VCS call (metrics::build -> vcs::detect) has a directory.
        assert_eq!(result.root, display_root(&parent));
    }

    #[test]
    fn scanning_a_single_text_file_is_empty_not_an_error() {
        let fixture = Fixture::new("single-text");
        let file = fixture.dir.join("notes.txt");
        std::fs::write(&file, "just prose\n").expect("write text fixture");

        let opts = ScanOptions {
            root: file,
            ..Default::default()
        };
        let result = scan(&opts).expect("a .txt file is not an error");

        assert_eq!(result.files_analyzed, 0);
        assert!(result.files.is_empty());
        assert!(result.languages.is_empty());
        assert!(result.ccn.is_none());
        assert_eq!(result.loc_total, 0);
    }

    #[test]
    fn a_single_file_respects_the_lang_filter() {
        let fixture = Fixture::new("single-file-lang");
        let file = fixture.dir.join("sample.rs");
        let opts = ScanOptions {
            root: file,
            langs: vec!["cpp".to_string()],
            ..Default::default()
        };
        let result = scan(&opts).expect("scan a single file");
        assert_eq!(result.files_analyzed, 0, "{:?}", result.files);
    }

    #[test]
    fn missing_paths_are_reported_not_silently_empty() {
        let fixture = Fixture::new("missing");
        let opts = ScanOptions {
            root: fixture.dir.join("nope.rs"),
            ..Default::default()
        };
        assert!(matches!(scan(&opts), Err(ScanError::NotFound(_))));
    }

    /// Best-effort file symlink; Windows needs Developer Mode or elevation, so
    /// every symlink test skips its assertion when this returns `Err`.
    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(any(windows, unix)))]
    fn symlink_file(_target: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no symlink support",
        ))
    }

    #[test]
    fn a_symlinked_file_outside_the_root_is_measured_under_its_link_path() {
        let fixture = Fixture::new("symlink");
        // Target lives in a sibling directory, i.e. outside the scan root.
        let outside =
            std::env::temp_dir().join(format!("ccn-loc-{}-symlink-target", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).expect("create outside dir");
        let target = outside.join("real.rs");
        std::fs::write(
            &target,
            "fn linked() {\n    if true {\n        return;\n    }\n}\n",
        )
        .expect("write symlink target");
        let link = fixture.dir.join("linked.rs");

        if symlink_file(&target, &link).is_err() {
            // Unprivileged Windows cannot create symlinks; the behaviour is
            // still covered on platforms that can.
            eprintln!("skipping symlink assertion: cannot create a file symlink");
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }

        let result = scan(&fixture.options(false)).expect("scan fixture");
        let _ = std::fs::remove_dir_all(&outside);

        let linked = result
            .files
            .iter()
            .find(|f| f.path == "linked.rs")
            .unwrap_or_else(|| panic!("symlinked file missing from {:?}", result.files));
        assert_eq!(linked.lang, "Rust");
        assert!(linked.loc_total > 0);
        // The key is the link path, never the resolved target.
        assert!(
            result
                .files
                .iter()
                .all(|f| !f.path.contains("symlink-target")),
            "{:?}",
            result.files
        );
        assert_eq!(result.files_analyzed, 2, "sample.rs plus the link");
    }

    #[test]
    fn no_follow_links_skips_symlinked_files() {
        let fixture = Fixture::new("no-follow-links");
        let target = fixture.dir.join("real.rs");
        std::fs::write(&target, "fn real() {}\n").expect("write target");
        let link = fixture.dir.join("linked.rs");

        if symlink_file(&target, &link).is_err() {
            eprintln!("skipping symlink assertion: cannot create a file symlink");
            return;
        }

        let opts = ScanOptions {
            no_follow_links: true,
            ..fixture.options(false)
        };
        let result = scan(&opts).expect("scan fixture");
        assert!(
            result.files.iter().all(|f| f.path != "linked.rs"),
            "links are not followed with --no-follow-links: {:?}",
            result.files
        );
        assert!(result.files.iter().any(|f| f.path == "real.rs"));
        assert!(result.files.iter().any(|f| f.path == "sample.rs"));
    }
}
