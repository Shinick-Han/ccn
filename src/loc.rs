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
use crate::model::{ExcludedFiles, FileLoc, LangSummary, RepoCcn, ScanResult};

/// Files above this size are not opened by the CCN pass. Generated bundles and
/// checked-in vendored blobs live here and a 40 MB `min.js` would dominate both
/// the runtime and the histogram.
const MAX_CCN_BYTES: u64 = 8 * 1024 * 1024;

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
    /// Ignore `.gitignore`, `.ignore`, `.tokeignore`.
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

/// Scan `opts.root`. Never fails on unreadable individual files: they are
/// skipped, and the CCN pass treats an unreadable file as "no CCN".
///
/// Two modes share one walk:
/// - **cold**: parse LOC with tokei's per-file parser, read the bytes once for
///   CCN and generated-file markers, then store everything in the cache;
/// - **warm**: a stat per file and one cache lookup. No read, no AST.
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
    let mut next = crate::cache::Cache {
        version: crate::cache::CACHE_VERSION,
        root: root_display.clone(),
        entries: std::collections::HashMap::new(),
    };

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
    let mut dirty = false;
    for item in resolved.into_iter().flatten() {
        let (loc, entry) = item;
        if let Some(entry) = entry {
            next.insert(loc.path.clone(), entry);
            dirty = true;
        }
        files.push(loc);
    }

    // Deterministic output: JSON diffs between runs should only move when the
    // code moved.
    files.sort_by(|a, b| a.path.cmp(&b.path));

    // Rewriting a multi-megabyte cache on a fully-cached run would cost more
    // than the parse it saves, so only write when a file was added or changed.
    if use_cache && (dirty || old.len() != next.len()) {
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
        // Perforce workspaces ignore build output through .p4ignore, not
        // .gitignore; honour it with the same syntax.
        .add_custom_ignore_filename(".p4ignore")
        // Symlinked sources and symlinked workspace roots are ordinary layouts.
        // The walker keeps the link path (so relative keys and VCS keys stay
        // the ones a user sees) while measuring the target; it also detects
        // cycles itself. `--no-follow-links` restores the strict behaviour.
        .follow_links(!opts.no_follow_links);

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

/// Resolve one file: cache hit -> cached record; miss -> parse and build a new
/// cache entry.
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
    let fingerprint = crate::cache::Fingerprint::of(abs);

    if let Some(fp) = fingerprint {
        if let Some(hit) = old.get(&path, fp) {
            return Some((
                FileLoc {
                    path,
                    lang: hit.lang.clone(),
                    loc_total: hit.loc_total,
                    loc: hit.loc,
                    comments: hit.comments,
                    blanks: hit.blanks,
                    ccn: hit.ccn.clone(),
                    is_generated: hit.is_generated,
                    is_vendored: hit.is_vendored,
                },
                None,
            ));
        }
    }

    // `parse` reads the file itself (with encoding detection). The CCN pass
    // needs the bytes anyway; on a miss that second read is the price of a
    // correct embedded-language LOC count.
    let report = lang.parse(abs.to_path_buf(), config).ok()?;
    let stats = report.stats.summarise();
    let loc_total = stats.lines();
    if loc_total == 0 {
        return None;
    }

    let mut is_generated = false;
    let mut is_vendored = false;
    if classify_exclusions {
        let class = generated::classify(&path, &[]);
        is_generated = class.is_generated;
        is_vendored = class.is_vendored;
    }

    let mut ccn = None;
    if !opts.no_ccn {
        // Size check before opening: a minified bundle is the one file class
        // that makes AST parsing both pointless and unbounded.
        let size_ok = std::fs::metadata(abs)
            .map(|m| m.len() <= MAX_CCN_BYTES)
            .unwrap_or(false);
        if size_ok {
            if let Ok(bytes) = std::fs::read(abs) {
                if classify_exclusions {
                    // Same bytes as the CCN pass: markers live in the head, and
                    // this is the only place the file is opened.
                    let class = generated::classify(&path, &bytes);
                    is_generated = class.is_generated;
                    is_vendored = class.is_vendored;
                }
                ccn = Some(crate::ccn::analyze_file(
                    Path::new(&path),
                    &bytes,
                    lang.name(),
                ));
            }
        }
    }

    let lang_name = lang.name().to_string();
    let entry = fingerprint.map(|fp| crate::cache::Entry {
        mtime_ns: fp.mtime_ns,
        size: fp.size,
        lang: lang_name.clone(),
        loc_total,
        loc: stats.code,
        comments: stats.comments,
        blanks: stats.blanks,
        ccn: ccn.clone(),
        is_generated,
        is_vendored,
    });

    Some((
        FileLoc {
            path,
            lang: lang_name,
            loc_total,
            loc: stats.code,
            comments: stats.comments,
            blanks: stats.blanks,
            ccn,
            is_generated,
            is_vendored,
        },
        entry,
    ))
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
    fn relative_paths_use_forward_slashes() {
        let root = Path::new("C:\\repo");
        let path = Path::new("C:\\repo\\src\\main.rs");
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
    fn no_ccn_still_excludes_vendored_trees_by_path() {
        // Without the CCN pass the file is never read, so markers are invisible;
        // path rules must still apply or `--no-ccn` would change the LOC
        // histogram depending on an unrelated flag.
        let fixture = fixture_with_exclusions("no-ccn-exclusions");
        let result = scan(&fixture.options(true)).expect("scan fixture");

        assert_eq!(result.files_excluded.vendored, 1);
        assert_eq!(
            result.files_excluded.generated, 0,
            "markers need the file's bytes; the extension alone is not enough"
        );
        let report = crate::metrics::build(&result);
        assert_eq!(
            report.files_measured, 2,
            "sample.rs and api.gen.ts are measured"
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
