//! Tokenization for syntax highlighting, built on sqlparser's tokenizer.

use std::ops::Range;

use sqlparser::dialect::GenericDialect;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace, Word};

/// A coarse token class for highlighting. Finer than necessary categories are
/// folded together (every operator/bracket is `Punct`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    Number,
    StringLit,
    Comment,
    Punct,
    Whitespace,
    Other,
}

/// A token with its 1-based source location (sqlparser convention). `end_col` is
/// the exclusive end column on `start_line` for single-line tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub kind: TokenKind,
    pub text: String,
    pub start_line: u64,
    pub start_col: u64,
    pub end_line: u64,
    pub end_col: u64,
}

fn classify(token: &Token) -> TokenKind {
    match token {
        Token::Word(Word { keyword, .. }) => {
            if *keyword == Keyword::NoKeyword {
                TokenKind::Identifier
            } else {
                TokenKind::Keyword
            }
        }
        Token::Number(_, _) => TokenKind::Number,
        Token::SingleQuotedString(_)
        | Token::DoubleQuotedString(_)
        | Token::TripleSingleQuotedString(_)
        | Token::TripleDoubleQuotedString(_)
        | Token::NationalStringLiteral(_)
        | Token::EscapedStringLiteral(_)
        | Token::UnicodeStringLiteral(_)
        | Token::HexStringLiteral(_)
        | Token::DollarQuotedString(_) => TokenKind::StringLit,
        Token::Whitespace(Whitespace::SingleLineComment { .. })
        | Token::Whitespace(Whitespace::MultiLineComment(_)) => TokenKind::Comment,
        Token::Whitespace(_) => TokenKind::Whitespace,
        Token::Char(_)
        | Token::Comma
        | Token::Eq
        | Token::DoubleEq
        | Token::Neq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::Plus
        | Token::Minus
        | Token::Mul
        | Token::Div
        | Token::Mod
        | Token::StringConcat
        | Token::LParen
        | Token::RParen
        | Token::Period
        | Token::Colon
        | Token::DoubleColon
        | Token::SemiColon
        | Token::LBracket
        | Token::RBracket
        | Token::LBrace
        | Token::RBrace
        | Token::Pipe
        | Token::Ampersand
        | Token::Caret => TokenKind::Punct,
        Token::EOF => TokenKind::Whitespace,
        _ => TokenKind::Other,
    }
}

/// Tokenize `sql` into spanned tokens (whitespace included). Positions are
/// 1-based. On a tokenizer error, returns the tokens collected so far (best
/// effort for a live editor).
pub fn lex(sql: &str) -> Vec<SpannedToken> {
    let dialect = GenericDialect {};
    let mut tokenizer = Tokenizer::new(&dialect, sql);
    let mut buf = Vec::new();
    // Ignore the error: `tokenize_with_location_into_buf` keeps everything it
    // managed to read before failing, which is what a live editor wants.
    let _ = tokenizer.tokenize_with_location_into_buf(&mut buf);
    buf.into_iter()
        .map(|tws| SpannedToken {
            kind: classify(&tws.token),
            text: tws.token.to_string(),
            start_line: tws.span.start.line,
            start_col: tws.span.start.column,
            end_line: tws.span.end.line,
            end_col: tws.span.end.column,
        })
        .collect()
}

/// Highlight spans for a single editor line, as **byte** ranges into `line`
/// paired with a kind. Whitespace is omitted (it keeps the default color).
pub fn highlight_spans(line: &str) -> Vec<(Range<usize>, TokenKind)> {
    lex_line(line).spans
}

/// One editor line, lexed for highlighting, plus whether it ends inside an
/// unterminated `/* … */` so the next line can start as a comment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LexedLine {
    pub spans: Vec<(Range<usize>, TokenKind)>,
    pub open_comment: bool,
}

/// Lex `line` for highlighting. Token boundaries are derived from successive
/// token start columns so we never depend on potentially-quirky end spans:
/// token *i* runs from its start up to the start of token *i+1* (or end of line
/// for the last token). An unterminated block comment (which stops sqlparser's
/// tokenizer) is colored as a comment to the end of the line and reported via
/// `open_comment`.
pub fn lex_line(line: &str) -> LexedLine {
    let dialect = GenericDialect {};
    let mut tokenizer = Tokenizer::new(&dialect, line);
    let mut buf = Vec::new();
    let err = tokenizer.tokenize_with_location_into_buf(&mut buf).err();
    let tokens: Vec<SpannedToken> = buf
        .into_iter()
        .map(|tws| SpannedToken {
            kind: classify(&tws.token),
            text: tws.token.to_string(),
            start_line: tws.span.start.line,
            start_col: tws.span.start.column,
            end_line: tws.span.end.line,
            end_col: tws.span.end.column,
        })
        .collect();

    // char-index -> byte-offset, with a trailing sentinel == line.len().
    let mut byte_at: Vec<usize> = line.char_indices().map(|(b, _)| b).collect();
    byte_at.push(line.len());
    let char_to_byte = |char_idx: usize| byte_at[char_idx.min(byte_at.len() - 1)];

    // Where an unterminated block comment begins, if the tokenizer hit one.
    let open_comment_at = err
        .filter(|e| e.message.contains("multi-line comment"))
        .and_then(|_| {
            let after_last = tokens
                .last()
                .map(|t| char_to_byte(t.end_col.max(t.start_col).saturating_sub(1) as usize))
                .unwrap_or(0);
            line[after_last..].find("/*").map(|i| after_last + i)
        });
    let line_end = open_comment_at.unwrap_or(line.len());

    let mut spans = Vec::with_capacity(tokens.len() + 1);
    for (i, tok) in tokens.iter().enumerate() {
        let start_char = tok.start_col.saturating_sub(1) as usize;
        let end_char = tokens
            .get(i + 1)
            .map(|next| next.start_col.saturating_sub(1) as usize)
            .unwrap_or(byte_at.len() - 1);
        if matches!(tok.kind, TokenKind::Whitespace) {
            continue;
        }
        let start = char_to_byte(start_char);
        let end = char_to_byte(end_char).min(line_end);
        if start < end {
            spans.push((start..end, tok.kind));
        }
    }
    if let Some(at) = open_comment_at
        && at < line.len()
    {
        spans.push((at..line.len(), TokenKind::Comment));
    }
    LexedLine {
        spans,
        open_comment: open_comment_at.is_some(),
    }
}

/// Byte offset just past the `*/` that closes a comment opened on an earlier
/// line, or `None` when the whole line is still inside the comment.
pub fn block_comment_end(line: &str) -> Option<usize> {
    line.find("*/").map(|i| i + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_vs_identifier() {
        let toks = lex("SELECT foo");
        let kinds: Vec<_> = toks
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::Keyword, TokenKind::Identifier]);
    }

    #[test]
    fn string_and_number() {
        let toks: Vec<_> = lex("WHERE a = 'x' AND b = 42")
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .collect();
        assert!(toks.iter().any(|t| t.kind == TokenKind::StringLit));
        assert!(toks.iter().any(|t| t.kind == TokenKind::Number));
    }

    #[test]
    fn highlight_spans_cover_expected_text() {
        let line = "SELECT foo";
        let spans = highlight_spans(line);
        // First span = SELECT keyword.
        let (range, kind) = &spans[0];
        assert_eq!(&line[range.clone()], "SELECT");
        assert_eq!(*kind, TokenKind::Keyword);
        // Second span = foo identifier.
        let (range, kind) = &spans[1];
        assert_eq!(&line[range.clone()], "foo");
        assert_eq!(*kind, TokenKind::Identifier);
    }

    #[test]
    fn unterminated_block_comment_colors_rest_of_line() {
        let line = "SELECT 1 /* start";
        let lexed = lex_line(line);
        assert!(lexed.open_comment);
        let (range, kind) = lexed.spans.last().unwrap();
        assert_eq!(&line[range.clone()], "/* start");
        assert_eq!(*kind, TokenKind::Comment);
        // The number before it keeps its own span.
        assert!(
            lexed
                .spans
                .iter()
                .any(|(r, k)| &line[r.clone()] == "1" && *k == TokenKind::Number)
        );
        assert!(!lex_line("SELECT 1 /* done */").open_comment);
        assert_eq!(block_comment_end("x */ SELECT"), Some(4));
        assert_eq!(block_comment_end("still inside"), None);
    }

    #[test]
    fn highlight_spans_handle_multibyte() {
        // A multi-byte character before a token must not corrupt byte offsets.
        let line = "SELECT 'café', x";
        let spans = highlight_spans(line);
        for (range, _) in &spans {
            // Ranges must land on char boundaries (indexing would panic otherwise).
            assert!(line.is_char_boundary(range.start));
            assert!(line.is_char_boundary(range.end));
        }
    }
}
