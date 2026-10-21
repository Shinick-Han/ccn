use std::path::Path;
use std::process::Command;

use clap::Parser;

fn main() {
    let cli = ccn::cli::Cli::parse();

    let result = match ccn::loc::scan(&cli.scan_options()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ccn: {e}");
            std::process::exit(2);
        }
    };

    let mut report = ccn::metrics::build_with(&result, cli.hotspots, cli.history_limit);

    // Record the snapshot only after the report exists: the trend compares
    // reports, not scans. A failed write must not fail the command.
    if cli.trend {
        let snapshot = ccn::history::Snapshot::from_report(&report);
        match ccn::history::record(&result.root, snapshot) {
            Ok(trend) => report.trend = trend,
            Err(e) => eprintln!("ccn: could not write history: {e}"),
        }
    }

    if cli.report_json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("ccn: failed to serialise report: {e}");
                std::process::exit(2);
            }
        }
        return;
    }

    let mut exit_code = 0;

    if cli.tui {
        match ccn::tui::run(&result, &report) {
            Ok(()) => return,
            Err(e) => eprintln!("ccn: dashboard unavailable ({e}); showing the summary instead"),
        }
    }

    if cli.json {
        match serde_json::to_string_pretty(&result) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("ccn: failed to serialise result: {e}");
                std::process::exit(2);
            }
        }
    } else {
        let opts = ccn::terminal::TerminalOptions {
            top: cli.top,
            ci: cli.ci,
            distributions: cli.ci,
        };
        print!("{}", ccn::terminal::render(&result, &report, &opts));
    }

    if cli.view {
        let path = temp_report_path(&result.root);
        match write_report(&path, &report) {
            Ok(()) => {
                let shown = path.display().to_string().replace('\\', "/");
                println!("  view    {shown}");
                open_path(&path);
            }
            Err(e) => {
                eprintln!("ccn: could not write report: {e}");
                exit_code = 2;
            }
        }
    } else if cli.wants_report() && !cli.json {
        let html = ccn::report::html::render(&report);
        match std::fs::write(&cli.report, html.as_bytes()) {
            Ok(()) => {
                let shown = cli.report.display().to_string().replace('\\', "/");
                println!("  report  {shown}");
                if cli.open {
                    open_path(&cli.report);
                }
            }
            Err(e) => {
                eprintln!(
                    "ccn: could not write report to {}: {e}",
                    cli.report.display()
                );
                exit_code = 2;
            }
        }
    }

    // CI gate: the percentage of functions above the McCabe limit. Only
    // meaningful when something was actually parsed.
    if let (Some(limit), Some(health)) = (cli.fail_over, report.health.as_ref()) {
        if health.pct_over_10 > limit {
            eprintln!(
                "ccn: {:.1}% of functions exceed CCN 10 (limit {:.1}%)",
                health.pct_over_10, limit
            );
            exit_code = 1;
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
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
