//! SARIF 2.1.0 output.
//!
//! SARIF is what a code-scanning UI (GitHub code scanning, VS Code, ...) reads,
//! so this module emits *findings*, not a repository dump:
//!
//! - full mode: one result per measured function at or above `min_ccn`;
//! - delta mode (`report.baseline` is `Some`): one result per baseline
//!   [`Change`], which is what a pull request should see.
//!
//! Everything is built as [`serde_json::Value`], never by string concatenation,
//! so a function name containing JSON metacharacters cannot corrupt the
//! document. Paths are scan-root-relative with `/` separators; no absolute
//! path is ever written, so a report stays valid when copied to another machine.

use serde_json::{json, Value};

use crate::baseline::{Change, Kind};
use crate::ccn::types::CcnTier;
use crate::metrics::Report;
use crate::model::ScanResult;

const SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const INFORMATION_URI: &str = "https://github.com/Shinick-Han/ccn";
const HELP_URI: &str = "https://github.com/Shinick-Han/ccn#sarif";

/// Render a SARIF 2.1.0 document for `result` / `report`.
pub fn render(result: &ScanResult, report: &Report, min_ccn: u32) -> String {
    let mut findings: Vec<(String, usize, &'static str, Value)> = match &report.baseline {
        Some(baseline) => baseline
            .changes
            .iter()
            .map(|change| {
                let rule = rule_for(change.kind);
                (
                    change.path.clone(),
                    change.line,
                    rule,
                    delta_result(change, rule, baseline.threshold, &report.root),
                )
            })
            .collect(),
        None => full_findings(result, min_ccn),
    };

    // Deterministic order independent of scan or baseline scheduling.
    findings.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(b.2))
    });

    let results: Vec<Value> = findings.into_iter().map(|(_, _, _, value)| value).collect();

    let document = json!({
        "$schema": SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "ccn",
                    "version": env!("CARGO_PKG_VERSION"),
                    "semanticVersion": env!("CARGO_PKG_VERSION"),
                    "informationUri": INFORMATION_URI,
                    "rules": rules(),
                }
            },
            "invocations": [{
                "executionSuccessful": true
            }],
            "results": results,
            "properties": {
                "ccnVersion": env!("CARGO_PKG_VERSION"),
                "minCcn": min_ccn
            }
        }]
    });

    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

/// One result per measured function at or above `min_ccn`.
fn full_findings(result: &ScanResult, min_ccn: u32) -> Vec<(String, usize, &'static str, Value)> {
    let mut findings = Vec::new();

    for file in &result.files {
        // Generated / vendored files and files with no function-level CCN (a
        // heuristic file-level number, an unreadable file, `--no-ccn`) are not
        // findings.
        if file.is_excluded() {
            continue;
        }
        let Some(summary) = &file.ccn else { continue };
        if summary.tier == CcnTier::Heuristic {
            continue;
        }

        let path = relative_uri(&file.path, &result.root);
        for function in &summary.detail {
            if function.ccn < min_ccn {
                continue;
            }
            findings.push((
                path.clone(),
                function.line,
                RULE_COMPLEX,
                json!({
                    "ruleId": RULE_COMPLEX,
                    "level": full_level(function.ccn),
                    "message": {
                        "text": format!(
                            "Function {} has cyclomatic complexity {} (limit {}).",
                            function.name, function.ccn, min_ccn
                        )
                    },
                    "locations": [location(&path, function.line, function.end_line)]
                }),
            ));
        }
    }

    findings
}

/// One result per baseline change (added or worsened).
fn delta_result(change: &Change, rule: &'static str, threshold: u32, root: &str) -> Value {
    let path = relative_uri(&change.path, root);
    let (level, message) = match change.kind {
        Kind::Added => (
            added_level(change.ccn),
            format!(
                "Function {} is new and has cyclomatic complexity {} (limit {}).",
                change.name, change.ccn, threshold
            ),
        ),
        Kind::Worsened => (
            worsened_level(change.ccn),
            format!(
                "Function {} increased from {} to {} cyclomatic complexity (limit {}).",
                change.name,
                change.base_ccn.unwrap_or(0),
                change.ccn,
                threshold
            ),
        ),
    };

    json!({
        "ruleId": rule,
        "level": level,
        "message": { "text": message },
        "locations": [location(&path, change.line, 0)]
    })
}

fn location(path: &str, start_line: usize, end_line: usize) -> Value {
    let mut region = json!({ "startLine": start_line.max(1) });
    if end_line > 0 {
        region["endLine"] = json!(end_line);
    }
    json!({
        "physicalLocation": {
            "artifactLocation": { "uri": path },
            "region": region
        }
    })
}

/// SARIF's severity bands, aligned with the report's published CCN buckets.
fn full_level(ccn: u32) -> &'static str {
    if ccn > 50 {
        "error"
    } else if ccn >= 21 {
        "warning"
    } else {
        "note"
    }
}

/// A new complex function is never merely informational.
fn added_level(ccn: u32) -> &'static str {
    if ccn > 50 {
        "error"
    } else {
        "warning"
    }
}

/// A regression is actionable, but not fatal unless it is in the worst band.
fn worsened_level(ccn: u32) -> &'static str {
    if ccn > 50 {
        "error"
    } else {
        "note"
    }
}

fn rule_for(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => RULE_ADDED,
        Kind::Worsened => RULE_WORSENED,
    }
}

const RULE_COMPLEX: &str = "ccn/complex-function";
const RULE_ADDED: &str = "ccn/added-complex-function";
const RULE_WORSENED: &str = "ccn/worsened-complex-function";

fn rules() -> Value {
    json!([
        {
            "id": RULE_COMPLEX,
            "name": "ComplexFunction",
            "shortDescription": {
                "text": "Function at or above the cyclomatic complexity limit"
            },
            "helpUri": HELP_URI,
            "defaultConfiguration": { "level": "warning" }
        },
        {
            "id": RULE_ADDED,
            "name": "AddedComplexFunction",
            "shortDescription": {
                "text": "New function already at or above the cyclomatic complexity limit"
            },
            "helpUri": HELP_URI,
            "defaultConfiguration": { "level": "warning" }
        },
        {
            "id": RULE_WORSENED,
            "name": "WorsenedComplexFunction",
            "shortDescription": {
                "text": "Function crossed the cyclomatic complexity limit since the baseline"
            },
            "helpUri": HELP_URI,
            "defaultConfiguration": { "level": "note" }
        }
    ])
}

/// Normalise a scan-root-relative path for an `artifactLocation.uri`.
///
/// The model already promises root-relative paths; this additionally strips a
/// leaked Windows/absolute root prefix and converts separators, so an absolute
/// path can never reach the document.
fn relative_uri(path: &str, root: &str) -> String {
    let normalised = path.replace('\\', "/");
    let normalised_root = root.replace('\\', "/");
    let normalised_root = normalised_root.trim_end_matches('/');
    let mut uri = normalised.as_str();

    // The contract is already root-relative, but a caller may hand over an
    // absolute path; strip the scan root when it is present.
    if !normalised_root.is_empty() {
        if uri == normalised_root {
            uri = "";
        } else if let Some(rest) = uri
            .strip_prefix(normalised_root)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            uri = rest;
        }
    }

    // Fallback for a leaked drive prefix whose root did not match: drop the
    // `C:/` part so the document still cannot name an absolute location.
    if uri.len() >= 3 && uri.as_bytes()[1] == b':' && uri.as_bytes()[2] == b'/' {
        uri = &uri[3..];
    }
    uri = uri.trim_start_matches('/');
    while let Some(stripped) = uri.strip_prefix("./") {
        uri = stripped;
    }

    uri.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::Baseline;
    use crate::ccn::types::{CcnSummary, FileCcn, FunctionCcn};
    use crate::metrics;

    fn function(name: &str, line: usize, end_line: usize, ccn: u32) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            line,
            end_line,
            nloc: end_line.saturating_sub(line).max(1),
            ccn,
            params: 0,
        }
    }

    fn file(path: &str, functions: Vec<FunctionCcn>) -> crate::model::FileLoc {
        crate::model::FileLoc {
            path: path.to_string(),
            lang: "Rust".to_string(),
            loc_total: 100,
            loc: 100,
            comments: 0,
            blanks: 0,
            ccn: Some(CcnSummary::from_file(FileCcn {
                tier: CcnTier::Exact,
                functions,
            })),
            is_generated: false,
            is_vendored: false,
        }
    }

    fn scan_result(files: Vec<crate::model::FileLoc>) -> ScanResult {
        let loc: usize = files.iter().map(|f| f.loc).sum();
        ScanResult {
            root: "C:/Users/example/repo".to_string(),
            files_analyzed: files.len(),
            loc_total: loc,
            loc,
            comments: 0,
            blanks: 0,
            files_excluded: crate::model::ExcludedFiles::default(),
            functions_total: 0,
            ccn: None,
            languages: Vec::new(),
            files,
        }
    }

    fn parsed(document: &str) -> Value {
        serde_json::from_str(document).expect("valid JSON")
    }

    fn results(document: &Value) -> &Vec<Value> {
        document["runs"][0]["results"]
            .as_array()
            .expect("results array")
    }

    #[test]
    fn document_has_the_sarif_envelope() {
        let result = scan_result(vec![file("src/a.rs", vec![function("f", 1, 5, 22)])]);
        let report = metrics::build(&result);
        let document = parsed(&render(&result, &report, 10));

        assert_eq!(
            document["$schema"],
            "https://json.schemastore.org/sarif-2.1.0.json"
        );
        assert_eq!(document["version"], "2.1.0");
        assert_eq!(document["runs"].as_array().unwrap().len(), 1);

        let driver = &document["runs"][0]["tool"]["driver"];
        assert_eq!(driver["name"], "ccn");
        assert_eq!(driver["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(driver["semanticVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(driver["informationUri"], INFORMATION_URI);
        assert_eq!(driver["rules"].as_array().unwrap().len(), 3);

        assert_eq!(
            document["runs"][0]["invocations"][0]["executionSuccessful"],
            true
        );
        assert_eq!(document["runs"][0]["properties"]["minCcn"], 10);
    }

    #[test]
    fn full_mode_maps_ccn_bands_to_levels_and_lines() {
        let result = scan_result(vec![file(
            "src/a.rs",
            vec![
                function("simple", 5, 7, 5),
                function("noted", 11, 14, 11),
                function("warned", 20, 25, 22),
                function("failed", 30, 40, 51),
            ],
        )]);
        let report = metrics::build(&result);
        let document = parsed(&render(&result, &report, 10));
        let found = results(&document);

        assert_eq!(found.len(), 3, "CCN 5 is below the limit of 10");
        assert_eq!(found[0]["level"], "note");
        assert_eq!(
            found[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            11
        );
        assert_eq!(
            found[0]["locations"][0]["physicalLocation"]["region"]["endLine"],
            14
        );
        assert_eq!(found[1]["level"], "warning");
        assert_eq!(
            found[1]["locations"][0]["physicalLocation"]["region"]["startLine"],
            20
        );
        assert_eq!(found[2]["level"], "error");
        assert_eq!(
            found[2]["locations"][0]["physicalLocation"]["region"]["startLine"],
            30
        );
        assert!(found.iter().all(|r| r["ruleId"] == RULE_COMPLEX));
    }

    #[test]
    fn min_ccn_filters_results() {
        let result = scan_result(vec![file(
            "src/a.rs",
            vec![
                function("a", 1, 2, 5),
                function("b", 3, 4, 11),
                function("c", 5, 6, 22),
                function("d", 7, 8, 51),
            ],
        )]);
        let report = metrics::build(&result);
        let document = parsed(&render(&result, &report, 30));
        let found = results(&document);

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]["message"]["text"],
            "Function d has cyclomatic complexity 51 (limit 30)."
        );
    }

    #[test]
    fn delta_mode_emits_exactly_the_baseline_changes() {
        let result = scan_result(vec![file(
            "src/a.rs",
            vec![function("grew", 3, 9, 25), function("new", 12, 16, 15)],
        )]);
        let mut report = metrics::build(&result);
        report.baseline = Some(Baseline {
            rev: "main".to_string(),
            threshold: 10,
            files_compared: 1,
            files_skipped: 0,
            changes: vec![
                Change {
                    path: "src/a.rs".to_string(),
                    name: "grew".to_string(),
                    line: 3,
                    ccn: 25,
                    base_ccn: Some(4),
                    kind: Kind::Worsened,
                },
                Change {
                    path: "src/a.rs".to_string(),
                    name: "new".to_string(),
                    line: 12,
                    ccn: 15,
                    base_ccn: None,
                    kind: Kind::Added,
                },
            ],
        });

        let document = parsed(&render(&result, &report, 10));
        let found = results(&document);

        assert_eq!(found.len(), 2, "delta mode must not dump the whole repo");
        assert_eq!(found[0]["ruleId"], RULE_WORSENED);
        assert_eq!(found[0]["level"], "note");
        assert!(found[0]["message"]["text"]
            .as_str()
            .unwrap()
            .contains("from 4 to 25"));
        assert_eq!(found[1]["ruleId"], RULE_ADDED);
        assert_eq!(found[1]["level"], "warning");
        assert!(found[1]["message"]["text"]
            .as_str()
            .unwrap()
            .contains("cyclomatic complexity 15"));
        assert_eq!(
            found[1]["locations"][0]["physicalLocation"]["region"]["startLine"],
            12
        );
    }

    #[test]
    fn excluded_heuristic_and_unmeasured_files_are_never_emitted() {
        let mut generated = file("gen/b.rs", vec![function("huge", 1, 2, 90)]);
        generated.is_generated = true;
        let mut vendored = file("vendor/c.rs", vec![function("big", 1, 2, 80)]);
        vendored.is_vendored = true;
        let mut heuristic = file("legacy/x.vb", Vec::new());
        heuristic.ccn = Some(CcnSummary::heuristic(70));
        let mut without_ccn = file("src/none.rs", Vec::new());
        without_ccn.ccn = None;

        let result = scan_result(vec![
            generated,
            vendored,
            heuristic,
            without_ccn,
            file("src/ok.rs", vec![function("real", 1, 2, 30)]),
        ]);
        let report = metrics::build(&result);
        let document = parsed(&render(&result, &report, 1));
        let found = results(&document);

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "src/ok.rs"
        );
    }

    #[test]
    fn output_is_deterministic() {
        let result = scan_result(vec![
            file(
                "src/b.rs",
                vec![function("b", 4, 6, 31), function("a", 1, 2, 12)],
            ),
            file("src/a.rs", vec![function("c", 2, 3, 60)]),
        ]);
        let report = metrics::build(&result);

        let first = render(&result, &report, 10);
        let second = render(&result, &report, 10);
        assert_eq!(first, second);
    }

    #[test]
    fn no_absolute_path_leaks_into_the_document() {
        let result = scan_result(vec![file(
            "C:/Users/example/repo/src/a.rs",
            vec![function("f", 1, 2, 40)],
        )]);
        let report = metrics::build(&result);
        let document = render(&result, &report, 10);

        assert!(!document.contains("C:/"), "{document}");
        assert!(!document.contains("C:\\\\"), "{document}");
        let value = parsed(&document);
        assert_eq!(
            value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "src/a.rs"
        );
    }

    #[test]
    fn json_metacharacters_in_names_round_trip() {
        let name = "a & b < c > \"quoted\"";
        let result = scan_result(vec![file("src/a.rs", vec![function(name, 1, 2, 22)])]);
        let report = metrics::build(&result);
        let document = parsed(&render(&result, &report, 10));

        let text = document["runs"][0]["results"][0]["message"]["text"]
            .as_str()
            .expect("message text");
        assert!(text.contains(name), "{text}");
    }
}
