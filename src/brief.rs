//! Agent-oriented complexity brief.
//!
//! `ccn brief` is a bounded, deterministic document over the same scan used by
//! the rest of ccn. It selects the most complex functions, optionally compares
//! them against a baseline, and attaches a capped source snippet. The output
//! is intentionally a *pre-processing* artifact: no model call, no network and
//! no writes to the workspace.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use crate::baseline::{self, Kind};
use crate::loc::{self, ScanOptions};
use crate::model::ScanResult;
use crate::vcs;

const DEFAULT_TOP: usize = 8;
const DEFAULT_MIN_CCN: u32 = 10;
const DEFAULT_MAX_LINES: usize = 120;

#[derive(Debug, Parser)]
#[command(
    name = "ccn brief",
    version,
    about = "Emit a bounded complexity brief for an agent",
    long_about = None,
    after_help = "Examples:\n  ccn brief .\n  ccn brief . --top 12 --min-ccn 15\n  ccn brief . --baseline HEAD~1\n  ccn brief . --json --no-source"
)]
struct BriefCli {
    /// Directory to scan.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Maximum number of functions in the brief.
    #[arg(long, default_value_t = DEFAULT_TOP)]
    top: usize,

    /// Only include functions at or above this CCN.
    #[arg(long, default_value_t = DEFAULT_MIN_CCN)]
    min_ccn: u32,

    /// Compare against a revision and include only new/worsened functions.
    ///
    /// Git: `HEAD~1`, a branch, or a commit. Perforce: a changelist number.
    #[arg(long, value_name = "REV")]
    baseline: Option<String>,

    /// Emit the stable JSON form instead of Markdown.
    #[arg(long)]
    json: bool,

    /// Maximum source lines attached to one function.
    #[arg(long, default_value_t = DEFAULT_MAX_LINES)]
    max_lines: usize,

    /// Do not read or attach source snippets.
    #[arg(long)]
    no_source: bool,

    /// Restrict to these languages (comma separated).
    #[arg(long, value_delimiter = ',')]
    lang: Vec<String>,

    #[arg(long)]
    hidden: bool,

    #[arg(long)]
    no_ignore: bool,

    #[arg(long)]
    no_follow_links: bool,

    #[arg(long)]
    no_default_excludes: bool,

    #[arg(long)]
    include_generated: bool,

    #[arg(long)]
    no_cache: bool,

    #[arg(long)]
    verify_cache: bool,
}

#[derive(Debug, Serialize)]
struct Brief {
    root: String,
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<String>,
    baseline_complete: bool,
    threshold: u32,
    total_functions: usize,
    returned: usize,
    functions: Vec<BriefFunction>,
}

#[derive(Debug, Serialize)]
struct BriefFunction {
    path: String,
    name: String,
    identity: String,
    line: usize,
    end_line: usize,
    ccn: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_ccn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<Kind>,
    nloc: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<usize>,
    max_nesting: u32,
    reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

impl BriefCli {
    fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            root: self.path.clone(),
            hidden: self.hidden,
            no_ignore: self.no_ignore,
            no_follow_links: self.no_follow_links,
            no_default_excludes: self.no_default_excludes,
            langs: self.lang.clone(),
            no_ccn: false,
            include_generated: self.include_generated,
            no_cache: self.no_cache,
            verify_cache: self.verify_cache,
            since: None,
        }
    }
}

/// Dispatch entry point for `ccn brief`, called before the normal clap parser.
pub fn run(args: &[OsString]) -> i32 {
    let mut argv = vec![OsString::from("ccn brief")];
    argv.extend(args.iter().cloned());
    let cli = BriefCli::parse_from(argv);

    match execute(&cli) {
        Ok(document) => {
            print!("{document}");
            0
        }
        Err(error) => {
            eprintln!("ccn brief: {error}");
            2
        }
    }
}

fn execute(cli: &BriefCli) -> Result<String, String> {
    let result = loc::scan(&cli.scan_options()).map_err(|error| error.to_string())?;
    let brief = build(&result, cli)?;
    if cli.json {
        let mut json = serde_json::to_string_pretty(&brief).map_err(|error| error.to_string())?;
        json.push('\n');
        Ok(json)
    } else {
        Ok(render_markdown(&brief))
    }
}

fn build(result: &ScanResult, cli: &BriefCli) -> Result<Brief, String> {
    let mut functions = Vec::new();
    let mut mode = "workspace";
    let mut baseline_rev = None;
    let mut baseline_complete = true;
    let mut total_functions = result.functions_total;

    if let Some(rev) = &cli.baseline {
        let root = Path::new(&result.root);
        let backend = vcs::detect(root)
            .ok_or_else(|| "--baseline needs a Git or Perforce workspace".to_string())?;
        if !vcs::rev_exists(root, backend, rev) {
            return Err(format!("cannot resolve baseline revision {rev}"));
        }

        let baseline = baseline::collect(result, backend, rev, cli.min_ccn);
        total_functions = baseline.changes.len();
        baseline_complete = baseline.complete;
        baseline_rev = Some(baseline.rev.clone());
        mode = "changed";

        for change in baseline.changes {
            if change.ccn < cli.min_ccn {
                continue;
            }
            functions.push(BriefFunction {
                path: change.path,
                name: change.name,
                identity: change.identity,
                line: change.line,
                end_line: (change.line + change.nloc)
                    .saturating_sub(1)
                    .max(change.line),
                ccn: change.ccn,
                base_ccn: change.base_ccn,
                kind: Some(change.kind),
                nloc: change.nloc,
                params: None,
                max_nesting: change.max_nesting,
                reasons: change.reasons,
                source: None,
                truncated: false,
            });
        }
    } else {
        for file in &result.files {
            if file.is_excluded() {
                continue;
            }
            let Some(summary) = &file.ccn else {
                continue;
            };
            for function in &summary.detail {
                if function.ccn < cli.min_ccn {
                    continue;
                }
                functions.push(BriefFunction {
                    path: file.path.clone(),
                    name: function.name.clone(),
                    identity: function.identity.clone(),
                    line: function.line,
                    end_line: function.end_line,
                    ccn: function.ccn,
                    base_ccn: None,
                    kind: None,
                    nloc: function.nloc,
                    params: Some(function.params),
                    max_nesting: function.max_nesting,
                    reasons: Vec::new(),
                    source: None,
                    truncated: false,
                });
            }
        }
    }

    functions.sort_by(|a, b| {
        b.ccn
            .cmp(&a.ccn)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    functions.truncate(cli.top);

    if !cli.no_source {
        for function in &mut functions {
            let (source, truncated) = read_source(
                Path::new(&result.root),
                &function.path,
                function.line,
                function.end_line,
                cli.max_lines.max(1),
            );
            function.source = source;
            function.truncated = truncated;
        }
    }

    let returned = functions.len();
    Ok(Brief {
        root: result.root.clone(),
        mode,
        baseline: baseline_rev,
        baseline_complete,
        threshold: cli.min_ccn,
        total_functions,
        returned,
        functions,
    })
}

fn read_source(
    root: &Path,
    relative: &str,
    line: usize,
    end_line: usize,
    max_lines: usize,
) -> (Option<String>, bool) {
    let path = root.join(relative);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (None, false);
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return (Some(String::new()), false);
    }

    let start = line.saturating_sub(1).min(lines.len());
    let mut end = end_line.max(line).min(lines.len());
    let truncated = end.saturating_sub(start) > max_lines;
    if truncated {
        end = start + max_lines;
    }
    (Some(lines[start..end].join("\n")), truncated)
}

fn render_markdown(brief: &Brief) -> String {
    let mut out = vec![
        "# CCN brief".to_string(),
        String::new(),
        format!("Root: `{}`", brief.root),
        format!(
            "Mode: {}",
            match (&brief.baseline, brief.mode) {
                (Some(rev), _) => format!("changed since `{rev}`"),
                (None, _) => "workspace hotspots".to_string(),
            }
        ),
        format!("Threshold: CCN >= {}", brief.threshold),
        format!(
            "Functions: {} measured · {} returned",
            brief.total_functions, brief.returned
        ),
        String::new(),
    ];

    if !brief.baseline_complete {
        out.push("_Baseline incomplete: some files could not be compared._".to_string());
        out.push(String::new());
    }

    if brief.functions.is_empty() {
        out.push(format!(
            "No functions at or above CCN {}. Pass `--min-ccn 0` to include simpler functions.",
            brief.threshold
        ));
        return format!("{}\n", out.join("\n"));
    }

    for (index, function) in brief.functions.iter().enumerate() {
        out.push(format!(
            "## {}. `{}:{}` — CCN {}",
            index + 1,
            function.path,
            function.name,
            function.ccn
        ));
        let mut metric = format!(
            "- line {}–{}, {} nloc, nesting {}",
            function.line, function.end_line, function.nloc, function.max_nesting
        );
        if let Some(params) = function.params {
            metric.push_str(&format!(", {params} params"));
        }
        out.push(metric);
        if let Some(base) = function.base_ccn {
            out.push(format!("- base CCN: {base}"));
        }
        if let Some(kind) = function.kind {
            out.push(format!("- change: {}", kind_label(kind)));
        }
        if !function.reasons.is_empty() {
            out.push(format!("- reasons: {}", function.reasons.join(", ")));
        }
        out.push(String::new());

        if let Some(source) = &function.source {
            let fence = if source.contains("```") {
                "````"
            } else {
                "```"
            };
            out.push(format!("{fence}{}", language_for(&function.path)));
            out.push(source.clone());
            out.push(fence.to_string());
            if function.truncated {
                out.push("_(source truncated)_".to_string());
            }
            out.push(String::new());
        }
    }

    format!("{}\n", out.join("\n"))
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => "added",
        Kind::Worsened => "worsened",
    }
}

fn language_for(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "js" | "jsx" | "mjs" => "javascript",
        "py" => "python",
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> BriefCli {
        let mut argv = vec!["ccn brief"];
        argv.extend_from_slice(args);
        BriefCli::parse_from(argv)
    }

    #[test]
    fn cli_defaults_are_bounded() {
        let cli = parse(&[]);
        assert_eq!(cli.top, DEFAULT_TOP);
        assert_eq!(cli.min_ccn, DEFAULT_MIN_CCN);
        assert_eq!(cli.max_lines, DEFAULT_MAX_LINES);
        assert!(!cli.no_source);
        assert!(!cli.json);
        assert_eq!(cli.path, PathBuf::from("."));
    }

    #[test]
    fn cli_accepts_agent_options() {
        let cli = parse(&[
            "src",
            "--top",
            "3",
            "--min-ccn",
            "20",
            "--baseline",
            "HEAD~1",
            "--json",
            "--no-source",
            "--lang",
            "c,rust",
        ]);
        assert_eq!(cli.path, PathBuf::from("src"));
        assert_eq!(cli.top, 3);
        assert_eq!(cli.min_ccn, 20);
        assert_eq!(cli.baseline.as_deref(), Some("HEAD~1"));
        assert!(cli.json);
        assert!(cli.no_source);
        assert_eq!(cli.lang, vec!["c", "rust"]);
    }

    #[test]
    fn brief_selects_complex_functions_and_bounds_source() {
        let root = std::env::temp_dir().join(format!(
            "ccn-brief-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("complex.c");
        std::fs::write(
            &source,
            "int classify(int x) {\n  if (x > 0) { if (x > 1) return 2; }\n  return 1;\n}\n",
        )
        .unwrap();

        let options = ScanOptions {
            root: root.clone(),
            hidden: false,
            no_ignore: false,
            no_follow_links: false,
            no_default_excludes: false,
            langs: Vec::new(),
            no_ccn: false,
            include_generated: false,
            no_cache: true,
            verify_cache: false,
            since: None,
        };
        let result = loc::scan(&options).unwrap();
        let cli = parse(&[root.to_str().unwrap(), "--min-ccn", "0", "--top", "1"]);
        let brief = build(&result, &cli).unwrap();
        assert_eq!(brief.returned, 1);
        assert_eq!(brief.functions[0].name, "classify");
        assert!(brief.functions[0].source.is_some());
        assert!(brief.functions[0]
            .source
            .as_deref()
            .unwrap()
            .contains("return 1"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn markdown_names_the_baseline_and_says_when_empty() {
        let brief = Brief {
            root: "/tmp/work".to_string(),
            mode: "changed",
            baseline: Some("HEAD~1".to_string()),
            baseline_complete: false,
            threshold: 10,
            total_functions: 0,
            returned: 0,
            functions: Vec::new(),
        };
        let markdown = render_markdown(&brief);
        assert!(markdown.contains("changed since `HEAD~1`"));
        assert!(markdown.contains("Baseline incomplete"));
        assert!(markdown.contains("No functions at or above CCN 10"));
    }
}
