//! GUI-agnostic SQL IDE support for `datafusion-ui`.
//!
//! Three independent capabilities, each a free function over `&str`:
//! - [`lex`] / [`highlight_spans`] — tokenize for syntax highlighting.
//! - [`complete`] — schema-aware autocomplete driven by a [`Catalog`].
//! - [`diagnostics`] — parser-only syntax validation.
//! - [`statement_spans`] / [`statement_at`] — split a buffer into statements.
//! - [`matching_bracket`] — paren pair at the caret.
//! - [`format_sql`] — pretty-printer.
//!
//! This crate deliberately has no GUI dependency. Positions follow sqlparser's
//! convention (1-based line and column); the caller is responsible for any
//! conversion to a 0-based editor coordinate system.

mod brackets;
mod catalog;
mod complete;
mod diagnostics;
mod format;
mod lex;
mod statements;

pub use brackets::matching_bracket;
pub use catalog::{Catalog, ColumnMeta, Database, SchemaNs, TableMeta};
pub use complete::{Completion, CompletionKind, complete, first_table_name};
pub use diagnostics::{Diagnostic, diagnostics};
pub use format::{FormatStyle, format_sql};
pub use lex::{
    LexedLine, SpannedToken, TokenKind, block_comment_end, highlight_spans, lex, lex_line,
};
pub use statements::{statement_at, statement_index, statement_spans};
