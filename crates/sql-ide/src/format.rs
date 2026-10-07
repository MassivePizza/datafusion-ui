//! SQL pretty-printing on top of the `sqlformat` crate (comments survive).

/// Formatting preferences. Keywords are upper-cased by default to match what
/// completion inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatStyle {
    pub indent_spaces: u8,
    pub uppercase_keywords: bool,
}

impl Default for FormatStyle {
    fn default() -> Self {
        FormatStyle {
            indent_spaces: 2,
            uppercase_keywords: true,
        }
    }
}

/// Reformat `sql`. Blank input is returned unchanged.
pub fn format_sql(sql: &str, style: &FormatStyle) -> String {
    if sql.trim().is_empty() {
        return sql.to_string();
    }
    let options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(style.indent_spaces),
        uppercase: Some(style.uppercase_keywords),
        ..sqlformat::FormatOptions::default()
    };
    sqlformat::format(sql, &sqlformat::QueryParams::None, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercases_and_breaks_clauses() {
        let out = format_sql("select a, b from t where a > 1", &FormatStyle::default());
        assert!(out.starts_with("SELECT"));
        assert!(out.contains("\nFROM"));
        assert!(out.contains("\nWHERE"));
    }

    #[test]
    fn keeps_comments() {
        let out = format_sql(
            "-- leading\nselect 1 /* inline */ as x",
            &FormatStyle::default(),
        );
        assert!(out.contains("-- leading"));
        assert!(out.contains("/* inline */"));
    }

    #[test]
    fn idempotent() {
        let style = FormatStyle::default();
        let once = format_sql("select a from t join u on t.id = u.id order by a", &style);
        assert_eq!(format_sql(&once, &style), once);
    }

    #[test]
    fn blank_is_unchanged() {
        assert_eq!(format_sql("  \n", &FormatStyle::default()), "  \n");
    }
}
