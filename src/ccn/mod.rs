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
/// 2. a wired grammar + clean parse -> `Exact`,
/// 3. a wired grammar + parse errors but recovered functions -> `BestEffort`
///    (the functions tree-sitter found are reported, not thrown away),
/// 4. a compatible grammar (C <-> C++) if the first one recovers nothing,
/// 5. anything else -> keyword heuristic, `Heuristic`.
pub fn analyze_file(path: &Path, source: &[u8], tokei_lang: &str) -> CcnSummary {
    let lang =
        Lang::from_path(&path.to_string_lossy()).or_else(|| Lang::from_tokei_name(tokei_lang));

    if let Some(lang) = lang {
        if let Ok(file) = engine::analyze_file(path, source, lang) {
            return CcnSummary::from_file(file);
        }
        // A `.h` header is commonly C++ (templates, classes, namespaces) even
        // when the extension looks like C. Retry with the sibling grammar
        // instead of degrading the whole file to a keyword count. Only a tree
        // that recovered no function at all reaches this point: a partial parse
        // is `BestEffort` and returns early, so its functions are never lost.
        for alternative in fallbacks(lang) {
            if let Ok(file) = engine::analyze_file(path, source, *alternative) {
                return CcnSummary::from_file(file);
            }
        }
    }

    CcnSummary::heuristic(heuristic::analyze_file(source, tokei_lang))
}

/// Grammars that can plausibly parse the same file.
fn fallbacks(lang: Lang) -> &'static [Lang] {
    match lang {
        Lang::C => &[Lang::Cpp],
        Lang::Cpp => &[Lang::C],
        _ => &[],
    }
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
    fn a_cpp_header_with_a_h_extension_is_parsed_as_cpp() {
        // `.h` maps to C, but this is C++: the C grammar rejects it and the
        // dispatcher must retry with C++ rather than fall back to a heuristic.
        let source = br#"
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}
    T get() const { return value_; }
private:
    T value_;
};
"#;
        let summary = analyze_file(Path::new("include/box.h"), source, "C++");
        assert_eq!(summary.tier, CcnTier::Exact);
        assert!(summary.functions >= 1, "box.h should expose a function");
    }

    #[test]
    fn a_plain_c_header_still_parses_as_c() {
        let source = b"int add(int a, int b);\n";
        let summary = analyze_file(Path::new("add.h"), source, "C");
        assert_eq!(summary.tier, CcnTier::Exact);
    }

    #[test]
    fn unparseable_known_language_degrades_to_heuristic() {
        // Unbalanced braces: tree-sitter reports ERROR nodes.
        let s = analyze_file(Path::new("a.rs"), b"fn main( {", "Rust");
        assert_eq!(s.tier, CcnTier::Heuristic);
    }
}
