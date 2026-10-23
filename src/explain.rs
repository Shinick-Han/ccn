//! Explain why a function's CCN is high.
//!
//! The CCN itself is tree-sitter-exact. This module is an *explanation aid*,
//! not a second score: it slices the function's source text and counts the
//! decision tokens it can see with a small, dependency-free heuristic. The
//! result is always presented as a `heuristic breakdown`, because the text
//! scan cannot see macros, templates or the language's real grammar.
//!
//! The scan is deliberately conservative:
//!
//! * `//`, `/* ... */`, string and char literals are blanked out first, so a
//!   commented-out `if` or an `"&&"` inside a message is not counted;
//! * lines that look like preprocessor directives (leading `#`) are skipped,
//!   so `#if` in C/C++ is not mistaken for a branch;
//! * `else if` contributes one decision (`if`) because `else` is not a
//!   decision token at all;
//! * `?` is only counted when it is not part of `??`, `?:` or a `:`-adjacent
//!   construct such as `::`.

/// One kind of decision point found in the function text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Token label, e.g. `if`, `&&` or `?`.
    pub label: &'static str,
    /// Number of occurrences.
    pub count: usize,
    /// 1-based file lines (not function-relative) of each occurrence.
    pub lines: Vec<usize>,
}

/// Heuristic breakdown of one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    pub function: String,
    pub ccn: u32,
    pub nesting: u32,
    /// Non-blank lines inside the clamped function range.
    pub nloc: usize,
    /// Non-zero decisions, sorted by count descending then label.
    pub decisions: Vec<Decision>,
    /// First few decision sites, in source order.
    pub top_lines: Vec<(usize, &'static str)>,
}

/// How many decision sites `top_lines` carries.
const TOP_LIMIT: usize = 6;

/// Labels the heuristic understands, in display-independent order.
const TOKENS: [&str; 10] = [
    "if", "for", "while", "do", "case", "catch", "switch", "&&", "||", "?",
];

/// Explain `ccn` for `function` from its inclusive 1-based source range.
///
/// `start_line` and `end_line` are clamped to the file. Reversed or
/// out-of-file ranges produce an empty breakdown rather than an error.
pub fn explain(
    source: &str,
    start_line: usize,
    end_line: usize,
    function: &str,
    ccn: u32,
    nesting: u32,
) -> Explanation {
    let all: Vec<&str> = source.lines().collect();
    let empty = || Explanation {
        function: function.to_string(),
        ccn,
        nesting,
        nloc: 0,
        decisions: Vec::new(),
        top_lines: Vec::new(),
    };

    let total = all.len();
    let start = start_line.max(1);
    if total == 0 || start > total {
        return empty();
    }
    let end = end_line.min(total);
    if end < start {
        return empty();
    }

    let range = &all[start - 1..end];
    let nloc = range.iter().filter(|line| !line.trim().is_empty()).count();

    let cleaned = strip_noise(&range.join("\n"));
    let cleaned_lines: Vec<&str> = cleaned.split('\n').collect();

    // `(line, label)` in source order, which makes `top_lines` trivial.
    let mut sites: Vec<(usize, &'static str)> = Vec::new();
    for (offset, line) in cleaned_lines.iter().enumerate() {
        // Preprocessor directives are not code paths here.
        if line.trim_start().starts_with('#') {
            continue;
        }
        let lineno = start + offset;
        for label in scan_line(line) {
            sites.push((lineno, label));
        }
    }

    let mut decisions = Vec::new();
    for label in TOKENS {
        let lines: Vec<usize> = sites
            .iter()
            .filter(|(_, found)| *found == label)
            .map(|(line, _)| *line)
            .collect();
        if !lines.is_empty() {
            decisions.push(Decision {
                label,
                count: lines.len(),
                lines,
            });
        }
    }
    decisions.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(b.label)));

    let top_lines = sites.iter().take(TOP_LIMIT).copied().collect();

    Explanation {
        function: function.to_string(),
        ccn,
        nesting,
        nloc,
        decisions,
        top_lines,
    }
}

/// Blank comments and simple string/char literals while preserving every
/// newline, so cleaned line indices still match the source.
fn strip_noise(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut block_comment = false;
    let mut string = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if block_comment {
            if c == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                out.push_str("  ");
                i += 2;
                block_comment = false;
            } else if c == '\n' {
                out.push('\n');
                i += 1;
            } else {
                out.push(' ');
                i += 1;
            }
            continue;
        }

        if string {
            match c {
                '\\' => {
                    out.push(' ');
                    if i + 1 < chars.len() {
                        // An escaped newline is still a newline for the caller.
                        if chars[i + 1] == '\n' {
                            out.push('\n');
                        } else {
                            out.push(' ');
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                '"' => {
                    out.push(' ');
                    i += 1;
                    string = false;
                }
                '\n' => {
                    out.push('\n');
                    i += 1;
                }
                _ => {
                    out.push(' ');
                    i += 1;
                }
            }
            continue;
        }

        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            out.push_str("  ");
            i += 2;
            block_comment = true;
            continue;
        }
        if c == '"' {
            out.push(' ');
            i += 1;
            string = true;
            continue;
        }
        if c == '\'' {
            // Only treat `'` as a char literal when it really closes soon;
            // otherwise it may be a Rust lifetime such as `'static`.
            if let Some(end) = char_literal_end(&chars, i) {
                for _ in i..end {
                    out.push(' ');
                }
                i = end;
                continue;
            }
        }

        out.push(c);
        i += 1;
    }

    out
}

/// End index (exclusive) of a char literal starting at `i`, if it is one.
fn char_literal_end(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j >= chars.len() || chars[j] == '\n' {
        return None;
    }
    if chars[j] == '\\' {
        j += 1;
        if j >= chars.len() {
            return None;
        }
        if chars[j] == 'u' && j + 1 < chars.len() && chars[j + 1] == '{' {
            j += 2;
            while j < chars.len() && chars[j] != '}' {
                j += 1;
            }
            if j >= chars.len() {
                return None;
            }
            j += 1;
        } else if chars[j] == 'x' {
            j += 1;
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
        } else {
            j += 1;
        }
    } else {
        j += 1;
    }
    (j < chars.len() && chars[j] == '\'').then_some(j + 1)
}

/// Decision tokens on one cleaned line, in source order.
fn scan_line(line: &str) -> Vec<&'static str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let label = match &line[start..i] {
                "if" => Some("if"),
                "for" => Some("for"),
                "while" => Some("while"),
                "do" => Some("do"),
                "case" => Some("case"),
                "catch" => Some("catch"),
                "switch" => Some("switch"),
                _ => None,
            };
            if let Some(label) = label {
                out.push(label);
            }
            continue;
        }

        if b == b'&' && i + 1 < bytes.len() && bytes[i + 1] == b'&' {
            out.push("&&");
            i += 2;
            continue;
        }
        if b == b'|' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            out.push("||");
            i += 2;
            continue;
        }
        if b == b'?' {
            let prev = if i > 0 { bytes[i - 1] } else { 0 };
            let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
            // `??` (null coalescing), `?:` (elvis / optional property) and a
            // `:` on either side are not ternary branches.
            if prev != b'?' && next != b'?' && prev != b':' && next != b':' {
                out.push("?");
            }
        }

        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(explanation: &Explanation) -> Vec<(&'static str, usize)> {
        explanation
            .decisions
            .iter()
            .map(|decision| (decision.label, decision.count))
            .collect()
    }

    #[test]
    fn counts_if_for_and_boolean_operators() {
        let source = "\
int f(int a, int b) {
    if (a && b) {
        for (int i = 0; i < 10; i++) {
            a++;
        }
    }
    return a;
}
";
        let explanation = explain(source, 1, 7, "f", 4, 2);
        assert_eq!(labels(&explanation), vec![("&&", 1), ("for", 1), ("if", 1)]);
        assert_eq!(explanation.ccn, 4);
        assert_eq!(explanation.nesting, 2);
        assert_eq!(explanation.nloc, 7);
        assert_eq!(explanation.decisions[0].lines, vec![2]);
        assert_eq!(
            explanation.top_lines,
            vec![(2, "if"), (2, "&&"), (3, "for")]
        );
    }

    #[test]
    fn else_if_counts_once_as_if() {
        let source = "\
if (a) {
} else if (b) {
} else if (c) {
}
";
        let explanation = explain(source, 1, 4, "f", 4, 0);
        // `else` is not a token: each `else if` contributes exactly one `if`.
        assert_eq!(labels(&explanation), vec![("if", 3)]);
        assert_eq!(explanation.decisions[0].lines, vec![1, 2, 3]);
    }

    #[test]
    fn nested_decisions_and_line_numbers_are_absolute() {
        let source = "\
// header
// header
fn f(a: bool, b: bool) {
    if a && b {
        while a || b {
            if a {
                a = false;
            }
        }
    }
}
";
        let explanation = explain(source, 3, 10, "f", 5, 3);
        assert_eq!(
            labels(&explanation),
            vec![("if", 2), ("&&", 1), ("while", 1), ("||", 1)]
        );
        assert_eq!(explanation.decisions[0].lines, vec![4, 6]);
        assert_eq!(explanation.top_lines[0], (4, "if"));
        assert_eq!(explanation.top_lines[1], (4, "&&"));
        assert_eq!(explanation.top_lines[2], (5, "while"));
        assert_eq!(explanation.top_lines[3], (5, "||"));
    }

    #[test]
    fn comments_and_literals_are_ignored() {
        let source = "\
fn f() {
    // if a && b
    /* for (;;) { while (x || y) {} } */
    let s = \"if a && b ? c : d\";
    let c = '?';
    let message = \"switch/case/catch\";
    if true {
        do_work();
    }
}
";
        let explanation = explain(source, 1, 9, "f", 2, 1);
        assert_eq!(labels(&explanation), vec![("if", 1)]);
        assert_eq!(explanation.decisions[0].lines, vec![7]);
    }

    #[test]
    fn block_comments_span_lines_without_losing_numbering() {
        let source = "\
fn f() {
    /* if a
       && b
       for */
    if c {
    }
}
";
        let explanation = explain(source, 1, 6, "f", 2, 0);
        assert_eq!(labels(&explanation), vec![("if", 1)]);
        assert_eq!(explanation.decisions[0].lines, vec![5]);
    }

    #[test]
    fn preprocessor_lines_are_skipped() {
        let source = "\
#if defined(A) && defined(B)
    prepare();
#endif
    if (c) {
    }
}
";
        let explanation = explain(source, 1, 5, "f", 2, 0);
        assert_eq!(labels(&explanation), vec![("if", 1)]);
        assert_eq!(explanation.decisions[0].lines, vec![4]);
    }

    #[test]
    fn no_decisions_is_empty() {
        let source = "\
fn answer() -> i32 {
    42
}
";
        let explanation = explain(source, 1, 3, "answer", 1, 0);
        assert!(explanation.decisions.is_empty());
        assert!(explanation.top_lines.is_empty());
        assert_eq!(explanation.nloc, 3);
    }

    #[test]
    fn range_is_clamped_safely() {
        let source = "\
one
if a {
}
";
        let all = explain(source, 0, 999, "f", 2, 0);
        assert_eq!(labels(&all), vec![("if", 1)]);
        assert_eq!(all.nloc, 3);

        let only_one = explain(source, 1, 1, "f", 1, 0);
        assert!(only_one.decisions.is_empty());
        assert_eq!(only_one.nloc, 1);

        let past_end = explain(source, 10, 20, "f", 1, 0);
        assert!(past_end.decisions.is_empty());
        assert_eq!(past_end.nloc, 0);

        let reversed = explain(source, 3, 1, "f", 1, 0);
        assert!(reversed.decisions.is_empty());
        assert_eq!(reversed.nloc, 0);
    }

    #[test]
    fn top_lines_is_limited_to_six_sites_in_source_order() {
        let source = "\
if a {
}
if b {
}
if c {
}
if d {
}
if e {
}
if f {
}
if g {
}
";
        let explanation = explain(source, 1, 14, "f", 8, 0);
        assert_eq!(explanation.top_lines.len(), 6);
        assert_eq!(
            explanation.top_lines,
            vec![
                (1, "if"),
                (3, "if"),
                (5, "if"),
                (7, "if"),
                (9, "if"),
                (11, "if")
            ]
        );
    }

    #[test]
    fn optional_and_null_coalescing_question_marks_are_not_ternaries() {
        let source = "\
fn f(a: Option<i32>, b: Option<i32>) {
    let x = a?.map(|v| v + 1);
    let y = b ?? 0;
    let z = a.map(|v| v).unwrap_or(0);
    let w = if x.is_some() { 1 } else { 2 };
}
";
        let explanation = explain(source, 1, 5, "f", 3, 0);
        assert_eq!(labels(&explanation), vec![("?", 1), ("if", 1)]);
    }

    #[test]
    fn ternary_question_marks_do_not_collide_with_paths() {
        let source = "\
fn f(a: i32) -> i32 {
    let module = std::cmp::max(a, 0);
    let value = a > 0 ? a : 0;
    value
}
";
        let explanation = explain(source, 1, 4, "f", 2, 0);
        assert_eq!(labels(&explanation), vec![("?", 1)]);
    }

    #[test]
    fn all_supported_tokens_are_counted() {
        let source = "\
fn f(x: i32) -> i32 {
    switch x {
    case 1:
        break;
    }
    do { x += 1; } while x < 3;
    try_call() catch { }
    if x > 0 && x < 10 || x == 42 {
        1
    } else {
        0
    }
}
";
        let explanation = explain(source, 1, 11, "f", 9, 1);
        assert_eq!(
            labels(&explanation),
            vec![
                ("&&", 1),
                ("case", 1),
                ("catch", 1),
                ("do", 1),
                ("if", 1),
                ("switch", 1),
                ("while", 1),
                ("||", 1)
            ]
        );
    }

    #[test]
    fn words_are_matched_on_identifier_boundaries() {
        let source = "\
fn f() {
    let iffy = 1;
    let for_each = 2;
    let doing = 3;
    if iffy > 0 {
    }
}
";
        let explanation = explain(source, 1, 6, "f", 2, 0);
        assert_eq!(labels(&explanation), vec![("if", 1)]);
    }
}
