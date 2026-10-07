//! Schema-aware autocomplete.
//!
//! This is a heuristic, token-based engine — not a full binder. It scans the
//! token stream around the cursor to decide what kind of name is expected
//! (table, column, keyword) and resolves simple `FROM x [AS] a` aliases. That is
//! enough for the common editing cases without the cost of full semantic
//! analysis.

use crate::catalog::Catalog;
use crate::lex::{SpannedToken, TokenKind, lex};
use crate::statements::line_col_to_byte as cursor_byte_offset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Function,
    Table,
    Column,
}

/// A single completion candidate. `replace_len` is how many characters of the
/// partially-typed word (immediately before the cursor) the caller should
/// remove before inserting `insert_text`.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    pub insert_text: String,
    pub detail: Option<String>,
    pub replace_len: usize,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "BETWEEN",
    "DISTINCT",
    "WITH",
    "UNION",
    "ALL",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "EXPLAIN",
];

const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "abs",
    "round",
    "floor",
    "ceil",
    "coalesce",
    "cast",
    "length",
    "lower",
    "upper",
    "trim",
    "substr",
    "concat",
    "now",
    "date_trunc",
    "extract",
    "to_timestamp",
    "array_agg",
    "approx_distinct",
    "stddev",
    "variance",
];

/// Compute completions for the cursor at (1-based) `line`, `column` in `sql`.
///
/// Candidates are ranked: prefix matches before substring matches, and within
/// a tier in the order they were gathered (columns in scope, then functions,
/// then keywords). Keywords follow the typed case: an all-lowercase prefix
/// inserts a lowercase keyword.
pub fn complete(sql: &str, line: u64, column: u64, catalog: &Catalog) -> Vec<Completion> {
    let cursor = cursor_byte_offset(sql, line, column);
    let before = &sql[..cursor];

    let (prefix, after_dot, qualifier) = parse_prefix(before);
    let mut gather = Gather::new(&prefix);

    // Member access `qualifier.<prefix>` → that table's columns only.
    if after_dot {
        if let Some(table) = resolve_qualifier(&qualifier, sql, catalog) {
            for col in &table.columns {
                gather.push(
                    &col.name,
                    CompletionKind::Column,
                    Some(col.data_type.clone()),
                );
            }
        }
        return gather.finish();
    }

    let functions: Vec<&str> = if catalog.functions.is_empty() {
        FUNCTIONS.to_vec()
    } else {
        catalog.functions.iter().map(String::as_str).collect()
    };

    match clause_context(before) {
        Clause::Table => {
            for table in catalog.tables() {
                gather.push(
                    &table.name,
                    CompletionKind::Table,
                    Some(table.qualified.clone()),
                );
                if !table.qualified.eq_ignore_ascii_case(&table.name) {
                    gather.push(
                        &table.qualified,
                        CompletionKind::Table,
                        Some("table".into()),
                    );
                }
            }
        }
        Clause::Expr => {
            for table in tables_in_scope(sql, catalog) {
                for col in &table.columns {
                    gather.push(
                        &col.name,
                        CompletionKind::Column,
                        Some(format!("{} · {}", table.name, col.data_type)),
                    );
                }
            }
            for f in &functions {
                gather.push(f, CompletionKind::Function, Some("function".into()));
            }
            for kw in KEYWORDS {
                gather.push(kw, CompletionKind::Keyword, None);
            }
        }
        Clause::Start => {
            for kw in KEYWORDS {
                gather.push(kw, CompletionKind::Keyword, None);
            }
        }
    }

    gather.finish()
}

/// Accumulates candidates with their match tier, then orders them.
struct Gather {
    prefix_lc: String,
    replace_len: usize,
    lowercase_keywords: bool,
    /// `(tier, insertion order, completion)`; tier 0 = prefix, 1 = substring.
    items: Vec<(u8, usize, Completion)>,
}

impl Gather {
    fn new(prefix: &str) -> Self {
        Gather {
            prefix_lc: prefix.to_ascii_lowercase(),
            replace_len: prefix.chars().count(),
            lowercase_keywords: !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|c| !c.is_alphabetic() || c.is_lowercase()),
            items: Vec::new(),
        }
    }

    fn push(&mut self, candidate: &str, kind: CompletionKind, detail: Option<String>) {
        let lc = candidate.to_ascii_lowercase();
        let tier = if self.prefix_lc.is_empty() || lc.starts_with(&self.prefix_lc) {
            0
        } else if lc.contains(&self.prefix_lc) {
            1
        } else {
            return;
        };
        let insert_text = if kind == CompletionKind::Keyword && self.lowercase_keywords {
            lc
        } else {
            candidate.to_string()
        };
        self.items.push((
            tier,
            self.items.len(),
            Completion {
                label: candidate.to_string(),
                kind,
                insert_text,
                detail,
                replace_len: self.replace_len,
            },
        ));
    }

    fn finish(mut self) -> Vec<Completion> {
        self.items.sort_by_key(|(tier, order, _)| (*tier, *order));
        self.items.into_iter().map(|(_, _, c)| c).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// Right after FROM/JOIN — expect a table name.
    Table,
    /// In an expression position — expect columns/functions/keywords.
    Expr,
    /// Empty / start of statement — keywords only.
    Start,
}

/// Decide what is expected at the cursor from the last meaningful keyword.
fn clause_context(before: &str) -> Clause {
    let tokens: Vec<SpannedToken> = lex(before)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .collect();

    // Walk backwards for the most recent anchor keyword.
    for tok in tokens.iter().rev() {
        if tok.kind != TokenKind::Keyword {
            continue;
        }
        let kw = tok.text.to_ascii_uppercase();
        match kw.as_str() {
            "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE" => return Clause::Table,
            "SELECT" | "WHERE" | "ON" | "HAVING" | "BY" | "SET" | "AND" | "OR" | "NOT" | "IN"
            | "VALUES" | "WHEN" | "THEN" | "ELSE" => return Clause::Expr,
            _ => return Clause::Expr,
        }
    }
    if tokens.is_empty() {
        Clause::Start
    } else {
        Clause::Expr
    }
}

/// Extract the identifier being typed immediately before the cursor. Returns
/// `(prefix, after_dot, qualifier)` where `after_dot` indicates `qualifier.`
/// member access.
fn parse_prefix(before: &str) -> (String, bool, String) {
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();
    while i > 0 && is_ident_char(chars[i - 1]) {
        i -= 1;
    }
    let prefix: String = chars[i..].iter().collect();

    // Is the char before the prefix a '.'? If so, read the qualifier ident.
    if i > 0 && chars[i - 1] == '.' {
        let mut j = i - 1;
        while j > 0 && is_ident_char(chars[j - 1]) {
            j -= 1;
        }
        let qualifier: String = chars[j..i - 1].iter().collect();
        return (prefix, true, qualifier);
    }
    (prefix, false, String::new())
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Resolve a `qualifier` (table name or alias) to its [`TableMeta`].
fn resolve_qualifier<'a>(
    qualifier: &str,
    sql: &str,
    catalog: &'a Catalog,
) -> Option<&'a crate::catalog::TableMeta> {
    // Direct table name first.
    if let Some(t) = catalog.find_table(qualifier) {
        return Some(t);
    }
    // Otherwise look for `FROM/JOIN <table> [AS] <qualifier>`.
    let refs = from_references(sql);
    for (table, alias) in refs {
        if alias
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(qualifier))
        {
            return catalog.find_table(&table);
        }
    }
    None
}

/// Tables referenced by FROM/JOIN clauses, looked up in the catalog.
fn tables_in_scope<'a>(sql: &str, catalog: &'a Catalog) -> Vec<&'a crate::catalog::TableMeta> {
    let mut out = Vec::new();
    for (table, _alias) in from_references(sql) {
        if let Some(t) = catalog.find_table(&table)
            && !out
                .iter()
                .any(|existing: &&crate::catalog::TableMeta| std::ptr::eq(*existing, t))
        {
            out.push(t);
        }
    }
    out
}

/// The first table referenced by a FROM/JOIN clause, for naming editor tabs.
pub fn first_table_name(sql: &str) -> Option<String> {
    from_references(sql).into_iter().next().map(|(t, _)| t)
}

/// Parse `(table_name, optional_alias)` pairs from FROM/JOIN clauses by scanning
/// the token stream. Handles `FROM t`, `FROM t a`, and `FROM t AS a`.
fn from_references(sql: &str) -> Vec<(String, Option<String>)> {
    let tokens: Vec<SpannedToken> = lex(sql)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .collect();

    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let kw = tokens[i].text.to_ascii_uppercase();
        let is_anchor = tokens[i].kind == TokenKind::Keyword && (kw == "FROM" || kw == "JOIN");
        if is_anchor && i + 1 < tokens.len() {
            // Collect a (possibly dotted) table name: ident (. ident)*
            let mut name = String::new();
            let mut j = i + 1;
            loop {
                if j >= tokens.len() || tokens[j].kind == TokenKind::Keyword {
                    break;
                }
                if tokens[j].kind == TokenKind::Identifier {
                    name.push_str(&tokens[j].text);
                    j += 1;
                    if j < tokens.len() && tokens[j].text == "." {
                        name.push('.');
                        j += 1;
                        continue;
                    }
                }
                break;
            }
            if name.is_empty() {
                i += 1;
                continue;
            }
            // Optional alias: `AS ident` or bare `ident`.
            let mut alias = None;
            if j < tokens.len() {
                if tokens[j].kind == TokenKind::Keyword && tokens[j].text.eq_ignore_ascii_case("AS")
                {
                    j += 1;
                }
                if j < tokens.len() && tokens[j].kind == TokenKind::Identifier {
                    alias = Some(tokens[j].text.clone());
                    j += 1;
                }
            }
            out.push((name, alias));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, ColumnMeta, Database, SchemaNs, TableMeta};

    fn sample_catalog() -> Catalog {
        Catalog {
            databases: vec![Database {
                name: "cat".into(),
                schemas: vec![SchemaNs {
                    name: Some("sch".into()),
                    tables: vec![TableMeta {
                        name: "orders".into(),
                        qualified: "cat.sch.orders".into(),
                        columns: vec![
                            ColumnMeta {
                                name: "id".into(),
                                data_type: "Int64".into(),
                            },
                            ColumnMeta {
                                name: "amount".into(),
                                data_type: "Float64".into(),
                            },
                        ],
                    }],
                }],
            }],
            functions: Vec::new(),
        }
    }

    fn labels(c: &[Completion]) -> Vec<String> {
        c.iter().map(|x| x.label.clone()).collect()
    }

    #[test]
    fn after_from_suggests_tables() {
        let cat = sample_catalog();
        let sql = "SELECT * FROM ";
        let comps = complete(sql, 1, (sql.len() + 1) as u64, &cat);
        assert!(labels(&comps).contains(&"orders".to_string()));
    }

    #[test]
    fn from_prefix_filters() {
        let cat = sample_catalog();
        let sql = "SELECT * FROM or";
        let comps = complete(sql, 1, (sql.len() + 1) as u64, &cat);
        // Prefix matches lead; substring matches (e.g. `cat.sch.orders`) follow.
        assert!(comps[0].label.to_lowercase().starts_with("or"));
        assert!(comps.iter().all(|c| c.label.to_lowercase().contains("or")));
        assert_eq!(comps[0].replace_len, 2);
    }

    #[test]
    fn member_access_suggests_columns() {
        let cat = sample_catalog();
        let sql = "SELECT o. FROM orders o";
        // Cursor right after the dot (char position 9 -> column 10).
        let dot = sql.find('.').unwrap();
        let comps = complete(sql, 1, (dot + 2) as u64, &cat);
        let l = labels(&comps);
        assert!(l.contains(&"id".to_string()));
        assert!(l.contains(&"amount".to_string()));
    }

    #[test]
    fn substring_matches_rank_after_prefix_matches() {
        let cat = sample_catalog();
        let sql = "SELECT * FROM der";
        let comps = complete(sql, 1, (sql.len() + 1) as u64, &cat);
        // "orders" only contains "der"; it is still offered (tier 1).
        assert!(labels(&comps).contains(&"orders".to_string()));
    }

    #[test]
    fn lowercase_prefix_inserts_lowercase_keyword() {
        let cat = sample_catalog();
        let sql = "sel";
        let comps = complete(sql, 1, 4, &cat);
        let select = comps.iter().find(|c| c.label == "SELECT").unwrap();
        assert_eq!(select.insert_text, "select");
        let comps = complete("SEL", 1, 4, &cat);
        let select = comps.iter().find(|c| c.label == "SELECT").unwrap();
        assert_eq!(select.insert_text, "SELECT");
    }

    #[test]
    fn catalog_functions_replace_builtin_list() {
        let mut cat = sample_catalog();
        cat.functions = vec!["date_bin".into()];
        let sql = "SELECT date_b";
        let comps = complete(sql, 1, (sql.len() + 1) as u64, &cat);
        assert_eq!(labels(&comps), vec!["date_bin".to_string()]);
    }

    #[test]
    fn select_suggests_columns_from_scope() {
        let cat = sample_catalog();
        let sql = "SELECT  FROM orders";
        // Cursor after "SELECT " (column 8).
        let comps = complete(sql, 1, 8, &cat);
        let l = labels(&comps);
        assert!(l.contains(&"id".to_string()));
        assert!(l.contains(&"amount".to_string()));
    }
}
