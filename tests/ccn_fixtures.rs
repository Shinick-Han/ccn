//! Golden tests for the tree-sitter CCN engine.
//!
//! `tests/fixtures/expected.json` is the hand-computed oracle. Expected values
//! follow `src/ccn/rules.rs` exactly (`ccn = 1 + decision_points`). A mismatch
//! means the engine disagrees with the rules table; do not "fix" it by editing
//! `src/`, and do not quietly change the expected numbers without re-reading
//! the rules and showing the arithmetic.
//!
//! `name: null` in the expectations means "match this entry by position in the
//! source order", which is how anonymous callbacks are covered without
//! depending on whether the engine recovers a name (`(anonymous)`, `formatter`,
//! …) for them.

use std::fs;
use std::path::{Path, PathBuf};

use ccn::ccn::engine::analyze_file;
use ccn::ccn::types::{CcnTier, FunctionCcn, Lang};
use serde_json::Value;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Fixture directory -> file extension, resolved through `Lang::from_path`.
///
/// Going through the extension keeps the harness independent of enum variant
/// spelling while the Java/C/C++/C# grammars land: a directory whose extension
/// is not mapped yet is reported as a failure instead of breaking compilation.
fn lang_for_dir(dir: &str) -> Option<Lang> {
    let ext = match dir {
        "javascript" => "js",
        "tsx" => "tsx",
        "python" => "py",
        "rust" => "rs",
        "go" => "go",
        "java" => "java",
        "c" => "c",
        "cpp" => "cpp",
        "csharp" => "cs",
        // Unknown names are a fixture-authoring bug, not a missing grammar.
        other => panic!("expected.json names unsupported fixture directory `{other}`"),
    };
    Lang::from_path(&format!("fixture.{ext}"))
}

/// Extensions scanned as fixtures. This is deliberately an explicit list rather
/// than "anything `Lang::from_path` accepts", so adding a directory without
/// adding its extension here fails loudly instead of silently skipping files.
const FIXTURE_EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "mts", "cts", "tsx", "py", "pyi", "rs", "go", "java", "c",
    "cc", "cpp", "cxx", "hpp", "cs",
];

/// Compact one-line description of whatever the engine actually returned.
fn describe(functions: &[FunctionCcn]) -> String {
    if functions.is_empty() {
        return "[]".to_string();
    }
    let rendered: Vec<String> = functions
        .iter()
        .map(|f| format!("{}@{}:ccn={}", f.name, f.line, f.ccn))
        .collect();
    format!("[{}]", rendered.join(", "))
}

#[test]
fn ccn_fixtures_match_expected() {
    let root = fixtures_root();
    let expected_path = root.join("expected.json");
    let raw = fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", expected_path.display()));
    let doc: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("cannot parse {}: {e}", expected_path.display()));
    let entries = doc
        .as_array()
        .unwrap_or_else(|| panic!("{} must be a JSON array", expected_path.display()));

    let mut failures: Vec<String> = Vec::new();

    for entry in entries {
        let Some(file_rel) = entry.get("file").and_then(Value::as_str) else {
            failures.push(format!(
                "expected.json entry without a string `file`: {entry}"
            ));
            continue;
        };
        let Some(lang_name) = entry.get("lang").and_then(Value::as_str) else {
            failures.push(format!(
                "{file_rel}: expected.json entry without a string `lang`"
            ));
            continue;
        };
        let lang = match lang_for_dir(lang_name) {
            Some(lang) => lang,
            None => {
                failures.push(format!(
                    "{file_rel}: no CCN grammar is wired for `{lang_name}` fixtures yet"
                ));
                continue;
            }
        };
        let Some(expected_functions) = entry.get("functions").and_then(Value::as_array) else {
            failures.push(format!(
                "{file_rel}: expected.json entry without a `functions` array"
            ));
            continue;
        };

        let path = root.join(file_rel);
        let source = match fs::read(&path) {
            Ok(source) => source,
            Err(e) => {
                failures.push(format!("{file_rel}: cannot read fixture: {e}"));
                continue;
            }
        };

        let file = match analyze_file(&path, &source, lang) {
            Ok(file) => file,
            Err(err) => {
                failures.push(format!("{file_rel}: analyze_file failed: {err}"));
                continue;
            }
        };
        let actual = &file.functions;

        if actual.len() != expected_functions.len() {
            failures.push(format!(
                "{file_rel}: expected {} functions, got {} - functions: {}",
                expected_functions.len(),
                actual.len(),
                describe(actual)
            ));
        }

        // Each actual function is matched at most once: named entries by name,
        // positional entries by their index in expected.json (which is source
        // order). A missing match is reported, never panicked on.
        let mut matched = vec![false; actual.len()];

        for (position, expected_fn) in expected_functions.iter().enumerate() {
            let expected_name = expected_fn.get("name").and_then(Value::as_str);
            let label = match expected_name {
                Some(name) => name.to_string(),
                None => format!("#{position} (anonymous)"),
            };

            let found = match expected_name {
                Some(name) => actual
                    .iter()
                    .enumerate()
                    .position(|(i, f)| !matched[i] && f.name == name),
                None => {
                    if position < actual.len() && !matched[position] {
                        Some(position)
                    } else {
                        None
                    }
                }
            };

            let Some(index) = found else {
                failures.push(format!(
                    "{file_rel}::{label}: expected function not found - functions: {}",
                    describe(actual)
                ));
                continue;
            };
            matched[index] = true;
            let got = &actual[index];

            let expected_ccn = expected_fn.get("ccn").and_then(Value::as_u64);
            match expected_ccn {
                Some(expected_ccn) if got.ccn as u64 != expected_ccn => {
                    failures.push(format!(
                        "{file_rel}::{label}: expected CCN {expected_ccn}, got {} - functions: {}",
                        got.ccn,
                        describe(actual)
                    ));
                }
                Some(_) => {}
                None => failures.push(format!("{file_rel}::{label}: expected entry has no `ccn`")),
            }

            if let Some(expected_params) = expected_fn.get("params").and_then(Value::as_u64) {
                if got.params as u64 != expected_params {
                    failures.push(format!(
                        "{file_rel}::{label}: expected {} params, got {} - functions: {}",
                        expected_params,
                        got.params,
                        describe(actual)
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "CCN fixture mismatches ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .is_some_and(|ext| FIXTURE_EXTENSIONS.contains(&ext.as_str()))
        {
            out.push(path);
        }
    }
}

/// A fixture that stops parsing would otherwise silently drop out of the
/// golden comparison (or degrade to the heuristic tier); this catches that.
#[test]
fn every_fixture_parses_as_exact_tier() {
    let root = fixtures_root();
    let mut files = Vec::new();
    collect_sources(&root, &mut files);
    files.sort();

    assert!(
        !files.is_empty(),
        "no fixture sources found under {}",
        root.display()
    );

    let mut failures: Vec<String> = Vec::new();

    for path in &files {
        let display = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let Some(lang) = Lang::from_path(&path.to_string_lossy()) else {
            failures.push(format!("{display}: no grammar for this extension"));
            continue;
        };
        let source = match fs::read(path) {
            Ok(source) => source,
            Err(e) => {
                failures.push(format!("{display}: cannot read fixture: {e}"));
                continue;
            }
        };
        match analyze_file(path, &source, lang) {
            Ok(file) if file.tier == CcnTier::Exact => {}
            Ok(file) => failures.push(format!("{display}: tier {:?}, expected Exact", file.tier)),
            Err(err) => failures.push(format!("{display}: analyze_file failed: {err}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) did not parse exactly:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
