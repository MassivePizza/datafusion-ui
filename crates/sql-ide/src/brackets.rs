//! Matching-paren lookup for the editor's bracket highlight.

use crate::lex::{TokenKind, lex};

/// The `(`/`)` pair the caret touches, as 1-based `(line, char column)` token
/// positions `[open, close]`. A caret directly after a bracket or directly
/// before one both count. Brackets inside strings/comments are invisible to the
/// lexer and never match.
pub fn matching_bracket(sql: &str, line: u64, column: u64) -> Option<[(u64, u64); 2]> {
    let brackets: Vec<(bool, u64, u64)> = lex(sql)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Punct && (t.text == "(" || t.text == ")"))
        .map(|t| (t.text == "(", t.start_line, t.start_col))
        .collect();

    // Prefer the bracket just before the caret, then the one at the caret.
    let at = |l: u64, c: u64| brackets.iter().position(|b| b.1 == l && b.2 == c);
    let idx = column
        .checked_sub(1)
        .filter(|c| *c >= 1)
        .and_then(|c| at(line, c))
        .or_else(|| at(line, column))?;

    let (is_open, l, c) = brackets[idx];
    let mut depth = 0i32;
    if is_open {
        for &(open, ml, mc) in &brackets[idx..] {
            depth += if open { 1 } else { -1 };
            if depth == 0 {
                return Some([(l, c), (ml, mc)]);
            }
        }
    } else {
        for &(open, ml, mc) in brackets[..=idx].iter().rev() {
            depth += if open { -1 } else { 1 };
            if depth == 0 {
                return Some([(ml, mc), (l, c)]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_after_open_paren_finds_close() {
        let sql = "SELECT count(a) FROM t";
        // `(` is at column 13; caret right after it is column 14.
        assert_eq!(matching_bracket(sql, 1, 14), Some([(1, 13), (1, 15)]));
    }

    #[test]
    fn caret_before_close_paren_finds_open() {
        let sql = "SELECT (1 + (2)) AS x";
        // Caret after the outer `)` (column 17) matches the `(` at 8.
        assert_eq!(matching_bracket(sql, 1, 17), Some([(1, 8), (1, 16)]));
        // Caret right before the outer `(` (column 8) also pairs it.
        assert_eq!(matching_bracket(sql, 1, 8), Some([(1, 8), (1, 16)]));
        // The bracket before the caret wins over the one at the caret.
        assert_eq!(matching_bracket(sql, 1, 16), Some([(1, 13), (1, 15)]));
    }

    #[test]
    fn unbalanced_returns_none() {
        assert_eq!(matching_bracket("SELECT (1", 1, 9), None);
        assert_eq!(matching_bracket("SELECT 1", 1, 3), None);
    }

    #[test]
    fn parens_in_strings_are_ignored() {
        let sql = "SELECT '(' , (1)";
        assert_eq!(matching_bracket(sql, 1, 15), Some([(1, 14), (1, 16)]));
    }
}
