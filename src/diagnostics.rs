//! Aggregated diagnostics for a scan and an optional baseline comparison.
//!
//! A [`Diagnostic`] is a short, stable statement about something the user may
//! need to act on: a file the CCN pass could not read, a measurement that is
//! only heuristic, or a baseline comparison that is incomplete. The model is
//! deliberately flat and serialisable-friendly so the TUI panel, the report and
//! future consumers all describe the same problems with the same codes.

use crate::baseline::Baseline;
use crate::ccn::types::CcnTier;
use crate::model::{CcnSkip, ScanResult};

/// How urgently a diagnostic needs attention.
///
/// Declaration order is the display order: errors first, then warnings, then
/// information. [`Severity`] derives `Ord` so sorting a mixed list needs no
/// custom rank table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warn,
    Info,
}

/// Which part of the tool produced the diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    Scan,
    Baseline,
    Editor,
}

/// One actionable finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub area: Area,
    pub severity: Severity,
    /// Scan-relative path with `/` separators, when the finding is about a file.
    pub path: Option<String>,
    /// 1-based line number, when the finding can point at one.
    pub line: Option<usize>,
    /// Short, stable identifier such as `read-error` or `baseline-incomplete`.
    pub code: String,
    pub message: String,
}

/// Ordered collection of diagnostics.
///
/// The model never caps its item count: the UI scrolls, and a truncated report
/// would hide exactly the files that made a baseline incomplete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn extend(&mut self, items: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(items);
    }

    /// Derive diagnostics from a scan and, when present, its baseline.
    ///
    /// Scan findings are file-level and one per file: a file carries an
    /// explicit skip reason *or* a summary tier, never two warnings for the
    /// same missing measurement.
    pub fn from_scan(result: &ScanResult, baseline: Option<&Baseline>) -> Self {
        let mut diagnostics = Self::new();

        for file in &result.files {
            if file.is_excluded() {
                continue;
            }
            if let Some(skip) = file.ccn_skip {
                let (severity, code, message) = match skip {
                    CcnSkip::ReadError => (
                        Severity::Error,
                        "read-error",
                        format!("could not read {}", file.lang),
                    ),
                    CcnSkip::Oversize => (
                        Severity::Warn,
                        "oversize",
                        "skipped by the CCN size guard".to_string(),
                    ),
                    CcnSkip::NoGrammar => (
                        Severity::Info,
                        "no-grammar",
                        format!("no CCN grammar for {}", file.lang),
                    ),
                };
                diagnostics.push(Diagnostic {
                    area: Area::Scan,
                    severity,
                    path: Some(file.path.clone()),
                    line: None,
                    code: code.to_string(),
                    message,
                });
                // The skip is the authoritative account of why there is no
                // summary; do not add a second tier diagnostic for it.
                continue;
            }
            let Some(summary) = &file.ccn else {
                continue;
            };
            let (severity, code, message) = match summary.tier {
                CcnTier::Exact => continue,
                CcnTier::BestEffort => (
                    Severity::Warn,
                    "best-effort",
                    "CCN is best-effort: the parser recovered from syntax errors".to_string(),
                ),
                CcnTier::Heuristic => (
                    Severity::Warn,
                    "heuristic",
                    "CCN is heuristic: file-level number only".to_string(),
                ),
            };
            diagnostics.push(Diagnostic {
                area: Area::Scan,
                severity,
                path: Some(file.path.clone()),
                line: None,
                code: code.to_string(),
                message,
            });
        }

        if let Some(baseline) = baseline {
            let reasons = baseline.incomplete_reasons();
            let (severity, message) = if baseline.complete {
                (Severity::Info, "baseline comparison complete".to_string())
            } else if reasons.is_empty() {
                (
                    Severity::Error,
                    "baseline comparison is incomplete".to_string(),
                )
            } else {
                (
                    Severity::Error,
                    format!("baseline comparison incomplete: {}", reasons.join("; ")),
                )
            };
            diagnostics.push(Diagnostic {
                area: Area::Baseline,
                severity,
                path: None,
                line: None,
                code: "baseline-incomplete".to_string(),
                message,
            });

            let push_count =
                |count: usize, severity: Severity, code: &str, what: &str| -> Option<Diagnostic> {
                    (count > 0).then(|| Diagnostic {
                        area: Area::Baseline,
                        severity,
                        path: None,
                        line: None,
                        code: code.to_string(),
                        message: format!("{count} {what}"),
                    })
                };

            let items = [
                push_count(
                    baseline.files_unavailable,
                    Severity::Error,
                    "baseline-unavailable",
                    "baseline file(s) could not be read from the revision",
                ),
                push_count(
                    baseline.files_unknown,
                    Severity::Error,
                    "baseline-unknown",
                    "file(s) whose version-control view status is unknown",
                ),
                push_count(
                    baseline.files_ambiguous,
                    Severity::Error,
                    "baseline-ambiguous",
                    "file(s) whose functions could not be matched unambiguously",
                ),
                push_count(
                    baseline.files_unreadable,
                    Severity::Error,
                    "baseline-unreadable",
                    "file(s) could not be read",
                ),
                push_count(
                    baseline.files_unmeasured,
                    Severity::Error,
                    "baseline-unmeasured",
                    "current file(s) could not be measured",
                ),
                push_count(
                    baseline.files_skipped,
                    Severity::Warn,
                    "baseline-skipped",
                    "baseline copy(ies) could not be parsed",
                ),
                push_count(
                    baseline.files_unmapped,
                    Severity::Info,
                    "baseline-unmapped",
                    "file(s) outside the version-control view",
                ),
            ];
            diagnostics.extend(items.into_iter().flatten());
        }

        diagnostics.sort();
        diagnostics
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `(errors, warnings, infos)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for item in &self.items {
            match item.severity {
                Severity::Error => counts.0 += 1,
                Severity::Warn => counts.1 += 1,
                Severity::Info => counts.2 += 1,
            }
        }
        counts
    }

    /// One ASCII-only line for a status bar or a narrow panel.
    pub fn summary(&self) -> String {
        let (errors, warnings, infos) = self.counts();
        format!("{errors} error(s), {warnings} warning(s), {infos} info",)
    }

    /// Severity first, then area, then path/line, then code for determinism.
    fn sort(&mut self) {
        self.items.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.area.cmp(&b.area))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.message.cmp(&b.message))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::CcnSummary;
    use crate::model::{ExcludedFiles, FileLoc};

    fn file(path: &str, lang: &str) -> FileLoc {
        FileLoc {
            path: path.to_string(),
            lang: lang.to_string(),
            loc_total: 10,
            loc: 10,
            comments: 0,
            blanks: 0,
            ccn: None,
            ccn_skip: None,
            is_generated: false,
            is_vendored: false,
        }
    }

    fn scan(files: Vec<FileLoc>) -> ScanResult {
        ScanResult {
            root: ".".to_string(),
            files_analyzed: files.len(),
            loc_total: 0,
            loc: 0,
            comments: 0,
            blanks: 0,
            files_excluded: ExcludedFiles::default(),
            functions_total: 0,
            ccn: None,
            languages: Vec::new(),
            files,
        }
    }

    fn baseline() -> Baseline {
        Baseline {
            rev: "HEAD~1".to_string(),
            threshold: 10,
            files_expected: 4,
            files_compared: 1,
            files_skipped: 1,
            files_unmeasured: 1,
            files_oversize: 1,
            files_unreadable: 1,
            files_unavailable: 1,
            files_unmapped: 1,
            files_unknown: 1,
            files_ambiguous: 1,
            complete: false,
            changes: Vec::new(),
        }
    }

    fn find<'a>(diagnostics: &'a Diagnostics, code: &str) -> &'a Diagnostic {
        diagnostics
            .items()
            .iter()
            .find(|item| item.code == code)
            .unwrap_or_else(|| panic!("missing diagnostic {code}"))
    }

    #[test]
    fn read_error_is_an_error_with_the_language() {
        let mut broken = file("src/broken.rs", "Rust");
        broken.ccn_skip = Some(CcnSkip::ReadError);
        let diagnostics = Diagnostics::from_scan(&scan(vec![broken]), None);

        assert_eq!(diagnostics.len(), 1);
        let item = find(&diagnostics, "read-error");
        assert_eq!(item.severity, Severity::Error);
        assert_eq!(item.area, Area::Scan);
        assert_eq!(item.path.as_deref(), Some("src/broken.rs"));
        assert_eq!(item.message, "could not read Rust");
    }

    #[test]
    fn oversize_is_a_warning_and_no_grammar_is_info() {
        let mut big = file("src/big.rs", "Rust");
        big.ccn_skip = Some(CcnSkip::Oversize);
        let mut doc = file("README.md", "Markdown");
        doc.ccn_skip = Some(CcnSkip::NoGrammar);
        let diagnostics = Diagnostics::from_scan(&scan(vec![big, doc]), None);

        assert_eq!(diagnostics.len(), 2);
        let oversize = find(&diagnostics, "oversize");
        assert_eq!(oversize.severity, Severity::Warn);
        assert_eq!(oversize.message, "skipped by the CCN size guard");
        let grammar = find(&diagnostics, "no-grammar");
        assert_eq!(grammar.severity, Severity::Info);
        assert_eq!(grammar.message, "no CCN grammar for Markdown");
        assert_eq!(diagnostics.counts(), (0, 1, 1));
    }

    #[test]
    fn best_effort_summary_is_a_file_level_warning() {
        let mut degraded = file("src/degraded.cpp", "C++");
        degraded.ccn = Some(CcnSummary::from_file(crate::ccn::types::FileCcn {
            tier: CcnTier::BestEffort,
            functions: Vec::new(),
        }));
        let diagnostics = Diagnostics::from_scan(&scan(vec![degraded]), None);

        assert_eq!(diagnostics.len(), 1);
        let item = find(&diagnostics, "best-effort");
        assert_eq!(item.severity, Severity::Warn);
        assert_eq!(item.line, None, "a tier finding is file-level");
    }

    #[test]
    fn heuristic_summary_is_reported_once() {
        let mut fallback = file("src/fallback.vb", "Visual Basic");
        fallback.ccn = Some(CcnSummary::heuristic(42));
        let diagnostics = Diagnostics::from_scan(&scan(vec![fallback]), None);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(find(&diagnostics, "heuristic").severity, Severity::Warn);
    }

    #[test]
    fn a_skip_wins_over_a_summary_tier() {
        let mut weird = file("src/weird.rs", "Rust");
        weird.ccn_skip = Some(CcnSkip::ReadError);
        weird.ccn = Some(CcnSummary::heuristic(9));
        let diagnostics = Diagnostics::from_scan(&scan(vec![weird]), None);

        assert_eq!(diagnostics.len(), 1, "no duplicate tier diagnostic");
        assert_eq!(diagnostics.items()[0].code, "read-error");
    }

    #[test]
    fn exact_and_excluded_files_stay_silent() {
        let mut exact = file("src/clean.rs", "Rust");
        exact.ccn = Some(CcnSummary::from_file(crate::ccn::types::FileCcn {
            tier: CcnTier::Exact,
            functions: Vec::new(),
        }));
        let mut generated = file("src/api.gen.ts", "TypeScript");
        generated.is_generated = true;
        generated.ccn_skip = Some(CcnSkip::ReadError);
        let mut vendored = file("vendor/x.js", "JavaScript");
        vendored.is_vendored = true;
        vendored.ccn_skip = Some(CcnSkip::Oversize);

        let diagnostics = Diagnostics::from_scan(&scan(vec![exact, generated, vendored]), None);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn complete_baseline_is_informational() {
        let complete = Baseline {
            complete: true,
            files_compared: 3,
            ..Baseline::default()
        };
        let diagnostics = Diagnostics::from_scan(&scan(Vec::new()), Some(&complete));

        assert_eq!(diagnostics.len(), 1);
        let item = find(&diagnostics, "baseline-incomplete");
        assert_eq!(item.severity, Severity::Info);
        assert_eq!(item.message, "baseline comparison complete");
        assert_eq!(item.path, None);
    }

    #[test]
    fn incomplete_baseline_has_a_summary_and_counted_items() {
        let diagnostics = Diagnostics::from_scan(&scan(Vec::new()), Some(&baseline()));

        let summary = find(&diagnostics, "baseline-incomplete");
        assert_eq!(summary.severity, Severity::Error);
        assert_eq!(summary.area, Area::Baseline);
        assert!(
            summary
                .message
                .starts_with("baseline comparison incomplete:"),
            "{}",
            summary.message
        );
        assert!(summary.message.contains("1 file(s) could not be read"));

        assert_eq!(
            find(&diagnostics, "baseline-unavailable").severity,
            Severity::Error
        );
        assert_eq!(
            find(&diagnostics, "baseline-unknown").severity,
            Severity::Error
        );
        assert_eq!(
            find(&diagnostics, "baseline-ambiguous").severity,
            Severity::Error
        );
        assert_eq!(
            find(&diagnostics, "baseline-unreadable").severity,
            Severity::Error
        );
        assert_eq!(
            find(&diagnostics, "baseline-unmeasured").severity,
            Severity::Error
        );
        assert_eq!(
            find(&diagnostics, "baseline-skipped").severity,
            Severity::Warn
        );
        assert_eq!(
            find(&diagnostics, "baseline-unmapped").severity,
            Severity::Info
        );
        assert_eq!(
            find(&diagnostics, "baseline-unavailable").message,
            "1 baseline file(s) could not be read from the revision"
        );
    }

    #[test]
    fn zero_count_baseline_categories_are_omitted() {
        let baseline = Baseline {
            files_compared: 2,
            complete: true,
            ..Baseline::default()
        };
        let diagnostics = Diagnostics::from_scan(&scan(Vec::new()), Some(&baseline));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics.items()[0].code, "baseline-incomplete");
    }

    #[test]
    fn counts_and_summary_are_ascii() {
        let mut broken = file("src/broken.rs", "Rust");
        broken.ccn_skip = Some(CcnSkip::ReadError);
        let mut big = file("src/big.rs", "Rust");
        big.ccn_skip = Some(CcnSkip::Oversize);
        let mut doc = file("README.md", "Markdown");
        doc.ccn_skip = Some(CcnSkip::NoGrammar);
        let diagnostics = Diagnostics::from_scan(&scan(vec![broken, big, doc]), Some(&baseline()));

        let (errors, warnings, infos) = diagnostics.counts();
        assert_eq!(errors, 7, "read-error + incomplete + 5 counted errors");
        assert_eq!(warnings, 2, "oversize + skipped");
        assert_eq!(infos, 2, "no-grammar + unmapped");
        let summary = diagnostics.summary();
        assert!(
            summary.is_ascii(),
            "summary must render in a narrow ASCII terminal: {summary}"
        );
        assert_eq!(summary, "7 error(s), 2 warning(s), 2 info");
    }

    #[test]
    fn sort_is_severity_then_area_then_path() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.push(Diagnostic {
            area: Area::Editor,
            severity: Severity::Info,
            path: None,
            line: None,
            code: "editor-note".to_string(),
            message: "editor".to_string(),
        });
        diagnostics.push(Diagnostic {
            area: Area::Baseline,
            severity: Severity::Error,
            path: None,
            line: None,
            code: "baseline".to_string(),
            message: "baseline".to_string(),
        });
        diagnostics.push(Diagnostic {
            area: Area::Scan,
            severity: Severity::Error,
            path: Some("z.rs".to_string()),
            line: None,
            code: "z".to_string(),
            message: "z".to_string(),
        });
        diagnostics.push(Diagnostic {
            area: Area::Scan,
            severity: Severity::Error,
            path: Some("a.rs".to_string()),
            line: None,
            code: "a".to_string(),
            message: "a".to_string(),
        });
        diagnostics.sort();

        let order: Vec<&str> = diagnostics
            .items()
            .iter()
            .map(|item| item.code.as_str())
            .collect();
        assert_eq!(order, vec!["a", "z", "baseline", "editor-note"]);
    }
}
