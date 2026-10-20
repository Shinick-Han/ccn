//! Shared CCN contract: the types every module (rules, engine, heuristic,
//! model) agrees on.
//!
//! **Do not change this file without updating `docs/PLAN.md` §5.3.** The JSON
//! shape here is the public output contract.

use serde::{Deserialize, Serialize};

use crate::model::percentile;

/// How much to trust a file's CCN.
///
/// The tier is per file, never per report: a repository can (and usually does)
/// mix tiers, and silently averaging them is the failure mode this enum exists
/// to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CcnTier {
    /// Real AST, per-function CCN, golden-fixture verified.
    Exact,
    /// Real AST, curated rules, but not fixture-covered for this language.
    BestEffort,
    /// Keyword scan. File-level CCN only; never enters the function histogram.
    Heuristic,
}

impl CcnTier {
    pub fn as_str(self) -> &'static str {
        match self {
            CcnTier::Exact => "exact",
            CcnTier::BestEffort => "best-effort",
            CcnTier::Heuristic => "heuristic",
        }
    }
}

/// Languages with a tier-1 tree-sitter grammar wired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Rust,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
}

impl Lang {
    pub const ALL: [Lang; 10] = [
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::Python,
        Lang::Rust,
        Lang::Go,
        Lang::Java,
        Lang::C,
        Lang::Cpp,
        Lang::CSharp,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Lang::JavaScript => "JavaScript",
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::Python => "Python",
            Lang::Rust => "Rust",
            Lang::Go => "Go",
            Lang::Java => "Java",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
        }
    }

    /// Map tokei's language name to a grammar.
    ///
    /// tokei calls `.tsx` files "TSX", which needs a different grammar from
    /// `.ts`; everything else is 1:1.
    pub fn from_tokei_name(name: &str) -> Option<Self> {
        match name {
            "JavaScript" => Some(Lang::JavaScript),
            "TypeScript" => Some(Lang::TypeScript),
            "TSX" => Some(Lang::Tsx),
            "Python" => Some(Lang::Python),
            "Rust" => Some(Lang::Rust),
            "Go" => Some(Lang::Go),
            "Java" => Some(Lang::Java),
            "C" => Some(Lang::C),
            "C++" => Some(Lang::Cpp),
            "C#" => Some(Lang::CSharp),
            _ => None,
        }
    }

    /// Extension is preferred over tokei's name because it distinguishes
    /// `.ts` from `.tsx` even when tokei's detection is overridden.
    ///
    /// `.h` is claimed by C: real-world C headers vastly outnumber C++ ones,
    /// and tokei reports both as "C Header"/"C++ Header", which we do not use.
    /// C++ headers should use `.hpp`/`.hh`/`.hxx`/`.c++`.
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.').map(|(_, e)| e)?.to_ascii_lowercase();
        match ext.as_str() {
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            "py" | "pyi" => Some(Lang::Python),
            "rs" => Some(Lang::Rust),
            "go" => Some(Lang::Go),
            "java" => Some(Lang::Java),
            "c" | "h" => Some(Lang::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "c++" => Some(Lang::Cpp),
            "cs" => Some(Lang::CSharp),
            _ => None,
        }
    }
}

/// One function/method/closure measured from the AST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCcn {
    /// Best-effort name. Anonymous callbacks become `(anonymous)`; the engine
    /// may recover a name from `const x = () => {}` / object properties.
    pub name: String,
    /// 1-based start line.
    pub line: usize,
    /// 1-based inclusive end line.
    pub end_line: usize,
    /// Physical lines covered, including blanks and comments.
    pub nloc: usize,
    /// `1 + decision_points`, minimum 1.
    pub ccn: u32,
    pub params: usize,
}

/// Raw engine output for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileCcn {
    pub tier: CcnTier,
    pub functions: Vec<FunctionCcn>,
}

/// Per-file CCN roll-up. This is what lands in `FileLoc.ccn` and therefore in
/// the JSON report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcnSummary {
    pub tier: CcnTier,
    /// Only set for `heuristic` files, where there is no function breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ccn: Option<u32>,
    pub functions: usize,
    pub sum: u32,
    pub max: u32,
    /// Requires >= 5 functions; 0 otherwise (the caller decides how to display).
    pub p90: u32,
    pub over_10: usize,
    pub over_20: usize,
    pub over_50: usize,
    /// Every function, ascending by line. `--detail` gates the *report*, not
    /// the JSON contract.
    pub detail: Vec<FunctionCcn>,
}

impl CcnSummary {
    /// Roll up AST output.
    ///
    /// `sum`/`max` describe functions only; a file with no functions gets
    /// zeros, not `1` — an empty file has no routine to be simple.
    pub fn from_file(file: FileCcn) -> Self {
        let mut ccn: Vec<usize> = file.functions.iter().map(|f| f.ccn as usize).collect();
        let sum: u32 = file.functions.iter().map(|f| f.ccn).sum();
        let max = file.functions.iter().map(|f| f.ccn).max().unwrap_or(0);
        ccn.sort_unstable();
        let p90 = if file.functions.len() >= 5 {
            percentile(&ccn, 90.0) as u32
        } else {
            0
        };

        Self {
            tier: file.tier,
            file_ccn: None,
            functions: file.functions.len(),
            sum,
            max,
            p90,
            over_10: file.functions.iter().filter(|f| f.ccn > 10).count(),
            over_20: file.functions.iter().filter(|f| f.ccn > 20).count(),
            over_50: file.functions.iter().filter(|f| f.ccn > 50).count(),
            detail: file.functions,
        }
    }

    /// Heuristic output: one number for the whole file.
    ///
    /// `functions` stays 0 so this can never be mistaken for a measured
    /// per-function distribution.
    pub fn heuristic(file_ccn: u32) -> Self {
        Self {
            tier: CcnTier::Heuristic,
            file_ccn: Some(file_ccn.max(1)),
            functions: 0,
            sum: 0,
            max: 0,
            p90: 0,
            over_10: 0,
            over_20: 0,
            over_50: 0,
            detail: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, ccn: u32) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            line: 1,
            end_line: 1,
            nloc: 1,
            ccn,
            params: 0,
        }
    }

    #[test]
    fn summary_counts_thresholds_strictly_greater_than() {
        let s = CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions: vec![f("a", 1), f("b", 10), f("c", 11), f("d", 21), f("e", 51)],
        });
        assert_eq!(s.functions, 5);
        assert_eq!(s.sum, 94);
        assert_eq!(s.max, 51);
        assert_eq!(s.over_10, 3, "11, 21 and 51 are over 10; 10 is not");
        assert_eq!(s.over_20, 2);
        assert_eq!(s.over_50, 1);
        assert_eq!(s.p90, 51);
        assert_eq!(s.file_ccn, None);
    }

    #[test]
    fn p90_needs_five_functions() {
        let s = CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions: vec![f("a", 1), f("b", 2), f("c", 3), f("d", 4)],
        });
        assert_eq!(s.p90, 0);
    }

    #[test]
    fn empty_file_has_no_functions_and_no_complexity() {
        let s = CcnSummary::from_file(FileCcn {
            tier: CcnTier::Exact,
            functions: vec![],
        });
        assert_eq!(s.functions, 0);
        assert_eq!(s.sum, 0);
        assert_eq!(s.max, 0);
    }

    #[test]
    fn heuristic_never_reports_function_stats() {
        let s = CcnSummary::heuristic(0);
        assert_eq!(s.tier, CcnTier::Heuristic);
        assert_eq!(s.file_ccn, Some(1), "CCN is never below 1");
        assert_eq!(s.functions, 0);
        assert_eq!(s.sum, 0);
    }

    #[test]
    fn extensions_map_to_grammars() {
        assert_eq!(Lang::from_path("a/b.ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_path("a/b.tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_path("a/b.mjs"), Some(Lang::JavaScript));
        assert_eq!(Lang::from_path("a/b.pyi"), Some(Lang::Python));
        assert_eq!(Lang::from_path("a/b.rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_path("a/b.go"), Some(Lang::Go));
        assert_eq!(Lang::from_path("a/b.java"), Some(Lang::Java));
        assert_eq!(Lang::from_path("a/b.c"), Some(Lang::C));
        assert_eq!(Lang::from_path("a/b.h"), Some(Lang::C));
        assert_eq!(Lang::from_path("a/b.cc"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.cpp"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.cxx"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.c++"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.hpp"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.hh"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/b.hxx"), Some(Lang::Cpp));
        assert_eq!(Lang::from_path("a/B.CS"), Some(Lang::CSharp));
        assert_eq!(Lang::from_path("a/b.cs"), Some(Lang::CSharp));
        assert_eq!(Lang::from_path("a/b.kt"), None);
    }

    #[test]
    fn tokei_names_map_to_grammars() {
        assert_eq!(Lang::from_tokei_name("TSX"), Some(Lang::Tsx));
        assert_eq!(Lang::from_tokei_name("JavaScript"), Some(Lang::JavaScript));
        assert_eq!(Lang::from_tokei_name("Java"), Some(Lang::Java));
        assert_eq!(Lang::from_tokei_name("C"), Some(Lang::C));
        assert_eq!(Lang::from_tokei_name("C++"), Some(Lang::Cpp));
        assert_eq!(Lang::from_tokei_name("C#"), Some(Lang::CSharp));
        assert_eq!(Lang::from_tokei_name("Kotlin"), None);
    }

    #[test]
    fn all_lists_every_language_once_with_a_unique_name() {
        assert_eq!(Lang::ALL.len(), 10);
        let mut names: Vec<&str> = Lang::ALL.iter().map(|l| l.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Lang::ALL.len(), "names must be unique");
        for (i, a) in Lang::ALL.iter().enumerate() {
            for b in &Lang::ALL[i + 1..] {
                assert_ne!(a, b, "ALL must not repeat a language");
            }
        }
    }

    /// Every language must be reachable from both entry points, or a scanner
    /// would silently drop real files for it.
    #[test]
    fn every_language_is_reachable_from_path_and_tokei_name() {
        let probes = [
            (Lang::JavaScript, "a.js"),
            (Lang::TypeScript, "a.ts"),
            (Lang::Tsx, "a.tsx"),
            (Lang::Python, "a.py"),
            (Lang::Rust, "a.rs"),
            (Lang::Go, "a.go"),
            (Lang::Java, "a.java"),
            (Lang::C, "a.c"),
            (Lang::Cpp, "a.cpp"),
            (Lang::CSharp, "a.cs"),
        ];
        assert_eq!(probes.len(), Lang::ALL.len(), "table must cover ALL");
        for (lang, path) in probes {
            assert_eq!(Lang::from_path(path), Some(lang));
            assert_eq!(Lang::from_tokei_name(lang.name()), Some(lang));
        }
    }
}
