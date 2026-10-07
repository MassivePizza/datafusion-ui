//! Statement splitting for "run the statement under the cursor".
//!
//! Statements are separated by top-level `;` tokens. Strings and comments are
//! consumed by the tokenizer, so a `;` inside a literal never splits.

use std::ops::Range;

use crate::lex::{TokenKind, lex};

/// Byte ranges of each non-blank statement in `sql`, trailing `;` excluded and
/// surrounding whitespace trimmed, in source order.
pub fn statement_spans(sql: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for tok in lex(sql) {
        if tok.kind == TokenKind::Punct && tok.text == ";" {
            let end = line_col_to_byte(sql, tok.start_line, tok.start_col);
            push_trimmed(sql, start..end, &mut spans);
            start = (end + 1).min(sql.len());
        }
    }
    push_trimmed(sql, start..sql.len(), &mut spans);
    spans
}

/// The statement the caret (byte `offset`) belongs to: the last statement that
/// starts at or before the caret, so a caret on the blank line after `;` still
/// targets the statement it follows. `None` only when `sql` has no statements.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    let spans = statement_spans(sql);
    let idx = spans.iter().rposition(|s| s.start <= offset).unwrap_or(0);
    spans.get(idx).cloned()
}

/// 0-based index of the statement at `offset` plus the total count, for a
/// "statement 2 of 3" hint. `None` when there is at most one statement.
pub fn statement_index(sql: &str, offset: usize) -> Option<(usize, usize)> {
    let spans = statement_spans(sql);
    if spans.len() < 2 {
        return None;
    }
    let idx = spans.iter().rposition(|s| s.start <= offset).unwrap_or(0);
    Some((idx, spans.len()))
}

fn push_trimmed(sql: &str, range: Range<usize>, out: &mut Vec<Range<usize>>) {
    if range.start >= range.end || range.end > sql.len() {
        return;
    }
    let slice = &sql[range.clone()];
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return;
    }
    let lead = slice.len() - slice.trim_start().len();
    let start = range.start + lead;
    out.push(start..start + trimmed.len());
}

/// Byte offset of a 1-based sqlparser `(line, column)`; `column` counts chars.
pub(crate) fn line_col_to_byte(sql: &str, line: u64, column: u64) -> usize {
    let mut off = 0usize;
    for (idx, l) in sql.split_inclusive('\n').enumerate() {
        if (idx as u64) + 1 == line {
            for (chars, (b, _)) in l.char_indices().enumerate() {
                if chars as u64 == column.saturating_sub(1) {
                    return off + b;
                }
            }
            return off + l.trim_end_matches('\n').len();
        }
        off += l.len();
    }
    sql.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_statement_is_whole_text() {
        let sql = "  SELECT 1  ";
        let spans = statement_spans(sql);
        assert_eq!(spans.len(), 1);
        assert_eq!(&sql[spans[0].clone()], "SELECT 1");
    }

    #[test]
    fn splits_on_top_level_semicolons() {
        let sql = "SELECT 1;\nSELECT ';' AS s; -- c; d\nSELECT 3";
        let spans = statement_spans(sql);
        let texts: Vec<&str> = spans.iter().map(|s| &sql[s.clone()]).collect();
        assert_eq!(
            texts,
            vec!["SELECT 1", "SELECT ';' AS s", "-- c; d\nSELECT 3"]
        );
    }

    #[test]
    fn trailing_semicolon_adds_no_empty_statement() {
        assert_eq!(statement_spans("SELECT 1;").len(), 1);
        assert_eq!(statement_spans("SELECT 1;  \n").len(), 1);
    }

    #[test]
    fn statement_at_picks_preceding_statement() {
        let sql = "SELECT 1;\n\nSELECT 2";
        let first = statement_at(sql, 0).unwrap();
        assert_eq!(&sql[first], "SELECT 1");
        // Caret on the blank line after `;` still targets the first statement.
        let blank = statement_at(sql, 10).unwrap();
        assert_eq!(&sql[blank], "SELECT 1");
        let second = statement_at(sql, sql.len()).unwrap();
        assert_eq!(&sql[second], "SELECT 2");
        assert_eq!(statement_index(sql, sql.len()), Some((1, 2)));
        assert_eq!(statement_index("SELECT 1", 0), None);
    }

    #[test]
    fn blank_input_has_no_statements() {
        assert!(statement_spans("   \n").is_empty());
        assert!(statement_at("", 0).is_none());
    }
}
