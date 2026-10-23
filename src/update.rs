//! `ccn update` — check the published npm version and update npm installs.
//!
//! No HTTP client: the npm CLI is the only network dependency, and it is run
//! with a bounded helper. A native (non-npm) binary is never overwritten
//! silently; the exact release asset commands are printed instead.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PACKAGE: &str = "@shinick/ccn";
const RELEASES: &str = "https://github.com/Shinick-Han/ccn/releases";
/// A short bound for both `npm view` and `npm root -g`. npm is local, but a
/// registry lookup on a bad network must not hang the command.
const NPM_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CAPTURE: usize = 64 * 1024;

#[derive(Debug)]
struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Entry point for the pre-clap `update` dispatch.
pub fn run(args: &[OsString]) -> i32 {
    let mut check_only = false;
    let mut apply = false;

    for arg in args {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "--check" => check_only = true,
            "--apply" => apply = true,
            "-h" | "--help" => {
                help();
                return 0;
            }
            other => {
                eprintln!("ccn update: unknown option: {other}");
                usage();
                return 1;
            }
        }
    }

    if check_only && apply {
        eprintln!("ccn update: --check and --apply cannot be combined");
        usage();
        return 1;
    }

    let current = env!("CARGO_PKG_VERSION");
    let latest = match latest_version() {
        Ok(version) => version,
        Err(e) => {
            eprintln!("ccn update: {e}");
            eprintln!(
                "ccn update: install Node.js/npm and check the network, or download a release from {RELEASES}"
            );
            return 1;
        }
    };

    println!("current  {current}");
    println!("latest   {latest}");

    match compare_versions(&latest, current) {
        Some(Ordering::Equal) => {
            println!("ccn is up to date");
            return 0;
        }
        Some(Ordering::Less) => {
            println!("the local version is ahead of the published one");
            return 0;
        }
        Some(Ordering::Greater) => {}
        None => {
            eprintln!("ccn update: could not compare {current:?} with {latest:?}");
            return 1;
        }
    }

    let npm_installed = global_npm_root().as_deref().is_some_and(is_npm_install);

    if check_only {
        println!("an update is available");
        return 0;
    }

    if apply {
        if !npm_installed {
            print_native_guidance(&latest, &current_exe());
            eprintln!(
                "ccn update: --apply refused: this binary is not a global npm install; \
                 replace it with the release asset or reinstall through npm"
            );
            return 1;
        }

        println!("updating {PACKAGE} to {latest} with npm");
        let spec = format!("{PACKAGE}@latest");
        let mut command = npm_command();
        command.args(["install", "-g", spec.as_str()]);
        return match command.status() {
            Ok(status) if status.success() => 0,
            Ok(status) => {
                eprintln!("ccn update: npm exited with {status}");
                1
            }
            Err(e) => {
                eprintln!("ccn update: could not run npm: {e}");
                1
            }
        };
    }

    if npm_installed {
        println!("update with: ccn update --apply");
        println!("or:          npm install -g {PACKAGE}@latest");
    } else {
        print_native_guidance(&latest, &current_exe());
    }
    0
}

fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ccn"))
}

/// Version of the package published on npm, or an actionable error.
fn latest_version() -> Result<String, String> {
    let mut command = npm_command();
    command.args(["view", PACKAGE, "version", "--silent"]);
    let output = run_bounded(command, NPM_TIMEOUT)
        .ok_or_else(|| "could not run npm (is Node.js/npm installed and on PATH?)".to_string())?;

    if !output.success {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            "npm view failed".to_string()
        } else {
            format!("npm view failed: {detail}")
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "npm returned no version".to_string())?;

    if parse_version(version).is_none() {
        return Err(format!("npm returned an unrecognised version: {version:?}"));
    }
    Ok(version.trim_start_matches('v').to_string())
}

/// Root of the global npm tree, if npm can report one.
fn global_npm_root() -> Option<PathBuf> {
    let mut command = npm_command();
    command.args(["root", "-g"]);
    let output = run_bounded(command, NPM_TIMEOUT)?;
    if !output.success {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

fn npm_command() -> Command {
    #[cfg(windows)]
    {
        // npm is a `.cmd` shim; Command cannot execute it directly.
        let mut command = Command::new("cmd");
        command.arg("/C").arg("npm");
        command
    }
    #[cfg(not(windows))]
    {
        Command::new("npm")
    }
}

/// Print the native-install instructions for this platform.
fn print_native_guidance(latest: &str, exe: &Path) {
    println!("this binary is not a global npm install");
    println!("release: {RELEASES}/tag/v{latest}");

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match asset_name(os, arch) {
        Some(asset) => {
            let url = format!("{RELEASES}/download/v{latest}/{asset}");
            println!("asset:   {asset}");
            #[cfg(windows)]
            {
                println!("download: curl -L -o ccn.exe {url}");
                println!("replace:  move /Y ccn.exe \"{}\"", exe.display());
            }
            #[cfg(not(windows))]
            {
                println!("download: curl -L -o ccn {url}");
                println!("install:  chmod +x ccn && mv ccn \"{}\"", exe.display());
            }
        }
        None => {
            println!("no prebuilt asset for {os}/{arch}");
            println!("build:   cargo build --release --locked");
        }
    }
    println!("checksums: {RELEASES}/download/v{latest}/SHA256SUMS");
    println!("or install through npm: npm install -g {PACKAGE}@latest");
}

/// Release asset for an OS/architecture pair, using the names published by the
/// release workflow. Unknown pairs have no prebuilt binary.
fn asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("ccn-windows-x64.exe"),
        ("linux", "x86_64") => Some("ccn-linux-x64"),
        ("macos", "aarch64") => Some("ccn-macos-arm64"),
        ("macos", "x86_64") => Some("ccn-macos-x64"),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Version {
    numbers: Vec<u64>,
    pre: Option<String>,
}

fn parse_version(value: &str) -> Option<Version> {
    let value = value.trim().trim_start_matches('v');
    if value.is_empty() {
        return None;
    }
    let (core, pre) = match value.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
        Some(_) => return None,
        None => (value, None),
    };
    let numbers = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    if numbers.is_empty() {
        return None;
    }
    Some(Version { numbers, pre })
}

/// Order two dotted versions, with semver pre-release precedence. Missing
/// numeric components are zero, so `1.2` equals `1.2.0`.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let a = parse_version(a)?;
    let b = parse_version(b)?;

    let len = a.numbers.len().max(b.numbers.len());
    for index in 0..len {
        let left = a.numbers.get(index).copied().unwrap_or(0);
        let right = b.numbers.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            other => return Some(other),
        }
    }

    match (a.pre.as_deref(), b.pre.as_deref()) {
        (None, None) => Some(Ordering::Equal),
        (None, Some(_)) => Some(Ordering::Greater),
        (Some(_), None) => Some(Ordering::Less),
        (Some(left), Some(right)) => Some(compare_pre_release(left, right)),
    }
}

fn compare_pre_release(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// Spawn `cmd` and read both pipes to EOF, killing it after `timeout`.
///
/// `try_wait` polling makes the deadline real even when npm is a shim that
/// waits on a network socket.
fn run_bounded(mut command: Command, timeout: Duration) -> Option<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;

    let read = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.len() < MAX_CAPTURE {
                            let take = n.min(MAX_CAPTURE - buf.len());
                            buf.extend_from_slice(&chunk[..take]);
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            buf
        })
    };
    let stdout_reader = read(Box::new(stdout));
    let stderr_reader = read(Box::new(stderr));

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(Output {
                    success: status.success(),
                    stdout: stdout_reader.join().ok()?,
                    stderr: stderr_reader.join().ok()?,
                });
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return None;
            }
        }
    }
}

/// True when `root` contains the npm package's manifest, i.e. the running
/// install is the global npm package rather than a standalone binary.
fn is_npm_install(root: &Path) -> bool {
    root.join(PACKAGE).join("package.json").is_file()
}

fn usage() {
    eprintln!("usage: ccn update [--check | --apply]");
}

fn help() {
    println!("ccn update - compare this binary with the published npm version");
    println!();
    println!("usage: ccn update [--check | --apply]");
    println!();
    println!("  (none)   check and print update instructions");
    println!("  --check  check only");
    println!("  --apply  run `npm install -g @shinick/ccn@latest` when this");
    println!("           binary is a global npm install; refuse otherwise");
    println!();
    println!("exit: 0 success, 1 npm/network failure or refused --apply");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_dotted_versions_numerically() {
        assert_eq!(compare_versions("0.9.10", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.26.0", "0.26.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("v1.2.3", "1.2.3"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.2", "1.2.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.25.9", "0.26.0"), Some(Ordering::Less));
    }

    #[test]
    fn pre_releases_sort_below_the_release() {
        assert_eq!(
            compare_versions("1.0.0-rc.1", "1.0.0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-alpha"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.0.0-rc.10", "1.0.0-rc.2"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.0.0-alpha", "1.0.0-beta"),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn rejects_unparseable_versions() {
        assert_eq!(compare_versions("", "1.0.0"), None);
        assert_eq!(compare_versions("latest", "1.0.0"), None);
        assert_eq!(compare_versions("1.x.0", "1.0.0"), None);
        assert_eq!(compare_versions("1.0.0-", "1.0.0"), None);
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        assert_eq!(asset_name("windows", "x86_64"), Some("ccn-windows-x64.exe"));
        assert_eq!(asset_name("linux", "x86_64"), Some("ccn-linux-x64"));
        assert_eq!(asset_name("macos", "aarch64"), Some("ccn-macos-arm64"));
        assert_eq!(asset_name("macos", "x86_64"), Some("ccn-macos-x64"));
        assert_eq!(asset_name("linux", "aarch64"), None);
    }

    #[test]
    fn npm_install_detection_uses_the_package_manifest() {
        let root = std::env::temp_dir().join(format!(
            "ccn-update-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package = root.join(PACKAGE);
        std::fs::create_dir_all(&package).unwrap();
        assert!(!is_npm_install(&root));
        std::fs::write(package.join("package.json"), "{}").unwrap();
        assert!(is_npm_install(&root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
