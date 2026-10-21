use std::path::Path;
use std::process::Command;

use clap::Parser;

fn main() {
    let cli = ccn::cli::Cli::parse();

    // Usage checks run before the scan and before any output, so a bad
    // invocation cannot print half a document and then fail.
    if let Some(limit) = cli.fail_over {
        if !limit.is_finite() || !(0.0..=100.0).contains(&limit) {
            usage_error(format_args!(
                "--fail-over must be a finite percentage between 0 and 100 (got {limit})"
            ));
        }
    }
    // Both writers own stdout; interleaving them would produce two JSON
    // documents that no decoder can read.
    if cli.sarif.as_deref() == Some("-") && (cli.json || cli.report_json) {
        usage_error(format_args!(
            "--sarif - cannot be combined with --json or --report-json: both write a document to stdout"
        ));
    }

    let result = match ccn::loc::scan(&cli.scan_options()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ccn: {e}");
            std::process::exit(2);
        }
    };

    let mut report = ccn::metrics::build_with(&result, cli.hotspots, cli.history_limit);

    // Every failure that is independent of the gate outcome sets this flag.
    // The exit status is decided exactly once, after rendering and after the
    // gates, so no output mode can bypass or downgrade it.
    let mut operational_error = false;

    // Record the snapshot only after the report exists: the trend compares
    // reports, not scans. A failed write must not fail the command.
    if cli.trend {
        let snapshot = ccn::history::Snapshot::from_report(&report);
        match ccn::history::record(&result.root, snapshot) {
            Ok(trend) => report.trend = trend,
            Err(e) => eprintln!("ccn: could not write history: {e}"),
        }
    }

    // Baseline needs the VCS to hand back the old file contents.
    if let Some(rev) = &cli.baseline {
        match ccn::vcs::detect(Path::new(&result.root)) {
            Some(backend) => {
                // Validate once: a typo must not make every file look "new".
                if !ccn::vcs::rev_exists(Path::new(&result.root), backend, rev) {
                    eprintln!("ccn: cannot resolve baseline revision {rev}");
                    std::process::exit(2);
                }
                report.baseline = Some(ccn::baseline::collect(&result, backend, rev, 10));
            }
            None => {
                eprintln!("ccn: --baseline needs a git or Perforce workspace");
                std::process::exit(2);
            }
        }
    }

    // `--sarif -` makes SARIF the sole document on stdout, so the terminal
    // summary and path notices are suppressed (or moved to stderr).
    let sarif_owns_stdout = cli.sarif.as_deref() == Some("-");

    // SARIF is written before any machine-readable output so that
    // `--json --sarif out.sarif` still produces the file.
    if let Some(target) = &cli.sarif {
        let json = ccn::sarif::render(&result, &report, cli.sarif_min_ccn);
        if sarif_owns_stdout {
            println!("{json}");
        } else {
            match std::fs::write(target, json.as_bytes()) {
                Ok(()) => eprintln!("ccn: wrote SARIF to {target}"),
                Err(e) => {
                    eprintln!("ccn: could not write SARIF to {target}: {e}");
                    operational_error = true;
                }
            }
        }
    }

    // Machine-readable modes render their document and then fall through to
    // the gates; they must not return early.
    let mut tui_handled = false;
    if cli.report_json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("ccn: failed to serialise report: {e}");
                operational_error = true;
            }
        }
    } else if cli.tui {
        match ccn::tui::run(&result, &report) {
            // The user quitting the dashboard is not an exit: gates still run.
            Ok(()) => tui_handled = true,
            Err(e) => {
                eprintln!("ccn: dashboard unavailable ({e}); showing the summary instead");
                if !sarif_owns_stdout {
                    print_summary(&cli, &result, &report);
                }
            }
        }
    } else if cli.json {
        match serde_json::to_string_pretty(&result) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("ccn: failed to serialise result: {e}");
                operational_error = true;
            }
        }
    } else if !sarif_owns_stdout {
        print_summary(&cli, &result, &report);
    }

    // The dashboard replaces the summary and the HTML report; everything else
    // keeps writing the report as before.
    if !tui_handled && !cli.report_json && !cli.json {
        if cli.view {
            let path = temp_report_path(&result.root);
            match write_report(&path, &report) {
                Ok(()) => {
                    let shown = path.display().to_string().replace('\\', "/");
                    notice(sarif_owns_stdout, &format!("  view    {shown}"));
                    open_path(&path);
                }
                Err(e) => {
                    eprintln!("ccn: could not write report: {e}");
                    operational_error = true;
                }
            }
        } else if cli.wants_report() {
            let html = ccn::report::html::render(&report);
            match std::fs::write(&cli.report, html.as_bytes()) {
                Ok(()) => {
                    let shown = cli.report.display().to_string().replace('\\', "/");
                    notice(sarif_owns_stdout, &format!("  report  {shown}"));
                    if cli.open {
                        open_path(&cli.report);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "ccn: could not write report to {}: {e}",
                        cli.report.display()
                    );
                    operational_error = true;
                }
            }
        }
    }

    let mut gate_failed = false;

    // PR gate: any function that became complex in this change.
    if cli.fail_on_new {
        if let Some(baseline) = &report.baseline {
            // An unreadable file means the gate outcome is unknown, and unknown
            // is not a pass: the caller must treat this as an operational error.
            if baseline.files_unavailable > 0 {
                eprintln!(
                    "ccn: {} file(s) could not be read at {}; the baseline gate is incomplete",
                    baseline.files_unavailable, baseline.rev
                );
                operational_error = true;
            }
            if ccn::baseline::has_changes(baseline) {
                eprintln!(
                    "ccn: {} function(s) added or worsened above CCN {} since {}",
                    baseline.changes.len(),
                    baseline.threshold,
                    baseline.rev
                );
                gate_failed = true;
            }
        }
    }

    // CI gate: the percentage of functions above the McCabe limit. Only
    // meaningful when something was parsed.
    if let Some(limit) = cli.fail_over {
        match report.health.as_ref() {
            Some(health) => {
                if health.pct_over_10 > limit {
                    eprintln!(
                        "ccn: {:.1}% of functions exceed CCN 10 (limit {:.1}%)",
                        health.pct_over_10, limit
                    );
                    gate_failed = true;
                }
            }
            None => {
                // No exactly parsed functions: the gate cannot be evaluated, so
                // it must not silently pass.
                eprintln!("ccn: --fail-over could not be evaluated: no exactly parsed functions");
                operational_error = true;
            }
        }
    }

    let status = final_status(operational_error, gate_failed);
    if status != 0 {
        std::process::exit(status);
    }
}

/// The one place the process exit status is decided.
///
/// Precedence is operational error (2) > gate violation (1) > success (0), so
/// an output-mode failure can never look like a passing gate and a gate
/// failure can never be overwritten by later output handling.
fn final_status(operational_error: bool, gate_failed: bool) -> i32 {
    if operational_error {
        2
    } else if gate_failed {
        1
    } else {
        0
    }
}

fn usage_error(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("ccn: {message}");
    std::process::exit(2);
}

fn print_summary(
    cli: &ccn::cli::Cli,
    result: &ccn::model::ScanResult,
    report: &ccn::metrics::Report,
) {
    let opts = ccn::terminal::TerminalOptions {
        top: cli.top,
        ci: cli.ci,
        distributions: cli.ci,
    };
    print!("{}", ccn::terminal::render(result, report, &opts));
}

/// One informational line about a written file. Moved to stderr when SARIF
/// owns stdout, so that stream stays exactly one JSON document.
fn notice(quiet_stdout: bool, message: &str) {
    if quiet_stdout {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

fn write_report(path: &Path, report: &ccn::metrics::Report) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, ccn::report::html::render(report).as_bytes())
}

/// One stable temp path per repository, so `--view` replaces the previous
/// report instead of littering the temp directory.
fn temp_report_path(root: &str) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    std::env::temp_dir()
        .join("ccn")
        .join(format!("report-{:016x}.html", hasher.finish()))
}

fn open_path(path: &Path) {
    let target = path.display().to_string();
    let result = {
        #[cfg(target_os = "windows")]
        {
            // `start` treats its first quoted argument as the window title, so
            // the empty argument is required, not decorative.
            Command::new("cmd")
                .args(["/C", "start", "", &target])
                .spawn()
        }
        #[cfg(target_os = "macos")]
        {
            Command::new("open").arg(&target).spawn()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Command::new("xdg-open").arg(&target).spawn()
        }
    };
    if let Err(e) = result {
        eprintln!("ccn: could not open the report: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::final_status;

    #[test]
    fn operational_errors_outrank_gate_failures() {
        assert_eq!(final_status(true, true), 2);
        assert_eq!(final_status(true, false), 2);
        assert_eq!(final_status(false, true), 1);
        assert_eq!(final_status(false, false), 0);
    }
}
