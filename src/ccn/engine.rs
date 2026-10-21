//! tree-sitter CCN engine.
//!
//! Contract:
//! - `analyze_file` is called from a rayon worker pool; it must be `Send + Sync`
//!   callable and must not share mutable state between calls.
//! - `ccn == 1 + decision_points`, minimum 1, per function.
//! - Nested functions are measured separately and **excluded** from the
//!   enclosing function's count (otherwise a single factory function swallows
//!   all its callbacks and the histogram becomes a lie).
//! - A parse *failure* (no function recovered at all) returns `Err`; the
//!   caller degrades that file to the heuristic tier. It must never panic on
//!   malformed input.
//! - A parse with errors that still yields functions returns `Ok` at
//!   `CcnTier::BestEffort`: tree-sitter's recovery keeps the intact functions,
//!   and throwing them away because one line of the file is broken is worse
//!   than reporting them with a lower trust tier.
//!
//! The node tables live in `rules.rs`; do not hard-code node kinds here.

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Language, Node, Parser, Tree};

use crate::ccn::rules::{self, LanguageRules};
use crate::ccn::types::{CcnTier, FileCcn, FunctionCcn, Lang};

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
    "init_declarator",
    "assignment",
    "let_declaration",
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
/// `{ handler: function () {} }` from degrading to `(anonymous)`. C++
/// `auto a = []` names the lambda through `init_declarator.declarator`, Python
/// `n = lambda` through `assignment.left`, and Rust `let f = |..|` through
/// `let_declaration.pattern`.
const NAMING_FIELDS: &[&str] = &["name", "left", "key", "declarator", "pattern"];

/// Function-expression kinds that are *lambdas* for identity purposes: an
/// initializer of a named variable becomes `<enclosing>::lambda:<binding>`.
/// Named declarations (`function f()`, `fn f()`, methods) never take this
/// branch, so their identity stays `scope::name(params)`.
const LAMBDA_KINDS: &[&str] = &[
    "lambda_expression",
    "closure_expression",
    "lambda",
    "arrow_function",
];

/// Leaf node kinds that can be the declared name inside a C/C++ declarator
/// chain. Used only to cut the name out of a parameter's text.
const DECLARED_NAME_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "operator_name",
    "destructor_name",
];

/// Preprocessor nodes whose `condition` can be decided statically.
///
/// In both tree-sitter-c and tree-sitter-cpp an `#if`/`#elif` node carries the
/// following `#else`/`#elif` chain in its `alternative` field, so a dead branch
/// can be dropped by keeping only that one child. `#ifdef`/`#ifndef` are
/// deliberately absent: the flags are unknown, so both branches stay live.
const PREPROC_CONDITIONALS: &[&str] = &["preproc_if", "preproc_elif"];

/// Child kinds that begin an `#else`/`#elif` chain.
const PREPROC_ALTERNATIVES: &[&str] = &["preproc_else", "preproc_elif", "preproc_elifdef"];

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
///
/// A tree that contains `ERROR`/`MISSING` nodes is still walked: whatever
/// functions tree-sitter recovered are reported at `CcnTier::BestEffort`. Only
/// a tree that recovered *no* function at all is an error, so the dispatcher
/// can try a sibling grammar (or the heuristic) without losing a whole file to
/// one broken line.
pub fn analyze_file(path: &Path, source: &[u8], lang: Lang) -> Result<FileCcn, CcnError> {
    analyze_file_diagnostic(path, source, lang).map(|(file, _had_error)| file)
}

/// Same as [`analyze_file`], plus whether the tree contained `ERROR`/`MISSING`
/// nodes.
///
/// The dispatcher needs that signal to decide whether a sibling grammar is
/// worth trying: `CcnTier::BestEffort` alone says the parse was degraded, but
/// not whether a different grammar would have been clean. Callers that only
/// want the measurement should keep using [`analyze_file`].
pub fn analyze_file_diagnostic(
    _path: &Path,
    source: &[u8],
    lang: Lang,
) -> Result<(FileCcn, bool), CcnError> {
    let table = rules::rules(lang);
    let tier = rules::tier(lang);

    // Whitespace-only input still parses, but skipping it keeps the empty-file
    // path free of grammar quirks (notably Python indentation errors).
    if source.iter().all(u8::is_ascii_whitespace) {
        return Ok((
            FileCcn {
                tier,
                functions: Vec::new(),
            },
            false,
        ));
    }

    let parsed =
        with_parser(lang, |parser| parser.parse(source, None)).ok_or(CcnError::NoGrammar(lang))?;
    let tree: Tree = parsed.ok_or_else(|| CcnError::ParseFailed {
        lang,
        detail: "parser produced no tree".to_string(),
    })?;

    let root = tree.root_node();
    let walker = Walker::new(lang, table, source);

    let mut functions: Vec<FunctionCcn> = collect_function_nodes(root, &walker)
        .into_iter()
        .map(|node| measure(node, &walker))
        .collect();

    let has_error = root.has_error();
    if has_error && functions.is_empty() {
        return Err(CcnError::ParseFailed {
            lang,
            detail: first_syntax_error(root),
        });
    }

    // Outer before inner when both start on the same line.
    functions.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| b.end_line.cmp(&a.end_line))
    });

    let file = FileCcn {
        tier: if has_error { CcnTier::BestEffort } else { tier },
        functions,
    };
    Ok((file, has_error))
}

/// Read-only context shared by the two iterative walkers.
struct Walker<'a> {
    lang: Lang,
    table: &'a LanguageRules,
    source: &'a [u8],
    /// Only C/C++ understand `#if 0`: their preprocessor nodes are real nodes,
    /// whereas other grammars either lack them or model them differently.
    preproc: bool,
}

impl<'a> Walker<'a> {
    fn new(lang: Lang, table: &'a LanguageRules, source: &'a [u8]) -> Self {
        Self {
            lang,
            table,
            source,
            preproc: matches!(lang, Lang::C | Lang::Cpp),
        }
    }

    /// Push the children of `node` that are actually compiled.
    ///
    /// Iterative on purpose: tree depth is attacker-controlled and a recursive
    /// walker would blow the stack on a deeply nested file. The allocation for
    /// the filtered case only happens on `#if`/`#elif` nodes, which are rare
    /// compared to the nodes visited in a function body.
    fn push_children<'tree>(&self, stack: &mut Vec<Node<'tree>>, node: Node<'tree>) {
        self.for_each_live_child(node, |child| stack.push(child));
    }

    /// Like [`Walker::push_children`], but tags every child with the nesting
    /// depth it lives at. The caller decides what that depth is; this only
    /// applies the same `#if 0` filtering as `push_children`.
    fn push_children_at<'tree>(
        &self,
        stack: &mut Vec<(Node<'tree>, u32)>,
        node: Node<'tree>,
        depth: u32,
    ) {
        self.for_each_live_child(node, |child| stack.push((child, depth)));
    }

    /// Visit the children of `node` that are actually compiled, in source
    /// order.
    ///
    /// Both walkers share this so `#if 0` handling can never drift between the
    /// decision count and the nesting depth: a dead branch is invisible to
    /// both.
    fn for_each_live_child<'tree>(&self, node: Node<'tree>, mut f: impl FnMut(Node<'tree>)) {
        if self.preproc && PREPROC_CONDITIONALS.contains(&node.kind()) {
            if let Some(children) = live_preproc_children(node, self.source) {
                for child in children {
                    f(child);
                }
                return;
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            f(child);
        }
    }
}

/// Children of an `#if`/`#elif` that survive constant folding, or `None` when
/// the condition is unknown and everything has to be kept.
fn live_preproc_children<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    let take_branch = condition_truth(node, source)?;
    let alternative = node.child_by_field_name("alternative");

    let mut cursor = node.walk();
    let children: Vec<Node<'tree>> = node.children(&mut cursor).collect();
    Some(
        children
            .into_iter()
            .filter(|child| {
                let is_alternative = alternative.is_some_and(|alt| alt == *child)
                    || PREPROC_ALTERNATIVES.contains(&child.kind());
                if is_alternative {
                    // `#if 0 ... #else` / `#elif`: the alternative is the live
                    // branch. `#if 1 ... #else` / `#elif`: it is dead.
                    !take_branch
                } else {
                    take_branch
                }
            })
            .collect(),
    )
}

/// `Some(true)` for `#if 1`/`#if true`, `Some(false)` for `#if 0`/`#if false`,
/// `None` when the condition depends on a macro or expression we cannot
/// evaluate.
fn condition_truth(node: Node<'_>, source: &[u8]) -> Option<bool> {
    let condition = node.child_by_field_name("condition")?;
    let text = condition.utf8_text(source).ok()?;
    match strip_outer_parens(text.trim()) {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Strip redundant outer parentheses from a preprocessor condition.
///
/// `#if (0)` is as dead as `#if 0`; a plain `trim` would miss it. Only pairs
/// that wrap the whole condition are removed, so `#if (1) && (0)` is left
/// alone (and then reads as unknown).
fn strip_outer_parens(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim();
        if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
            return trimmed;
        }
        let mut depth = 0i32;
        let mut wraps = true;
        for (index, ch) in trimmed.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        // The first paren closes before the end: not a wrapper.
                        wraps = index + 1 == trimmed.len();
                        break;
                    }
                }
                _ => {}
            }
        }
        if !wraps || depth != 0 {
            return trimmed;
        }
        text = &trimmed[1..trimmed.len() - 1];
    }
}

/// All nodes whose kind opens a function scope, in no particular order.
///
/// `ERROR` subtrees are not descended into: their shape is the parser's guess,
/// and a "function" inside one is a phantom that would pad the histogram. The
/// valid functions before and after the error are unaffected.
fn collect_function_nodes<'tree>(root: Node<'tree>, walker: &Walker<'_>) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() {
            continue;
        }
        // `is_named` matters: Python's anonymous `lambda` *keyword token* has
        // kind `"lambda"` too, so a bare kind match turns every lambda into two
        // functions (one of them a zero-width phantom).
        if node.is_named() && walker.table.function_nodes.contains(&node.kind()) {
            found.push(node);
        }
        // Descend into functions too: they can contain nested ones, which are
        // measured separately.
        walker.push_children(&mut stack, node);
    }
    found
}

fn measure(node: Node<'_>, walker: &Walker<'_>) -> FunctionCcn {
    let start = node.start_position();
    let end = node.end_position();
    let name = function_name(node, walker.table, walker.source);
    let identity = function_identity(node, walker, &name);
    let complexity = count_decisions(node, walker);

    FunctionCcn {
        name,
        identity,
        line: start.row + 1,
        end_line: end.row + 1,
        nloc: end.row - start.row + 1,
        ccn: complexity.decisions.saturating_add(1),
        params: param_count(node, walker.table),
        max_nesting: complexity.max_nesting,
    }
}

/// Decisions and maximum control-flow nesting for one function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Complexity {
    /// One per decision node / decision operator. `ccn` is this plus one.
    decisions: u32,
    /// Deepest chain of enclosing decision constructs (0 for a flat body).
    max_nesting: u32,
}

/// Decisions in `func`'s subtree and the deepest nesting among them,
/// excluding nested functions and `ERROR` subtrees.
///
/// A function can contain an `ERROR` child without being unusable (a macro
/// call in the middle of a statement, say). The decisions outside that error
/// are real and are still counted; only the unreliable subtree is skipped.
///
/// Nesting is *structural*: a node starts one level deeper than its parent
/// whenever the parent is a decision node from `rules.decision_nodes`. That
/// gives the intended reading directly, because in every wired grammar the
/// switch case node (which is what the table counts) wraps its own body:
/// `switch { case 1: if (x) {} }` is 1 for the case and 2 for the `if`.
/// Siblings never add up (`if {} if {}` is 1), and a nested function starts a
/// fresh count of its own. The consequence is that an `else if` / `elif`
/// chain reads one level deeper than a sibling `if` chain, because the grammar
/// places the next branch inside the previous decision node.
fn count_decisions<'tree>(func: Node<'tree>, walker: &Walker<'_>) -> Complexity {
    let mut decisions = 0u32;
    let mut max_nesting = 0u32;
    let mut stack: Vec<(Node<'tree>, u32)> = Vec::new();
    // The function node itself is not a decision construct.
    walker.push_children_at(&mut stack, func, 0);

    while let Some((node, depth)) = stack.pop() {
        if node.is_error() || node.is_missing() {
            continue;
        }
        let kind = node.kind();
        if node.is_named() && walker.table.function_nodes.contains(&kind) {
            // A nested function's decisions belong to that function.
            continue;
        }
        if is_decision(node, kind, walker) {
            decisions += 1;
            max_nesting = max_nesting.max(depth + 1);
            walker.push_children_at(&mut stack, node, depth + 1);
        } else {
            walker.push_children_at(&mut stack, node, depth);
        }
    }

    Complexity {
        decisions,
        max_nesting,
    }
}

/// Whether `node` is one decision point under the table for this language.
fn is_decision(node: Node<'_>, kind: &str, walker: &Walker<'_>) -> bool {
    if walker.table.decision_nodes.contains(&kind) {
        return true;
    }
    if kind != "binary_expression" {
        return false;
    }
    node.child_by_field_name("operator")
        .and_then(|operator| operator.utf8_text(walker.source).ok())
        .is_some_and(|text| walker.table.decision_operators.contains(&text))
}

fn function_name(node: Node<'_>, table: &LanguageRules, source: &[u8]) -> String {
    if let Some(name) = node.child_by_field_name(table.name_field) {
        if let Some(text) = field_text(name, source) {
            return text;
        }
    }

    // C/C++: the name is the innermost `declarator`. A declarator that still
    // carries a `parameters` field is a lambda's abstract declarator, not a
    // name, so lambdas stay `(anonymous)` unless a binding names them.
    if let Some(decl) = innermost_declarator(node, table.declarator_field) {
        if decl.child_by_field_name("parameters").is_none() {
            if let Some(text) = field_text(decl, source) {
                return text;
            }
        }
    }

    bound_name(node, source).unwrap_or_else(|| ANONYMOUS.to_string())
}

/// Name supplied by the declaration that initializes this function expression:
/// `auto a = [](..)`, `const cb = (..) =>`, `n = lambda`, `let f = |..|`.
///
/// This is context, not a line number, so reordering two bound lambdas does not
/// change either identity.
fn bound_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    if !NAMING_PARENTS.contains(&parent.kind()) {
        return None;
    }
    for field in NAMING_FIELDS {
        if let Some(name) = parent.child_by_field_name(field) {
            if let Some(text) = field_text(name, source) {
                return Some(text);
            }
        }
    }
    None
}

/// Node kinds that introduce a named scope. Never contains a function node: a
/// nested function is part of its own identity, not its parent's.
fn scope_nodes(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Cpp => &[
            "namespace_definition",
            "class_specifier",
            "struct_specifier",
            "union_specifier",
        ],
        Lang::C => &[],
        Lang::Java => &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "record_declaration",
        ],
        Lang::CSharp => &[
            "namespace_declaration",
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
            "record_declaration",
        ],
        Lang::Rust => &["impl_item", "trait_item", "mod_item"],
        Lang::Python => &["class_definition"],
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            &["class_declaration", "class", "interface_declaration"]
        }
        // Go methods already carry their receiver in the name; there is no
        // extra scope to qualify with.
        Lang::Go => &[],
    }
}

/// Identity key for one measured function.
///
/// Grammar:
///
/// ```text
/// named      := [scope "::"] name ["<" targs ">"] "(" ptypes ")" [" " qualifiers]
/// lambda     := [enclosing "::"] "lambda:" binding "(" ptypes ")"
/// anonymous  := "(anonymous)" "(" ptypes ")"
/// ```
///
/// - `scope` is unchanged (`N::X::m`), and the reported `name` stays short.
/// - `ptypes` are parameter *types*, never declaration text (see
///   [`parameter_types`]), so renaming a parameter cannot change identity.
/// - `qualifiers` are C++ trailing `const`/`volatile`/`&`/`&&`/`noexcept`
///   (see [`qualifiers`]), so `f(int) const` and `f(int)` are distinct.
/// - `targs` is C++/Rust template context (see [`template_args`]).
/// - Lambdas bound to a named variable get `lambda:<binding>` nested under
///   their enclosing function/lambda identity; unbound lambdas stay
///   `(anonymous)(...)` and deliberately carry **no line number**.
///
/// The scope is collected from the enclosing declarations so that overload
/// reordering inside a class cannot look like a different function. The short
/// `name` is deliberately left unqualified: the report still shows `m`, not
/// `N::X::m`.
fn function_identity(node: Node<'_>, walker: &Walker<'_>, name: &str) -> String {
    function_identity_depth(node, walker, name, 0)
}

/// [`function_identity`] with a recursion budget for nested bound lambdas.
///
/// Tree depth is attacker-controlled: a pathological chain of nested lambdas
/// must return a bounded identity rather than overflow the stack. At the cap
/// the enclosing part is dropped, which can only make an identity less
/// specific, never panic.
fn function_identity_depth(node: Node<'_>, walker: &Walker<'_>, name: &str, depth: u32) -> String {
    let params = parameter_types(node, walker.table, walker.source);

    if LAMBDA_KINDS.contains(&node.kind()) {
        if name == ANONYMOUS {
            // No stable binding: keep the compact anonymous identity. A line
            // number would fabricate stability across reformatting.
            return format!("{ANONYMOUS}{params}");
        }
        let enclosing = enclosing_identity(node, walker, depth);
        if enclosing.is_empty() {
            return format!("lambda:{name}{params}");
        }
        return format!("{enclosing}::lambda:{name}{params}");
    }

    let scope = scope_path(node, walker);
    let targs = template_args(node, walker.lang, walker.source);
    let trailing = normalized_qualifiers(qualifiers(node, walker.source));
    let mut out = String::new();
    if !scope.is_empty() {
        out.push_str(&scope);
        out.push_str("::");
    }
    out.push_str(name);
    out.push_str(&targs);
    out.push_str(&params);
    if !trailing.is_empty() {
        out.push(' ');
        out.push_str(&trailing);
    }
    out
}

/// Identity of the nearest enclosing function/lambda, if any.
///
/// This is the *context* a bound lambda is nested under. It never uses line
/// numbers, so reordering siblings leaves both identities unchanged.
fn enclosing_identity(node: Node<'_>, walker: &Walker<'_>, depth: u32) -> String {
    if depth >= 16 {
        return String::new();
    }
    let mut current = node.parent();
    for _ in 0..32 {
        let Some(parent) = current else {
            return String::new();
        };
        if parent.is_named() && walker.table.function_nodes.contains(&parent.kind()) {
            let name = function_name(parent, walker.table, walker.source);
            return function_identity_depth(parent, walker, &name, depth + 1);
        }
        current = parent.parent();
    }
    String::new()
}

/// Outermost-first `::`-joined names of the enclosing declarations.
///
/// Bounded by a depth cap because tree depth is attacker-controlled; an
/// anonymous scope (a lambda class, say) is simply skipped rather than
/// producing an empty `::` segment.
fn scope_path(node: Node<'_>, walker: &Walker<'_>) -> String {
    let allowed = scope_nodes(walker.lang);
    if allowed.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();
    let mut current = node.parent();
    for _ in 0..32 {
        let Some(parent) = current else { break };
        if allowed.contains(&parent.kind()) {
            if let Some(name) = scope_name(parent, walker.source) {
                parts.push(name);
            }
        }
        current = parent.parent();
    }
    parts.reverse();
    parts.join("::")
}

/// Name of a scope node. `name` covers every listed kind except Rust
/// `impl_item`, which names its target through the `type` field.
fn scope_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    for field in ["name", "type"] {
        if let Some(name) = node.child_by_field_name(field) {
            if let Some(text) = field_text(name, source) {
                return Some(squeeze_whitespace(&text));
            }
        }
    }
    None
}

/// The node carrying a function's parameter list, if the grammar has one.
fn parameter_node<'tree>(node: Node<'tree>, table: &LanguageRules) -> Option<Node<'tree>> {
    for field in table.params_fields {
        if let Some(params) = node.child_by_field_name(field) {
            return Some(params);
        }
    }
    // C/C++: parameters live under the declarator, not on the function node.
    declarator_params(node, table.declarator_field)
}

/// Parameter list as normalized **types**, parentheses included once.
///
/// Parameter names are not identity. For statically typed languages
/// (C/C++/Java/C#/Rust/Go) each entry is derived from the declaration's type
/// and declarator punctuation, with the declared name cut out. For
/// dynamically typed C-like languages (Python/JS/TS-without-annotation) an
/// untyped parameter becomes the positional marker `_`; an annotation, when
/// present, is preferred. That choice is deliberate: a rename must not change
/// identity, and two parameters of the same function are already ordered, so
/// a positional marker fully preserves arity without inventing a type.
///
/// If an entry cannot be classified it falls back to the full normalized
/// declaration text rather than guessing a type.
fn parameter_types(node: Node<'_>, table: &LanguageRules, source: &[u8]) -> String {
    let Some(params) = parameter_node(node, table) else {
        return "()".to_string();
    };

    // A single unparenthesized parameter is a bare `identifier` (Java
    // `x -> ...`), `implicit_parameter` (C# `x => ...`) or a single Rust
    // closure parameter. It is not a list.
    if params.kind() == "identifier"
        || params.kind() == "implicit_parameter"
        || params.kind() == "self"
    {
        return format!("({})", parameter_placeholder(params, table, source));
    }

    let lang = language_of(table);
    let mut entries: Vec<String> = Vec::new();
    let mut cursor = params.walk();
    // All children, not only named ones: C/C++ model `...` as an anonymous
    // token and it must survive into the identity.
    for child in params.children(&mut cursor) {
        let kind = child.kind();
        if matches!(kind, "(" | ")" | "," | "{" | "}") || kind.contains("comment") {
            continue;
        }
        if kind == "..." {
            entries.push("...".to_string());
            continue;
        }
        if !child.is_named() {
            // Other punctuation (annotations' `@`, Python's bare `*`, ...) is
            // not a parameter entry.
            continue;
        }
        entries.extend(parameter_entries(lang, child, source));
    }
    format!("({})", entries.join(","))
}

/// One declaration can yield several entries: Go groups names sharing a type
/// (`y, z string` is two parameters of type `string`). Every other language
/// yields exactly one.
fn parameter_entries(lang: Option<Lang>, node: Node<'_>, source: &[u8]) -> Vec<String> {
    if lang == Some(Lang::Go) {
        return go_parameter_entries(node, source);
    }
    vec![parameter_entry(lang, node, source)]
}

/// The type (or placeholder) for one parameter declaration.
fn parameter_entry(lang: Option<Lang>, node: Node<'_>, source: &[u8]) -> String {
    match lang {
        Some(Lang::C | Lang::Cpp) => cpp_parameter_text(node, source),
        Some(Lang::Java) => java_parameter_text(node, source),
        Some(Lang::CSharp) => csharp_parameter_text(node, source),
        Some(Lang::Rust) => rust_parameter_text(node, source),
        Some(Lang::Python) => python_parameter_text(node, source),
        Some(Lang::JavaScript | Lang::TypeScript | Lang::Tsx) => js_parameter_text(node, source),
        // Go is handled by `go_parameter_entries`; this arm also covers a
        // future table without a dedicated extractor.
        Some(Lang::Go) | None => fallback_parameter_text(node, source),
    }
}

/// A bare, list-less parameter: untyped by construction in Java/C#/Go arrows
/// and lambdas, so it gets the positional marker; JS/Python/Rust helpers also
/// return the marker or an annotated form.
fn parameter_placeholder(node: Node<'_>, table: &LanguageRules, source: &[u8]) -> String {
    match language_of(table) {
        Some(Lang::Java | Lang::CSharp | Lang::Go) => "_".to_string(),
        lang => parameter_entry(lang, node, source),
    }
}

/// Map a rules table back to the language that owns it.
///
/// The JS/TS/TSX grammars share one table and one parameter model, so returning
/// JavaScript for all three is intentional. `None` means "unknown table": the
/// caller falls back to normalized declaration text.
fn language_of(table: &LanguageRules) -> Option<Lang> {
    Lang::ALL
        .into_iter()
        .find(|lang| std::ptr::eq(rules::rules(*lang), table))
}

/// Full normalized declaration text, used when no type can be derived.
fn fallback_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    let Ok(raw) = node.utf8_text(source) else {
        return String::new();
    };
    squeeze_whitespace(&strip_comments(raw))
}

/// C/C++: declaration specifiers plus pointer/reference/array declarators, with
/// the declared name and any default argument removed.
///
/// `int x` -> `int`, `const char* p` -> `const char*`, `T&& v` -> `T&&`,
/// `int a[3]` -> `int[3]`. Structurally identical declarations therefore keep
/// one identity across parameter renames and reformatting.
fn cpp_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    if node.kind() == "variadic_parameter" {
        return "...".to_string();
    }
    // K&R-style identifier-only parameter: the type is declared elsewhere, so
    // there is nothing structural to keep. A positional marker stays stable
    // across renames instead of leaking the name.
    if node.kind() == "identifier" {
        return "_".to_string();
    }
    let declarator = node.child_by_field_name("declarator");
    let mut end = declarator.map_or(node.end_byte(), |decl| decl.end_byte());
    // A default argument is not part of the signature.
    if let Some(default) = node.child_by_field_name("default_value") {
        end = end.min(default.start_byte());
    }
    let raw = text_between(source, node.start_byte(), end);
    let name_span = declarator
        .and_then(declared_name)
        .map(|name| (name.start_byte(), name.end_byte()));
    let without_name = match (raw, name_span) {
        (Some(raw), Some((start, stop))) if start >= node.start_byte() && stop <= end => {
            let a = start - node.start_byte();
            let b = stop - node.start_byte();
            format!("{}{}", &raw[..a], &raw[b..])
        }
        (Some(raw), _) => raw.to_string(),
        (None, _) => return String::new(),
    };
    squeeze_whitespace(&strip_comments(&without_name))
}

/// Innermost leaf that spells the declared variable name inside a C/C++
/// declarator chain. Bounded so a malformed tree cannot hang the scan.
fn declared_name(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node;
    for _ in 0..16 {
        if DECLARED_NAME_KINDS.contains(&current.kind()) {
            return Some(current);
        }
        if let Some(inner) = current.child_by_field_name("declarator") {
            current = inner;
            continue;
        }
        // `reference_declarator` spells `T&& v` as an unnamed identifier
        // child; array/pointer declarators normally have a `declarator` field,
        // but fall back to the first declarator-ish named child.
        let mut cursor = current.walk();
        let children: Vec<Node<'_>> = current.named_children(&mut cursor).collect();
        let next = children
            .iter()
            .find(|child| child.kind().contains("declarator"))
            .or_else(|| {
                children
                    .iter()
                    .find(|child| DECLARED_NAME_KINDS.contains(&child.kind()))
            })
            .copied()?;
        if next == current {
            return None;
        }
        current = next;
    }
    None
}

/// Java: the `type` field (varargs keep their `...`).
fn java_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "spread_parameter" => {
            let mut cursor = node.walk();
            let ty = node
                .named_children(&mut cursor)
                .find(|child| child.kind() != "modifiers")
                .and_then(|child| child.utf8_text(source).ok())
                .map(|text| squeeze_whitespace(text.trim()))
                .unwrap_or_default();
            format!("{ty}...")
        }
        "receiver_parameter" => {
            let mut cursor = node.walk();
            let ty = node
                .named_children(&mut cursor)
                .find(|child| !child.kind().contains("annotation"))
                .and_then(|child| child.utf8_text(source).ok())
                .map(|text| squeeze_whitespace(text.trim()));
            ty.unwrap_or_else(|| fallback_parameter_text(node, source))
        }
        _ => node
            .child_by_field_name("type")
            .and_then(|ty| ty.utf8_text(source).ok())
            .map(|text| squeeze_whitespace(text.trim()))
            .unwrap_or_else(|| fallback_parameter_text(node, source)),
    }
}

/// C#: declaration text minus the variable name and any default value.
/// Leading `ref`/`out`/`in`/`params`/`this` modifiers are preserved, and so is
/// the array suffix of a `params` parameter.
fn csharp_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    let mut end = node.end_byte();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "=" {
            end = end.min(child.start_byte());
            break;
        }
    }
    let Some(raw) = text_between(source, node.start_byte(), end) else {
        return fallback_parameter_text(node, source);
    };
    let without_name = match node.child_by_field_name("name") {
        Some(name) if name.start_byte() >= node.start_byte() && name.end_byte() <= end => {
            let a = name.start_byte() - node.start_byte();
            let b = name.end_byte() - node.start_byte();
            format!("{}{}", &raw[..a], &raw[b..])
        }
        _ => raw.to_string(),
    };
    squeeze_whitespace(&strip_comments(&without_name))
}

/// Rust: the `type` field of a parameter; `&self`/`&mut self` keep their
/// receiver spelling; an untyped closure parameter is `_`.
fn rust_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "self_parameter" => fallback_parameter_text(node, source),
        "identifier" | "mutable_specifier" => "_".to_string(),
        _ => node
            .child_by_field_name("type")
            .and_then(|ty| ty.utf8_text(source).ok())
            .map(|text| squeeze_whitespace(text.trim()))
            .unwrap_or_else(|| "_".to_string()),
    }
}

/// Python: annotations when present, otherwise the positional marker `_`.
/// `def m(self, x, y=1)` -> `(_,_,_)`.
fn python_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "typed_parameter" | "typed_default_parameter" => node
            .child_by_field_name("type")
            .and_then(|ty| ty.utf8_text(source).ok())
            .map(|text| squeeze_whitespace(text.trim()))
            .unwrap_or_else(|| "_".to_string()),
        "identifier"
        | "default_parameter"
        | "list_splat_pattern"
        | "dictionary_splat_pattern"
        | "tuple_pattern"
        | "list_pattern" => "_".to_string(),
        _ => "_".to_string(),
    }
}

/// JavaScript/TypeScript: a type annotation when present, else `_`.
/// TypeScript `required_parameter`/`optional_parameter` wrap the annotation in
/// a `type_annotation` (`: number`), so a leading colon is stripped.
fn js_parameter_text(node: Node<'_>, source: &[u8]) -> String {
    if matches!(node.kind(), "required_parameter" | "optional_parameter") {
        if let Some(ty) = node.child_by_field_name("type") {
            if let Ok(text) = ty.utf8_text(source) {
                let trimmed = text.trim().trim_start_matches(':').trim();
                return squeeze_whitespace(trimmed);
            }
        }
    }
    if let Some(ty) = node.child_by_field_name("type") {
        if let Ok(text) = ty.utf8_text(source) {
            let trimmed = text.trim().trim_start_matches(':').trim();
            if !trimmed.is_empty() {
                return squeeze_whitespace(trimmed);
            }
        }
    }
    "_".to_string()
}

/// Go: one entry per declared name (they share a type); a variadic parameter
/// keeps its `...`. A type-only declaration yields one entry.
fn go_parameter_entries(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let ty = node
        .child_by_field_name("type")
        .and_then(|ty| ty.utf8_text(source).ok())
        .map(|text| squeeze_whitespace(text.trim()))
        .unwrap_or_else(|| fallback_parameter_text(node, source));
    let ty = if node.kind() == "variadic_parameter_declaration" {
        format!("...{ty}")
    } else {
        ty
    };

    let mut cursor = node.walk();
    let names = node.children_by_field_name("name", &mut cursor).count();
    vec![ty; names.max(1)]
}

/// Trailing cv/ref/noexcept qualifiers of a C/C++ declarator.
///
/// The scan starts after the parameter list's closing `)` and stops at the
/// first token that is not one of `const`, `volatile`, `&`, `&&`, `noexcept` or
/// `noexcept(...)`. That keeps a trailing return type (`-> int`) out of the
/// identity. The returned slice is untrimmed source text so callers can
/// normalize it; it is empty when there is no declarator (every non-C/C++
/// language) or nothing but a non-qualifier follows.
///
/// Bounded in both bytes visited and paren depth; malformed or truncated input
/// yields `""` instead of panicking.
fn qualifiers<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    let Some(declarator) = declarator_with_parameters(node) else {
        return "";
    };
    let Some(params) = declarator.child_by_field_name("parameters") else {
        return "";
    };
    let start = params.end_byte();
    let end = declarator.end_byte().min(source.len());
    if start >= end {
        return "";
    }
    let Some(text) = text_between(source, start, end) else {
        return "";
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut visited = 0usize;

    while i < bytes.len() && visited < 512 {
        visited += 1;
        let byte = bytes[i];
        if byte.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if byte == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }

        let mut stop = i;
        let accepted = match byte {
            b'&' => {
                stop = if bytes.get(i + 1) == Some(&b'&') {
                    i + 2
                } else {
                    i + 1
                };
                true
            }
            b'=' => false,
            _ if byte.is_ascii_alphabetic() || byte == b'_' => {
                while stop < bytes.len()
                    && (bytes[stop].is_ascii_alphanumeric() || bytes[stop] == b'_')
                {
                    stop += 1;
                }
                match &text[i..stop] {
                    "const" | "volatile" => true,
                    "noexcept" => {
                        // Optional `( condition )`, balanced and bounded.
                        let mut j = stop;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        if bytes.get(j) == Some(&b'(') {
                            let mut depth = 0usize;
                            let mut k = j;
                            let mut steps = 0usize;
                            while k < bytes.len() && steps < 256 {
                                steps += 1;
                                match bytes[k] {
                                    b'(' => depth += 1,
                                    b')' => {
                                        depth = depth.saturating_sub(1);
                                        if depth == 0 {
                                            k += 1;
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                                k += 1;
                            }
                            stop = k;
                        }
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if !accepted {
            break;
        }
        first.get_or_insert(i);
        last = stop;
        i = stop;
    }

    first.map_or("", |start| &text[start..last])
}

/// The declarator node that owns a parameter list, if the grammar hides both
/// under a C/C++ declarator chain.
fn declarator_with_parameters(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.child_by_field_name("declarator")?;
    for _ in 0..16 {
        if current.child_by_field_name("parameters").is_some() {
            return Some(current);
        }
        current = current.child_by_field_name("declarator")?;
    }
    None
}

/// Normalize the raw trailing-qualifier slice into its canonical spelling.
///
/// `const &` and `const&` both become `const&`, while a following qualifier is
/// space-separated: `const& noexcept`, `const&& noexcept(true)`.
fn normalized_qualifiers(raw: &str) -> String {
    let without_comments = strip_comments(raw);
    let mut out = String::new();
    for token in without_comments.split_whitespace() {
        if !out.is_empty() && token != "&" && token != "&&" {
            out.push(' ');
        }
        out.push_str(token);
    }
    out
}

/// C++/Rust template context: `<T>` / `<A,B>` appended after the name.
///
/// C++ reads the enclosing `template_declaration` parameter lists (outermost
/// first, so a nested member template reads `<T,U>`); Rust reads the function's
/// own `type_parameters`. Declared template **names** are used, not their
/// bounds and defaults, so renaming a bound or changing `typename` to `class`
/// does not churn identity.
///
/// Known limit: a C++ template type parameter that the grammar cannot expose
/// (for example inside a macro-generated declaration) is omitted rather than
/// hashed; the resulting identity is less specific but stable.
fn template_args(node: Node<'_>, lang: Lang, source: &[u8]) -> String {
    let mut names: Vec<String> = Vec::new();
    match lang {
        Lang::Rust => {
            if let Some(list) = node.child_by_field_name("type_parameters") {
                collect_rust_template_names(list, source, &mut names);
            }
        }
        Lang::Cpp => {
            let mut current = node.parent();
            for _ in 0..32 {
                let Some(parent) = current else {
                    break;
                };
                if parent.kind() == "template_declaration" {
                    if let Some(list) = parent.child_by_field_name("parameters") {
                        collect_cpp_template_names(list, source, &mut names);
                    }
                }
                current = parent.parent();
            }
            // Collected innermost-first; spell the outermost template first.
            names.reverse();
        }
        _ => {}
    }
    if names.is_empty() {
        String::new()
    } else {
        format!("<{}>", names.join(","))
    }
}

fn collect_cpp_template_names(list: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        let name = match child.kind() {
            "parameter_declaration"
            | "optional_parameter_declaration"
            | "variadic_parameter_declaration" => cpp_parameter_text(child, source),
            "template_template_parameter_declaration" => child
                .child_by_field_name("parameters")
                .and_then(|nested| nested_template_parameter_name(nested, source))
                .unwrap_or_default(),
            _ => template_parameter_name(child, source).unwrap_or_default(),
        };
        if !name.is_empty() {
            out.push(name);
        }
    }
}

/// Declared name of a C++ template type parameter.
///
/// `optional_type_parameter_declaration` exposes `name`; the plain and
/// variadic forms only carry a `type_identifier` child.
fn template_parameter_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        if let Some(text) = field_text(name, source) {
            return Some(squeeze_whitespace(&text));
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children
        .iter()
        .rev()
        .find(|child| {
            matches!(
                child.kind(),
                "type_identifier" | "identifier" | "type_descriptor"
            )
        })
        .and_then(|child| child.utf8_text(source).ok())
        .map(|text| squeeze_whitespace(text.trim()))
}

/// First nested type-parameter declaration inside a template-template
/// parameter (`template <template <class> class C>` -> `C`).
fn nested_template_parameter_name(list: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if child.kind().contains("type_parameter") {
            return template_parameter_name(child, source);
        }
    }
    None
}

fn collect_rust_template_names(list: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if let Some(name) = child.child_by_field_name("name") {
            if let Some(text) = field_text(name, source) {
                out.push(squeeze_whitespace(&text));
            }
        }
    }
}

/// Slice `source[start..end]` as `&str`, or `None` on any out-of-range or
/// non-UTF-8 boundary. Never panics.
fn text_between(source: &[u8], start: usize, end: usize) -> Option<&str> {
    if start >= end || end > source.len() {
        return None;
    }
    std::str::from_utf8(source.get(start..end)?).ok()
}

/// Punctuation that never needs surrounding spaces in an identity.
const IDENTITY_PUNCT: &[char] = &[',', '(', ')', '[', ']', '*', '&', '<', '>'];

/// Collapse whitespace runs and drop spaces adjacent to [`IDENTITY_PUNCT`].
///
/// `const char * p` and `const char*p` become the same string, and so do
/// `int a[3]` and `int a [ 3 ]`, which is what makes identity survive
/// reformatting.
fn squeeze_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if IDENTITY_PUNCT.contains(&ch) {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push(ch);
            pending_space = false;
            continue;
        }
        if pending_space
            && out
                .chars()
                .last()
                .is_some_and(|last| !IDENTITY_PUNCT.contains(&last))
        {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Remove `//...` and `/*...*/` comments from a parameter list.
///
/// Deliberately textual: a comment can sit between two parameters and would
/// otherwise make the same signature compare unequal across revisions.
fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            let len = utf8_char_len(bytes[i]);
            let end = (i + len).min(bytes.len());
            out.push_str(&text[i..end]);
            i = end;
        }
    }
    out
}

/// Length in bytes of the UTF-8 character starting with `first`.
///
/// `text` is a valid `&str`, so a leading byte fully determines the width.
fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
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
    parameter_node(node, table).map_or(0, count_params)
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
        let closure = function_named(&file, "f");
        assert_eq!(closure.ccn, 2);
        assert_eq!(closure.line, 2);
        assert_eq!(closure.identity, "main()::lambda:f(i32)");
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
        let lambda = function_named(&file, "g");
        assert_eq!(lambda.ccn, 2);
        assert_eq!(lambda.params, 1);
        assert_eq!(lambda.line, 1);
        assert_eq!(lambda.identity, "f()::lambda:g(int)");
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

    // -- Partial parsing (C/C++) ------------------------------------------

    /// Analyze without asserting `Exact`: partial parses are `BestEffort`.
    fn analyze_partial(src: &str, lang: Lang) -> FileCcn {
        analyze_file(Path::new("fixture"), src.as_bytes(), lang)
            .unwrap_or_else(|e| panic!("{} fixture must yield functions: {e}", lang.name()))
    }

    fn names(file: &FileCcn) -> Vec<&str> {
        file.functions.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn cpp_clean_file_is_exact() {
        let file = analyze(
            "int f(int x) { return x; }\nint g(int x) { if (x) { return 1; } return 0; }\n",
            Lang::Cpp,
        );
        assert_eq!(file.tier, CcnTier::Exact);
        assert_eq!(names(&file), vec!["f", "g"]);
        assert_eq!(function_named(&file, "g").ccn, 2);

        let c = analyze("int f(int x) { if (x) { return 1; } return 0; }\n", Lang::C);
        assert_eq!(c.tier, CcnTier::Exact);
        assert_eq!(ccn_of("int f(int x) { return x; }", Lang::C, "f"), 1);
    }

    #[test]
    fn cpp_truncated_file_keeps_the_complete_functions() {
        // The last function is cut in half: an unbalanced brace at EOF.
        let src = "int a(void) { return 1; }\n\
                   int b(int x) { if (x) { return 2; } return 3; }\n\
                   int truncated(void) {\n";
        let file = analyze_partial(src, Lang::Cpp);
        assert_eq!(file.tier, CcnTier::BestEffort);
        let found = names(&file);
        assert!(found.contains(&"a"), "found: {found:?}");
        assert!(found.contains(&"b"), "found: {found:?}");
        assert_eq!(function_named(&file, "b").ccn, 2);
    }

    #[test]
    fn c_macro_heavy_preamble_still_yields_functions() {
        // `DECLARE_LIST(, int)` is not valid C; tree-sitter puts it in an
        // ERROR node, but the function after it is intact.
        let src = "#define BEGIN(x) do { x; } while (0)\n\
                   #define LIKELY(x) __builtin_expect(!!(x), 1)\n\
                   #define DECLARE_LIST(name, type) type name##_a; type name##_b\n\
                   DECLARE_LIST(, int)\n\
                   int first(int x) { BEGIN(x++); return x; }\n\
                   int second(int x) { if (LIKELY(x > 0)) { return 1; } return 0; }\n";
        let file = analyze_partial(src, Lang::C);
        let found = names(&file);
        assert!(found.contains(&"first"), "found: {found:?}");
        assert!(found.contains(&"second"), "found: {found:?}");
        assert_eq!(function_named(&file, "second").ccn, 2);
        assert!(
            matches!(file.tier, CcnTier::Exact | CcnTier::BestEffort),
            "macro noise must not drop the file to the heuristic tier, got {:?}",
            file.tier
        );
    }

    #[test]
    fn c_if_zero_body_is_not_counted_but_following_code_is() {
        let src = "int kept(void) { return 1; }\n\
                   #if 0\n\
                   int dead(void) { return 2; }\n\
                   #endif\n\
                   int after(void) { return 3; }\n";
        let file = analyze(src, Lang::C);
        assert_eq!(file.tier, CcnTier::Exact);
        let found = names(&file);
        assert!(!found.contains(&"dead"), "found: {found:?}");
        assert!(found.contains(&"kept"), "found: {found:?}");
        assert!(found.contains(&"after"), "found: {found:?}");

        // Nested inside a live branch: still dead.
        let nested = "int live(void) {\n\
                      #if 0\n\
                      if (1) { return 2; }\n\
                      #endif\n\
                      return 0;\n\
                      }\n";
        assert_eq!(ccn_of(nested, Lang::C, "live"), 1);
    }

    #[test]
    fn c_if_zero_else_branch_is_live() {
        let src = "#if 0\n\
                   int dead(void) { return 0; }\n\
                   #else\n\
                   int alive(void) { return 1; }\n\
                   #endif\n";
        let file = analyze(src, Lang::C);
        let found = names(&file);
        assert!(found.contains(&"alive"), "found: {found:?}");
        assert!(!found.contains(&"dead"), "found: {found:?}");

        // The same shape in C++, with an `#elif`.
        let cpp = "#if 0\n\
                   int dead(void) { return 0; }\n\
                   #elif 1\n\
                   int alive(void) { return 1; }\n\
                   #else\n\
                   int also_dead(void) { return 2; }\n\
                   #endif\n";
        let file = analyze(cpp, Lang::Cpp);
        let found = names(&file);
        assert!(found.contains(&"alive"), "found: {found:?}");
        assert!(!found.contains(&"dead"), "found: {found:?}");
        assert!(!found.contains(&"also_dead"), "found: {found:?}");
    }

    #[test]
    fn c_if_one_else_branch_is_dead() {
        let src = "#if 1\n\
                   int alive(void) { return 1; }\n\
                   #else\n\
                   int dead(void) { return 2; }\n\
                   #endif\n";
        let file = analyze(src, Lang::C);
        let found = names(&file);
        assert!(found.contains(&"alive"), "found: {found:?}");
        assert!(!found.contains(&"dead"), "found: {found:?}");

        // `#if false` is the spelled-out form.
        let spelled = "#if false\n\
                       int dead(void) { return 2; }\n\
                       #else\n\
                       int alive(void) { return 1; }\n\
                       #endif\n";
        assert!(names(&analyze(spelled, Lang::C)).contains(&"alive"));
    }

    #[test]
    fn c_ifdef_keeps_both_branches() {
        let src = "#ifdef FEATURE\n\
                   int when_defined(void) { return 1; }\n\
                   #else\n\
                   int when_undefined(void) { return 2; }\n\
                   #endif\n";
        let file = analyze(src, Lang::C);
        let found = names(&file);
        assert!(found.contains(&"when_defined"), "found: {found:?}");
        assert!(found.contains(&"when_undefined"), "found: {found:?}");

        let cpp = "#ifndef FEATURE\n\
                   int when_undefined(void) { return 2; }\n\
                   #else\n\
                   int when_defined(void) { return 1; }\n\
                   #endif\n";
        let cpp_file = analyze(cpp, Lang::Cpp);
        let found = names(&cpp_file);
        assert!(found.contains(&"when_defined"), "found: {found:?}");
        assert!(found.contains(&"when_undefined"), "found: {found:?}");
    }

    #[test]
    fn cpp_error_only_input_is_an_error_not_a_panic() {
        for src in ["}{ )(", "@@@ ###", "int f( {"] {
            let err = analyze_file(Path::new("broken.cpp"), src.as_bytes(), Lang::Cpp)
                .expect_err("no function recovered must be an error");
            assert!(
                matches!(
                    err,
                    CcnError::ParseFailed {
                        lang: Lang::Cpp,
                        ..
                    }
                ),
                "{src:?}: unexpected error {err:?}"
            );
        }
        let err = analyze_file(Path::new("broken.c"), b"}{ )(", Lang::C)
            .expect_err("no function recovered must be an error");
        assert!(matches!(err, CcnError::ParseFailed { lang: Lang::C, .. }));
    }

    #[test]
    fn c_hybrid_partial_parse_is_best_effort_with_the_error_child_left_out() {
        // `BROKEN();` cannot be parsed, but the surrounding decisions are real
        // and must be counted; the ERROR subtree itself adds nothing.
        let src = "int f(int x) {\n\
                   if (x) { return 1; }\n\
                   BROKEN_THING(;\n\
                   if (x > 2) { return 2; }\n\
                   return 0;\n\
                   }\n";
        let file = analyze_partial(src, Lang::C);
        assert_eq!(file.tier, CcnTier::BestEffort);
        assert_eq!(names(&file), vec!["f"]);
        assert_eq!(
            function_named(&file, "f").ccn,
            3,
            "both `if`s outside the ERROR node still count"
        );
    }

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

    // -- Maximum nesting --------------------------------------------------

    fn nesting_of(src: &str, lang: Lang, name: &str) -> u32 {
        function_named(&analyze(src, lang), name).max_nesting
    }

    #[test]
    fn nesting_flat_function_is_zero() {
        assert_eq!(
            nesting_of("int f(int x) { return x + 1; }", Lang::C, "f"),
            0
        );
        assert_eq!(
            nesting_of("function f() { return g(); }", Lang::JavaScript, "f"),
            0
        );
    }

    #[test]
    fn nesting_single_if_is_one() {
        assert_eq!(
            nesting_of(
                "int f(int x) { if (x) { return 1; } return 0; }",
                Lang::C,
                "f"
            ),
            1
        );
    }

    #[test]
    fn nesting_if_containing_for_is_two() {
        assert_eq!(
            nesting_of(
                "int f(int n) { int s = 0; if (n > 0) { for (int i = 0; i < n; i++) { s += i; } } return s; }",
                Lang::C,
                "f"
            ),
            2
        );
    }

    #[test]
    fn nesting_three_ifs_is_three() {
        assert_eq!(
            nesting_of(
                "int f(int a, int b, int c) { if (a) { if (b) { if (c) { return 1; } } } return 0; }",
                Lang::C,
                "f"
            ),
            3
        );
    }

    #[test]
    fn nesting_sibling_ifs_do_not_add_up() {
        assert_eq!(
            nesting_of(
                "int f(int a, int b) { if (a) { g(); } if (b) { g(); } return 0; }",
                Lang::C,
                "f"
            ),
            1,
            "two constructs at the same level are depth 1, not 2"
        );
    }

    #[test]
    fn nesting_switch_case_body_inherits_one_level() {
        // The case is the switch's level (1); the `if` inside it makes it 2.
        assert_eq!(
            nesting_of(
                "int f(int x, int y) { switch (x) { case 1: if (y) { return 2; } return 1; default: return 0; } }",
                Lang::C,
                "f"
            ),
            2
        );
        // Two flat cases stay at 1: a case never adds a level per sibling.
        assert_eq!(
            nesting_of(
                "int f(int x) { switch (x) { case 1: return 1; case 2: return 2; } return 0; }",
                Lang::C,
                "f"
            ),
            1
        );
    }

    #[test]
    fn nesting_nested_function_keeps_its_own_depth() {
        let file = analyze(
            "function outer() {\n  function inner() {\n    if (a) { if (b) { if (c) {} } }\n  }\n  if (d) { inner(); }\n}\n",
            Lang::JavaScript,
        );
        assert_eq!(function_named(&file, "outer").max_nesting, 1);
        assert_eq!(function_named(&file, "inner").max_nesting, 3);
    }

    #[test]
    fn nesting_python_if_inside_for_is_two() {
        assert_eq!(
            nesting_of(
                "def f(xs):\n    for x in xs:\n        if x:\n            return 1\n    return 0\n",
                Lang::Python,
                "f"
            ),
            2
        );
    }

    #[test]
    fn nesting_rust_match_arm_with_if_is_two() {
        assert_eq!(
            nesting_of(
                "fn f(x: Option<i32>) -> i32 { match x { Some(v) => { if v > 0 { 1 } else { 0 } }, None => 0 } }",
                Lang::Rust,
                "f"
            ),
            2
        );
    }

    #[test]
    fn nesting_if_zero_body_does_not_count() {
        let src = "int f(void) {\n\
                   #if 0\n\
                   if (1) { for (;;) { while (1) { } } }\n\
                   #endif\n\
                   return 0;\n\
                   }\n";
        assert_eq!(nesting_of(src, Lang::C, "f"), 0);
    }

    // -- Identities -------------------------------------------------------

    /// Sorted identity list, so a source reorder can be compared without
    /// depending on line order.
    fn sorted_identities(file: &FileCcn) -> Vec<&str> {
        let mut ids: Vec<&str> = file.functions.iter().map(|f| f.identity.as_str()).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn cpp_const_and_plain_overloads_have_distinct_identities() {
        // Definitions, not declarations: only a definition is measured. The
        // identity rule is what a declaration would use once defined.
        let file = analyze(
            "struct X { int f(int x) const { return x; } int f(int y) { return y; } };",
            Lang::Cpp,
        );
        assert_eq!(file.functions.len(), 2);
        assert_eq!(
            sorted_identities(&file),
            vec!["X::f(int)", "X::f(int) const"],
            "cv qualifier is identity"
        );
        assert_ne!(
            file.functions[0].identity, file.functions[1].identity,
            "the const and non-const overloads must not collide"
        );
        assert_eq!(
            file.functions
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["f", "f"],
            "the reported short name stays unqualified and unchanged"
        );
    }

    #[test]
    fn reordering_const_and_plain_overloads_changes_no_identity() {
        let plain_first = analyze(
            "struct X { int f(int y) { return y; } int f(int x) const { return x; } };",
            Lang::Cpp,
        );
        let const_first = analyze(
            "struct X { int f(int x) const { return x; } int f(int y) { return y; } };",
            Lang::Cpp,
        );
        assert_eq!(
            sorted_identities(&plain_first),
            sorted_identities(&const_first)
        );
        assert_eq!(
            sorted_identities(&const_first),
            vec!["X::f(int)", "X::f(int) const"]
        );
    }

    #[test]
    fn renaming_a_parameter_changes_no_identity() {
        let before = analyze(
            "int f(int x) { return x; }\nint f(double x) { return 0; }\n",
            Lang::Cpp,
        );
        let renamed = analyze(
            "int f(int y) { return y; }\nint f(double y) { return 0; }\n",
            Lang::Cpp,
        );
        assert_eq!(sorted_identities(&before), sorted_identities(&renamed));
        assert_eq!(sorted_identities(&renamed), vec!["f(double)", "f(int)"]);
    }

    #[test]
    fn cpp_pointer_and_rvalue_reference_parameters_keep_their_types() {
        let f = only("int f(const char* p, T&& v) { return 0; }", Lang::Cpp);
        assert_eq!(f.identity, "f(const char*,T&&)");
    }

    #[test]
    fn cpp_parameter_names_are_never_part_of_the_identity() {
        // `int (*cb)(int)` demonstrates that pointer/array declarators keep
        // their shape while only the declared name disappears.
        let f = only(
            "int f(int x, const char * p, int a[3], int (*cb)(int)) { return 0; }",
            Lang::Cpp,
        );
        assert_eq!(f.identity, "f(int,const char*,int[3],int(*)(int))");
    }

    #[test]
    fn cpp_trailing_noexcept_and_ref_qualifiers_are_identity() {
        let file = analyze(
            "struct X { \
             void a() const & { } \
             void b() && noexcept { } \
             void c() const noexcept(true) { } \
             };",
            Lang::Cpp,
        );
        assert_eq!(
            sorted_identities(&file),
            vec![
                "X::a() const&",
                "X::b() && noexcept",
                "X::c() const noexcept(true)"
            ]
        );
    }

    #[test]
    fn bound_lambdas_take_the_variable_name_not_a_line_number() {
        // File-scope lambdas: no enclosing function, so the identity is the
        // binding context alone. (The grammar parses `auto x = [] ...` as a
        // declaration even though C++ requires function scope.)
        let file = analyze(
            "auto a = [](int x){ return x; };\nauto b = [](int x){ return x; };\n",
            Lang::Cpp,
        );
        assert_eq!(file.functions.len(), 2);
        assert_eq!(
            file.functions
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            sorted_identities(&file),
            vec!["lambda:a(int)", "lambda:b(int)"]
        );

        // Swapping the two declarations swaps the reported lines but not the
        // identities attached to the bindings.
        let swapped = analyze(
            "auto b = [](int x){ return x; };\nauto a = [](int x){ return x; };\n",
            Lang::Cpp,
        );
        assert_eq!(sorted_identities(&file), sorted_identities(&swapped));
        assert_eq!(
            sorted_identities(&swapped),
            vec!["lambda:a(int)", "lambda:b(int)"]
        );
    }

    #[test]
    fn unbound_lambdas_stay_anonymous_without_a_line_number() {
        let file = analyze("void f() { g([](int x){ return x; }); }", Lang::Cpp);
        let lambda = file
            .functions
            .iter()
            .find(|f| f.name == "(anonymous)")
            .expect("unbound lambda stays anonymous");
        assert_eq!(lambda.identity, "(anonymous)(int)");
        assert!(!lambda.identity.contains(&format!(":{}", lambda.line)));
    }

    #[test]
    fn nested_bound_lambdas_include_the_enclosing_identity() {
        let file = analyze(
            "void f() { auto outer = [](int x){ auto inner = [](double y){ return y; }; return x; }; }",
            Lang::Cpp,
        );
        let outer = function_named(&file, "outer");
        let inner = function_named(&file, "inner");
        assert_eq!(outer.identity, "f()::lambda:outer(int)");
        assert_eq!(
            inner.identity,
            "f()::lambda:outer(int)::lambda:inner(double)"
        );
    }

    #[test]
    fn cpp_template_context_is_part_of_the_identity() {
        let f = only("template <typename T> T f(T x) { return x; }", Lang::Cpp);
        assert_eq!(f.identity, "f<T>(T)");

        // A method inside a class template carries the class template
        // parameters; the nested member template adds its own, outermost
        // first.
        let file = analyze(
            "template <class T> struct S { template <class U> void m(U u, T t) { } };",
            Lang::Cpp,
        );
        assert_eq!(function_named(&file, "m").identity, "S::m<T,U>(U,T)");
    }

    #[test]
    fn rust_generic_context_and_references_are_identity() {
        let file = analyze(
            "fn g<A: Clone, B>(a: A, b: &B) -> B { b.clone() }",
            Lang::Rust,
        );
        assert_eq!(function_named(&file, "g").identity, "g<A,B>(A,&B)");
    }

    #[test]
    fn rust_receiver_is_preserved_but_parameter_names_are_not() {
        let file = analyze(
            "struct Type;\nimpl Type {\n    fn m(&self, x: i32) { let _ = x; }\n}\n",
            Lang::Rust,
        );
        let f = function_named(&file, "m");
        assert_eq!(f.name, "m");
        assert_eq!(f.identity, "Type::m(&self,i32)");
    }

    #[test]
    fn java_identity_uses_types_and_preserves_varargs() {
        let f = only("class A { void m(int x, String s) { } }", Lang::Java);
        assert_eq!(f.name, "m");
        assert_eq!(f.identity, "A::m(int,String)");

        let varargs = only("class A { void m(int x, String... s) { } }", Lang::Java);
        assert_eq!(varargs.identity, "A::m(int,String...)");
    }

    #[test]
    fn python_untyped_parameters_are_positional_markers() {
        let f = only(
            "class A:\n    def m(self, x, y=1):\n        return x\n",
            Lang::Python,
        );
        assert_eq!(f.name, "m");
        assert_eq!(f.identity, "A::m(_,_,_)");

        // An annotation is preferred when it exists.
        let typed = only(
            "class A:\n    def m(self, x: int, y: str):\n        return x\n",
            Lang::Python,
        );
        assert_eq!(typed.identity, "A::m(_,int,str)");
    }

    #[test]
    fn js_and_ts_lambdas_use_bindings_and_annotations() {
        let untyped = only("const f = (x, y) => x;", Lang::JavaScript);
        assert_eq!(untyped.identity, "lambda:f(_,_)");

        let typed = only("function f(x: number, y?: string) {}", Lang::TypeScript);
        assert_eq!(typed.identity, "f(number,string)");
    }

    #[test]
    fn go_grouped_names_are_one_parameter_each_and_variadics_are_kept() {
        let f = only(
            "package p\n\nfunc f(x int, y, z string, rest ...int) {}\n",
            Lang::Go,
        );
        assert_eq!(f.identity, "f(int,string,string,...int)");
    }

    #[test]
    fn a_true_body_change_leaves_the_identity_alone() {
        let before = only("int f(int x) { return x; }", Lang::C);
        let after = only(
            "int f(int x) { if (x) { x++; } if (x > 2) { x++; } return x; }",
            Lang::C,
        );
        assert_eq!(before.identity, after.identity);
        assert_ne!(before.ccn, after.ccn, "only complexity changed");
    }

    #[test]
    fn malformed_cpp_identity_extraction_does_not_panic() {
        // Truncated parameter list: tree-sitter may recover the function or
        // report an error. Either outcome is acceptable; a panic is not.
        match analyze_file(Path::new("broken.cpp"), b"int f(int x) { if (", Lang::Cpp) {
            Ok(file) => {
                for function in &file.functions {
                    // Must have produced a string, and no panic occurred.
                    assert!(!function.name.is_empty());
                }
            }
            Err(err) => assert!(matches!(err, CcnError::ParseFailed { .. })),
        }
    }

    #[test]
    fn cpp_identity_qualifies_namespace_and_class_but_the_name_does_not() {
        let file = analyze(
            "namespace N { class X { int m(int v) { return v; } }; }",
            Lang::Cpp,
        );
        assert_eq!(file.functions.len(), 1);
        let f = &file.functions[0];
        assert_eq!(f.name, "m");
        assert_eq!(f.identity, "N::X::m(int)");
        assert!(f.identity.starts_with("N::X::"));
    }

    #[test]
    fn anonymous_functions_share_an_identity_and_are_split_by_line() {
        let file = analyze(
            "const xs = [\n  function (x) { return x; },\n  function (x) { return x; },\n];\n",
            Lang::JavaScript,
        );
        assert_eq!(file.functions.len(), 2);
        assert_eq!(file.functions[0].name, "(anonymous)");
        assert_eq!(file.functions[1].name, "(anonymous)");
        assert_eq!(
            file.functions[0].identity, file.functions[1].identity,
            "identical signatures collide on purpose"
        );
        assert_eq!(file.functions[0].identity, "(anonymous)(_)");
        assert_ne!(
            file.functions[0].line, file.functions[1].line,
            "matching resolves the collision by source order"
        );
    }

    #[test]
    fn parameter_whitespace_and_comments_are_normalized_away() {
        let a = only(
            "int f( int  x , const char * p /* keep me */ ) { return 0; }",
            Lang::Cpp,
        );
        let b = only("int f(int y, const char*p) { return 0; }", Lang::Cpp);
        assert_eq!(a.identity, b.identity);
        assert_eq!(a.identity, "f(int,const char*)");
    }
}
