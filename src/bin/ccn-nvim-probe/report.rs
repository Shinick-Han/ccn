//! Output shapes: one JSON document, or the compact human table.

use ccn::ccn::CcnSummary;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ProbeReport {
    pub file: String,
    pub tier: &'static str,
    pub functions: Vec<FunctionRow>,
    pub sum: u32,
    pub max: u32,
}

#[derive(Debug, Serialize)]
pub struct FunctionRow {
    pub name: String,
    pub identity: String,
    pub line: usize,
    pub ccn: u32,
    pub nloc: usize,
    pub nesting: u32,
    pub params: usize,
}

impl ProbeReport {
    pub fn new(file: &str, summary: &CcnSummary) -> Self {
        Self {
            file: file.to_string(),
            tier: summary.tier.as_str(),
            functions: summary
                .detail
                .iter()
                .map(|f| FunctionRow {
                    name: f.name.clone(),
                    identity: f.identity.clone(),
                    line: f.line,
                    ccn: f.ccn,
                    nloc: f.nloc,
                    nesting: f.max_nesting,
                    params: f.params,
                })
                .collect(),
            sum: summary.sum,
            max: summary.max,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn to_table(&self) -> String {
        let mut out = format!(
            "file: {}  tier: {}  functions: {}  sum: {}  max: {}\n",
            self.file,
            self.tier,
            self.functions.len(),
            self.sum,
            self.max
        );
        out.push_str("  line  ccn  nloc  nest  params  name\n");
        for f in &self.functions {
            out.push_str(&format!(
                "  {:>4}  {:>3}  {:>4}  {:>4}  {:>6}  {}  ({})\n",
                f.line, f.ccn, f.nloc, f.nesting, f.params, f.name, f.identity
            ));
        }
        out
    }
}

/// One extmark read back from Neovim after annotation.
#[derive(Debug, Serialize)]
pub struct MarkRow {
    pub id: i64,
    pub row: i64,
    pub col: i64,
    pub text: String,
    pub hl: String,
}

impl MarkRow {
    pub fn to_table(marks: &[MarkRow]) -> String {
        let mut out = format!("extmarks in namespace ccn_rpc: {}\n", marks.len());
        for m in marks {
            out.push_str(&format!(
                "  id={} row={} col={} text={:?} hl={}\n",
                m.id, m.row, m.col, m.text, m.hl
            ));
        }
        out
    }
}
