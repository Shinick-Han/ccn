//! Self-contained HTML report.
//!
//! One `<!doctype html>` document, inline CSS, inline SVG, no requests. The
//! renderer is intentionally dumb: every number comes from [`Report`], nothing
//! is recomputed here (see `docs/PLAN.md` §6 and
//! `docs/research-metrics-and-viz.md` §4).
//!
//! Layout is verdict → distribution → outliers, one column, SVG bars, with a
//! visually hidden table carrying the same numbers for screen readers.
//!
//! Two constraints shape the code:
//!
//! - Every value that originates in the scan (root, paths, language names) is
//!   escaped with [`escape_html`] before it reaches the document.
//! - The output is deterministic: no timestamps, no random ids, no iteration
//!   over hash maps.
//!
//! The theme toggle lives in inline event-handler attributes rather than a
//! `<script>` element so the document contains no script tag at all; this keeps
//! "a path must never be able to inject a script" true by construction and
//! keeps the escaping test unambiguous.

use std::fmt::Write as _;

use crate::metrics::{Histogram, Report, HEALTH_CALIBRATION};

/// Render the whole report as one self-contained HTML document.
pub fn render(report: &Report) -> String {
    let mut out = String::with_capacity(24_576);

    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        out,
        "<title>CCN report — {}</title>",
        escape_html(&report.root)
    );
    out.push_str("<style>\n");
    out.push_str(CSS);
    out.push_str("\n</style>\n</head>\n");
    let _ = writeln!(out, "<body onload=\"{THEME_INIT}\">");
    out.push_str("<div class=\"wrap\">\n");
    out.push_str("<a class=\"skip\" href=\"#main\">Skip to content</a>\n");

    // --- verdict -----------------------------------------------------------
    out.push_str("<header class=\"verdict\">\n<div class=\"verdict-top\">\n");
    match &report.health {
        Some(h) => {
            let _ = writeln!(
                out,
                "<p class=\"score\">Health <strong>{:.1}</strong> / 100</p>",
                h.score
            );
            let _ = writeln!(
                out,
                "<span class=\"grade grade-{}\">grade {}</span>",
                h.grade.to_ascii_lowercase(),
                escape_html(&h.grade.to_string())
            );
        }
        None => out.push_str("<p class=\"score none\">No parsed functions</p>\n"),
    }
    let _ = writeln!(
        out,
        "<button type=\"button\" id=\"tt\" class=\"theme-toggle\" aria-pressed=\"false\" \
         onclick=\"{THEME_TOGGLE}\">theme</button>"
    );
    out.push_str("</div>\n");

    if let Some(h) = &report.health {
        let _ = write!(
            out,
            "<p class=\"derivation\"><strong>{:.2}</strong> of lines above the McCabe limit \
             (size-weighted, log-scaled) · {} functions · {} lines",
            h.debt,
            int(h.functions as u64),
            int(h.lines as u64)
        );
        if report.health_experimental {
            out.push_str(
                "<span class=\"tag\" title=\"Grade bands are provisional and have not been \
                 calibrated against real repositories yet (M5). Treat the letter as a summary, \
                 not a release gate.\">experimental</span>",
            );
        }
        out.push_str("</p>\n");
    }

    let _ = writeln!(
        out,
        "<p class=\"meta\"><code>{}</code> · {} files · {} measured · {} LOC · bin spec v{}</p>",
        escape_html(&report.root),
        int(report.files_analyzed as u64),
        int(report.files_measured as u64),
        int(report.loc_total as u64),
        report.bin_spec_version
    );
    out.push_str("</header>\n<main id=\"main\">\n");

    // --- trend (history only) ---------------------------------------------
    // A gloss on the verdict, so it sits between the header and the charts.
    if let Some(trend) = &report.trend {
        out.push_str(&trend_section(trend));
    }

    // --- CCN per function (primary) ---------------------------------------
    out.push_str("<section class=\"panel\" aria-labelledby=\"ccn-h\">\n");
    out.push_str("<h2 id=\"ccn-h\">CCN per function</h2>\n");
    out.push_str(
        "<p class=\"sub\">Cyclomatic complexity of every measured function. \
         Bar height is a linear count of functions.</p>\n",
    );
    if report.ccn.is_empty() {
        out.push_str("<p class=\"empty\">no functions parsed</p>\n");
    } else {
        out.push_str(&ccn_chart(&report.ccn));
        out.push_str(&ccn_table(&report.ccn));
    }
    let _ = writeln!(
        out,
        "<p class=\"caption\">functions measured: {} · heuristic files excluded: {}</p>",
        int(report.stats.functions_total as u64),
        int(report.stats.files_heuristic as u64)
    );
    out.push_str("</section>\n");

    // --- LOC per file (dual encoding) -------------------------------------
    out.push_str("<section class=\"panel\" aria-labelledby=\"loc-h\">\n");
    out.push_str("<h2 id=\"loc-h\">LOC per file</h2>\n");
    if report.loc.is_empty() {
        out.push_str("<p class=\"empty\">no files</p>\n");
        out.push_str("<p class=\"caption\">no files were analyzed</p>\n");
    } else {
        out.push_str(
            "<p class=\"sub\">Two encodings per bin: how many files land there, \
             and how much of the code they contain.</p>\n",
        );
        out.push_str(
            "<ul class=\"legend\">\n\
             <li><span class=\"swatch files\" aria-hidden=\"true\"></span>% of files (outer bar)</li>\n\
             <li><span class=\"swatch lines\" aria-hidden=\"true\"></span>% of lines (inner bar)</li>\n\
             </ul>\n",
        );
        out.push_str(&loc_chart(&report.loc));
        out.push_str(&loc_table(&report.loc));
        let _ = writeln!(
            out,
            "<p class=\"caption\">top 10% of files hold {:.1}% of lines</p>",
            report.stats.loc_share_top10pct
        );
    }
    out.push_str("</section>\n");

    // --- worst files -------------------------------------------------------
    out.push_str("<section class=\"panel\" aria-labelledby=\"top-h\">\n");
    out.push_str("<h2 id=\"top-h\">Worst 20 files</h2>\n");
    out.push_str(
        "<p class=\"sub\">Ranked by risk, the same size-weighted formula the health \
         score uses.</p>\n",
    );
    if report.top.is_empty() {
        out.push_str("<p class=\"empty\">no files with measured functions</p>\n");
    } else {
        out.push_str(&top_table(report));
    }
    out.push_str("</section>\n");

    // --- hotspots (git only) ----------------------------------------------
    // The whole panel is absent without history: a table of zeroes would read
    // as "nothing is hot" when the truth is "we did not look".
    if report.git_available && !report.hotspots.is_empty() {
        out.push_str("<section class=\"panel\" aria-labelledby=\"hot-h\">\n");
        out.push_str("<h2 id=\"hot-h\">Hotspots</h2>\n");
        out.push_str(
            "<p class=\"sub\">Complex code that changed often in the last 365 days. \
             Score = percentile rank of commits × percentile rank of risk; a file \
             with no commit in the window scores 0.</p>\n",
        );
        out.push_str(&hotspot_table(report));
        let _ = writeln!(
            out,
            "<p class=\"caption\">top 10% by hotspot hold {:.1}% of measured lines</p>",
            report.hotspot_debt_share
        );
        out.push_str("</section>\n");
    }

    out.push_str("</main>\n<footer>\n");
    let _ = writeln!(
        out,
        "<p>bin spec v{} · {}.</p>",
        report.bin_spec_version,
        escape_html(HEALTH_CALIBRATION)
    );
    let _ = writeln!(
        out,
        "<p>tiers: exact {} files · heuristic {} files · no CCN {} files. \
         Heuristic file-level counts never enter the function histogram.</p>",
        int(report.stats.files_exact as u64),
        int(report.stats.files_heuristic as u64),
        int(report.stats.files_without_ccn as u64)
    );
    let excluded_generated = report.stats.files_excluded_generated;
    let excluded_vendored = report.stats.files_excluded_vendored;
    if excluded_generated + excluded_vendored > 0 {
        // Says what the numbers above are *not* made of. Printed only when
        // something was dropped, so a clean repo's footer stays clean.
        let _ = writeln!(
            out,
            "<p>excluded: {} generated, {} vendored files.</p>",
            int(excluded_generated as u64),
            int(excluded_vendored as u64)
        );
    }
    if report.git_available {
        let _ = writeln!(
            out,
            "<p>{}: {} {} in the last 365 days.</p>",
            escape_html(report.vcs.as_deref().unwrap_or("git")),
            int(u64::from(report.commits_scanned)),
            if report.vcs.as_deref() == Some("p4") {
                "changelists"
            } else {
                "commits"
            }
        );
    }
    let _ = writeln!(
        out,
        "<p>generated by ccn {}</p>",
        escape_html(env!("CARGO_PKG_VERSION"))
    );
    out.push_str("</footer>\n</div>\n</body>\n</html>\n");

    out
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Escape `& < > " '` for both element text and quoted attribute values.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Thousands-separated integer. Grouping is fixed (not locale-aware) so two
/// runs on one machine produce the same bytes.
fn int(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn pct(share: f64) -> String {
    format!("{share:.1}%")
}

/// One-decimal delta with an explicit sign; zero stays unsigned so "no change"
/// cannot be read as an improvement or a regression.
fn signed(delta: f64) -> String {
    let rounded = (delta * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "0.0".to_string()
    } else {
        format!("{rounded:+.1}")
    }
}

/// Direction of a one-decimal delta. The sign is always printed too, so the
/// colour is a second signal, never the only one.
fn delta_class(delta: f64, higher_is_better: bool) -> &'static str {
    let rounded = (delta * 10.0).round() / 10.0;
    if rounded == 0.0 {
        ""
    } else if (rounded > 0.0) == higher_is_better {
        " good"
    } else {
        " bad"
    }
}

/// "89.0 → 87.2 −1.8" style comparison with the previous snapshot.
///
/// The snapshot times are deliberately not rendered: the report has to be
/// byte-identical for identical inputs.
fn trend_section(trend: &crate::history::Trend) -> String {
    let mut s = String::from(
        "<section class=\"trend\" aria-labelledby=\"trend-h\">\n\
         <h2 id=\"trend-h\">Trend</h2>\n<p class=\"trend-row\">\n",
    );

    match (
        trend.previous.health,
        trend.current.health,
        trend.health_delta(),
    ) {
        (Some(prev), Some(cur), Some(delta)) => {
            let _ = writeln!(
                s,
                "<span class=\"trend-item\"><span class=\"trend-label\">health</span> \
                 <span class=\"trend-num\">{prev:.1}</span> \
                 <span class=\"trend-arrow\">→</span> \
                 <span class=\"trend-num\">{cur:.1}</span> \
                 <span class=\"delta{}\">{}</span></span>",
                delta_class(delta, true),
                signed(delta)
            );
        }
        _ => s.push_str(
            "<span class=\"trend-item\"><span class=\"trend-label\">health</span> n/a</span>\n",
        ),
    }

    let pct_delta = trend.pct_over_10_delta();
    let _ = writeln!(
        s,
        "<span class=\"trend-item\"><span class=\"trend-label\">%func &gt;10</span> \
         <span class=\"trend-num\">{:.1}%</span> \
         <span class=\"trend-arrow\">→</span> \
         <span class=\"trend-num\">{:.1}%</span> \
         <span class=\"delta{}\">{}pp</span></span>",
        trend.previous.pct_over_10,
        trend.current.pct_over_10,
        delta_class(pct_delta, false),
        signed(pct_delta)
    );

    if let (Some(prev), Some(cur)) = (trend.previous.grade, trend.current.grade) {
        if prev != cur {
            let _ = writeln!(
                s,
                "<span class=\"trend-item trend-grade\">grade {} \
                 <span class=\"trend-arrow\">→</span> {}</span>",
                escape_html(&prev.to_string()),
                escape_html(&cur.to_string())
            );
        }
    }

    s.push_str("</p>\n");

    if let Some(warning) = trend.compatibility_warning() {
        let _ = writeln!(
            s,
            "<p class=\"trend-warn\" role=\"status\">! {}</p>",
            escape_html(&warning)
        );
    }

    s.push_str("</section>\n");
    s
}

/// Minimum-visible-width for a bar; zero stays zero, never divide by zero.
fn bar_width(share: f64, denom: f64, available: f64, present: bool) -> f64 {
    if !present {
        return 0.0;
    }
    let width = (share / denom) * available;
    if width < 2.0 {
        2.0
    } else {
        width
    }
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

/// Vertical columns: x is the fixed CCN bins, y is a linear count of functions.
fn ccn_chart(hist: &Histogram) -> String {
    const W: f64 = 560.0;
    const H: f64 = 300.0;
    const LEFT: f64 = 48.0;
    const RIGHT: f64 = 16.0;
    const TOP: f64 = 44.0;
    const BOTTOM: f64 = 52.0;

    let max_count = hist.bins.iter().map(|b| b.count).max().unwrap_or(0);
    let denom = max_count.max(1) as f64;
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let baseline = TOP + plot_h;
    let n = hist.bins.len().max(1) as f64;
    let col_w = plot_w / n;
    let bar_w = (col_w - 20.0).max(6.0);

    let mut s = String::with_capacity(4096);
    let aria = format!(
        "CCN per function histogram: {} functions across {} fixed bins, linear y axis.",
        int(hist.total as u64),
        hist.bins.len()
    );
    let _ = writeln!(
        s,
        "<svg class=\"chart-svg\" viewBox=\"0 0 {W:.0} {H:.0}\" role=\"img\" aria-label=\"{}\">",
        escape_html(&aria)
    );
    let _ = writeln!(
        s,
        "<text class=\"tick\" x=\"{LEFT:.1}\" y=\"16\">functions (linear scale)</text>"
    );

    // Linear gridlines; at most three, labeled with counts.
    let levels: Vec<usize> = if max_count == 0 {
        vec![0]
    } else if max_count <= 2 {
        vec![0, max_count]
    } else {
        vec![0, max_count / 2, max_count]
    };
    for level in levels {
        let y = baseline - (level as f64 / denom) * plot_h;
        let _ = writeln!(
            s,
            "<line class=\"grid\" x1=\"{LEFT:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\"/>",
            W - RIGHT
        );
        let _ = writeln!(
            s,
            "<text class=\"tick\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            LEFT - 6.0,
            y + 3.5,
            int(level as u64)
        );
    }

    for (i, bin) in hist.bins.iter().enumerate() {
        let cx = LEFT + col_w * (i as f64 + 0.5);
        let bh = if bin.count == 0 {
            0.0
        } else {
            ((bin.count as f64 / denom) * plot_h).max(2.0)
        };
        let y = baseline - bh;
        let x = cx - bar_w / 2.0;
        let share = pct(bin.share_count);
        let tip = format!(
            "{}: {} functions · {} of measured",
            bin.label,
            int(bin.count as u64),
            share
        );
        let _ = writeln!(
            s,
            "<g class=\"mark\" tabindex=\"0\"><title>{}</title>",
            escape_html(&tip)
        );
        if bin.count > 0 {
            let _ = writeln!(
                s,
                "<rect class=\"bar s{}\" x=\"{x:.1}\" y=\"{y:.1}\" width=\"{bar_w:.1}\" \
                 height=\"{bh:.1}\" rx=\"2\"/>",
                i.min(5)
            );
        }
        let value_y = if bin.count == 0 {
            baseline - 8.0
        } else {
            y - 8.0
        };
        let _ = writeln!(
            s,
            "<text class=\"val\" x=\"{cx:.1}\" y=\"{value_y:.1}\" text-anchor=\"middle\">\
             {} · {}</text>",
            int(bin.count as u64),
            share
        );
        if bin.hi.is_none() {
            let _ = writeln!(
                s,
                "<text class=\"tick\" x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\">max {}</text>",
                value_y - 14.0,
                int(u64::from(bin.max))
            );
        }
        let _ = writeln!(
            s,
            "<text class=\"axis\" x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text></g>",
            baseline + 19.0,
            escape_html(&bin.label)
        );
    }

    // The line sits on the category boundary between 5–10 and 11–20.
    if hist.bins.len() >= 4 {
        let bx = LEFT + col_w * 3.0;
        let _ = writeln!(
            s,
            "<line class=\"limit\" x1=\"{bx:.1}\" y1=\"{:.1}\" x2=\"{bx:.1}\" y2=\"{baseline:.1}\"/>",
            TOP - 16.0
        );
        let _ = writeln!(
            s,
            "<text class=\"limit-label\" x=\"{:.1}\" y=\"{:.1}\">McCabe limit (10)</text>",
            bx + 6.0,
            TOP - 18.0
        );
    }

    s.push_str("</svg>\n");
    s
}

/// Horizontal rows: label, then a thick outer bar for `% of files` and a
/// lighter inner bar for `% of lines`, then the raw numbers as text.
fn loc_chart(hist: &Histogram) -> String {
    const W: f64 = 660.0;
    const ROW: f64 = 26.0;
    const TOP: f64 = 28.0;
    const LABEL_X: f64 = 88.0;
    const X0: f64 = 100.0;
    const X1: f64 = 470.0;
    const VALUE_X: f64 = 478.0;

    let n = hist.bins.len();
    let height = TOP + ROW * n as f64 + 26.0;
    let plot_w = X1 - X0;
    let denom = hist
        .bins
        .iter()
        .map(|b| b.share_count.max(b.share_weight))
        .fold(0.0_f64, f64::max)
        .max(0.000_001);

    let mut s = String::with_capacity(8192);
    let aria = format!(
        "LOC per file histogram: {} files, {} lines, {} bins. Each row encodes \
         share of files and share of lines.",
        int(hist.total as u64),
        int(hist.total_weight),
        n
    );
    let _ = writeln!(
        s,
        "<svg class=\"chart-svg\" viewBox=\"0 0 {W:.0} {height:.0}\" role=\"img\" aria-label=\"{}\">",
        escape_html(&aria)
    );

    for frac in [0.0_f64, 0.5, 1.0] {
        let x = X0 + frac * plot_w;
        let _ = writeln!(
            s,
            "<line class=\"grid\" x1=\"{x:.1}\" y1=\"{:.1}\" x2=\"{x:.1}\" y2=\"{:.1}\"/>",
            TOP - 6.0,
            TOP + ROW * n as f64
        );
        let _ = writeln!(
            s,
            "<text class=\"tick\" x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{:.1}%</text>",
            TOP - 10.0,
            frac * denom
        );
    }

    for (i, bin) in hist.bins.iter().enumerate() {
        let cy = TOP + ROW * (i as f64 + 0.5);
        let files_w = bar_width(bin.share_count, denom, plot_w, bin.count > 0);
        let lines_w = bar_width(bin.share_weight, denom, plot_w, bin.weight > 0);
        let tip = format!(
            "{} lines: {} files, {} of files, {} of lines",
            bin.label,
            int(bin.count as u64),
            pct(bin.share_count),
            pct(bin.share_weight)
        );
        let _ = writeln!(
            s,
            "<g class=\"mark\" tabindex=\"0\"><title>{}</title>",
            escape_html(&tip)
        );
        if bin.count > 0 {
            let _ = writeln!(
                s,
                "<rect class=\"files\" x=\"{X0:.1}\" y=\"{:.1}\" width=\"{files_w:.1}\" \
                 height=\"14\" rx=\"2\"/>",
                cy - 7.0
            );
        }
        if bin.weight > 0 {
            let _ = writeln!(
                s,
                "<rect class=\"lines\" x=\"{X0:.1}\" y=\"{:.1}\" width=\"{lines_w:.1}\" \
                 height=\"7\" rx=\"2\"/>",
                cy - 3.5
            );
        }

        let mut values = format!(
            "{} · {} · {}",
            int(bin.count as u64),
            pct(bin.share_count),
            pct(bin.share_weight)
        );
        if bin.hi.is_none() {
            let _ = write!(values, " · max {}", int(u64::from(bin.max)));
        }
        let _ = writeln!(
            s,
            "<text class=\"axis\" x=\"{LABEL_X:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            cy + 3.5,
            escape_html(&bin.label)
        );
        let _ = writeln!(
            s,
            "<text class=\"val\" x=\"{VALUE_X:.1}\" y=\"{:.1}\">{}</text></g>",
            cy + 3.5,
            escape_html(&values)
        );
    }

    s.push_str("</svg>\n");
    s
}

// ---------------------------------------------------------------------------
// Accessible equivalents (same numbers, no color, no SVG)
// ---------------------------------------------------------------------------

fn ccn_table(hist: &Histogram) -> String {
    let mut s = String::from(
        "<div class=\"sr-only\"><table><caption>CCN per function, exact numbers</caption>\
         <thead><tr><th scope=\"col\">CCN</th><th scope=\"col\">Functions</th>\
         <th scope=\"col\">Share</th></tr></thead><tbody>",
    );
    for bin in &hist.bins {
        let _ = writeln!(
            s,
            "<tr><th scope=\"row\">{}</th><td>{}</td><td>{}</td></tr>",
            escape_html(&bin.label),
            int(bin.count as u64),
            pct(bin.share_count)
        );
    }
    s.push_str("</tbody></table></div>\n");
    s
}

fn loc_table(hist: &Histogram) -> String {
    let mut s = String::from(
        "<div class=\"sr-only\"><table><caption>LOC per file, exact numbers</caption>\
         <thead><tr><th scope=\"col\">LOC</th><th scope=\"col\">Files</th>\
         <th scope=\"col\">% of files</th><th scope=\"col\">Lines</th>\
         <th scope=\"col\">% of lines</th><th scope=\"col\">Max</th></tr></thead><tbody>",
    );
    for bin in &hist.bins {
        let _ = writeln!(
            s,
            "<tr><th scope=\"row\">{}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
             <td>{}</td></tr>",
            escape_html(&bin.label),
            int(bin.count as u64),
            pct(bin.share_count),
            int(bin.weight),
            pct(bin.share_weight),
            int(u64::from(bin.max))
        );
    }
    s.push_str("</tbody></table></div>\n");
    s
}

fn top_table(report: &Report) -> String {
    let mut s = String::from("<div class=\"table-wrap\"><table>\n");
    s.push_str("<caption class=\"sr-only\">Worst 20 files by risk</caption>\n");
    s.push_str(
        "<thead><tr><th scope=\"col\">path</th><th scope=\"col\">lang</th>\
         <th scope=\"col\" class=\"num\">LOC</th><th scope=\"col\" class=\"num\">max CCN</th>\
         <th scope=\"col\" class=\"num\">&gt;10</th>\
         <th scope=\"col\" class=\"num\">risk</th></tr></thead>\n<tbody>\n",
    );
    for row in &report.top {
        let _ = writeln!(
            s,
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"num\">{}</td>\
             <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{:.2}</td></tr>",
            escape_html(&row.path),
            escape_html(&row.lang),
            int(u64::from(row.loc)),
            int(u64::from(row.ccn_max)),
            int(row.over_10 as u64),
            row.risk
        );
    }
    s.push_str("</tbody>\n</table></div>\n");
    s
}

/// Hotspot ranking. Every field comes from [`Report::hotspots`]; the renderer
/// only formats and escapes.
fn hotspot_table(report: &Report) -> String {
    // Ownership is a git-history detail: without a named author on any row the
    // column would be a stack of empty cells, so it is omitted entirely.
    let with_owner = report.hotspots.iter().any(|r| !r.top_author.is_empty());

    let mut s = String::from("<div class=\"table-wrap\"><table>\n");
    s.push_str("<caption class=\"sr-only\">Hotspots: change frequency times risk</caption>\n");
    s.push_str("<thead><tr><th scope=\"col\">path</th>");
    if with_owner {
        s.push_str("<th scope=\"col\">owner</th>");
    }
    s.push_str(
        "<th scope=\"col\" class=\"num\">commits</th>\
         <th scope=\"col\" class=\"num\">lines changed</th><th scope=\"col\" class=\"num\">max CCN</th>\
         <th scope=\"col\" class=\"num\">risk</th><th scope=\"col\" class=\"num\">hotspot</th>\
         </tr></thead>\n<tbody>\n",
    );
    for row in &report.hotspots {
        let _ = write!(s, "<tr><td><code>{}</code></td>", escape_html(&row.path));
        if with_owner {
            let owner = if row.top_author.is_empty() {
                String::new()
            } else {
                let mut owner = escape_html(&row.top_author);
                if row.authors > 1 {
                    let _ = write!(owner, " +{}", row.authors - 1);
                }
                owner
            };
            let _ = write!(s, "<td>{owner}</td>");
        }
        let _ = writeln!(
            s,
            "<td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td class=\"num\">{}</td><td class=\"num\">{:.2}</td><td class=\"num\">{:.2}</td></tr>",
            int(u64::from(row.commits)),
            int(u64::from(row.lines_changed)),
            int(u64::from(row.ccn_max)),
            row.risk,
            row.hotspot
        );
    }
    s.push_str("</tbody>\n</table></div>\n");
    s
}

// ---------------------------------------------------------------------------
// Theme toggle (inline handlers, no <script> element)
// ---------------------------------------------------------------------------

const THEME_INIT: &str = "try{var r=document.documentElement,t=null;\
try{t=localStorage.getItem('ccn-theme')}catch(e){}\
if(t==='dark'||t==='light'){r.setAttribute('data-theme',t)}\
var b=document.getElementById('tt');if(b){var d=r.getAttribute('data-theme');\
if(!d){d=window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'}\
b.textContent='theme: '+d;b.setAttribute('aria-pressed',d==='dark'?'true':'false')}}catch(e){}";

const THEME_TOGGLE: &str = "try{var r=document.documentElement,d=r.getAttribute('data-theme');\
if(!d){d=window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'}\
var n=d==='dark'?'light':'dark';r.setAttribute('data-theme',n);\
try{localStorage.setItem('ccn-theme',n)}catch(e){}\
this.textContent='theme: '+n;this.setAttribute('aria-pressed',n==='dark'?'true':'false')}catch(e){}";

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

const CSS: &str = r##":root{
  color-scheme: light;
  /* Paper-light take on AuraDeck's cyber-pastel: lilac paper, mauve/sky rules. */
  --bg:#f8f6fc; --panel:#ffffff; --fg:#22213a; --muted:#5f5e78; --rule:#e4e0ef;
  --accent:#6b4fb8; --focus:#4b3596; --chip:#f1ebfb; --chip-fg:#46307f; --grid:#ece8f6;
  --good:#2f7d5b; --bad:#b23a48;
  --mauve:#8b6fd6; --sky:#3a9db3;
  --glow1:rgba(139,111,214,.11); --glow2:rgba(58,157,179,.09);
  --s0:#6c5ce0; --s1:#7b6bea; --s2:#5e86d8; --s3:#2f8fa6; --s4:#9a6b00; --s5:#b3263b;
  --bar-files:#6c5ce0; --bar-lines:#3a9db3;
  --grade-a:#2f7d5b; --grade-b:#35599e; --grade-c:#8a6100; --grade-d:#a24c22; --grade-e:#a8263c;
}
@media (prefers-color-scheme: dark){
  :root:not([data-theme="light"]){
    color-scheme: dark;
    /* AuraDeck "Aura Cyber-Pastel": canvas 12121C, card 1A1B2A, mauve -> sky. */
    --bg:#12121c; --panel:#1a1b2a; --fg:#cdd4f4; --muted:#8b92b8; --rule:#2e3150;
    --accent:#cba6f7; --focus:#89dceb; --chip:#2d2f46; --chip-fg:#e9e2ff; --grid:#2a2d48;
    --good:#a6e3a1; --bad:#f38ba8;
    --mauve:#cba6f7; --sky:#89dceb;
    --glow1:rgba(203,166,247,.10); --glow2:rgba(137,220,235,.08);
    --s0:#7c6cf2; --s1:#8b7bf5; --s2:#7fa6f7; --s3:#89dceb; --s4:#f9e2af; --s5:#f38ba8;
    --bar-files:#8b7bf5; --bar-lines:#89dceb;
    --grade-a:#a6e3a1; --grade-b:#89b4fa; --grade-c:#f9e2af; --grade-d:#fab387; --grade-e:#f38ba8;
  }
}
:root[data-theme="dark"]{
  color-scheme: dark;
  --bg:#12121c; --panel:#1a1b2a; --fg:#cdd4f4; --muted:#8b92b8; --rule:#2e3150;
  --accent:#cba6f7; --focus:#89dceb; --chip:#2d2f46; --chip-fg:#e9e2ff; --grid:#2a2d48;
  --good:#a6e3a1; --bad:#f38ba8;
  --mauve:#cba6f7; --sky:#89dceb;
  --glow1:rgba(203,166,247,.10); --glow2:rgba(137,220,235,.08);
  --s0:#7c6cf2; --s1:#8b7bf5; --s2:#7fa6f7; --s3:#89dceb; --s4:#f9e2af; --s5:#f38ba8;
  --bar-files:#8b7bf5; --bar-lines:#89dceb;
  --grade-a:#a6e3a1; --grade-b:#89b4fa; --grade-c:#f9e2af; --grade-d:#fab387; --grade-e:#f38ba8;
}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{
  margin:0; color:var(--fg);
  background:
    radial-gradient(900px 480px at 8% -12%, var(--glow1), transparent 62%),
    radial-gradient(820px 460px at 100% 0%, var(--glow2), transparent 58%),
    var(--bg);
  font:15px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  transition:background-color .12s ease,color .12s ease;
}
.wrap{max-width:960px;margin:0 auto;padding:24px 16px 40px}
.skip{position:absolute;left:-999px;top:0;background:var(--panel);color:var(--fg);padding:.5rem .75rem;z-index:10}
.skip:focus{left:8px;top:8px}
.verdict{background:var(--panel);border:1px solid var(--rule);border-radius:12px;padding:18px 20px}
.verdict-top{display:flex;flex-wrap:wrap;align-items:center;gap:12px}
.score{margin:0;color:var(--muted);font-size:1rem}
.score strong{font-size:2.6rem;line-height:1;color:var(--fg);font-variant-numeric:tabular-nums}
.score.none{font-size:1.5rem;font-weight:600;color:var(--fg)}
.grade{font-weight:600;font-size:.9rem;padding:.25rem .55rem;border-radius:999px;background:var(--chip);color:var(--chip-fg);border:1px solid var(--rule)}
.grade-a{color:var(--grade-a);border-color:var(--grade-a)}
.grade-b{color:var(--grade-b);border-color:var(--grade-b)}
.grade-c{color:var(--grade-c);border-color:var(--grade-c)}
.grade-d{color:var(--grade-d);border-color:var(--grade-d)}
.grade-e{color:var(--grade-e);border-color:var(--grade-e)}
/* Aura-style hairline: a frozen slice of the deck's border wave. */
.verdict,.panel,.trend{position:relative;overflow:hidden}
.verdict::before,.panel::before,.trend::before{
  content:"";position:absolute;left:0;right:0;top:0;height:2px;
  background:linear-gradient(90deg,var(--mauve),var(--sky));opacity:.85;
}
.theme-toggle{margin-left:auto;font:inherit;font-size:.8rem;padding:.35rem .6rem;border-radius:8px;border:1px solid var(--rule);background:transparent;color:var(--muted);cursor:pointer}
.theme-toggle:hover{color:var(--fg)}
.theme-toggle:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
.derivation{margin:.6rem 0 0;color:var(--muted);font-size:.9rem}
.derivation strong{color:var(--fg);font-variant-numeric:tabular-nums}
.tag{margin-left:.5rem;display:inline-block;font-size:.7rem;text-transform:uppercase;letter-spacing:.06em;padding:.1rem .4rem;border:1px dashed var(--rule);border-radius:999px;color:var(--muted);cursor:help}
.meta{margin:.5rem 0 0;font-size:.8rem;color:var(--muted)}
.meta code,td code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.trend{background:var(--panel);border:1px solid var(--rule);border-radius:12px;padding:12px 20px;margin-top:16px}
.trend h2{margin:0;font-size:.72rem;text-transform:uppercase;letter-spacing:.06em;color:var(--muted)}
.trend-row{display:flex;flex-wrap:wrap;align-items:baseline;gap:.35rem 1.6rem;margin:.35rem 0 0;font-size:.9rem}
.trend-item{display:inline-flex;flex-wrap:wrap;align-items:baseline;gap:.35rem;font-variant-numeric:tabular-nums}
.trend-warn{margin:.35rem 0 0;font-size:.82rem;color:var(--bad)}
.trend-label{color:var(--muted)}
.trend-num{font-weight:600}
.trend-arrow{color:var(--muted)}
.trend-grade{color:var(--muted)}
.delta{font-weight:600}
.delta.good{color:var(--good)}
.delta.bad{color:var(--bad)}
.panel{background:var(--panel);border:1px solid var(--rule);border-radius:12px;padding:18px 20px;margin-top:16px}
.panel h2{margin:0 0 .35rem;font-size:.78rem;font-weight:600;text-transform:uppercase;letter-spacing:.09em;color:var(--muted);display:flex;align-items:center;gap:.5rem}
.panel h2::before{content:"";flex:none;width:14px;height:2px;border-radius:2px;background:linear-gradient(90deg,var(--mauve),var(--sky))}
.sub{margin:0 0 .75rem;color:var(--muted);font-size:.85rem}
.caption{margin:.75rem 0 0;color:var(--muted);font-size:.8rem}
.empty{margin:.5rem 0 0;padding:1.5rem;text-align:center;color:var(--muted);border:1px dashed var(--rule);border-radius:8px;font-size:.9rem}
.chart-svg{display:block;width:100%;height:auto;max-width:760px}
.chart-svg text{font-family:inherit}
.tick{fill:var(--muted);font-size:10px;font-variant-numeric:tabular-nums}
.axis{fill:var(--fg);font-size:11px}
.val{fill:var(--fg);font-size:10.5px;font-variant-numeric:tabular-nums}
.grid{stroke:var(--grid);stroke-width:1}
.limit{stroke:var(--accent);stroke-width:1.5;stroke-dasharray:4 3;fill:none}
.limit-label{fill:var(--accent);font-size:11px;font-weight:600}
g.mark{cursor:default;outline:none}
g.mark .bar{stroke:var(--panel);stroke-width:1}
g.mark:focus .bar,g.mark:focus-visible .bar{stroke:var(--focus);stroke-width:2.5}
g.mark:focus .files,g.mark:focus-visible .files,
g.mark:focus .lines,g.mark:focus-visible .lines{stroke:var(--focus);stroke-width:1.5}
g.mark:focus{outline:none}
.s0{fill:var(--s0)}.s1{fill:var(--s1)}.s2{fill:var(--s2)}
.s3{fill:var(--s3)}.s4{fill:var(--s4)}.s5{fill:var(--s5)}
.files{fill:var(--bar-files)}
.lines{fill:var(--bar-lines)}
.legend{display:flex;flex-wrap:wrap;gap:.9rem;margin:.25rem 0 .5rem;padding:0;list-style:none;font-size:.8rem;color:var(--muted)}
.legend li{display:flex;align-items:center;gap:.4rem}
.swatch{position:relative;flex:none;width:22px;height:12px;border-radius:2px;background:var(--bar-files)}
.swatch.lines::after{content:"";position:absolute;left:0;right:0;top:3px;height:6px;background:var(--bar-lines);border-radius:2px}
.table-wrap{overflow-x:auto;border:1px solid var(--rule);border-radius:8px}
table{border-collapse:collapse;width:100%;font-size:.85rem}
th,td{padding:.45rem .6rem;border-bottom:1px solid var(--rule);text-align:left;white-space:nowrap}
th{font-weight:600;color:var(--muted);font-size:.72rem;text-transform:uppercase;letter-spacing:.04em;background:var(--bg)}
tr:last-child td{border-bottom:0}
.num{text-align:right;font-variant-numeric:tabular-nums}
td code{font-size:.82rem;white-space:normal;overflow-wrap:anywhere}
.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap;border:0}
footer{margin-top:18px;font-size:.78rem;color:var(--muted);line-height:1.6}
footer p{margin:.2rem 0}
footer code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
@media (max-width:560px){
  .wrap{padding:16px 12px 32px}
  .verdict{padding:14px}
  .panel{padding:14px}
  .score strong{font-size:2.1rem}
  .axis{font-size:13px}
  .tick{font-size:12px}
  .limit-label{font-size:13px}
}
@media (prefers-reduced-motion: reduce){
  *{transition:none !important;animation:none !important}
}
"##;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{
        histogram, Health, HotspotRow, RankedFile, RepoStats, CCN_BINS, LOC_BINS,
    };

    fn stats() -> RepoStats {
        RepoStats {
            loc_median: 84,
            loc_p90: 312,
            loc_p99: 1_940,
            loc_max: 12_408,
            ccn_median: 2,
            ccn_p90: 7,
            ccn_p99: 19,
            ccn_max: 74,
            functions_total: 12,
            loc_share_top10pct: 61.0,
            files_exact: 11,
            files_heuristic: 2,
            files_without_ccn: 1,
            files_excluded_generated: 0,
            files_excluded_vendored: 0,
        }
    }

    fn row(path: &str, risk: f64) -> RankedFile {
        RankedFile {
            path: path.to_string(),
            lang: "TypeScript".to_string(),
            loc: 1_930,
            ccn_max: 74,
            ccn_sum: 210,
            over_10: 3,
            functions: 40,
            risk,
        }
    }

    fn sample() -> Report {
        let loc_values: [u32; 13] = [
            0, 12, 60, 120, 210, 320, 520, 760, 1_200, 2_400, 6_000, 12_000, 40_000,
        ];
        let loc = histogram(&loc_values, Some(&loc_values), &LOC_BINS);
        let ccn_values: [u32; 12] = [1, 1, 1, 2, 2, 3, 5, 8, 11, 14, 22, 51];
        let ccn = histogram(&ccn_values, None, &CCN_BINS);
        Report {
            root: "C:/repo".to_string(),
            files_analyzed: loc_values.len(),
            files_measured: loc_values.len(),
            loc_total: 50_000,
            loc,
            ccn: ccn.clone(),
            ccn_file: ccn,
            health: Some(Health {
                score: 72.0,
                grade: 'C',
                debt: 0.18,
                functions: 12,
                lines: 50_000,
                over_10: 3,
                over_20: 2,
                over_50: 1,
                pct_over_10: 25.0,
            }),
            stats: stats(),
            top: vec![row("src/parser/grammar.ts", 0.62)],
            git_available: false,
            vcs: None,
            commits_scanned: 0,
            hotspot_debt_share: 0.0,
            hotspots: Vec::new(),
            churn: std::collections::HashMap::new(),
            trend: None,
            bin_spec_version: 1,
            health_experimental: true,
        }
    }

    fn hot(path: &str, commits: u32, changed: u32, loc: u32, hotspot: f64) -> HotspotRow {
        HotspotRow {
            path: path.to_string(),
            loc,
            ccn_max: 74,
            risk: 0.62,
            commits,
            lines_changed: changed,
            hotspot,
            top_author: String::new(),
            authors: 0,
        }
    }

    #[test]
    fn renders_a_complete_document() {
        let html = render(&sample());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn makes_no_external_references() {
        let html = render(&sample());
        for needle in ["http://", "https://", "<script src", "@import", "<script"] {
            assert!(!html.contains(needle), "output contains {needle}");
        }
    }

    #[test]
    fn escapes_paths_from_the_scan() {
        let mut r = sample();
        r.root = "C:/repo/<script>alert(1)</script>".to_string();
        r.top = vec![row("src/<b>&\"drop.sql", 0.62)];
        let html = render(&r);
        assert!(!html.contains("<script"));
        assert!(!html.contains("</script>"));
        assert!(html.contains("C:/repo/&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("src/&lt;b&gt;&amp;&quot;drop.sql"));
    }

    #[test]
    fn prints_the_computed_numbers() {
        let html = render(&sample());
        assert!(html.contains("Health <strong>72.0</strong> / 100"));
        assert!(html.contains("grade C"));
        assert!(html.contains("0.18"));
        assert!(html.contains("11–20"));
        assert!(html.contains("≥10,000"));
        assert!(html.contains("McCabe limit (10)"));
        assert!(html.contains("0.62"));
        assert!(html.contains("top 10% of files hold 61.0% of lines"));
    }

    #[test]
    fn missing_health_does_not_invent_a_perfect_score() {
        let mut r = sample();
        r.health = None;
        r.ccn = histogram(&[], None, &CCN_BINS);
        r.ccn_file = r.ccn.clone();
        let html = render(&r);
        assert!(html.contains("No parsed functions"));
        assert!(html.contains("no functions parsed"));
        assert!(!html.contains("100 / 100"));
    }

    #[test]
    fn empty_loc_gets_an_empty_state() {
        let mut r = sample();
        r.loc = histogram(&[], Some(&[]), &LOC_BINS);
        let html = render(&r);
        assert!(html.contains("no files"));
        assert!(!html.contains("NaN"));
    }

    #[test]
    fn is_deterministic() {
        let r = sample();
        assert_eq!(render(&r), render(&r));
    }

    #[test]
    fn exclusions_appear_in_the_footer_and_measured_files_in_the_header() {
        let mut r = sample();
        r.files_analyzed = 1_595;
        r.files_measured = 1_243;
        r.stats.files_excluded_generated = 12;
        r.stats.files_excluded_vendored = 340;

        let html = render(&r);
        assert!(
            html.contains("1,595 files · 1,243 measured"),
            "header must separate found from measured"
        );
        assert!(
            html.contains("excluded: 12 generated, 340 vendored files"),
            "footer must name the exclusion"
        );
        assert!(html.len() < 120_000, "rendered {} bytes", html.len());
    }

    #[test]
    fn a_clean_repository_prints_no_exclusion_line() {
        let html = render(&sample());
        // "heuristic files excluded: 2" is a different sentence in the CCN
        // caption; the footer line is the one with the counts.
        assert!(!html.contains(" vendored files"), "no exclusions, no line");
        assert!(!html.contains("excluded: 0 generated"));
    }

    #[test]
    fn a_full_table_stays_under_the_size_budget() {
        let mut r = sample();
        r.top = (0..20)
            .map(|i| {
                row(
                    &format!("packages/app-{i}/src/very/long/path/component-with-a-name-{i}.tsx"),
                    0.02 * f64::from(i),
                )
            })
            .collect();
        let html = render(&r);
        assert!(html.len() < 120_000, "rendered {} bytes", html.len());
    }

    fn snap(
        health: Option<f64>,
        grade: Option<char>,
        pct_over_10: f64,
    ) -> crate::history::Snapshot {
        crate::history::Snapshot {
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
        }
    }

    fn snapshot_trend() -> crate::history::Trend {
        crate::history::Trend {
            previous: snap(Some(89.0), Some('C'), 5.3),
            current: snap(Some(87.2), Some('D'), 5.8),
        }
    }

    #[test]
    fn trend_panel_appears_only_with_a_trend() {
        let plain = render(&sample());
        assert!(!plain.contains("class=\"trend\""), "{plain}");
        assert!(!plain.contains("Trend"), "{plain}");

        let mut r = sample();
        r.trend = Some(snapshot_trend());
        let html = render(&r);

        assert!(html.contains("<section class=\"trend\""), "{html}");
        // Both comparisons, and the deltas keep their signs.
        assert!(html.contains("health"), "{html}");
        assert!(html.contains("89.0"), "{html}");
        assert!(html.contains("87.2"), "{html}");
        assert!(html.contains("class=\"delta bad\">-1.8</span>"), "{html}");
        assert!(html.contains("%func &gt;10"), "{html}");
        assert!(html.contains("5.3%"), "{html}");
        assert!(html.contains("5.8%"), "{html}");
        assert!(html.contains("class=\"delta bad\">+0.5pp</span>"), "{html}");
        // Grade moved, so the letters are shown.
        assert!(html.contains("class=\"trend-item trend-grade\""), "{html}");
        assert!(html.contains("grade C"), "{html}");
        assert!(html.contains(" D</span>"), "{html}");

        // Between the verdict header and the first chart panel.
        let trend_at = html.find("class=\"trend\"").unwrap();
        let ccn_at = html.find("id=\"ccn-h\"").unwrap();
        assert!(trend_at < ccn_at, "trend must precede the CCN chart");
    }

    #[test]
    fn trend_colours_follow_the_direction_of_travel() {
        let mut r = sample();
        r.trend = Some(crate::history::Trend {
            previous: snap(Some(80.0), Some('D'), 6.0),
            current: snap(Some(82.5), Some('D'), 5.2),
        });
        let html = render(&r);

        assert!(html.contains("class=\"delta good\">+2.5</span>"), "{html}");
        // Fewer functions above 10 is an improvement, so a minus sign is good.
        assert!(
            html.contains("class=\"delta good\">-0.8pp</span>"),
            "{html}"
        );
        // Unchanged grade is not printed (the CSS class may still exist).
        assert!(!html.contains("trend-item trend-grade"), "{html}");

        // No change is unsigned and carries no good/bad tone.
        let mut flat = sample();
        flat.trend = Some(crate::history::Trend {
            previous: snap(Some(80.0), Some('C'), 5.0),
            current: snap(Some(80.0), Some('C'), 5.0),
        });
        let html = render(&flat);
        assert!(html.contains("class=\"delta\">0.0</span>"), "{html}");
        assert!(html.contains("class=\"delta\">0.0pp</span>"), "{html}");
        assert!(!html.contains("delta bad"), "{html}");
        assert!(!html.contains("delta good"), "{html}");
    }

    #[test]
    fn trend_without_health_says_not_available() {
        let mut r = sample();
        r.trend = Some(crate::history::Trend {
            previous: snap(None, None, 5.3),
            current: snap(Some(87.2), Some('C'), 5.8),
        });
        let html = render(&r);
        assert!(html.contains("trend-label\">health</span> n/a"), "{html}");
    }

    #[test]
    fn owner_column_appears_only_when_an_author_is_known() {
        let mut r = sample();
        r.git_available = true;
        r.hotspots = vec![hot("src/app.rs", 48, 12_340, 900, 0.87)];
        let plain = render(&r);
        assert!(!plain.contains("<th scope=\"col\">owner</th>"), "{plain}");

        let mut with = r.clone();
        with.hotspots[0].top_author = "alice".into();
        with.hotspots[0].authors = 1;
        let one = render(&with);
        assert!(one.contains("<th scope=\"col\">owner</th>"), "{one}");
        assert!(one.contains("<td>alice</td>"), "{one}");

        with.hotspots[0].authors = 3;
        let many = render(&with);
        assert!(many.contains("<td>alice +2</td>"), "{many}");
    }

    #[test]
    fn owner_names_are_escaped() {
        let mut r = sample();
        r.git_available = true;
        r.hotspots = vec![hot("src/app.rs", 3, 10, 20, 0.4)];
        r.hotspots[0].top_author = "a<b&\"c\"".into();
        r.hotspots[0].authors = 2;
        let html = render(&r);

        assert!(
            !html.contains("<b>&"),
            "raw markup leaked into the owner cell"
        );
        assert!(
            html.contains("<td>a&lt;b&amp;&quot;c&quot; +1</td>"),
            "{html}"
        );
    }

    #[test]
    fn a_trend_panel_stays_deterministic_and_small() {
        let mut r = sample();
        r.trend = Some(snapshot_trend());
        let a = render(&r);
        let b = render(&r);
        assert_eq!(a, b);
        assert!(a.len() < 120_000, "rendered {} bytes", a.len());
        // The snapshot's timestamp never reaches the document.
        assert!(!a.contains("at_unix"));
    }

    #[test]
    fn hotspots_render_only_with_history() {
        let plain = render(&sample());
        assert!(!plain.contains("id=\"hot-h\""), "no panel without git");
        assert!(
            !plain.contains("commits in the last 365 days"),
            "no footer note without git"
        );

        let mut r = sample();
        r.git_available = true;
        r.commits_scanned = 1_234;
        r.hotspot_debt_share = 41.2;
        r.hotspots = vec![hot("src/app.rs", 48, 12_340, 900, 0.87)];
        let html = render(&r);

        assert!(html.contains("<h2 id=\"hot-h\">Hotspots</h2>"), "{html}");
        assert!(html.contains("src/app.rs"));
        assert!(html.contains("12,340"), "lines changed, grouped");
        assert!(html.contains("0.87"), "hotspot score");
        assert!(html.contains("top 10% by hotspot hold 41.2% of measured lines"));
        assert!(html.contains("git: 1,234 commits in the last 365 days"));
    }

    #[test]
    fn hotspot_paths_are_escaped() {
        let mut r = sample();
        r.git_available = true;
        r.hotspots = vec![hot("src/<b>&\"drop.sql", 3, 10, 20, 0.4)];
        let html = render(&r);

        assert!(!html.contains("<b>&"), "raw markup leaked into the table");
        assert!(html.contains("src/&lt;b&gt;&amp;&quot;drop.sql"));
    }

    #[test]
    fn a_full_hotspot_table_stays_under_the_size_budget() {
        let mut r = sample();
        r.git_available = true;
        r.hotspots = (0..20)
            .map(|i| {
                hot(
                    &format!("packages/app-{i}/src/very/long/path/component-with-a-name-{i}.tsx"),
                    40 + i,
                    12_340,
                    900,
                    0.9,
                )
            })
            .collect();
        let html = render(&r);
        assert!(html.len() < 120_000, "rendered {} bytes", html.len());
    }

    #[test]
    fn a_version_change_renders_a_comparability_warning() {
        use crate::history::{Snapshot, Trend};

        let mut r = sample();
        let mut previous = Snapshot::from_report(&r);
        previous.tool_version = "0.2.0".to_string();
        let current = Snapshot::from_report(&r);
        r.trend = Some(Trend { previous, current });

        let html = render(&r);
        assert!(html.contains("class=\"trend-warn\""), "{html}");
        assert!(html.contains("not comparable"), "{html}");
        assert!(html.contains("ccn 0.2.0"), "{html}");
    }
}
