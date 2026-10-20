//! Tier-3 fallback: keyword CCN for languages we cannot parse.
//!
//! This is the `scc` approach, and it is honest about it: a file-level number
//! with no function breakdown. It exists so an unknown language still appears
//! in the report, labelled `heuristic`, instead of vanishing.
//!
//! It must be fast (one pass over bytes), allocation-free per token, and must
//! never panic on invalid UTF-8 (work on bytes, not `str`).

/// Count `1 + decision keywords` in `source`.
///
/// The caller supplies the language name only to pick a token set; unknown
/// names fall back to the universal set.
pub fn analyze_file(source: &[u8], _tokei_lang: &str) -> u32 {
    count_decisions(source).saturating_add(1)
}

/// Universal keyword set. Word-like tokens are matched on identifier
/// boundaries; operators are matched literally.
fn count_decisions(source: &[u8]) -> u32 {
    let mut count = 0u32;
    for kw in [
        b"if".as_slice(),
        b"for".as_slice(),
        b"while".as_slice(),
        b"case".as_slice(),
        b"catch".as_slice(),
        b"elif".as_slice(),
        b"except".as_slice(),
        b"match".as_slice(),
        b"switch".as_slice(),
    ] {
        count += count_word(source, kw);
    }
    count += count_bytes(source, b"&&");
    count += count_bytes(source, b"||");
    count
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Count `needle` where it is not adjacent to identifier characters.
fn count_word(haystack: &[u8], needle: &[u8]) -> u32 {
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !is_ident(haystack[i - 1]);
            let after = i + needle.len();
            let after_ok = after == haystack.len() || !is_ident(haystack[after]);
            if before_ok && after_ok {
                count += 1;
                i = after;
                continue;
            }
        }
        i += 1;
    }
    count
}

fn count_bytes(haystack: &[u8], needle: &[u8]) -> u32 {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_is_still_ccn_1() {
        assert_eq!(analyze_file(b"", "Unknown"), 1);
    }

    #[test]
    fn counts_if_and_boolean_operators() {
        let src = b"fn f(a: bool, b: bool) { if a && b { } }";
        // if + && = 2 decisions
        assert_eq!(analyze_file(src, "Rust"), 3);
    }

    #[test]
    fn word_tokens_respect_identifier_boundaries() {
        let src = b"let iffy = 1; let for_each = 2; if x { }";
        assert_eq!(analyze_file(src, "Rust"), 2, "only the real `if` counts");
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        let src: &[u8] = &[0xff, b'i', b'f', b' ', 0xfe, b'{'];
        assert_eq!(analyze_file(src, "Unknown"), 2);
    }

    #[test]
    fn double_ampersand_is_not_counted_twice_as_single() {
        let src = b"a && b";
        assert_eq!(count_bytes(src, b"&&"), 1);
    }
}
