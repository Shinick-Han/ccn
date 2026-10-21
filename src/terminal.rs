//! Human-readable terminal summary.
//!
//! Deliberately plain: no colour, and every line stays under ~100 columns so it
//! survives being pasted into a chat or a CI log. `--ci` swaps every non-ASCII
//! glyph (bar cells, separators, the truncation ellipsis) for an ASCII
//! equivalent, so the whole block can be piped through tools that mangle UTF-8.

use crate::metrics::{Histogram, Report};
use crate::model::ScanResult;

/// Widest bar, in cells. Bars are scaled to the largest bin, never to a size
/// axis: these are counts, and a count chart whose tallest bar is not full is
/// harder to read, not more honest.
const MAX_BAR: usize = 24;

/// Column where `(experimental)` starts on the health line.
const HEALTH_SUFFIX_COL: usize = 48;

/// Everything the terminal renderer needs that is not in the `Report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOptions {
    pub top: usize,
    pub ci: bool,
    /// Print ASCII histograms (only meaningful with `ci` or `--verbose`).
    pub distributions: bool,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            top: 10,
            ci: false,
            distributions: false,
        }
    }
}

/// Thousands separator without pulling in a formatting crate.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Tail-truncated path, with an ASCII ellipsis in `--ci` mode.
fn excerpt_with(path: &str, max: usize, ci: bool) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let marker = if ci { "..." } else { "…" };
    let keep = max.saturating_sub(marker.chars().count());
    let tail: String = path
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{marker}{tail}")
}

fn excerpt(path: &str, max: usize) -> String {
    excerpt_with(path, max, false)
}

/// Bar length for `value` on a scale whose maximum is `max`.
///
/// Rounds to the nearest cell, but keeps any non-zero value visible as one cell:
/// a bin with 1,412 functions and one with 6 must not both collapse to "empty".
pub fn bar_cells(value: f64, max: f64) -> usize {
    if value.is_nan() || max.is_nan() || value <= 0.0 || max <= 0.0 {
        return 0;
    }
    let cells = (value / max * MAX_BAR as f64).round() as usize;
    cells.clamp(1, MAX_BAR)
}

fn bar_char(ci: bool) -> char {
    if ci {
        '#'
    } else {
        '█'
    }
}

/// Bin labels carry en dashes and `≥`; CI logs get `-` and `>=`.
fn bin_label(label: &str, ci: bool) -> String {
    if ci {
        label.replace('–', "-").replace('≥', ">=")
    } else {
        label.to_string()
    }
}

/// Head-truncated label (names read left to right), with an ASCII ellipsis in
/// `--ci` mode.
fn truncate_head_with(value: &str, max: usize, ci: bool) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let marker = if ci { "..." } else { "…" };
    let keep = max.saturating_sub(marker.chars().count());
    let head: String = value.chars().take(keep).collect();
    format!("{head}{marker}")
}

/// One-decimal delta with an explicit sign, except zero: "0.0" is not "+0.0"
/// and not "-0.0", because a non-change should not look like a direction.
fn signed(delta: f64) -> String {
    let rounded = (delta * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "0.0".to_string()
    } else {
        format!("{rounded:+.1}")
    }
}

/// Comparison with the previous recorded snapshot.
///
/// Absent without `--trend` (or on the first recorded run): an empty string, not
/// a line full of dashes, because "no history" and "no change" are different.
/// The sign is always spelled out so the line survives being pasted anywhere
/// colour would not.
fn trend_block(report: &Report, sep: &str, ci: bool) -> String {
    let Some(trend) = &report.trend else {
        return String::new();
    };
    let arrow = if ci { "->" } else { "→" };
    let mut out = String::from("  Trend  ");

    match (
        trend.previous.health,
        trend.current.health,
        trend.health_delta(),
    ) {
        (Some(prev), Some(cur), Some(delta)) => {
            out.push_str(&format!(
                "health {prev:.1} {arrow} {cur:.1} ({})",
                signed(delta)
            ));
        }
        // One side unmeasured: show the change, not an invented score.
        _ => out.push_str("health n/a"),
    }

    // Only when the letter actually moved; a repeated grade is noise.
    if let (Some(prev), Some(cur)) = (trend.previous.grade, trend.current.grade) {
        if prev != cur {
            out.push_str(&format!(" {sep} grade {prev} {arrow} {cur}"));
        }
    }

    let delta = trend.pct_over_10_delta();
    out.push_str(&format!(
        " {sep} >10 {:.1}% {arrow} {:.1}% ({}pp)",
        trend.previous.pct_over_10,
        trend.current.pct_over_10,
        signed(delta),
    ));
    out.push('\n');

    // A delta across a tool/band change is still printed (the numbers are real
    // measurements), but the reader is told not to compare them.
    if let Some(warning) = trend.compatibility_warning() {
        let mut line = format!("  !      {warning}");
        if line.chars().count() > 100 {
            line = line.chars().take(97).collect::<String>();
            line.push_str("...");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Functions that became complex since `--baseline`, right under the trend.
fn baseline_block(report: &Report, sep: &str) -> String {
    let Some(baseline) = &report.baseline else {
        return String::new();
    };
    let added = baseline
        .changes
        .iter()
        .filter(|c| c.kind == crate::baseline::Kind::Added)
        .count();
    let worsened = baseline.changes.len() - added;

    let best_effort = baseline
        .changes
        .iter()
        .filter(|c| c.tier == crate::ccn::types::CcnTier::BestEffort)
        .count();

    let mut out = format!(
        "  Since {}  {added} new >{} {sep} {worsened} worsened   ({} files compared, {} skipped",
        baseline.rev, baseline.threshold, baseline.files_compared, baseline.files_skipped,
    );
    if baseline.files_unavailable > 0 {
        out.push_str(&format!(", {} unavailable", baseline.files_unavailable));
    }
    out.push_str(")\n");
    if best_effort > 0 {
        out.push_str(&format!(
            "  {} finding(s) come from best-effort parses (syntax errors) and are advisory\n",
            best_effort
        ));
    }
    for change in baseline.changes.iter().take(5) {
        let marker = match change.kind {
            crate::baseline::Kind::Added => '+',
            crate::baseline::Kind::Worsened => '~',
        };
        // The reasons are the actionable part: "ccn +3, nesting +1" tells the
        // reviewer what grew, not just that something did.
        let why = if change.reasons.is_empty() {
            match change.base_ccn {
                Some(previous) => format!(" (was {previous})"),
                None => String::new(),
            }
        } else {
            format!("  [{}]", change.reasons.join(", "))
        };
        let tier_tag = if change.tier == crate::ccn::types::CcnTier::BestEffort {
            " (best-effort)"
        } else {
            ""
        };
        let mut line = format!(
            "    {marker} {:<4} {:<24} {}:{}{}{}",
            change.ccn, change.name, change.path, change.line, tier_tag, why
        );
        if line.chars().count() > 100 {
            line = line.chars().take(97).collect::<String>();
            line.push_str("...");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn health_block(report: &Report, sep: &str) -> String {
    let mut out = String::new();
    let Some(health) = &report.health else {
        // "n/a" is not "0": no functions were measured, so the score is
        // unknown. Printing 100 here would read as a perfect repository.
        out.push_str("  Health  n/a   no parsed functions\n");
        return out;
    };

    let mut head = format!(
        "  Health {:.1} / 100   grade {}",
        health.score, health.grade
    );
    while head.chars().count() < HEALTH_SUFFIX_COL {
        head.push(' ');
    }
    if report.health_experimental {
        head.push_str("(experimental)");
    }
    out.push_str(head.trim_end());
    out.push('\n');
    out.push_str(&format!(
        "  {:.2} of lines above the McCabe limit {sep} {} functions measured\n",
        health.debt,
        thousands(health.functions),
    ));
    if health.best_effort_functions > 0 {
        out.push_str(&format!(
            "  {} functions in best-effort files excluded from the score \
             (syntax errors)\n",
            thousands(health.best_effort_functions),
        ));
    }
    out.push_str(&format!(
        "  {} functions > 10 ({:.1}%) {sep} {} > 20 {sep} {} > 50\n",
        thousands(health.over_10),
        health.pct_over_10,
        thousands(health.over_20),
        thousands(health.over_50),
    ));
    out
}

fn stats_block(report: &Report, sep: &str) -> String {
    let stats = &report.stats;
    let mut out = String::new();

    out.push_str(&format!(
        "  LOC   median {} {sep} p90 {} {sep} p99 {} {sep} max {}   \
         (top 10% of files = {:.0}% of lines)\n",
        thousands(stats.loc_median as usize),
        thousands(stats.loc_p90 as usize),
        thousands(stats.loc_p99 as usize),
        thousands(stats.loc_max as usize),
        stats.loc_share_top10pct,
    ));

    if stats.functions_total == 0 {
        out.push_str(&format!(
            "  CCN   no parsed functions ({} heuristic files)\n",
            thousands(stats.files_heuristic),
        ));
    } else {
        out.push_str(&format!(
            "  CCN   median {} {sep} p90 {} {sep} p99 {} {sep} max {}   \
             ({} exact / {} best-effort / {} heuristic files)\n",
            thousands(stats.ccn_median as usize),
            thousands(stats.ccn_p90 as usize),
            thousands(stats.ccn_p99 as usize),
            thousands(stats.ccn_max as usize),
            thousands(stats.files_exact),
            thousands(stats.files_best_effort),
            thousands(stats.files_heuristic),
        ));
    }

    // History is opt-in by presence: no repository, no line. The share is the
    // same number the HTML report calls the hotspot debt share.
    if report.git_available {
        // Label the backend instead of saying "Git" for a Perforce workspace.
        let label = match report.vcs.as_deref() {
            Some("p4") => "P4",
            _ => "Git",
        };
        let unit = if report.vcs.as_deref() == Some("p4") {
            "changelists"
        } else {
            "commits"
        };
        out.push_str(&format!(
            "  {label:<8}{} {unit}/365d {sep} hotspots: top 10% of files = {:.1}% of lines\n",
            thousands(report.commits_scanned as usize),
            report.hotspot_debt_share,
        ));
    }
    out
}

/// Top three hotspots, printed under the largest-files block.
///
/// Three, not twenty: the terminal is a summary, and a reader who wants the
/// table opens the HTML report.
fn hotspots_block(report: &Report, sep: &str, ci: bool) -> String {
    let mut out = String::new();
    out.push_str("  hotspots\n");
    for row in report.hotspots.iter().take(3) {
        let label = excerpt_with(&row.path, 24, ci);
        let mut line = format!(
            "    {:.2}  {:<24} {} CCN {sep} {} commits {sep} {} lines changed",
            row.hotspot,
            label,
            thousands(row.ccn_max as usize),
            thousands(row.commits as usize),
            thousands(row.lines_changed as usize),
        );
        let budget = 100usize.saturating_sub(line.chars().count());
        line.push_str(&author_suffix(row, ci, budget));
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// ` · alice (+2)` after a hotspot row, or nothing when history did not name an
/// author.
///
/// `budget` is the room left on a 100-column row. The author is capped at 24
/// characters, but the row wins: a long path and a long name must not wrap a CI
/// log, so the name is shortened further (or dropped) rather than overflow.
fn author_suffix(row: &crate::metrics::HotspotRow, ci: bool, budget: usize) -> String {
    if row.top_author.is_empty() {
        return String::new();
    }
    let lead = if ci { " - " } else { " · " };
    let tail = if row.authors > 1 {
        format!(" (+{})", row.authors - 1)
    } else {
        String::new()
    };
    let room = budget
        .saturating_sub(lead.chars().count())
        .saturating_sub(tail.chars().count());
    // Fewer than 4 columns (one glyph plus an ellipsis) reads as noise.
    if room < 4 {
        return String::new();
    }
    let name = truncate_head_with(&row.top_author, room.min(24), ci);
    format!("{lead}{name}{tail}")
}

/// One histogram block, prefixed by a blank line.
///
/// `dual` renders the LOC chart's two encodings (% of files | % of lines) on a
/// shared scale, so the pair in a row is directly comparable.
fn histogram_block(out: &mut String, title: &str, hist: &Histogram, ci: bool, dual: bool) {
    if hist.is_empty() {
        return;
    }

    let labels: Vec<String> = hist.bins.iter().map(|b| bin_label(&b.label, ci)).collect();
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let max_count = hist.bins.iter().map(|b| b.share_count).fold(0.0, f64::max);
    let max_weight = hist.bins.iter().map(|b| b.share_weight).fold(0.0, f64::max);
    let scale = if dual {
        max_count.max(max_weight)
    } else {
        max_count
    };
    let ch = bar_char(ci);

    out.push_str(&format!("\n  {title}\n"));
    for (bin, label) in hist.bins.iter().zip(&labels) {
        let count = thousands(bin.count);
        let bar = ch.to_string().repeat(bar_cells(bin.share_count, scale));
        let row = if dual {
            let weight_bar = ch.to_string().repeat(bar_cells(bin.share_weight, scale));
            let pair = if ci { " | " } else { " · " };
            let visual = if weight_bar.is_empty() {
                bar.clone()
            } else if bar.is_empty() {
                weight_bar
            } else {
                format!("{bar}{pair}{weight_bar}")
            };
            format!(
                "    {label:<width$}  {count:>8}  {:>5.1}%  {visual}",
                bin.share_count,
            )
        } else {
            format!(
                "    {label:<width$}  {count:>8}  {:>5.1}%  {bar}",
                bin.share_count,
            )
        };
        out.push_str(row.trim_end());
        out.push('\n');
    }
}

/// Render the whole summary. `opts.top` controls how many largest files are
/// listed; `opts.distributions` adds the histogram blocks.
pub fn render(result: &ScanResult, report: &Report, opts: &TerminalOptions) -> String {
    let sep = if opts.ci { "-" } else { "·" };
    let mut out = String::new();

    out.push_str(&format!("ccn {}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!("  {}\n", result.root));
    out.push_str(&format!(
        "  {} files {sep} {} lines {sep} {} code {sep} {} comment {sep} {} blank\n",
        thousands(result.files_analyzed),
        thousands(result.loc_total),
        thousands(result.loc),
        thousands(result.comments),
        thousands(result.blanks),
    ));
    // Only when something was actually dropped: a zero line would suggest the
    // tool looked for generated files and found the concept suspicious.
    let excluded = result.files_excluded;
    if !excluded.is_empty() {
        out.push_str(&format!(
            "  excluded  {} generated {sep} {} vendored files\n",
            thousands(excluded.generated),
            thousands(excluded.vendored),
        ));
    }
    out.push('\n');

    out.push_str(&health_block(report, sep));
    // Directly under the score it compares: history is a gloss on health, not
    // a separate section.
    out.push_str(&trend_block(report, sep, opts.ci));
    out.push_str(&baseline_block(report, sep));
    out.push_str(&stats_block(report, sep));

    if opts.distributions {
        histogram_block(&mut out, "CCN per function", &report.ccn, opts.ci, false);
        histogram_block(
            &mut out,
            "LOC per file   (% files | % lines)",
            &report.loc,
            opts.ci,
            true,
        );
    }

    if opts.top > 0 && !result.files.is_empty() {
        // Comment-only files (docs, licenses) are legitimate rows in the JSON
        // contract but meaningless as "largest code files".
        let mut files: Vec<_> = result.files.iter().filter(|f| f.loc > 0).collect();
        files.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.path.cmp(&b.path)));
        out.push_str("  largest\n");
        for f in files.iter().take(opts.top) {
            let label = if opts.ci {
                excerpt_with(&f.path, 64, true)
            } else {
                excerpt(&f.path, 64)
            };
            out.push_str(&format!("    {:>8}  {label}\n", thousands(f.loc)));
        }
    }

    // Only when there is history to show; without git every score is zero.
    if opts.top > 0 && report.git_available && !report.hotspots.is_empty() {
        out.push_str(&hotspots_block(report, sep, opts.ci));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::{CcnSummary, CcnTier, FileCcn, FunctionCcn};
    use crate::metrics::{build, CCN_BINS};

    #[test]
    fn thousands_separates_groups_of_three() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn excerpt_keeps_the_tail() {
        let path = "a/very/long/path/that/needs/truncation/file.rs";
        let e = excerpt(path, 20);
        assert!(e.chars().count() <= 20);
        assert!(e.ends_with("file.rs"));
    }

    #[test]
    fn excerpt_is_ascii_in_ci_mode() {
        let path = "a/very/long/path/that/needs/truncation/file.rs";
        let e = excerpt_with(path, 20, true);
        assert!(e.is_ascii(), "{e}");
        assert!(e.ends_with("file.rs"));
        assert!(e.chars().count() <= 20);
    }

    fn func(ccn: u32, nloc: usize) -> FunctionCcn {
        FunctionCcn {
            name: "f".into(),
            line: 1,
            end_line: nloc,
            nloc,
            ccn,
            params: 0,
            max_nesting: 0,
        }
    }

    fn exact_file(path: &str, loc: usize, ccns: &[u32]) -> crate::model::FileLoc {
        let functions: Vec<FunctionCcn> = ccns
            .iter()
            .map(|c| func(*c, (loc / ccns.len().max(1)).max(1)))
            .collect();
        let summary = CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions,
        });
        file(path, loc, Some(summary))
    }

    fn file(path: &str, loc: usize, ccn: Option<CcnSummary>) -> crate::model::FileLoc {
        crate::model::FileLoc {
            path: path.into(),
            lang: "Rust".into(),
            loc_total: loc + 2,
            loc,
            comments: 1,
            blanks: 1,
            ccn,
            is_generated: false,
            is_vendored: false,
        }
    }

    fn scan(files: Vec<crate::model::FileLoc>) -> ScanResult {
        let loc: usize = files.iter().map(|f| f.loc).sum();
        let comments: usize = files.iter().map(|f| f.comments).sum();
        let blanks: usize = files.iter().map(|f| f.blanks).sum();
        let functions_total = files
            .iter()
            .filter_map(|f| f.ccn.as_ref())
            .filter(|s| s.tier != CcnTier::Heuristic)
            .map(|s| s.functions)
            .sum();
        ScanResult {
            root: "C:/repo".to_string(),
            files_analyzed: files.len(),
            loc_total: loc + comments + blanks,
            loc,
            comments,
            blanks,
            files_excluded: crate::model::ExcludedFiles::default(),
            functions_total,
            ccn: None,
            languages: Vec::new(),
            files,
        }
    }

    /// A small repo with a spread of CCN values so every chart has bars.
    fn sample_scan() -> ScanResult {
        scan(vec![
            exact_file("src/a.rs", 120, &[1, 1, 2, 4, 6, 20]),
            exact_file("src/b.rs", 80, &[1, 3, 5, 12]),
            exact_file("src/big.rs", 3000, &[1, 2, 2, 51, 120]),
            file("vendor/x.vb", 400, Some(CcnSummary::heuristic(43))),
            file(
                "docs/very/long/path/that/needs/truncation/because/it/is/deep/file.rs",
                60,
                Some(CcnSummary::from_file(FileCcn {
                    tier: CcnTier::Exact,
                    functions: vec![func(1, 60)],
                })),
            ),
        ])
    }

    #[test]
    fn health_line_has_score_and_grade() {
        let result = sample_scan();
        let report = build(&result);
        assert!(report.health.is_some(), "fixture must produce a score");
        let out = render(&result, &report, &TerminalOptions::default());

        assert!(out.contains("Health"), "{out}");
        assert!(out.contains("/ 100"), "{out}");
        assert!(out.contains("grade "), "{out}");
        // The score itself is present, not just the label.
        let health = report.health.as_ref().unwrap();
        assert!(
            out.contains(&format!("Health {:.1} / 100", health.score)),
            "{out}"
        );
    }

    #[test]
    fn experimental_reports_are_marked() {
        let result = sample_scan();
        let mut report = build(&result);
        report.health_experimental = true;
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("experimental"), "{out}");
    }

    #[test]
    fn ci_output_is_pure_ascii_and_fits_ci_logs() {
        let result = sample_scan();
        let report = build(&result);
        let opts = TerminalOptions {
            top: 3,
            ci: true,
            distributions: true,
        };
        let out = render(&result, &report, &opts);

        assert!(out.is_ascii(), "non-ASCII in CI output:\n{out}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= 100,
                "line longer than 100 chars: {line:?}"
            );
        }
        assert!(out.contains('#'), "bars must use '#' in CI mode:\n{out}");
    }

    #[test]
    fn missing_health_prints_not_available_and_never_a_perfect_score() {
        // Parsed file with no functions: LOC is measured, CCN is unknowable.
        // Three files of different sizes keep the "top 10%" share under 100 so
        // this test can assert on the literal string "100" without ambiguity.
        let result = scan(vec![
            file(
                "a.rs",
                42,
                Some(CcnSummary::from_file(FileCcn {
                    tier: CcnTier::Exact,
                    functions: Vec::new(),
                })),
            ),
            file("b.rs", 7, None),
            file("c.rs", 3, None),
        ]);
        let report = build(&result);
        assert!(report.health.is_none());

        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("Health  n/a   no parsed functions"), "{out}");
        assert!(!out.contains("100"), "{out}");
    }

    #[test]
    fn bars_scale_to_the_largest_bin_and_drop_empty_bins() {
        assert_eq!(bar_cells(10.0, 10.0), MAX_BAR);
        assert_eq!(bar_cells(0.0, 10.0), 0);
        assert_eq!(bar_cells(5.0, 10.0), MAX_BAR / 2);
        assert_eq!(bar_cells(0.001, 10.0), 1, "a non-zero bin stays visible");
        assert_eq!(bar_cells(1.0, 0.0), 0, "an empty chart has no scale");

        // The rendered chart agrees: the tallest bin fills all 24 cells.
        let result = sample_scan();
        let report = build(&result);
        let out = render(
            &result,
            &report,
            &TerminalOptions {
                top: 0,
                ci: false,
                distributions: true,
            },
        );
        assert!(out.contains(&"█".repeat(MAX_BAR)), "{out}");
    }

    #[test]
    fn distributions_block_lists_every_bin_label() {
        let result = sample_scan();
        let report = build(&result);
        let out = render(
            &result,
            &report,
            &TerminalOptions {
                top: 0,
                ci: false,
                distributions: true,
            },
        );

        assert!(out.contains("CCN per function"), "{out}");
        assert!(out.contains("LOC per file"), "{out}");
        for label in CCN_BINS.labels {
            assert!(
                out.contains(label),
                "missing CCN bin label {label:?}:\n{out}"
            );
        }
        for bin in &report.loc.bins {
            assert!(
                out.contains(&bin.label),
                "missing LOC bin label {:?}:\n{out}",
                bin.label
            );
        }
    }

    #[test]
    fn distributions_are_off_by_default() {
        let result = sample_scan();
        let report = build(&result);
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(!out.contains("CCN per function"), "{out}");
    }

    #[test]
    fn largest_list_only_appears_when_requested() {
        let result = sample_scan();
        let report = build(&result);

        let none = render(
            &result,
            &report,
            &TerminalOptions {
                top: 0,
                ..TerminalOptions::default()
            },
        );
        assert!(!none.contains("largest"), "{none}");

        let some = render(
            &result,
            &report,
            &TerminalOptions {
                top: 2,
                ..TerminalOptions::default()
            },
        );
        assert!(some.contains("largest"), "{some}");
        assert!(some.contains("big.rs"), "{some}");
    }

    #[test]
    fn heuristic_files_are_named_in_the_ccn_line() {
        let result = sample_scan();
        let report = build(&result);
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(
            out.contains("exact / 0 best-effort / 1 heuristic files"),
            "{out}"
        );
    }

    #[test]
    fn exclusions_are_reported_under_the_meta_line() {
        let generated = file("src/api.gen.ts", 40, None);
        let vendored = file("vendor/lib/x.js", 900, None);
        let mut result = scan(vec![file("src/a.rs", 10, None), generated, vendored]);
        result.files_excluded = crate::model::ExcludedFiles {
            generated: 12,
            vendored: 340,
        };

        let out = render(&result, &build(&result), &TerminalOptions::default());
        assert!(
            out.contains("  excluded  12 generated · 340 vendored files\n"),
            "{out}"
        );
        // Directly under the meta line, before the blank separator.
        let lines: Vec<&str> = out.lines().collect();
        let meta = lines
            .iter()
            .position(|l| l.ends_with("blank"))
            .expect("meta line");
        assert!(lines[meta + 1].starts_with("  excluded"), "{out}");

        let ci = render(
            &result,
            &build(&result),
            &TerminalOptions {
                ci: true,
                ..TerminalOptions::default()
            },
        );
        assert!(
            ci.contains("  excluded  12 generated - 340 vendored files\n"),
            "{ci}"
        );
        assert!(ci.is_ascii(), "{ci}");
    }

    #[test]
    fn no_exclusion_line_when_nothing_was_excluded() {
        let result = sample_scan();
        let out = render(&result, &build(&result), &TerminalOptions::default());
        assert!(!out.contains("excluded"), "{out}");
    }

    /// A report with history attached: the numbers a scan of a real repository
    /// with git would produce.
    fn with_git(report: &mut crate::metrics::Report) {
        report.git_available = true;
        report.commits_scanned = 1_234;
        report.hotspot_debt_share = 41.2;
        report.hotspots = vec![
            crate::metrics::HotspotRow {
                path: "src/app.rs".into(),
                loc: 900,
                ccn_max: 302,
                risk: 0.61,
                commits: 48,
                lines_changed: 12_340,
                hotspot: 0.87,
                top_author: String::new(),
                authors: 0,
            },
            crate::metrics::HotspotRow {
                path: "src/parser/grammar.rs".into(),
                loc: 1_930,
                ccn_max: 74,
                risk: 0.62,
                commits: 21,
                lines_changed: 4_100,
                hotspot: 0.55,
                top_author: String::new(),
                authors: 0,
            },
            crate::metrics::HotspotRow {
                path: "a/very/long/path/that/needs/truncation/because/it/is/deep/file.rs".into(),
                loc: 60,
                ccn_max: 12,
                risk: 0.3,
                commits: 3,
                lines_changed: 90,
                hotspot: 0.21,
                top_author: String::new(),
                authors: 0,
            },
            crate::metrics::HotspotRow {
                path: "fourth.rs".into(),
                loc: 10,
                ccn_max: 2,
                risk: 0.1,
                commits: 1,
                lines_changed: 2,
                hotspot: 0.05,
                top_author: String::new(),
                authors: 0,
            },
        ];
    }

    #[test]
    fn the_git_line_appears_only_when_history_was_collected() {
        let result = sample_scan();
        let plain = render(&result, &build(&result), &TerminalOptions::default());
        assert!(!plain.contains("commits/365d"), "{plain}");
        assert!(!plain.contains("hotspots"), "{plain}");

        let mut report = build(&result);
        with_git(&mut report);
        let out = render(&result, &report, &TerminalOptions::default());

        assert!(
            out.contains(
                "  Git     1,234 commits/365d · hotspots: top 10% of files = 41.2% of lines\n"
            ),
            "{out}"
        );
        // Directly below the CCN line, above the largest block.
        let lines: Vec<&str> = out.lines().collect();
        let ccn = lines
            .iter()
            .position(|l| l.starts_with("  CCN "))
            .expect("CCN line");
        assert!(lines[ccn + 1].starts_with("  Git"), "{out}");
    }

    #[test]
    fn the_three_hottest_files_are_listed_under_largest() {
        let result = sample_scan();
        let mut report = build(&result);
        with_git(&mut report);
        let out = render(&result, &report, &TerminalOptions::default());

        assert!(out.contains("  hotspots\n"), "{out}");
        assert!(out.contains("0.87  src/app.rs"), "{out}");
        assert!(
            out.contains("302 CCN · 48 commits · 12,340 lines changed"),
            "{out}"
        );
        assert!(!out.contains("fourth.rs"), "only the top three:\n{out}");
        // Truncated, not wrapped.
        let hotspot_line = out
            .lines()
            .find(|l| l.contains("0.87"))
            .expect("hotspot row");
        assert!(hotspot_line.chars().count() <= 100, "{hotspot_line:?}");
    }

    #[test]
    fn ci_git_output_is_pure_ascii() {
        let result = sample_scan();
        let mut report = build(&result);
        with_git(&mut report);
        let out = render(
            &result,
            &report,
            &TerminalOptions {
                top: 3,
                ci: true,
                distributions: true,
            },
        );

        assert!(out.is_ascii(), "non-ASCII in CI output:\n{out}");
        assert!(
            out.contains(
                "  Git     1,234 commits/365d - hotspots: top 10% of files = 41.2% of lines\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("302 CCN - 48 commits - 12,340 lines changed"),
            "{out}"
        );
        for line in out.lines() {
            assert!(
                line.chars().count() <= 100,
                "line longer than 100 chars: {line:?}"
            );
        }
    }

    /// A snapshot pair with the fields the trend line reads.
    fn trend(prev: Option<f64>, cur: Option<f64>, pct: (f64, f64)) -> crate::history::Trend {
        use crate::history::{Snapshot, Trend};
        let snap = |health: Option<f64>, grade: Option<char>, pct_over_10: f64| Snapshot {
            at_unix: 0,
            health,
            grade,
            loc: 0,
            functions: 0,
            pct_over_10,
            ccn_max: 0,
            files_measured: 0,
            tool_version: "0.3.0".to_string(),
            bin_spec_version: 1,
            health_calibration: "test".to_string(),
        };
        Trend {
            // Same letter on both sides unless a test builds its own pair.
            previous: snap(prev, prev.map(|_| 'C'), pct.0),
            current: snap(cur, cur.map(|_| 'C'), pct.1),
        }
    }

    #[test]
    fn trend_line_renders_arrows_and_signs_under_health() {
        let result = sample_scan();
        let mut report = build(&result);
        report.trend = Some(crate::history::Trend {
            previous: crate::history::Snapshot {
                at_unix: 0,
                health: Some(89.0),
                grade: Some('C'),
                loc: 1,
                functions: 1,
                pct_over_10: 5.3,
                ccn_max: 1,
                files_measured: 1,
                tool_version: "0.3.0".to_string(),
                bin_spec_version: 1,
                health_calibration: "test".to_string(),
            },
            current: crate::history::Snapshot {
                at_unix: 0,
                health: Some(87.2),
                grade: Some('D'),
                loc: 1,
                functions: 1,
                pct_over_10: 5.8,
                ccn_max: 1,
                files_measured: 1,
                tool_version: "0.3.0".to_string(),
                bin_spec_version: 1,
                health_calibration: "test".to_string(),
            },
        });
        let out = render(&result, &report, &TerminalOptions::default());

        assert!(
            out.contains(
                "  Trend  health 89.0 → 87.2 (-1.8) · grade C → D · \
                 >10 5.3% → 5.8% (+0.5pp)\n"
            ),
            "{out}"
        );

        // Directly under the health block, above the LOC statistics.
        let lines: Vec<&str> = out.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("  Trend"))
            .expect("trend line");
        assert!(lines[at - 1].contains("functions > 10"), "{out}");
        assert!(lines[at + 1].starts_with("  LOC "), "{out}");
    }

    #[test]
    fn trend_line_signs_a_positive_delta() {
        let result = sample_scan();
        let mut report = build(&result);
        report.trend = Some(trend(Some(80.0), Some(82.5), (4.0, 4.4)));
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("health 80.0 → 82.5 (+2.5)"), "{out}");
        assert!(out.contains(">10 4.0% → 4.4% (+0.4pp)"), "{out}");
        // Same grade on both sides is omitted.
        let line = out
            .lines()
            .find(|l| l.starts_with("  Trend"))
            .expect("trend");
        assert!(!line.contains("grade"), "{line:?}");
    }

    #[test]
    fn trend_line_prints_zero_without_a_sign() {
        let result = sample_scan();
        let mut report = build(&result);
        report.trend = Some(trend(Some(80.0), Some(80.0), (5.0, 5.0)));
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("health 80.0 → 80.0 (0.0)"), "{out}");
        assert!(out.contains(">10 5.0% → 5.0% (0.0pp)"), "{out}");
        assert!(!out.contains("+0.0"), "{out}");
    }

    #[test]
    fn trend_line_is_ascii_in_ci_mode() {
        let result = sample_scan();
        let mut report = build(&result);
        report.trend = Some(trend(Some(89.0), Some(87.2), (5.3, 5.8)));
        let opts = TerminalOptions {
            top: 3,
            ci: true,
            distributions: true,
        };
        let out = render(&result, &report, &opts);

        assert!(out.is_ascii(), "non-ASCII in CI output:\n{out}");
        assert!(out.contains("health 89.0 -> 87.2 (-1.8)"), "{out}");
        assert!(out.contains(">10 5.3% -> 5.8% (+0.5pp)"), "{out}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= 100,
                "line longer than 100 chars: {line:?}"
            );
        }
    }

    #[test]
    fn trend_line_is_absent_without_a_trend() {
        let result = sample_scan();
        let out = render(&result, &build(&result), &TerminalOptions::default());
        assert!(!out.contains("Trend"), "{out}");
    }

    #[test]
    fn trend_line_says_not_available_when_health_is_missing() {
        let result = sample_scan();
        let mut report = build(&result);
        report.trend = Some(trend(None, Some(87.2), (5.3, 5.8)));
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("Trend  health n/a · >10 5.3% → 5.8%"), "{out}");
    }

    #[test]
    fn hotspot_author_appears_only_when_named() {
        let result = sample_scan();
        let mut report = build(&result);
        with_git(&mut report);
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(!out.contains(" · alice"), "{out}");

        report.hotspots[0].top_author = "alice".into();
        report.hotspots[0].authors = 1;
        let one = render(&result, &report, &TerminalOptions::default());
        assert!(
            one.contains("12,340 lines changed · alice\n"),
            "single author, no count:\n{one}"
        );
        assert!(!one.contains("(+"), "{one}");

        report.hotspots[0].authors = 3;
        let many = render(&result, &report, &TerminalOptions::default());
        assert!(
            many.contains("12,340 lines changed · alice (+2)\n"),
            "{many}"
        );

        // CI spells the separator in ASCII.
        let ci = render(
            &result,
            &report,
            &TerminalOptions {
                ci: true,
                ..TerminalOptions::default()
            },
        );
        assert!(ci.contains("12,340 lines changed - alice (+2)"), "{ci}");
        assert!(ci.is_ascii(), "{ci}");
    }

    #[test]
    fn long_author_names_are_truncated_and_stay_within_100_columns() {
        let result = sample_scan();
        let mut report = build(&result);
        with_git(&mut report);
        let name = "a-very-long-contributor-name-that-keeps-going";

        // A short row has room for the whole 24-character allowance.
        report.hotspots = vec![crate::metrics::HotspotRow {
            path: "a.rs".into(),
            loc: 10,
            ccn_max: 2,
            risk: 0.1,
            commits: 1,
            lines_changed: 1,
            hotspot: 0.5,
            top_author: name.into(),
            authors: 1,
        }];
        let out = render(&result, &report, &TerminalOptions::default());
        let line = out.lines().find(|l| l.contains("0.50")).expect("row");
        let author = line.rsplit(" · ").next().unwrap();
        assert_eq!(author, "a-very-long-contributor…", "{line:?}");
        assert!(author.chars().count() <= 24, "{line:?}");

        let ci = render(
            &result,
            &report,
            &TerminalOptions {
                ci: true,
                ..TerminalOptions::default()
            },
        );
        let line = ci.lines().find(|l| l.contains("0.50")).expect("row");
        assert!(line.is_ascii(), "{line:?}");
        let author = line.rsplit(" - ").next().unwrap();
        assert_eq!(author, "a-very-long-contribut...", "{line:?}");
        assert!(author.chars().count() <= 24, "{line:?}");

        // A long path leaves less room: the row stays inside 100 columns.
        let mut long = report.clone();
        long.hotspots[0].path = "a/very/long/path/that/needs/truncation/file.rs".into();
        long.hotspots[0].authors = 3;
        let out = render(&result, &long, &TerminalOptions::default());
        let line = out.lines().find(|l| l.contains("0.50")).expect("row");
        assert!(line.contains(" (+2)"), "{line:?}");
        assert!(line.chars().count() <= 100, "{line:?}");
    }

    #[test]
    fn no_hotspots_block_without_history_or_without_a_file_list() {
        let result = sample_scan();
        let report = build(&result);
        let out = render(&result, &report, &TerminalOptions::default());
        assert!(!out.contains("hotspots"), "{out}");

        let mut with = report.clone();
        with_git(&mut with);
        let none = render(
            &result,
            &with,
            &TerminalOptions {
                top: 0,
                ..TerminalOptions::default()
            },
        );
        assert!(
            !none.contains("  hotspots\n"),
            "--top 0 hides the block:\n{none}"
        );
        assert!(!none.contains("0.87"), "{none}");
    }

    #[test]
    fn a_version_change_produces_a_comparability_warning() {
        use crate::history::{Snapshot, Trend};

        let result = sample_scan();
        let mut report = build(&result);
        let mut previous = Snapshot::from_report(&report);
        previous.tool_version = "0.2.0".to_string();
        let current = Snapshot::from_report(&report);
        report.trend = Some(Trend { previous, current });

        let out = render(&result, &report, &TerminalOptions::default());
        assert!(out.contains("!      not comparable"), "{out}");
        assert!(out.contains("ccn 0.2.0 -> "), "{out}");
        // Still under the 100-column budget.
        for line in out.lines() {
            assert!(line.chars().count() <= 100, "too wide: {line}");
        }
    }
}
