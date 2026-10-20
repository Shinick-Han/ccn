//! CCN analysis: dispatcher, tree-sitter engine, keyword fallback.
//!
//! Degradation is per file, never per run. A single unparseable file (or a
//! language with no grammar) must not fail the scan or silently vanish; it
//! becomes `heuristic` and the report says so.

pub mod engine;
pub mod heuristic;
pub mod rules;
pub mod types;

use std::path::Path;

pub use types::{CcnSummary, CcnTier, FileCcn, FunctionCcn, Lang};

/// Analyse one file, choosing the best available engine.
///
/// Tier selection:
/// 1. extension wins over tokei's name (`.tsx` needs the TSX grammar),
/// 2. a wired grammar + successful parse -> `Exact`,
/// 3. anything else -> keyword heuristic, `Heuristic`.
pub fn analyze_file(path: &Path, source: &[u8], tokei_lang: &str) -> CcnSummary {
    let lang =
        Lang::from_path(&path.to_string_lossy()).or_else(|| Lang::from_tokei_name(tokei_lang));

    if let Some(lang) = lang {
        if let Ok(file) = engine::analyze_file(path, source, lang) {
            return CcnSummary::from_file(file);
        }
    }

    CcnSummary::heuristic(heuristic::analyze_file(source, tokei_lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_language_degrades_to_heuristic() {
        let s = analyze_file(Path::new("a.rb"), b"if x then y end", "Ruby");
        assert_eq!(s.tier, CcnTier::Heuristic);
        assert!(s.file_ccn.is_some());
        assert_eq!(s.functions, 0);
    }

    #[test]
    fn unparseable_known_language_degrades_to_heuristic() {
        // Unbalanced braces: tree-sitter reports ERROR nodes.
        let s = analyze_file(Path::new("a.rs"), b"fn main( {", "Rust");
        assert_eq!(s.tier, CcnTier::Heuristic);
    }
}
