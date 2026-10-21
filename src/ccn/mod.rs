//! CCN analysis: dispatcher, tree-sitter engine, keyword fallback.
//!
//! Degradation is per file, never per run. A single unparseable file (or a
//! language with no grammar) must not fail the scan or silently vanish; it
//! becomes `heuristic` and the report says so.

pub mod engine;
pub mod heuristic;
pub mod rules;
pub mod types;

use std::cmp::Ordering;
use std::path::Path;

pub use types::{CcnSummary, CcnTier, FileCcn, FunctionCcn, Lang};

/// Analyse one file, choosing the best available engine.
///
/// Tier selection:
/// 1. extension wins over tokei's name (`.tsx` needs the TSX grammar),
/// 2. a wired grammar + clean parse -> `Exact`,
/// 3. a wired grammar + parse errors but recovered functions -> `BestEffort`,
///    unless the sibling C/C++ grammar parses the same bytes better,
/// 4. a compatible grammar (C <-> C++) if the first one recovers nothing,
/// 5. anything else -> keyword heuristic, `Heuristic`.
pub fn analyze_file(path: &Path, source: &[u8], tokei_lang: &str) -> CcnSummary {
    let lang =
        Lang::from_path(&path.to_string_lossy()).or_else(|| Lang::from_tokei_name(tokei_lang));

    if let Some(lang) = lang {
        if let Some((_lang, file)) = select_parse(path, source, lang) {
            return CcnSummary::from_file(file);
        }
    }

    CcnSummary::heuristic(heuristic::analyze_file(source, tokei_lang))
}

/// One successful parse of `source`, tagged with the grammar that produced it.
struct Parsed {
    lang: Lang,
    file: FileCcn,
    had_error: bool,
}

/// Choose the grammar that measures this file best, returning its output and
/// the grammar that produced it.
///
/// A clean parse by the extension-selected grammar is authoritative and is
/// never compared against the sibling: re-parsing every clean `.c`/`.cpp` as
/// the other grammar is pure cost with no possible gain. Only a degraded tree
/// (or a parse that recovered nothing) is re-evaluated, because a `.h` full of
/// C++ can otherwise be measured as phantom C constructs.
fn select_parse(path: &Path, source: &[u8], lang: Lang) -> Option<(Lang, FileCcn)> {
    let primary = try_parse(path, source, lang);
    if primary.as_ref().is_some_and(|parsed| !parsed.had_error) {
        return primary.map(|parsed| (parsed.lang, parsed.file));
    }

    let sibling = fallbacks(lang)
        .iter()
        .find_map(|alternative| try_parse(path, source, *alternative));

    match (primary, sibling) {
        (Some(primary), Some(sibling)) => {
            let chosen = pick_better(primary, sibling, lang);
            Some((chosen.lang, chosen.file))
        }
        (Some(primary), None) => Some((primary.lang, primary.file)),
        (None, Some(sibling)) => Some((sibling.lang, sibling.file)),
        (None, None) => None,
    }
}

fn try_parse(path: &Path, source: &[u8], lang: Lang) -> Option<Parsed> {
    engine::analyze_file_diagnostic(path, source, lang)
        .ok()
        .map(|(file, had_error)| Parsed {
            lang,
            file,
            had_error,
        })
}

/// Pick between two results for the same bytes.
///
/// Order: fewer syntax errors (a clean tree beats a degraded one), then more
/// measured functions, then the grammar the extension implies. `preferred` is
/// the extension-selected grammar and is only consulted on a complete tie, so
/// a clean C++ parse still beats a degraded C one for a `.h` header.
fn pick_better(primary: Parsed, sibling: Parsed, preferred: Lang) -> Parsed {
    let score = |parsed: &Parsed| (!parsed.had_error, parsed.file.functions.len());
    match score(&primary).cmp(&score(&sibling)) {
        Ordering::Greater => primary,
        Ordering::Less => sibling,
        // Equal quality: keep the grammar the extension asked for. `primary`
        // is always that grammar today, but compare explicitly so a future
        // caller cannot silently reverse the preference.
        Ordering::Equal if primary.lang == preferred => primary,
        Ordering::Equal => sibling,
    }
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

    fn function_names(summary: &CcnSummary) -> Vec<&str> {
        summary.detail.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn a_cpp_header_keeps_const_overloads_distinct_through_the_dispatcher() {
        // `.h` prefers C, but the C++ parse wins and identities come from the
        // same engine path. This is the dispatcher-level guard for the P1-1
        // const/plain overload collision.
        let source = br#"
struct X {
    int f(int x) const { return x; }
    int f(int y) { return y; }
};
"#;
        let summary = analyze_file(Path::new("include/x.h"), source, "C++");
        assert_eq!(summary.tier, CcnTier::Exact);
        let mut identities: Vec<&str> =
            summary.detail.iter().map(|f| f.identity.as_str()).collect();
        identities.sort_unstable();
        assert_eq!(identities, vec!["X::f(int)", "X::f(int) const"]);
        let mut names = function_names(&summary);
        names.sort_unstable();
        assert_eq!(names, vec!["f", "f"]);
    }

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
        let (lang, _) = select_parse(Path::new("add.h"), source, Lang::C).expect("a parse");
        assert_eq!(lang, Lang::C);
    }

    /// The header from the P0-4 reproducer: C++ namespaces and an inline
    /// method. The C grammar recovers `N`/`X` as phantom functions; a clean
    /// C++ parse must win even though `.h` prefers C.
    const CPP_HEADER: &[u8] = br#"int cfun() { return 1; }
namespace N { class X { public: int method() { if(true) return 1; return 0; } }; }
"#;

    #[test]
    fn cpp_header_h_has_no_phantom_namespace_or_class_functions() {
        let summary = analyze_file(Path::new("x.h"), CPP_HEADER, "C");
        assert_eq!(summary.tier, CcnTier::Exact);
        assert_eq!(function_names(&summary), vec!["cfun", "method"]);
    }

    #[test]
    fn the_same_header_as_cpp_measures_the_same_functions() {
        let summary = analyze_file(Path::new("x.cpp"), CPP_HEADER, "C++");
        assert_eq!(summary.tier, CcnTier::Exact);
        assert_eq!(function_names(&summary), vec!["cfun", "method"]);
    }

    #[test]
    fn a_c_file_with_c_only_constructs_is_not_reparsed_as_cpp() {
        let source = br#"typedef struct point { int x; int y; } point;
enum color { RED, GREEN };
struct holder { point p; int (*cb)(void); };
int add(int a, int b) { return a + b; }
"#;
        let summary = analyze_file(Path::new("types.c"), source, "C");
        assert_eq!(summary.tier, CcnTier::Exact);
        assert_eq!(function_names(&summary), vec!["add"]);
        let (lang, _) = select_parse(Path::new("types.c"), source, Lang::C).expect("a parse");
        assert_eq!(lang, Lang::C, "a clean C parse is authoritative");
    }

    #[test]
    fn a_cpp_only_header_is_exact_with_the_method() {
        let source = br#"template <typename T>
class Box {
public:
    T get() const { return value_; }
private:
    T value_;
};
"#;
        let summary = analyze_file(Path::new("box.h"), source, "C");
        assert_eq!(summary.tier, CcnTier::Exact);
        assert_eq!(function_names(&summary), vec!["get"]);
        let (lang, _) = select_parse(Path::new("box.h"), source, Lang::C).expect("a parse");
        assert_eq!(lang, Lang::Cpp);
    }

    #[test]
    fn a_degraded_header_picks_the_grammar_with_more_functions() {
        // Both grammars report errors, but C discards the templated functions
        // while C++ still recovers every one of them.
        let source = br#"int c_ok() { return 1; }
template <typename T> T one(T x) { return x; }
template <typename T> T two(T x) { if (x) { return x; } return x; }
template <typename T> T three(T x) { return x; }
int truncated() {
"#;
        let summary = analyze_file(Path::new("mixed.h"), source, "C");
        assert_eq!(summary.tier, CcnTier::BestEffort);
        let names = function_names(&summary);
        assert!(
            names.contains(&"one")
                && names.contains(&"two")
                && names.contains(&"three")
                && names.contains(&"c_ok"),
            "found: {names:?}"
        );
        let (lang, _) = select_parse(Path::new("mixed.h"), source, Lang::C).expect("a parse");
        assert_eq!(lang, Lang::Cpp);
    }

    #[test]
    fn a_hpp_header_prefers_the_cpp_grammar() {
        let source = b"int add(int a, int b) { return a + b; }\n";
        let (lang, _) = select_parse(Path::new("add.hpp"), source, Lang::Cpp).expect("a parse");
        assert_eq!(lang, Lang::Cpp);

        // Degraded and equally sized: the extension preference decides.
        let degraded = b"int a() { return 1; }\nint truncated() {\n";
        let (lang, _) = select_parse(Path::new("add.hpp"), degraded, Lang::Cpp).expect("a parse");
        assert_eq!(lang, Lang::Cpp);
    }

    #[test]
    fn unparseable_known_language_degrades_to_heuristic() {
        // Unbalanced braces: tree-sitter reports ERROR nodes.
        let s = analyze_file(Path::new("a.rs"), b"fn main( {", "Rust");
        assert_eq!(s.tier, CcnTier::Heuristic);
    }
}
