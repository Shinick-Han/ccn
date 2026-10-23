//! `ccn doctor` — read-only operational self-check.
//!
//! The check list is collected into plain data ([`Report`]) so the exit-code
//! policy and the output shape are testable without a real Neovim install.
//! Every check is local and bounded: a missing tool is reported, never waited
//! on forever.

use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

/// `nvim --version` is a local command, but a wrapper script or a network
/// install can still hang; three seconds is far above a normal startup.
const NVIM_TIMEOUT: Duration = Duration::from_secs(3);

/// Build files that mark a repository as something ccn understands.
const BUILD_FILES: [&str; 5] = [
    "Cargo.toml",
    "CMakeLists.txt",
    "Makefile",
    "meson.build",
    "BUILD",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub version: String,
    pub path: String,
    pub checks: Vec<Check>,
}

/// Values that depend on the running process and environment.
///
/// Tests build this directly so check collection never depends on whether
/// Neovim happens to be installed on the machine.
#[derive(Debug, Clone)]
pub struct Ambient {
    pub version: String,
    pub exe: PathBuf,
    /// First line of `nvim --version`, or `None` when nvim is absent/hung.
    pub nvim: Option<String>,
    pub tty: bool,
}

impl Ambient {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ccn")),
            nvim: probe_nvim(),
            tty: std::io::stdout().is_terminal(),
        }
    }
}

/// 0 without failures, 2 when any check failed. Usage errors are `1` and are
/// decided by [`run`] before this policy is reached.
pub fn exit_code(checks: &[Check]) -> i32 {
    if checks.iter().any(|check| check.status == Status::Fail) {
        2
    } else {
        0
    }
}

/// Run the read-only checks for `root`.
pub fn collect(root: &Path, ambient: &Ambient) -> Report {
    let mut checks = Vec::new();

    checks.push(Check {
        name: "ccn",
        status: Status::Ok,
        detail: format!("ccn {} at {}", ambient.version, ambient.exe.display()),
    });

    match &ambient.nvim {
        Some(line) => checks.push(Check {
            name: "nvim",
            status: Status::Ok,
            detail: format!("found: {line}"),
        }),
        None => checks.push(Check {
            name: "nvim",
            status: Status::Fail,
            detail: "not found on PATH; needed by --editor nvim and the embedded workbench"
                .to_string(),
        }),
    }

    let config = crate::config::load(root);
    match config.editor.as_deref() {
        None => checks.push(Check {
            name: "editor",
            status: Status::Ok,
            detail: "not configured; defaults to nvim".to_string(),
        }),
        Some(editor) => {
            let command = editor.split_whitespace().next().unwrap_or(editor);
            let found = command_on_path(command);
            checks.push(Check {
                name: "editor",
                status: if found.is_some() {
                    Status::Ok
                } else {
                    Status::Warn
                },
                detail: match found {
                    Some(_) => format!("configured: {editor}"),
                    None => format!("configured editor not found on PATH: {editor}"),
                },
            });
        }
    }

    match crate::vcs::detect(root) {
        Some(backend) => checks.push(Check {
            name: "vcs",
            status: Status::Ok,
            detail: format!("detected: {}", backend.name()),
        }),
        None => checks.push(Check {
            name: "vcs",
            status: Status::Warn,
            detail: "no git or Perforce workspace detected".to_string(),
        }),
    }

    for name in ["compile_commands.json", "compile_flags.txt"] {
        let present = root.join(name).is_file();
        checks.push(Check {
            name: if name == "compile_commands.json" {
                "compile_commands"
            } else {
                "compile_flags"
            },
            status: if present { Status::Ok } else { Status::Warn },
            detail: if present {
                format!("found {name}")
            } else {
                format!("missing {name} (optional; used by clangd)")
            },
        });
    }

    let found: Vec<&str> = BUILD_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .collect();
    checks.push(Check {
        name: "build-files",
        status: if found.is_empty() {
            Status::Warn
        } else {
            Status::Ok
        },
        detail: if found.is_empty() {
            format!("none of {} found", BUILD_FILES.join(", "))
        } else {
            format!("found {}", found.join(", "))
        },
    });

    let root_text = root.to_string_lossy();
    match crate::cache::sidecar_path(&root_text, "session") {
        None => checks.push(Check {
            name: "cache",
            status: Status::Warn,
            detail: "no user cache directory could be determined".to_string(),
        }),
        Some(sidecar) => match cache_writable(&sidecar) {
            Ok(()) => checks.push(Check {
                name: "cache",
                status: Status::Ok,
                detail: format!("writable: {}", sidecar.display()),
            }),
            Err(e) => checks.push(Check {
                name: "cache",
                status: Status::Warn,
                detail: format!("not writable: {} ({e})", sidecar.display()),
            }),
        },
    }

    checks.push(Check {
        name: "terminal",
        status: if ambient.tty {
            Status::Ok
        } else {
            Status::Warn
        },
        detail: if ambient.tty {
            "stdout is a TTY".to_string()
        } else {
            "stdout is not a TTY; the dashboard and interactive output are unavailable".to_string()
        },
    });

    Report {
        version: ambient.version.clone(),
        path: root.display().to_string(),
        checks,
    }
}

/// Prove the sidecar's directory is writable by creating and removing a unique
/// file. The file is removed in every success path; a failure leaves nothing
/// behind because the create itself failed.
fn cache_writable(sidecar: &Path) -> Result<(), String> {
    let parent = sidecar
        .parent()
        .ok_or_else(|| "sidecar path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let probe = parent.join(format!(
        ".ccn-doctor-{}-{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::write(&probe, b"ccn doctor\n").map_err(|e| e.to_string())?;
    match std::fs::remove_file(&probe) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("could not remove probe file: {e}")),
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Locate an executable on `PATH` without spawning it. A name containing a
/// path separator is treated as a path and only checked for existence.
fn command_on_path(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.is_absolute() || name.contains('/') || name.contains('\\') {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for file in executable_names(name) {
            let full = dir.join(file);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

fn executable_names(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let mut names = vec![name.to_string()];
        if Path::new(name).extension().is_none() {
            let pathext =
                std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            for ext in pathext.split(';').filter(|ext| !ext.is_empty()) {
                names.push(format!("{name}{ext}"));
            }
        }
        names
    }
    #[cfg(not(windows))]
    {
        vec![name.to_string()]
    }
}

/// Spawn `nvim --version` and return its first line, giving up after
/// [`NVIM_TIMEOUT`]. `None` covers "not installed", "failed" and "hung".
fn probe_nvim() -> Option<String> {
    let mut child = Command::new("nvim")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;

    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < 8192 {
                        let take = n.min(8192 - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        buf
    });

    let deadline = Instant::now() + NVIM_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return first_line(&reader.join().ok()?),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

fn first_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(200).collect())
    }
}

/// Plain ASCII, no colour: doctor output must stay readable in CI logs and
/// terminal emulators without ANSI support.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("ccn doctor\n");
    out.push_str(&format!("version  {}\n", report.version));
    out.push_str(&format!("path     {}\n\n", report.path));
    for check in &report.checks {
        out.push_str(&format!(
            "{:<5} {:<18} {}\n",
            check.status.label(),
            check.name,
            check.detail
        ));
    }

    let ok = report
        .checks
        .iter()
        .filter(|check| check.status == Status::Ok)
        .count();
    let warn = report
        .checks
        .iter()
        .filter(|check| check.status == Status::Warn)
        .count();
    let fail = report
        .checks
        .iter()
        .filter(|check| check.status == Status::Fail)
        .count();
    out.push_str(&format!("\n{ok} ok, {warn} warn, {fail} fail\n"));
    out
}

fn usage() {
    eprintln!("usage: ccn doctor [PATH] [--json]");
}

fn help() {
    println!("ccn doctor - read-only checks of the local ccn setup");
    println!();
    println!("usage: ccn doctor [PATH] [--json]");
    println!();
    println!("PATH defaults to the current directory.");
    println!("exit: 0 no failures, 2 any check failed, 1 usage or I/O error");
}

/// Entry point for the pre-clap `doctor` dispatch.
pub fn run(args: &[OsString]) -> i32 {
    let mut root = PathBuf::from(".");
    let mut path_given = false;
    let mut json = false;

    for arg in args {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "--json" => json = true,
            "-h" | "--help" => {
                help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("ccn doctor: unknown option: {other}");
                usage();
                return 1;
            }
            other => {
                if path_given {
                    eprintln!("ccn doctor: unexpected extra path: {other}");
                    usage();
                    return 1;
                }
                root = PathBuf::from(other);
                path_given = true;
            }
        }
    }

    if !root.is_dir() {
        eprintln!("ccn doctor: not a directory: {}", root.display());
        return 1;
    }

    let ambient = Ambient::current();
    let report = collect(&root, &ambient);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("ccn doctor: could not serialise the report: {e}");
                return 1;
            }
        }
    } else {
        print!("{}", render(&report));
    }

    exit_code(&report.checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("ccn-doctor-{}-{name}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ambient(nvim: Option<&str>) -> Ambient {
        Ambient {
            version: "0.0.0-test".to_string(),
            exe: PathBuf::from("/opt/ccn"),
            nvim: nvim.map(str::to_string),
            tty: false,
        }
    }

    #[test]
    fn collects_the_expected_checks_for_a_temp_root() {
        let scratch = Scratch::new("checks");
        std::fs::write(scratch.0.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(scratch.0.join("compile_commands.json"), "[]\n").unwrap();

        let report = collect(&scratch.0, &ambient(None));
        assert_eq!(report.version, "0.0.0-test");
        assert_eq!(report.path, scratch.0.display().to_string());

        let names: Vec<&str> = report.checks.iter().map(|check| check.name).collect();
        for expected in [
            "ccn",
            "nvim",
            "editor",
            "vcs",
            "compile_commands",
            "compile_flags",
            "build-files",
            "cache",
            "terminal",
        ] {
            assert!(names.contains(&expected), "missing check {expected}");
        }

        let compile_commands = report
            .checks
            .iter()
            .find(|check| check.name == "compile_commands")
            .unwrap();
        assert_eq!(compile_commands.status, Status::Ok);

        let compile_flags = report
            .checks
            .iter()
            .find(|check| check.name == "compile_flags")
            .unwrap();
        assert_eq!(compile_flags.status, Status::Warn);

        let build = report
            .checks
            .iter()
            .find(|check| check.name == "build-files")
            .unwrap();
        assert_eq!(build.status, Status::Ok);
        assert!(build.detail.contains("Cargo.toml"));
    }

    #[test]
    fn missing_nvim_is_a_failure() {
        let scratch = Scratch::new("nvim-missing");
        let report = collect(&scratch.0, &ambient(None));
        let nvim = report
            .checks
            .iter()
            .find(|check| check.name == "nvim")
            .unwrap();
        assert_eq!(nvim.status, Status::Fail);
        assert_eq!(exit_code(&report.checks), 2);
    }

    #[test]
    fn present_nvim_clears_the_failure_and_shows_the_version_line() {
        let scratch = Scratch::new("nvim-present");
        let report = collect(&scratch.0, &ambient(Some("NVIM v0.11.2")));
        let nvim = report
            .checks
            .iter()
            .find(|check| check.name == "nvim")
            .unwrap();
        assert_eq!(nvim.status, Status::Ok);
        assert!(nvim.detail.contains("NVIM v0.11.2"));
        assert_eq!(exit_code(&report.checks), 0);
    }

    #[test]
    fn a_missing_configured_editor_is_only_a_warning() {
        let scratch = Scratch::new("editor");
        std::fs::write(
            scratch.0.join(".ccn.toml"),
            "editor = \"ccn-definitely-not-installed\"\n",
        )
        .unwrap();
        let report = collect(&scratch.0, &ambient(Some("NVIM v0.10.0")));
        let editor = report
            .checks
            .iter()
            .find(|check| check.name == "editor")
            .unwrap();
        assert_eq!(editor.status, Status::Warn);
        assert_eq!(exit_code(&report.checks), 0);
    }

    #[test]
    fn exit_code_policy_ignores_warnings() {
        let ok = Check {
            name: "a",
            status: Status::Ok,
            detail: String::new(),
        };
        let warn = Check {
            name: "b",
            status: Status::Warn,
            detail: String::new(),
        };
        let fail = Check {
            name: "c",
            status: Status::Fail,
            detail: String::new(),
        };
        assert_eq!(exit_code(&[]), 0);
        assert_eq!(exit_code(&[ok.clone(), warn.clone()]), 0);
        assert_eq!(exit_code(&[ok, warn, fail]), 2);
    }

    #[test]
    fn cache_probe_creates_and_removes_its_file() {
        let scratch = Scratch::new("cache");
        let sidecar = scratch.0.join("nested").join("session.json");
        cache_writable(&sidecar).unwrap();
        assert!(sidecar.parent().unwrap().is_dir());
        let leftovers: Vec<_> = std::fs::read_dir(sidecar.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "probe file was not removed");
    }

    #[test]
    fn report_renders_ascii_status_labels() {
        let scratch = Scratch::new("render");
        let report = collect(&scratch.0, &ambient(None));
        let text = render(&report);
        assert!(text.starts_with("ccn doctor\n"));
        assert!(text.contains("FAIL"));
        assert!(text.contains("nvim"));
        assert!(text.is_ascii());
    }
}
