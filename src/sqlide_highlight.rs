//! An iced [`Highlighter`] that colors SQL using `sql_ide`'s lexer, plus the
//! caret-driven overlays (matching bracket, first syntax error) that arrive
//! through the highlighter settings.
//!
//! Lines are lexed one at a time; the only cross-line state is whether a line
//! starts inside a `/* … */` block comment, cached per line so iced's
//! incremental re-highlighting (from the first changed line) stays correct.

use std::ops::Range;

use iced::Font;
use iced::Theme;
use iced::advanced::text::Highlighter;
use iced::advanced::text::highlighter::Format;
use sql_ide::TokenKind;

use crate::theme::{FONT_MONO_MEDIUM, palette};

/// Caret-dependent overlays, as 0-based `(line, byte column)` positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HighlightSettings {
    pub bracket_pair: Option<[(usize, usize); 2]>,
    pub error_at: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    Token(TokenKind),
    BracketMatch,
    Error,
}

#[derive(Debug)]
pub struct SqlHighlighter {
    settings: HighlightSettings,
    /// Index of the line `highlight_line` will receive next.
    current: usize,
    /// `in_comment[i]` is whether line `i` starts inside a block comment.
    in_comment: Vec<bool>,
}

impl Highlighter for SqlHighlighter {
    type Settings = HighlightSettings;
    type Highlight = Highlight;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Highlight)>;

    fn new(settings: &Self::Settings) -> Self {
        Self {
            settings: *settings,
            current: 0,
            in_comment: Vec::new(),
        }
    }

    /// New caret context: every line may need recolouring, so restart from
    /// the top. The comment cache depends only on the text and stays valid.
    fn update(&mut self, new_settings: &Self::Settings) {
        self.settings = *new_settings;
        self.current = 0;
    }

    /// iced calls this after `update` within the same layout pass when the
    /// text changed too; keeping the smaller index means a settings change is
    /// never skipped for the lines above the edit.
    fn change_line(&mut self, line: usize) {
        self.current = self.current.min(line);
        self.in_comment.truncate(line.saturating_add(1));
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let idx = self.current;
        self.current += 1;
        let starts_in_comment = self.in_comment.get(idx).copied().unwrap_or(false);

        let mut spans: Vec<(Range<usize>, Highlight)> = Vec::new();
        let mut ends_in_comment = false;
        let mut body_start = 0usize;
        if starts_in_comment {
            match sql_ide::block_comment_end(line) {
                Some(end) => {
                    spans.push((0..end, Highlight::Token(TokenKind::Comment)));
                    body_start = end;
                }
                None => {
                    if !line.is_empty() {
                        spans.push((0..line.len(), Highlight::Token(TokenKind::Comment)));
                    }
                    ends_in_comment = true;
                }
            }
        }
        if !ends_in_comment {
            let lexed = sql_ide::lex_line(&line[body_start..]);
            ends_in_comment = lexed.open_comment;
            spans.extend(lexed.spans.into_iter().map(|(r, kind)| {
                let r = r.start + body_start..r.end + body_start;
                let h = self.overlay(idx, r.start, kind);
                (r, h)
            }));
        }

        if self.in_comment.len() <= idx + 1 {
            self.in_comment.resize(idx + 2, false);
        }
        self.in_comment[idx + 1] = ends_in_comment;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

impl SqlHighlighter {
    /// Promote a token to a bracket-match or error highlight when the caret
    /// context says so.
    fn overlay(&self, line: usize, start: usize, kind: TokenKind) -> Highlight {
        if kind == TokenKind::Punct
            && self
                .settings
                .bracket_pair
                .is_some_and(|pair| pair.contains(&(line, start)))
        {
            return Highlight::BracketMatch;
        }
        if self
            .settings
            .error_at
            .is_some_and(|(l, c)| l == line && c == start)
            && kind != TokenKind::Comment
        {
            return Highlight::Error;
        }
        Highlight::Token(kind)
    }
}

/// Map a highlight to an editor color. A plain `fn` (not a closure) because
/// `text_editor::highlight_with` takes a function pointer.
pub fn to_format(h: &Highlight, _theme: &Theme) -> Format<Font> {
    let (color, font) = match h {
        Highlight::Token(kind) => (
            match kind {
                TokenKind::Keyword => palette::accent_warm(),
                TokenKind::StringLit => palette::accent_cool(),
                TokenKind::Number => palette::accent_violet(),
                TokenKind::Comment => palette::fg_dim(),
                TokenKind::Punct => palette::fg_muted(),
                TokenKind::Identifier | TokenKind::Other | TokenKind::Whitespace => {
                    palette::fg_primary()
                }
            },
            None,
        ),
        Highlight::BracketMatch => (palette::accent_cool(), Some(FONT_MONO_MEDIUM)),
        Highlight::Error => (palette::accent_rose(), Some(FONT_MONO_MEDIUM)),
    };
    Format {
        color: Some(color),
        font,
    }
}
