//! tree-sitter CCN engine.
//!
//! Contract:
//! - `analyze_file` is called from a rayon worker pool; it must be `Send + Sync`
//!   callable and must not share mutable state between calls.
//! - `ccn == 1 + decision_points`, minimum 1, per function.
//! - Nested functions are measured separately and **excluded** from the
//!   enclosing function's count (otherwise a single factory function swallows
//!   all its callbacks and the histogram becomes a lie).
//! - A parse failure returns `Err`; the caller degrades that file to the
//!   heuristic tier. It must never panic on malformed input.
//!
//! The node tables live in `rules.rs`; do not hard-code node kinds here.

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Language, Node, Parser, Tree};

use crate::ccn::rules::{self, LanguageRules};
use crate::ccn::types::{FileCcn, FunctionCcn, Lang};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcnError {
    /// No grammar is wired for this language (caller should use the heuristic).
    NoGrammar(Lang),
    /// tree-sitter produced a tree containing `ERROR`/`MISSING` nodes.
    ParseFailed { lang: Lang, detail: String },
}

impl std::fmt::Display for CcnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CcnError::NoGrammar(l) => write!(f, "no CCN grammar for {}", l.name()),
            CcnError::ParseFailed { lang, detail } => {
                write!(f, "parse failed for {}: {detail}", lang.name())
            }
        }
    }
}

impl std::error::Error for CcnError {}

/// Name used for callbacks that cannot be tied to a declaration.
const ANONYMOUS: &str = "(anonymous)";

/// Immediate-parent kinds that can carry an anonymous function's name.
const NAMING_PARENTS: &[&str] = &[
    "variable_declarator",
    "pair",
    "property_definition",
    "assignment_expression",
    "field_definition",
];

/// Fields to try, in order, when recovering a name from `NAMING_PARENTS`.
///
/// The contract says "the parent's `name` field", but the JS/TS grammars do not
/// give `pair` or `assignment_expression` a `name` field: object properties and
/// assignments name their function through `key`/`left`. Trying those too keeps
/// `{ handler: function () {} }` from degrading to `(anonymous)`.
const NAMING_FIELDS: &[&str] = &["name", "left", "key"];

/// Compiled grammar for `lang`, if one is wired.
fn grammar(lang: Lang) -> Option<Language> {
    match lang {
        Lang::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Lang::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Lang::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Lang::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Lang::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Lang::Go => Some(tree_sitter_go::LANGUAGE.into()),
        Lang::Java => Some(tree_sitter_java::LANGUAGE.into()),
        Lang::C => Some(tree_sitter_c::LANGUAGE.into()),
        Lang::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
        Lang::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
    }
}

thread_local! {
    /// One `Parser` per language per thread. `Parser` is not `Sync`, and
    /// rebuilding it for every one of ~6k files is the difference between a
    /// scan that finishes and one that does not. `thread_local` keeps rayon
    /// workers from sharing a mutable parser; nothing here is `unsafe`.
    static PARSERS: RefCell<HashMap<Lang, Parser>> = RefCell::new(HashMap::new());
}

/// Run `f` with this thread's cached parser for `lang`.
///
/// `None` means the language has no grammar or its grammar failed to load.
fn with_parser<R>(lang: Lang, f: impl FnOnce(&mut Parser) -> R) -> Option<R> {
    let language = grammar(lang)?;
    PARSERS.with(|slot| {
        let mut parsers = slot.borrow_mut();
        match parsers.entry(lang) {
            Entry::Occupied(entry) => Some(f(entry.into_mut())),
            Entry::Vacant(entry) => {
                let mut parser = Parser::new();
                if parser.set_language(&language).is_err() {
                    return None;
                }
                Some(f(entry.insert(parser)))
            }
        }
    })
}

/// Parse `source` and return every function with its CCN.
pub fn analyze_file(_path: &Path, source: &[u8], lang: Lang) -> Result<FileCcn, CcnError> {
    let table = rules::rules(lang);
    let tier = rules::tier(lang);

    // Whitespace-only input still parses, but skipping it keeps the empty-file
    // path free of grammar quirks (notably Python indentation errors).
    if source.iter().all(u8::is_ascii_whitespace) {
        return Ok(FileCcn {
            tier,
            functions: Vec::new(),
        });
    }

    let parsed =
        with_parser(lang, |parser| parser.parse(source, None)).ok_or(CcnError::NoGrammar(lang))?;
    let tree: Tree = parsed.ok_or_else(|| CcnError::ParseFailed {
        lang,
        detail: "parser produced no tree".to_string(),
    })?;

    let root = tree.root_node();
    if root.has_error() {
        return Err(CcnError::ParseFailed {
            lang,
            detail: first_syntax_error(root),
        });
    }

    let mut functions: Vec<FunctionCcn> = collect_function_nodes(root, table)
        .into_iter()
        .map(|node| measure(node, table, source))
        .collect();

    // Outer before inner when both start on the same line.
    functions.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| b.end_line.cmp(&a.end_line))
    });

    Ok(FileCcn { tier, functions })
}

/// All nodes whose kind opens a function scope, in no particular order.
fn collect_function_nodes<'tree>(root: Node<'tree>, table: &LanguageRules) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // `is_named` matters: Python's anonymous `lambda` *keyword token* has
        // kind `"lambda"` too, so a bare kind match turns every lambda into two
        // functions (one of them a zero-width phantom).
        if node.is_named() && table.function_nodes.contains(&node.kind()) {
            found.push(node);
        }
        // Descend into functions too: they can contain nested ones, which are
        // measured separately.
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    found
}

fn measure(node: Node<'_>, table: &LanguageRules, source: &[u8]) -> FunctionCcn {
    let start = node.start_position();
    let end = node.end_position();
    let name = function_name(node, table, source);

    FunctionCcn {
        name,
        line: start.row + 1,
        end_line: end.row + 1,
        nloc: end.row - start.row + 1,
        ccn: count_decisions(node, table, source).saturating_add(1),
        params: param_count(node, table),
    }
}

/// Decisions in `func`'s subtree, excluding nested functions.
fn count_decisions<'tree>(func: Node<'tree>, table: &LanguageRules, source: &[u8]) -> u32 {
    let mut decisions = 0u32;
    let mut stack: Vec<Node<'tree>> = Vec::new();
    let mut cursor = func.walk();
    stack.extend(func.children(&mut cursor));

    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node.is_named() && table.function_nodes.contains(&kind) {
            // A nested function's decisions belong to that function.
            continue;
        }
        if table.decision_nodes.contains(&kind) {
            decisions += 1;
        } else if kind == "binary_expression" {
            if let Some(operator) = node.child_by_field_name("operator") {
                if let Ok(text) = operator.utf8_text(source) {
                    if table.decision_operators.contains(&text) {
                        decisions += 1;
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }

    decisions
}

fn function_name(node: Node<'_>, table: &LanguageRules, source: &[u8]) -> String {
    if let Some(name) = node.child_by_field_name(table.name_field) {
        if let Some(text) = field_text(name, source) {
            return text;
        }
    }

    // C/C++: the name is the innermost `declarator`. A declarator that still
    // carries a `parameters` field is a lambda's abstract declarator, not a
    // name, so lambdas stay `(anonymous)`.
    if let Some(decl) = innermost_declarator(node, table.declarator_field) {
        if decl.child_by_field_name("parameters").is_none() {
            if let Some(text) = field_text(decl, source) {
                return text;
            }
        }
    }

    if let Some(parent) = node.parent() {
        if NAMING_PARENTS.contains(&parent.kind()) {
            for field in NAMING_FIELDS {
                if let Some(name) = parent.child_by_field_name(field) {
                    if let Some(text) = field_text(name, source) {
                        return text;
                    }
                }
            }
        }
    }

    ANONYMOUS.to_string()
}

/// Walk `node`'s `declarator` chain to its innermost declarator.
///
/// `None` when the language has no declarator field or the node has none.
/// The depth cap is belt-and-braces: a malformed tree must not hang the scan.
fn innermost_declarator<'tree>(node: Node<'tree>, field: &str) -> Option<Node<'tree>> {
    if field.is_empty() {
        return None;
    }
    let mut current = node.child_by_field_name(field)?;
    for _ in 0..16 {
        match current.child_by_field_name("declarator") {
            Some(inner) => current = inner,
            None => break,
        }
    }
    Some(current)
}

/// Find the parameter list hidden in a C/C++ declarator chain.
fn declarator_params<'tree>(node: Node<'tree>, field: &str) -> Option<Node<'tree>> {
    if field.is_empty() {
        return None;
    }
    let mut current = node.child_by_field_name(field)?;
    for _ in 0..16 {
        if let Some(params) = current.child_by_field_name("parameters") {
            return Some(params);
        }
        current = current.child_by_field_name("declarator")?;
    }
    None
}

fn field_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn param_count(node: Node<'_>, table: &LanguageRules) -> usize {
    for field in table.params_fields {
        if let Some(params) = node.child_by_field_name(field) {
            return count_params(params);
        }
    }
    // C/C++: parameters live under the declarator, not on the function node.
    if let Some(params) = declarator_params(node, table.declarator_field) {
        return count_params(params);
    }
    0
}

fn count_params(params: Node<'_>) -> usize {
    // A single unparenthesized parameter is a bare `identifier` (Java
    // `x -> ...`) or `implicit_parameter` (C# `x => ...`), not a list.
    if params.kind() == "identifier" || params.kind() == "implicit_parameter" {
        return 1;
    }
    let mut cursor = params.walk();
    params
        .named_children(&mut cursor)
        .filter(|child| !child.kind().contains("comment"))
        .count()
}

/// Describe the first `ERROR`/`MISSING` node in document order, 1-based.
fn first_syntax_error(root: Node<'_>) -> String {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            let pos = node.start_position();
            return format!("{} at {}:{}", node.kind(), pos.row + 1, pos.column + 1);
        }
        let mut cursor = node.walk();
        // Reversed so the stack pops the first child first.
        stack.extend(
            node.children(&mut cursor)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
    }
    "unknown syntax error".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccn::types::CcnTier;

    fn analyze(src: &str, lang: Lang) -> FileCcn {
        analyze_file(Path::new("fixture"), src.as_bytes(), lang)
            .unwrap_or_else(|e| panic!("{} fixture must parse: {e}", lang.name()))
    }

    fn function_named<'a>(file: &'a FileCcn, name: &str) -> &'a FunctionCcn {
        file.functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no function named {name:?}; found {:?}",
                    file.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
                )
            })
    }

    fn only(src: &str, lang: Lang) -> FunctionCcn {
        let file = analyze(src, lang);
        assert_eq!(file.tier, CcnTier::Exact);
        assert_eq!(file.functions.len(), 1, "expected exactly one function");
        file.functions.into_iter().next().expect("one function")
    }

    fn ccn_of(src: &str, lang: Lang, name: &str) -> u32 {
        function_named(&analyze(src, lang), name).ccn
    }

    // -- JavaScript -------------------------------------------------------

    #[test]
    fn js_trivial_arrow_is_one() {
        let f = only("const f = (a) => a + 1;", Lang::JavaScript);
        assert_eq!(f.name, "f", "name recovered from variable_declarator");
        assert_eq!(f.ccn, 1);
        assert_eq!(f.params, 1);
    }

    #[test]
    fn js_if_statement_is_two() {
        let f = only(
            "function g(x) { if (x) { return 1; } return 2; }",
            Lang::JavaScript,
        );
        assert_eq!(f.name, "g");
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn js_compound_predicates_count_once_each() {
        assert_eq!(
            ccn_of("const f = (a, b) => a && b;", Lang::JavaScript, "f"),
            2
        );
        assert_eq!(
            ccn_of("const f = (a, b) => a || b;", Lang::JavaScript, "f"),
            2
        );
        assert_eq!(
            ccn_of("const f = (a, b) => a ?? b;", Lang::JavaScript, "f"),
            2
        );
        // Compound predicate: `&&` and `||` are two decisions.
        assert_eq!(
            ccn_of("const f = (a, b, c) => a && b || c;", Lang::JavaScript, "f"),
            3
        );
    }

    #[test]
    fn js_ternary_is_two() {
        assert_eq!(
            ccn_of("const f = (x) => (x ? 1 : 2);", Lang::JavaScript, "f"),
            2
        );
    }

    #[test]
    fn js_switch_counts_cases_not_the_switch() {
        let f = only(
            "function g(x) { switch (x) { case 1: return 1; case 2: return 2; } }",
            Lang::JavaScript,
        );
        assert_eq!(f.ccn, 3, "1 + two switch_case nodes");
    }

    #[test]
    fn js_try_catch_is_two() {
        let f = only(
            "function g() { try { risky(); } catch (e) { recover(e); } }",
            Lang::JavaScript,
        );
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn js_anonymous_callback_is_found_and_named() {
        let file = analyze(
            "const xs = [1].map(function (x) { return x; });",
            Lang::JavaScript,
        );
        assert_eq!(file.functions.len(), 1);
        assert_eq!(file.functions[0].name, "(anonymous)");
        assert_eq!(file.functions[0].ccn, 1);
        assert_eq!(file.functions[0].params, 1);
    }

    #[test]
    fn js_object_property_function_takes_the_key_as_name() {
        let file = analyze(
            "const handlers = { onClick: function (e) { return e; } };",
            Lang::JavaScript,
        );
        assert_eq!(file.functions.len(), 1);
        assert_eq!(file.functions[0].name, "onClick");
    }

    #[test]
    fn js_records_span_lines_and_params() {
        let f = only(
            "function g(a, b) {\n  if (a) {\n    return 1;\n  }\n  return 2;\n}\n",
            Lang::JavaScript,
        );
        assert_eq!(f.line, 1);
        assert_eq!(f.end_line, 6);
        assert_eq!(f.nloc, 6);
        assert_eq!(f.params, 2);
        assert_eq!(f.ccn, 2);
    }

    // -- TSX --------------------------------------------------------------

    #[test]
    fn tsx_jsx_conditional_and_counts() {
        let f = only("const A = () => <div>{cond && <span />}</div>;", Lang::Tsx);
        assert_eq!(f.name, "A");
        assert_eq!(f.ccn, 2, "the `&&` in the JSX expression is a decision");
    }

    // -- Python -----------------------------------------------------------

    #[test]
    fn python_if_is_two() {
        let f = only("def f(x):\n    if x:\n        return 1\n", Lang::Python);
        assert_eq!(f.name, "f");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn python_elif_chain_counts_each_branch() {
        let f = only(
            "def f(x):\n    if x == 0:\n        return 0\n    elif x == 1:\n        return 1\n    elif x == 2:\n        return 2\n    return 3\n",
            Lang::Python,
        );
        assert_eq!(f.ccn, 4, "1 + if + two elif clauses");
    }

    #[test]
    fn python_comprehension_counts_loop_and_filter() {
        let f = only(
            "def f(xs):\n    return [x for x in xs if x]\n",
            Lang::Python,
        );
        assert_eq!(f.ccn, 3, "1 + for_in_clause + if_clause");
    }

    #[test]
    fn python_boolean_operator_counts() {
        let f = only("def f(a, b):\n    return a and b\n", Lang::Python);
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn python_lambda_keyword_token_is_not_a_second_function() {
        // The `lambda` keyword token is an anonymous node of kind `"lambda"`,
        // the same kind as the expression; only the expression is a function.
        let file = analyze(
            "def f(values):\n    return list(map(lambda v: v * 2, values))\n",
            Lang::Python,
        );
        assert_eq!(file.functions.len(), 2, "f plus exactly one lambda");
        let lambda = file
            .functions
            .iter()
            .find(|f| f.name == "(anonymous)")
            .expect("lambda is anonymous");
        assert_eq!(lambda.ccn, 1);
        assert_eq!(lambda.nloc, 1);
        assert_eq!(lambda.params, 1);
    }

    // -- Rust -------------------------------------------------------------

    #[test]
    fn rust_if_else_is_two() {
        let f = only(
            "fn f(x: i32) -> i32 { if x > 0 { 1 } else { 2 } }",
            Lang::Rust,
        );
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn rust_match_counts_every_arm() {
        let f = only(
            "fn f(x: i32) -> i32 { match x { 0 => 1, 1 => 2, _ => 3 } }",
            Lang::Rust,
        );
        assert_eq!(f.ccn, 4, "1 + three match_arm nodes");
    }

    #[test]
    fn rust_if_let_is_two() {
        let f = only(
            "fn f(y: Option<i32>) { if let Some(x) = y { let _ = x; } }",
            Lang::Rust,
        );
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn rust_closure_is_its_own_function() {
        let file = analyze(
            "fn main() {\n    let f = |x: i32| if x > 0 { 1 } else { 2 };\n    let _ = f;\n}\n",
            Lang::Rust,
        );
        assert_eq!(file.functions.len(), 2);
        let main = function_named(&file, "main");
        assert_eq!(main.ccn, 1, "the closure's `if` belongs to the closure");
        let closure = file
            .functions
            .iter()
            .find(|f| f.name == "(anonymous)")
            .expect("closure is anonymous");
        assert_eq!(closure.ccn, 2);
        assert_eq!(closure.line, 2);
    }

    #[test]
    fn rust_logical_operators_count() {
        let f = only("fn f(a: bool, b: bool) -> bool { a && b }", Lang::Rust);
        assert_eq!(f.ccn, 2);
    }

    // -- Go ---------------------------------------------------------------

    #[test]
    fn go_if_is_two() {
        let f = only(
            "package p\n\nfunc f(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t}\n\treturn 2\n}\n",
            Lang::Go,
        );
        assert_eq!(f.name, "f");
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn go_switch_counts_expression_cases() {
        let f = only(
            "package p\n\nfunc f(x int) int {\n\tswitch x {\n\tcase 1:\n\t\treturn 1\n\tcase 2:\n\t\treturn 2\n\t}\n\treturn 0\n}\n",
            Lang::Go,
        );
        assert_eq!(f.ccn, 3);
    }

    #[test]
    fn go_select_counts_communication_cases() {
        let f = only(
            "package p\n\nfunc f(a, b chan int) {\n\tselect {\n\tcase <-a:\n\tcase <-b:\n\t}\n}\n",
            Lang::Go,
        );
        assert_eq!(f.ccn, 3);
    }

    #[test]
    fn go_for_is_two() {
        let f = only(
            "package p\n\nfunc f(n int) {\n\tfor i := 0; i < n; i++ {\n\t}\n}\n",
            Lang::Go,
        );
        assert_eq!(f.ccn, 2);
    }

    // -- Nested functions -------------------------------------------------

    #[test]
    fn nested_function_is_excluded_from_enclosing_count() {
        let file = analyze(
            "function outer() {\n  function inner() {\n    if (a) { if (b) {} }\n  }\n  if (c) {}\n}\n",
            Lang::JavaScript,
        );
        assert_eq!(file.functions.len(), 2);
        assert_eq!(function_named(&file, "outer").ccn, 2);
        assert_eq!(function_named(&file, "inner").ccn, 3);
    }

    #[test]
    fn outer_function_sorts_before_inner_on_same_line() {
        let file = analyze(
            "function outer(){ function inner(){ if (a) {} } }",
            Lang::JavaScript,
        );
        assert_eq!(file.functions.len(), 2);
        assert_eq!(
            file.functions
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["outer", "inner"],
            "same start line: larger span first"
        );
    }

    // -- Java -------------------------------------------------------------

    #[test]
    fn java_trivial_method_is_one() {
        let f = only("class A { int f(int x) { return x; } }", Lang::Java);
        assert_eq!(f.name, "f");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 1);
    }

    #[test]
    fn java_if_else_is_two() {
        assert_eq!(
            ccn_of(
                "class A { int f(int x) { if (x > 0) { return 1; } else { return 2; } } }",
                Lang::Java,
                "f"
            ),
            2
        );
    }

    #[test]
    fn java_compound_predicate_is_two() {
        assert_eq!(
            ccn_of(
                "class A { boolean f(boolean a, boolean b) { return a && b; } }",
                Lang::Java,
                "f"
            ),
            2
        );
        assert_eq!(
            ccn_of(
                "class A { boolean f(boolean a, boolean b) { return a || b; } }",
                Lang::Java,
                "f"
            ),
            2
        );
    }

    #[test]
    fn java_switch_counts_every_label_including_default() {
        // Three `switch_label` nodes (two `case` + one `default`).
        let f = only(
            "class A { int f(int x) { switch (x) { case 1: return 1; case 2: return 2; default: return 3; } } }",
            Lang::Java,
        );
        assert_eq!(f.ccn, 4);
    }

    #[test]
    fn java_arrow_switch_and_switch_expression_count_labels() {
        // Arrow-form clauses are `switch_rule`s that still contain one
        // `switch_label` each, so the count is identical to `case X:`.
        assert_eq!(
            ccn_of(
                "class A { int f(int x) { switch (x) { case 1 -> { return 1; } case 2 -> { return 2; } default -> { return 3; } } } }",
                Lang::Java,
                "f"
            ),
            4
        );
        // A switch *expression* is a different parent but the same labels.
        assert_eq!(
            ccn_of(
                "class A { int f(int x) { return switch (x) { case 1 -> 1; case 2 -> 2; default -> 3; }; } }",
                Lang::Java,
                "f"
            ),
            4
        );
    }

    #[test]
    fn java_try_catch_is_two() {
        assert_eq!(
            ccn_of(
                "class A { void f() { try { g(); } catch (RuntimeException e) { } } }",
                Lang::Java,
                "f"
            ),
            2
        );
    }

    #[test]
    fn java_lambda_is_a_separate_function_with_its_ternary() {
        let file = analyze(
            "class A { void f() { java.util.function.IntUnaryOperator op = x -> x > 0 ? 1 : 2; } }",
            Lang::Java,
        );
        assert_eq!(file.functions.len(), 2, "f plus the lambda");
        assert_eq!(function_named(&file, "f").ccn, 1);
        let lambda = file
            .functions
            .iter()
            .find(|f| f.line == 1 && f.name != "f")
            .expect("lambda present");
        assert_eq!(lambda.ccn, 2, "the ternary belongs to the lambda");
        assert_eq!(lambda.params, 1, "bare `x` is one parameter");
    }

    #[test]
    fn java_constructor_is_its_own_function() {
        let f = only("class A { A(int x) { if (x > 0) { } } }", Lang::Java);
        assert_eq!(f.name, "A");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn java_enhanced_for_is_two() {
        assert_eq!(
            ccn_of(
                "class A { void f(int[] xs) { for (int x : xs) { g(x); } } }",
                Lang::Java,
                "f"
            ),
            2
        );
    }

    // -- C ----------------------------------------------------------------

    #[test]
    fn c_trivial_function_is_one() {
        let f = only("int f(int x) { return x; }", Lang::C);
        assert_eq!(f.name, "f", "name recovered from the declarator chain");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 1);
    }

    #[test]
    fn c_if_is_two() {
        assert_eq!(
            ccn_of(
                "int f(int x) { if (x > 0) { return 1; } return 2; }",
                Lang::C,
                "f"
            ),
            2
        );
    }

    #[test]
    fn c_switch_counts_case_statement_including_default() {
        // `case_statement` is the node for both `case X:` and `default:`, so
        // two cases plus `default` is three decisions.
        let with_default = only(
            "int f(int x) { switch (x) { case 1: return 1; case 2: return 2; default: return 3; } return 0; }",
            Lang::C,
        );
        assert_eq!(with_default.ccn, 4);

        let without_default = only(
            "int f(int x) { switch (x) { case 1: return 1; case 2: return 2; } return 0; }",
            Lang::C,
        );
        assert_eq!(without_default.ccn, 3);
    }

    #[test]
    fn c_ternary_is_two() {
        assert_eq!(
            ccn_of("int f(int x) { return x > 0 ? 1 : 2; }", Lang::C, "f"),
            2
        );
    }

    #[test]
    fn c_compound_predicate_is_two() {
        assert_eq!(
            ccn_of("int f(int a, int b) { return a && b; }", Lang::C, "f"),
            2
        );
    }

    // -- C++ --------------------------------------------------------------

    #[test]
    fn cpp_for_is_two() {
        assert_eq!(
            ccn_of(
                "int f(int n) { int s = 0; for (int i = 0; i < n; i++) { s += i; } return s; }",
                Lang::Cpp,
                "f"
            ),
            2
        );
    }

    #[test]
    fn cpp_range_for_is_two() {
        // `for_range_loop` is a different node kind from `for_statement`.
        assert_eq!(
            ccn_of(
                "int f() { int xs[3] = {1, 2, 3}; int s = 0; for (int x : xs) { s += x; } return s; }",
                Lang::Cpp,
                "f"
            ),
            2
        );
    }

    #[test]
    fn cpp_catch_is_two() {
        assert_eq!(
            ccn_of("void f() { try { g(); } catch (...) { } }", Lang::Cpp, "f"),
            2
        );
    }

    #[test]
    fn cpp_qualified_method_keeps_its_scope_in_the_name() {
        // `void A::b()`: the innermost declarator is a `qualified_identifier`,
        // so the reported name is the qualified spelling, not a bare `b`.
        let f = only("void A::b(int x) { if (x) { } }", Lang::Cpp);
        assert_eq!(f.name, "A::b");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn cpp_constructor_definition_is_named_through_its_qualifier() {
        let f = only("A::A(int x) { if (x > 0) { } }", Lang::Cpp);
        assert_eq!(f.name, "A::A");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 2);
    }

    #[test]
    fn cpp_nested_lambda_is_its_own_function() {
        let file = analyze(
            "int f() { auto g = [](int x) { if (x > 0) { return 1; } return 0; }; if (g(1)) { return 2; } return 3; }",
            Lang::Cpp,
        );
        assert_eq!(file.functions.len(), 2, "f plus the lambda");
        assert_eq!(
            function_named(&file, "f").ccn,
            2,
            "only the outer `if` counts; the lambda's `if` must not inflate it"
        );
        let lambda = file
            .functions
            .iter()
            .find(|f| f.name == "(anonymous)")
            .expect("lambda is anonymous");
        assert_eq!(lambda.ccn, 2);
        assert_eq!(lambda.params, 1);
        assert_eq!(lambda.line, 1);
    }

    // -- C# ---------------------------------------------------------------

    #[test]
    fn csharp_trivial_method_is_one() {
        let f = only("class A { int F(int x) { return x; } }", Lang::CSharp);
        assert_eq!(f.name, "F");
        assert_eq!(f.params, 1);
        assert_eq!(f.ccn, 1);
    }

    #[test]
    fn csharp_foreach_is_two() {
        assert_eq!(
            ccn_of(
                "class A { void F(int[] xs) { foreach (int x in xs) { G(x); } } }",
                Lang::CSharp,
                "F"
            ),
            2
        );
    }

    #[test]
    fn csharp_switch_counts_every_section() {
        // Two `switch_section` nodes -> 1 + 2.
        assert_eq!(
            ccn_of(
                "class A { int F(int x) { switch (x) { case 1: return 1; case 2: return 2; } return 0; } }",
                Lang::CSharp,
                "F"
            ),
            3
        );
        // `default` is part of the same `switch_section` kind, so it counts.
        assert_eq!(
            ccn_of(
                "class A { int F(int x) { switch (x) { case 1: return 1; case 2: return 2; default: return 3; } } }",
                Lang::CSharp,
                "F"
            ),
            4
        );
    }

    #[test]
    fn csharp_switch_expression_counts_every_arm() {
        // `switch_expression_arm` is a distinct kind; `_` counts like a case.
        assert_eq!(
            ccn_of(
                "class A { int F(int x) => x switch { 1 => 1, 2 => 2, _ => 0 }; }",
                Lang::CSharp,
                "F"
            ),
            4
        );
    }

    #[test]
    fn csharp_null_coalescing_is_two_but_conditional_access_is_not() {
        assert_eq!(
            ccn_of(
                "class A { string F(string a, string b) { return a ?? b; } }",
                Lang::CSharp,
                "F"
            ),
            2,
            "`??` is a two-way branch, like JS nullish coalescing"
        );
        assert_eq!(
            ccn_of(
                "class A { int F(string a) { return a?.Length ?? 0; } }",
                Lang::CSharp,
                "F"
            ),
            2,
            "only `??` counts; `?.` has no else-branch"
        );
    }

    #[test]
    fn csharp_lambda_is_a_separate_function() {
        let file = analyze(
            "class A { void F() { System.Func<int, int> g = x => { if (x > 0) { return 1; } return 2; }; } }",
            Lang::CSharp,
        );
        assert_eq!(file.functions.len(), 2, "F plus the lambda");
        assert_eq!(function_named(&file, "F").ccn, 1);
        let lambda = file
            .functions
            .iter()
            .find(|f| f.name != "F")
            .expect("lambda present");
        assert_eq!(lambda.ccn, 2);
        assert_eq!(lambda.params, 1, "`x` is an implicit_parameter");
    }

    #[test]
    fn csharp_local_function_is_its_own_function() {
        let file = analyze(
            "class A { void F() { int G(int x) { if (x > 0) { return 1; } return 0; } G(1); } }",
            Lang::CSharp,
        );
        assert_eq!(file.functions.len(), 2, "F plus the local function G");
        assert_eq!(function_named(&file, "F").ccn, 1, "G is a separate scope");
        assert_eq!(function_named(&file, "G").ccn, 2);
    }

    #[test]
    fn csharp_expression_bodied_method_counts_its_ternary() {
        assert_eq!(
            ccn_of(
                "class A { int F(int x) => x > 0 ? 1 : 2; }",
                Lang::CSharp,
                "F"
            ),
            2
        );
    }

    #[test]
    fn csharp_try_catch_is_two() {
        assert_eq!(
            ccn_of(
                "class A { void F() { try { G(); } catch (System.Exception e) { } } }",
                Lang::CSharp,
                "F"
            ),
            2
        );
    }

    // -- Degenerate input -------------------------------------------------

    #[test]
    fn empty_and_whitespace_sources_have_no_functions() {
        for src in ["", "   \n\t\n  "] {
            for lang in Lang::ALL {
                let file = analyze(src, lang);
                assert!(file.functions.is_empty(), "{}", lang.name());
            }
        }
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let err = analyze_file(Path::new("broken.js"), b"function f( {", Lang::JavaScript)
            .expect_err("unbalanced input must fail");
        assert!(
            matches!(
                err,
                CcnError::ParseFailed {
                    lang: Lang::JavaScript,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        let detail = match err {
            CcnError::ParseFailed { detail, .. } => detail,
            _ => unreachable!(),
        };
        assert!(
            detail.contains("ERROR") || detail.contains("MISSING"),
            "detail: {detail}"
        );

        assert!(matches!(
            analyze_file(Path::new("broken.rs"), b"fn main( {", Lang::Rust),
            Err(CcnError::ParseFailed { .. })
        ));
    }

    #[test]
    fn malformed_c_family_input_is_an_error_not_a_panic() {
        let cases = [
            (Lang::Java, "broken.java", "class A { void f( {"),
            (Lang::C, "broken.c", "int f( {"),
            (Lang::Cpp, "broken.cpp", "int f( {"),
            (Lang::CSharp, "broken.cs", "class A { void F( {"),
        ];
        for (lang, path, src) in cases {
            let err = analyze_file(Path::new(path), src.as_bytes(), lang)
                .expect_err("unbalanced input must fail");
            assert!(
                matches!(err, CcnError::ParseFailed { lang: got, .. } if got == lang),
                "{}: unexpected error {err:?}",
                lang.name()
            );
        }
    }

    // -- Every language ---------------------------------------------------

    /// A future `Lang` variant cannot land without a grammar arm and a table:
    /// this walks `Lang::ALL` and requires that each one parses a minimal
    /// program and finds exactly one function.
    #[test]
    fn every_language_has_a_grammar_and_a_table() {
        let samples = [
            (Lang::JavaScript, "function f() { return 1; }"),
            (Lang::TypeScript, "function f(): number { return 1; }"),
            (Lang::Tsx, "const A = () => <div />;"),
            (Lang::Python, "def f():\n    return 1\n"),
            (Lang::Rust, "fn f() -> i32 { 1 }"),
            (Lang::Go, "package p\n\nfunc f() int {\n\treturn 1\n}\n"),
            (Lang::Java, "class A { int f() { return 1; } }"),
            (Lang::C, "int f(void) { return 1; }"),
            (Lang::Cpp, "int f() { return 1; }"),
            (Lang::CSharp, "class A { int F() { return 1; } }"),
        ];
        assert_eq!(
            samples.len(),
            Lang::ALL.len(),
            "every Lang needs a sample here"
        );
        for lang in Lang::ALL {
            assert!(grammar(lang).is_some(), "{} has no grammar", lang.name());
            let (sample_lang, src) = samples
                .iter()
                .find(|(l, _)| *l == lang)
                .unwrap_or_else(|| panic!("{} has no sample", lang.name()));
            assert_eq!(*sample_lang, lang);
            let file = analyze(src, lang);
            assert_eq!(file.tier, CcnTier::Exact, "{}", lang.name());
            assert_eq!(
                file.functions.len(),
                1,
                "{}: expected one function, got {:?}",
                lang.name(),
                file.functions.iter().map(|f| &f.name).collect::<Vec<_>>()
            );
            assert_eq!(file.functions[0].ccn, 1, "{}", lang.name());
        }
        // And the rule tables agree that every language has a grammar.
        for lang in Lang::ALL {
            assert!(!rules::rules(lang).function_nodes.is_empty());
        }
    }
}
