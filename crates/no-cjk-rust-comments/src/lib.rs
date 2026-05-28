//! Scan Rust source for Han script inside lexer comment tokens only.

use std::{ops::Range, sync::LazyLock};

use regex::Regex;
use rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};

static HAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Han}").expect("Han regex"));

/// Byte ranges in `src` that belong to comment tokens (including doc comments).
pub fn comment_byte_ranges(src: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for token in tokenize(src, FrontmatterAllowed::No) {
        let start = cursor;
        let end = start + token.len as usize;
        if matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
        ) {
            out.push(start..end);
        }
        cursor = end;
    }
    out
}

/// Returns true if any comment token in `src` contains a Han character.
pub fn any_comment_contains_han(src: &str) -> bool {
    for r in comment_byte_ranges(src) {
        let slice = &src[r];
        if HAN.is_match(slice) {
            return true;
        }
    }
    false
}

/// 1-based line number for byte offset.
pub fn line_for_byte(src: &str, byte: usize) -> usize {
    src[..byte].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Diagnostics: (1-based line, excerpt of comment token, truncated).
pub fn diagnostics_for_comments_with_han(
    src: &str,
    max: usize,
) -> Vec<(usize, String)> {
    let mut v = Vec::new();
    for r in comment_byte_ranges(src) {
        let slice = &src[r.clone()];
        if HAN.is_match(slice) {
            let line = line_for_byte(src, r.start);
            let mut excerpt = slice.replace('\n', "\\n");
            if excerpt.chars().count() > 120 {
                excerpt = excerpt.chars().take(120).collect::<String>() + "…";
            }
            v.push((line, excerpt));
            if v.len() >= max {
                break;
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_han_is_ignored() {
        let src = r##"fn main() { let _ = "中文"; }"##;
        assert!(!any_comment_contains_han(src));
    }

    #[test]
    fn line_comment_han_fails() {
        let src = "// 你好\nfn f() {}\n";
        assert!(any_comment_contains_han(src));
    }

    #[test]
    fn doc_comment_han_fails() {
        let src = "/// 模块说明\npub fn f() {}\n";
        assert!(any_comment_contains_han(src));
    }

    #[test]
    fn block_comment_han_fails() {
        let src = "fn f() { /* 说明 */ }\n";
        assert!(any_comment_contains_han(src));
    }
}
