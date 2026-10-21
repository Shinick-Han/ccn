use std::path::PathBuf;

use clap::Parser;

/// Repository code-health scanner: per-file LOC now, per-function CCN next.
#[derive(Debug, Parser)]
#[command(name = "ccn", version, about, long_about = None)]
pub struct Cli {
    /// Directory to scan.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Emit the machine-readable contract instead of the terminal summary.
    #[arg(long)]
    pub json: bool,

    /// Emit the computed report metrics (health, bins, worst files) as JSON.
    #[arg(long)]
    pub report_json: bool,

    /// Restrict to these languages (comma separated, case-insensitive).
    #[arg(long, value_delimiter = ',')]
    pub lang: Vec<String>,

    /// Count hidden files and directories.
    #[arg(long)]
    pub hidden: bool,

    /// Ignore .gitignore / .ignore / .tokeignore.
    #[arg(long)]
    pub no_ignore: bool,

    /// Also scan build-output directories (target, node_modules, dist, ...).
    #[arg(long)]
    pub no_default_excludes: bool,

    /// How many of the largest files to list (0 disables).
    #[arg(long, default_value_t = 10)]
    pub top: usize,

    /// Skip CCN analysis entirely (LOC only). Much faster on huge trees.
    #[arg(long)]
    pub no_ccn: bool,

    /// Analyse generated and vendored files as first-party code.
    ///
    /// By default they are detected (generator markers, reserved filenames,
    /// dependency directories) and left out of every score and histogram.
    #[arg(long)]
    pub include_generated: bool,

    /// Disable the per-file cache (re-parse everything).
    #[arg(long)]
    pub no_cache: bool,

    /// Collect git churn and rank hotspots.
    ///
    /// Runs `git log --numstat` for the last year (typically 0.5-2 s on a busy
    /// repository), so it is opt-in: a plain scan stays instant.
    #[arg(long)]
    pub hotspots: bool,

    /// Compare function complexity against a revision.
    ///
    /// Git: `HEAD~1`, a branch, or a commit. Perforce: a changelist number.
    /// Reports functions that were added above CCN 10 or crossed it.
    #[arg(long, value_name = "REV")]
    pub baseline: Option<String>,

    /// Write SARIF 2.1.0 for GitHub code scanning (`-` for stdout).
    ///
    /// With `--baseline` only the delta is emitted, so a PR sees the functions
    /// it introduced rather than the repository's backlog.
    #[arg(long, value_name = "PATH")]
    pub sarif: Option<String>,

    /// Minimum CCN emitted in a full (non-baseline) SARIF scan.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub sarif_min_ccn: u32,

    /// Exit with code 1 when `--baseline` finds added or worsened functions.
    #[arg(long, requires = "baseline")]
    pub fail_on_new: bool,

    /// How many commits/changelists the history pass considers.
    ///
    /// Git can scan a year cheaply; Perforce needs one `p4 describe` per
    /// changelist, so keeping this small is what keeps `--hotspots` usable.
    #[arg(long, value_name = "N", default_value_t = crate::vcs::DEFAULT_HISTORY_LIMIT)]
    pub history_limit: u32,

    /// Record this scan in `<repo>/.ccn/history.json` and show the delta from
    /// the previous one.
    #[arg(long, conflicts_with = "since")]
    pub trend: bool,

    /// Only analyse files changed since this revision.
    ///
    /// Git: `HEAD~1`, a branch, or a commit. Perforce: a changelist number
    /// (`123456`) or `opened` for pending local edits. Useful in CI and code
    /// review: the report then describes the change, not the whole repository.
    #[arg(long, value_name = "REV", conflicts_with = "trend")]
    pub since: Option<String>,

    /// Write the self-contained HTML report to this path.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "ccn-report.html",
        conflicts_with = "no_report"
    )]
    pub report: PathBuf,

    /// Do not write an HTML report.
    #[arg(long)]
    pub no_report: bool,

    /// Open the report in the default browser after writing it.
    #[arg(long)]
    pub open: bool,

    /// Open the interactive terminal dashboard instead of printing the summary.
    #[arg(long, conflicts_with_all = ["json", "report_json", "ci", "view"])]
    pub tui: bool,

    /// Write the report to a temp file and open it, leaving the repo clean.
    #[arg(
        long,
        conflicts_with_all = ["json", "report_json", "no_report", "report"]
    )]
    pub view: bool,

    /// ASCII-only output for CI logs (no unicode bars, no colour).
    #[arg(long)]
    pub ci: bool,

    /// Exit with code 1 if the share of functions above CCN 10 exceeds this
    /// percentage (e.g. `--fail-over 2.0`).
    #[arg(long, value_name = "PCT")]
    pub fail_over: Option<f64>,
}

impl Cli {
    pub fn scan_options(&self) -> crate::loc::ScanOptions {
        crate::loc::ScanOptions {
            root: self.path.clone(),
            hidden: self.hidden,
            no_ignore: self.no_ignore,
            no_default_excludes: self.no_default_excludes,
            langs: self.lang.clone(),
            no_ccn: self.no_ccn,
            include_generated: self.include_generated,
            no_cache: self.no_cache,
            since: self.since.clone(),
        }
    }

    /// `true` when the HTML report should be written.
    pub fn wants_report(&self) -> bool {
        !self.no_report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_current_directory() {
        let cli = Cli::parse_from(["ccn"]);
        assert_eq!(cli.path, PathBuf::from("."));
        assert!(!cli.json);
    }

    #[test]
    fn accepts_multiple_languages() {
        let cli = Cli::parse_from(["ccn", "src", "--lang", "rust,python"]);
        assert_eq!(cli.lang, vec!["rust", "python"]);
    }

    #[test]
    fn no_ccn_reaches_scan_options() {
        let cli = Cli::parse_from(["ccn"]);
        assert!(!cli.no_ccn);
        assert!(!cli.scan_options().no_ccn, "CCN is on by default");

        let cli = Cli::parse_from(["ccn", "--no-ccn"]);
        assert!(cli.no_ccn);
        assert!(cli.scan_options().no_ccn);
    }

    #[test]
    fn report_defaults_to_ccn_report_html() {
        let cli = Cli::parse_from(["ccn"]);
        assert_eq!(cli.report, PathBuf::from("ccn-report.html"));
        assert!(!cli.no_report);
        assert!(cli.wants_report());
        assert!(!cli.open);
        assert!(!cli.ci);
        assert_eq!(cli.fail_over, None);
        assert_eq!(cli.top, 10);
    }

    #[test]
    fn no_report_disables_the_html_report() {
        let cli = Cli::parse_from(["ccn", "--no-report"]);
        assert!(cli.no_report);
        assert!(!cli.wants_report());
    }

    #[test]
    fn report_and_no_report_conflict() {
        let err = Cli::try_parse_from(["ccn", "--no-report", "--report", "x.html"]);
        assert!(
            err.is_err(),
            "--no-report and --report must not be combined"
        );
    }

    #[test]
    fn fail_over_parses_a_fractional_percentage() {
        let cli = Cli::parse_from(["ccn", "--fail-over", "2.5"]);
        assert_eq!(cli.fail_over, Some(2.5));
    }

    #[test]
    fn report_accepts_a_nested_path() {
        let cli = Cli::parse_from(["ccn", "--report", "out/x.html"]);
        assert_eq!(cli.report, PathBuf::from("out/x.html"));
    }

    #[test]
    fn include_generated_defaults_off_and_reaches_scan_options() {
        let cli = Cli::parse_from(["ccn"]);
        assert!(!cli.include_generated);
        assert!(
            !cli.scan_options().include_generated,
            "generated files are excluded by default"
        );

        let cli = Cli::parse_from(["ccn", "--include-generated"]);
        assert!(cli.include_generated);
        assert!(cli.scan_options().include_generated);
        // The other flags keep their meaning.
        assert!(!cli.scan_options().no_ccn);
    }

    #[test]
    fn ci_flag_parses() {
        let cli = Cli::parse_from(["ccn", "--ci"]);
        assert!(cli.ci);
    }

    #[test]
    fn tui_flag_parses_and_conflicts_with_machine_output() {
        let cli = Cli::parse_from(["ccn", "--tui"]);
        assert!(cli.tui);
        assert!(!cli.view);
        assert!(Cli::try_parse_from(["ccn", "--tui", "--json"]).is_err());
        assert!(Cli::try_parse_from(["ccn", "--tui", "--ci"]).is_err());
        assert!(Cli::try_parse_from(["ccn", "--tui", "--view"]).is_err());
    }

    #[test]
    fn view_flag_parses_and_conflicts_with_an_explicit_report_path() {
        let cli = Cli::parse_from(["ccn", "--view"]);
        assert!(cli.view);
        assert!(Cli::try_parse_from(["ccn", "--view", "--report", "x.html"]).is_err());
        assert!(Cli::try_parse_from(["ccn", "--view", "--json"]).is_err());
        // The default report path does not count as "explicitly provided".
        assert!(!Cli::try_parse_from(["ccn", "--view"]).unwrap().no_report);
    }
}
