//! Per-language decision-point tables.
//!
//! This file is the *specification* of what counts as `+1` CCN. It is data on
//! purpose: changing a number here changes every report, so the table is
//! versioned with the crate and copy-pasted into test fixtures.
//!
//! Rules follow the SonarQube per-language tables where they exist, because
//! that is the only public, per-language statement of what a "decision" is.
//! Documented divergences are listed next to each table.
//!
//! Invariant: `ccn = 1 + decisions`. Never count the function entry itself.

use crate::ccn::types::Lang;

#[derive(Debug)]
pub struct LanguageRules {
    /// Nodes that open a new function scope. Their subtrees are excluded from
    /// the enclosing function's count (and measured separately).
    pub function_nodes: &'static [&'static str],
    /// Nodes that add exactly one decision point wherever they appear.
    pub decision_nodes: &'static [&'static str],
    /// Binary operator spellings that add one decision point each. These are
    /// the "compound predicate" rule from McCabe: `a && b` is two branches.
    pub decision_operators: &'static [&'static str],
    /// Field holding the function's name, if the grammar has one.
    pub name_field: &'static str,
    /// C/C++ grammars put the name *and* the parameter list under a
    /// declarator chain (`function_definition -> function_declarator ->
    /// identifier`) instead of exposing them as direct fields. Empty for
    /// grammars that expose `name_field`/`params_fields` directly.
    pub declarator_field: &'static str,
    /// Fields that may hold the parameter list, most specific first
    /// (`arrow_function` uses `parameter` for `x => ...`).
    pub params_fields: &'static [&'static str],
}

/// JavaScript, TypeScript and TSX share one table.
///
/// Deliberate choices:
/// - `optional_chain` (`?.`) is **not** a decision: it is a null check with no
///   else-branch. Sonar's JS table agrees; scc's `!=` heuristic does not.
/// - `switch_default` is **not** counted; Sonar counts `case` only for JS.
/// - `??` **is** counted: it is a real two-way branch.
static JS_LIKE: LanguageRules = LanguageRules {
    function_nodes: &[
        "function_declaration",
        "generator_function_declaration",
        "function_expression",
        "generator_function",
        "arrow_function",
        "method_definition",
    ],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
        "switch_case",
        "catch_clause",
        "ternary_expression",
    ],
    decision_operators: &["&&", "||", "??"],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters", "parameter"],
};

/// Python.
///
/// - `elif_clause` is its own node and counts once; the enclosing
///   `if_statement` counts once. `if/elif/elif` is CCN 3, matching `ast`.
/// - Comprehension clauses (`for_in_clause`, `if_clause`) count: they are
///   loops and branches, even though they fit on one line.
/// - `and`/`or` are `boolean_operator` nodes; nested chains produce one node
///   per operator, which matches the compound-predicate rule.
static PYTHON: LanguageRules = LanguageRules {
    function_nodes: &["function_definition", "lambda"],
    decision_nodes: &[
        "if_statement",
        "elif_clause",
        "for_statement",
        "while_statement",
        "except_clause",
        "conditional_expression",
        "boolean_operator",
        "if_clause",
        "for_in_clause",
        "case_clause",
    ],
    decision_operators: &[],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters"],
};

/// Rust.
///
/// - `match_arm` counts once per arm, including `_ =>`. This is the
///   "switch case" rule; `if let` is an `if_expression` and needs no special
///   case. (lizard under-counts match arms; that is one reason it is only an
///   oracle.)
/// - `loop` counts because it is an unconditional jump target with a break
///   edge; Sonar's Rust rules count it.
/// - `?` (`try_expression`) is **not** counted: it is an early return, not a
///   branch, and McCabe's multi-exit correction would subtract rather than add.
static RUST: LanguageRules = LanguageRules {
    function_nodes: &["function_item", "closure_expression"],
    decision_nodes: &[
        "if_expression",
        "match_arm",
        "while_expression",
        "loop_expression",
        "for_expression",
    ],
    decision_operators: &["&&", "||"],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters"],
};

/// Go.
///
/// - Go's switch arms are `expression_case`, **not** `case_statement`. Getting
///   this wrong makes every `switch` disappear from the count.
/// - `select` uses `communication_case`; type switches use `type_case`.
/// - `go/defer` do not branch.
static GO: LanguageRules = LanguageRules {
    function_nodes: &["function_declaration", "method_declaration", "func_literal"],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "expression_case",
        "communication_case",
        "type_case",
    ],
    decision_operators: &["&&", "||"],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters"],
};

/// Java.
///
/// - `switch_label` is one node per `case`/`default` label in both switch
///   statements and switch expressions. `default` counts: it is a branch
///   target, exactly like `case`.
/// - `enhanced_for_statement` (`for (T x : xs)`) is a separate kind from
///   `for_statement`; missing it makes every for-each loop disappear.
/// - `ternary_expression`, not `conditional_expression`.
/// - `&&`/`||` are `binary_expression` nodes with a named `operator` field.
static JAVA: LanguageRules = LanguageRules {
    function_nodes: &[
        "method_declaration",
        "constructor_declaration",
        "lambda_expression",
    ],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "enhanced_for_statement",
        "while_statement",
        "do_statement",
        "switch_label",
        "catch_clause",
        "ternary_expression",
    ],
    decision_operators: &["&&", "||"],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters"],
};

/// C.
///
/// - No lambdas, classes or exceptions: `function_definition` is the only
///   function node.
/// - `case_statement` covers **both** `case X:` and `default:`. Counting it
///   means a `switch` with two cases plus `default` is CCN 4.
/// - `conditional_expression` is C's ternary; `&&`/`||` are
///   `binary_expression` nodes.
/// - `function_definition` exposes neither `name` nor `parameters`; both sit
///   under the `declarator` chain (see `LanguageRules::declarator_field`).
static C: LanguageRules = LanguageRules {
    function_nodes: &["function_definition"],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "while_statement",
        "do_statement",
        "case_statement",
        "conditional_expression",
    ],
    decision_operators: &["&&", "||"],
    name_field: "",
    declarator_field: "declarator",
    params_fields: &[],
};

/// C++.
///
/// - Everything in C, plus `catch_clause` and `lambda_expression`.
/// - Range-based `for` is `for_range_loop`, **not** `for_statement`; both are
///   listed so neither spelling can go uncounted.
/// - `case_statement` again covers `default:`.
static CPP: LanguageRules = LanguageRules {
    function_nodes: &["function_definition", "lambda_expression"],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "for_range_loop",
        "while_statement",
        "do_statement",
        "case_statement",
        "catch_clause",
        "conditional_expression",
    ],
    decision_operators: &["&&", "||"],
    name_field: "",
    declarator_field: "declarator",
    params_fields: &[],
};

/// C#.
///
/// - `local_function_statement` is a real nested function and gets its own
///   row, like a lambda.
/// - `switch_section` is one node per `case`/`default` label, including
///   pattern cases (`case int n when ...`). `switch_expression_arm` is the
///   expression-bodied form (`x switch { 1 => ..., _ => ... }`) and counts
///   once per arm, `_` included — the same rule as Rust `match_arm`.
/// - `??` **is** counted: it is a genuine two-way branch (`a ?? b` evaluates
///   `b` only when `a` is null) and the grammar models it as a
///   `binary_expression` with `operator: "??"`. `?.` is a
///   `conditional_access_expression`, a different kind, and is **not**
///   counted — it has no else-branch.
/// - `when_clause` guards are not counted separately: the `switch_section` /
///   `switch_expression_arm` they belong to already contributes one, and
///   double-counting a single arm would overstate it.
static CSHARP: LanguageRules = LanguageRules {
    function_nodes: &[
        "method_declaration",
        "constructor_declaration",
        "local_function_statement",
        "lambda_expression",
    ],
    decision_nodes: &[
        "if_statement",
        "for_statement",
        "foreach_statement",
        "while_statement",
        "do_statement",
        "switch_section",
        "switch_expression_arm",
        "catch_clause",
        "conditional_expression",
    ],
    decision_operators: &["&&", "||", "??"],
    name_field: "name",
    declarator_field: "",
    params_fields: &["parameters"],
};

pub fn rules(lang: Lang) -> &'static LanguageRules {
    match lang {
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &JS_LIKE,
        Lang::Python => &PYTHON,
        Lang::Rust => &RUST,
        Lang::Go => &GO,
        Lang::Java => &JAVA,
        Lang::C => &C,
        Lang::Cpp => &CPP,
        Lang::CSharp => &CSHARP,
    }
}

/// Tier-1 languages are fixture-covered, so every wired grammar reports
/// `Exact`. Kept next to the tables so the engine and the dispatcher cannot
/// disagree about a language's trust level.
pub fn tier(_lang: Lang) -> crate::ccn::types::CcnTier {
    crate::ccn::types::CcnTier::Exact
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_a_table() {
        for lang in Lang::ALL {
            let r = rules(lang);
            assert!(
                !r.function_nodes.is_empty(),
                "{} has no function nodes",
                lang.name()
            );
            assert!(
                !r.decision_nodes.is_empty(),
                "{} has no decision nodes",
                lang.name()
            );
        }
    }

    #[test]
    fn js_does_not_count_optional_chaining_but_does_count_nullish() {
        let r = rules(Lang::JavaScript);
        assert!(!r.decision_nodes.contains(&"optional_chain"));
        assert!(r.decision_operators.contains(&"??"));
        assert!(r.decision_operators.contains(&"&&"));
    }

    #[test]
    fn go_counts_case_nodes_not_case_statement() {
        let r = rules(Lang::Go);
        assert!(r.decision_nodes.contains(&"expression_case"));
        assert!(!r.decision_nodes.contains(&"case_statement"));
    }

    #[test]
    fn rust_counts_match_arms() {
        assert!(rules(Lang::Rust).decision_nodes.contains(&"match_arm"));
    }

    #[test]
    fn java_uses_enhanced_for_and_switch_labels() {
        let r = rules(Lang::Java);
        assert!(r.decision_nodes.contains(&"enhanced_for_statement"));
        assert!(r.decision_nodes.contains(&"switch_label"));
        assert!(!r.decision_nodes.contains(&"switch_case"));
        assert!(!r.decision_nodes.contains(&"conditional_expression"));
        assert!(r.function_nodes.contains(&"constructor_declaration"));
    }

    #[test]
    fn c_has_no_lambdas_and_uses_case_statement() {
        let r = rules(Lang::C);
        assert_eq!(r.function_nodes, &["function_definition"]);
        assert!(!r.function_nodes.contains(&"lambda_expression"));
        assert!(r.decision_nodes.contains(&"case_statement"));
        assert!(r.decision_nodes.contains(&"conditional_expression"));
        assert_eq!(r.declarator_field, "declarator");
    }

    #[test]
    fn cpp_adds_lambdas_range_for_and_catch() {
        let r = rules(Lang::Cpp);
        assert!(r.function_nodes.contains(&"lambda_expression"));
        assert!(r.decision_nodes.contains(&"for_range_loop"));
        assert!(r.decision_nodes.contains(&"catch_clause"));
        assert!(r.decision_nodes.contains(&"case_statement"));
    }

    #[test]
    fn csharp_uses_sections_and_switch_expression_arms() {
        let r = rules(Lang::CSharp);
        assert!(r.decision_nodes.contains(&"switch_section"));
        assert!(r.decision_nodes.contains(&"switch_expression_arm"));
        assert!(r.decision_nodes.contains(&"foreach_statement"));
        assert!(r.decision_operators.contains(&"??"));
        assert!(!r.decision_operators.contains(&"?."));
    }

    #[test]
    fn every_table_lists_each_kind_at_most_once() {
        for lang in Lang::ALL {
            let r = rules(lang);
            for (label, list) in [
                ("function_nodes", r.function_nodes),
                ("decision_nodes", r.decision_nodes),
                ("decision_operators", r.decision_operators),
            ] {
                let mut seen = list.to_vec();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(
                    seen.len(),
                    list.len(),
                    "{} lists a duplicate in {label}",
                    lang.name()
                );
            }
        }
    }

    /// Only C and C++ hide name/parameters under a declarator chain.
    #[test]
    fn only_c_and_cpp_use_a_declarator_field() {
        for lang in Lang::ALL {
            let expected = matches!(lang, Lang::C | Lang::Cpp);
            assert_eq!(
                !rules(lang).declarator_field.is_empty(),
                expected,
                "{} declarator_field",
                lang.name()
            );
        }
    }
}
