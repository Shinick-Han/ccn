//! Process tests for the P0-1 contract: the exit status must not depend on the
//! output mode.
//!
//! These tests drive the real binary (`CARGO_BIN_EXE_ccn`) in a temporary
//! directory, because the bug lived in `main`: `--report-json` returned before
//! the gates ran, SARIF write failures were swallowed, and two JSON documents
//! could share stdout. They need no network and skip only when `git` is absent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

const CCN: &str = env!("CARGO_BIN_EXE_ccn");

/// A temporary directory that removes itself on drop.
///
/// The cache lives outside the scanned tree so a warm cache can never change
/// what the walker sees, and so tests cannot poison each other's entries.
struct TempDir {
    path: PathBuf,
    cache: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("ccn-cli-modes-{}-{n}", std::process::id()));
        let path = base.join("repo");
        let cache = base.join("cache");
        fs::create_dir_all(&path).expect("create temp repo");
        fs::create_dir_all(&cache).expect("create temp cache dir");
        TempDir { path, cache }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("temp path must be UTF-8")
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let file = self.path.join(name);
        fs::write(&file, contents).expect("write fixture");
        file
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Some(base) = self.path.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run(dir: &TempDir, args: &[&str]) -> Run {
    let output = Command::new(CCN)
        .args(args)
        .current_dir(&dir.path)
        .env("CCN_CACHE_DIR", &dir.cache)
        .env_remove("CCN_TIMING")
        .output()
        .expect("failed to spawn the ccn binary");
    Run {
        status: output
            .status
            .code()
            .expect("ccn must exit with a status code, not a signal"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// One function above CCN 10 (twelve branches) plus one trivial function, so
/// `pct_over_10` is exactly 50%.
fn write_gated_repo(dir: &TempDir) -> PathBuf {
    dir.write(
        "sample.c",
        r#"
int complex_like(int x) {
    int n = 0;
    if (x > 0) n++;
    if (x > 1) n++;
    if (x > 2) n++;
    if (x > 3) n++;
    if (x > 4) n++;
    if (x > 5) n++;
    if (x > 6) n++;
    if (x > 7) n++;
    if (x > 8) n++;
    if (x > 9) n++;
    if (x > 10) n++;
    if (x > 11) n++;
    return n;
}

int trivial(int x) {
    return x + 1;
}
"#,
    )
}

fn parse_single_document(stdout: &str) -> Value {
    // `from_str` rejects trailing data, so this asserts "exactly one document".
    serde_json::from_str::<Value>(stdout)
        .unwrap_or_else(|e| panic!("stdout is not exactly one JSON document: {e}\n{stdout}"))
}

#[test]
fn ci_text_mode_fails_the_gate() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    let out = run(
        &dir,
        &["--ci", "--no-report", "--fail-over", "0", dir.path_str()],
    );
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("exceed CCN 10"),
        "expected the gate message, got: {}",
        out.stderr
    );
}

#[test]
fn report_json_mode_applies_the_gate_and_prints_one_document() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    let out = run(&dir, &["--report-json", "--fail-over", "0", dir.path_str()]);
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    let doc = parse_single_document(&out.stdout);
    assert!(doc.is_object(), "report JSON must be an object");
    assert!(doc.get("health").is_some(), "report JSON has no health");
}

#[test]
fn json_mode_applies_the_gate_and_prints_one_document() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    let out = run(&dir, &["--json", "--fail-over", "0", dir.path_str()]);
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    let doc = parse_single_document(&out.stdout);
    assert!(doc.is_object(), "scan JSON must be an object");
}

#[test]
fn non_finite_and_out_of_range_fail_over_are_usage_errors() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    // `--flag=value` keeps a leading `-` from looking like another flag.
    for arg in [
        "--fail-over=101",
        "--fail-over=-1",
        "--fail-over=NaN",
        "--fail-over=inf",
    ] {
        let out = run(&dir, &["--report-json", arg, dir.path_str()]);
        assert_eq!(out.status, 2, "{arg}: {}", out.stderr);
        assert!(
            out.stderr.contains("--fail-over"),
            "unhelpful message for {arg}: {}",
            out.stderr
        );
        assert!(
            out.stdout.trim().is_empty(),
            "{arg} must not print a document: {}",
            out.stdout
        );
    }
}

#[test]
fn sarif_write_failure_is_status_two_and_outranks_the_gate() {
    let dir = TempDir::new();
    write_gated_repo(&dir);
    let target = dir.path.join("missing").join("out.sarif");
    let target_str = target.to_str().unwrap();

    let out = run(
        &dir,
        &["--report-json", "--sarif", target_str, dir.path_str()],
    );
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("could not write SARIF"),
        "expected a SARIF write error, got: {}",
        out.stderr
    );
    assert!(
        !Path::new(target_str).exists(),
        "no SARIF file should exist"
    );

    // Operational error (2) must win even when the gate also fails.
    let out = run(
        &dir,
        &[
            "--report-json",
            "--fail-over",
            "0",
            "--sarif",
            target_str,
            dir.path_str(),
        ],
    );
    assert_eq!(
        out.status, 2,
        "gate must not mask a write error: {}",
        out.stderr
    );
}

#[test]
fn sarif_on_stdout_cannot_be_mixed_with_json_modes() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    for mode in ["--json", "--report-json"] {
        let out = run(&dir, &[mode, "--sarif", "-", dir.path_str()]);
        assert_eq!(out.status, 2, "{mode} + --sarif -: {}", out.stderr);
        assert!(
            out.stdout.trim().is_empty(),
            "{mode} + --sarif - leaked stdout: {}",
            out.stdout
        );
        assert!(
            out.stderr.contains("--sarif -"),
            "unclear rejection: {}",
            out.stderr
        );
    }
}

#[test]
fn sarif_alone_on_stdout_is_exactly_one_document() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    let out = run(&dir, &["--no-report", "--sarif", "-", dir.path_str()]);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    let doc = parse_single_document(&out.stdout);
    assert_eq!(
        doc.get("version").and_then(Value::as_str),
        Some("2.1.0"),
        "expected a SARIF document: {}",
        out.stdout
    );
}

#[test]
fn a_passing_repo_exits_zero_in_text_and_report_json_modes() {
    let dir = TempDir::new();
    write_gated_repo(&dir);

    let text = run(&dir, &["--no-report", "--fail-over", "100", dir.path_str()]);
    assert_eq!(text.status, 0, "stderr: {}", text.stderr);

    let json = run(
        &dir,
        &["--report-json", "--fail-over", "100", dir.path_str()],
    );
    assert_eq!(json.status, 0, "stderr: {}", json.stderr);
    parse_single_document(&json.stdout);
}

#[test]
fn fail_over_with_nothing_exactly_parsed_is_status_two() {
    let dir = TempDir::new();
    // No CCN grammar: the gate cannot be evaluated, so it must not pass.
    dir.write("README.md", "# notes\n\nno functions here\n");

    let out = run(&dir, &["--report-json", "--fail-over", "0", dir.path_str()]);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("could not be evaluated"),
        "expected an unevaluable-gate message, got: {}",
        out.stderr
    );
}

#[test]
fn baseline_gate_fails_report_json_mode() {
    if !git_available() {
        eprintln!("skipping baseline gate test: git is not available");
        return;
    }

    let dir = TempDir::new();
    // The committed state is simple; the uncommitted state adds a complex
    // function, so `--baseline HEAD` must see one added function.
    dir.write("sample.c", "int trivial(int x) {\n    return x + 1;\n}\n");

    assert!(
        git(&dir.path, &["init", "-q"]),
        "git init failed in the temp repo"
    );
    assert!(
        git(&dir.path, &["add", "sample.c"]),
        "git add failed in the temp repo"
    );
    assert!(
        git(
            &dir.path,
            &[
                "-c",
                "user.name=ccn tests",
                "-c",
                "user.email=ccn@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "baseline",
            ],
        ),
        "git commit failed in the temp repo"
    );

    write_gated_repo(&dir);

    let out = run(
        &dir,
        &[
            "--baseline",
            "HEAD",
            "--fail-on-new",
            "--report-json",
            dir.path_str(),
        ],
    );
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    let doc = parse_single_document(&out.stdout);
    let changes = doc
        .get("baseline")
        .and_then(|b| b.get("changes"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no baseline changes in report: {}", out.stdout));
    assert!(
        !changes.is_empty(),
        "expected added functions in the baseline delta"
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
