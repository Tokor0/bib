//! Compiling a [`Query`] to SQL.
//!
//! Every user-supplied value becomes a bound parameter, never string-pasted —
//! including the FTS5 match expression, which is additionally quoted so a
//! search for `foo(bar` is a search, not a syntax error from the FTS parser.

use super::query::{Field, Query, Text, Value};
use rusqlite::types::Value as SqlValue;

pub struct Compiled {
    /// A boolean expression over the alias `d` (the `documents` table).
    pub where_clause: String,
    pub params: Vec<SqlValue>,
}

pub fn compile(query: &Query) -> Compiled {
    let mut params = Vec::new();
    let where_clause = build(query, &mut params);
    Compiled {
        where_clause,
        params,
    }
}

fn build(query: &Query, params: &mut Vec<SqlValue>) -> String {
    match query {
        Query::All => "1".to_owned(),
        Query::And(a, b) => format!("({} AND {})", build(a, params), build(b, params)),
        Query::Or(a, b) => format!("({} OR {})", build(a, params), build(b, params)),
        Query::Not(inner) => format!("(NOT {})", build(inner, params)),
        Query::Text(text) => {
            params.push(SqlValue::Text(fts_expression(text)));
            "d.citekey IN (SELECT citekey FROM fts WHERE fts MATCH ?)".to_owned()
        }
        Query::Field { field, value } => field_clause(*field, value, params),
    }
}

fn field_clause(field: Field, value: &Value, params: &mut Vec<SqlValue>) -> String {
    match field {
        Field::Key => text_clause("d.citekey", value, params),
        Field::Type => text_clause("d.type", value, params),
        Field::Title => text_clause("d.title", value, params),
        Field::Publisher => text_clause("d.publisher", value, params),
        Field::Parent => text_clause("d.parent_title", value, params),

        Field::Year => match value {
            Value::Range { low, high } => {
                let mut parts = Vec::new();
                if let Some(low) = low {
                    params.push(SqlValue::Integer(*low));
                    parts.push("d.year >= ?");
                }
                if let Some(high) = high {
                    params.push(SqlValue::Integer(*high));
                    parts.push("d.year <= ?");
                }
                if parts.is_empty() {
                    "d.year IS NOT NULL".to_owned()
                } else {
                    format!("({})", parts.join(" AND "))
                }
            }
            // `parse_value` only produces ranges for numeric fields.
            _ => "0".to_owned(),
        },

        Field::Author => person_clause(Some("author"), value, params),
        Field::Editor => person_clause(Some("editor"), value, params),
        Field::Person => person_clause(None, value, params),

        Field::Tag => exists("tags t", "t.tag", value, params, None),
        Field::Doi => exists(
            "serials s",
            "s.value",
            value,
            params,
            Some(("s.kind", "doi")),
        ),
        Field::Arxiv => exists(
            "serials s",
            "s.value",
            value,
            params,
            Some(("s.kind", "arxiv")),
        ),
        Field::Isbn => exists(
            "serials s",
            "s.value",
            value,
            params,
            Some(("s.kind", "isbn")),
        ),
        Field::Id => exists("serials s", "s.value", value, params, None),

        Field::File => {
            let want = matches!(value, Value::Bool(true));
            let clause = "EXISTS (SELECT 1 FROM files f WHERE f.citekey = d.citekey)";
            if want {
                clause.to_owned()
            } else {
                format!("(NOT {clause})")
            }
        }
    }
}

/// `col LIKE ?` for substring search, `col = ?` for a quoted value.
///
/// SQLite's `LIKE` is case-insensitive for ASCII only; that is a known and
/// acceptable limit here, and exact match stays case-sensitive by design.
fn text_clause(column: &str, value: &Value, params: &mut Vec<SqlValue>) -> String {
    match value {
        Value::Contains(v) => {
            params.push(SqlValue::Text(like_pattern(v)));
            format!("{column} LIKE ? ESCAPE '\\'")
        }
        Value::Exact(v) => {
            params.push(SqlValue::Text(v.clone()));
            format!("{column} = ?")
        }
        Value::Range { .. } | Value::Bool(_) => "0".to_owned(),
    }
}

fn person_clause(role: Option<&str>, value: &Value, params: &mut Vec<SqlValue>) -> String {
    // Matched against the whole name, so `author:"van der Waals"` and
    // `author:waals` both work without the caller knowing how it was split.
    let condition = text_clause("p.name", value, params);
    match role {
        Some(role) => {
            params.push(SqlValue::Text(role.to_owned()));
            format!(
                "EXISTS (SELECT 1 FROM persons p WHERE p.citekey = d.citekey AND {condition} AND p.role = ?)"
            )
        }
        None => {
            format!("EXISTS (SELECT 1 FROM persons p WHERE p.citekey = d.citekey AND {condition})")
        }
    }
}

fn exists(
    table: &str,
    column: &str,
    value: &Value,
    params: &mut Vec<SqlValue>,
    extra: Option<(&str, &str)>,
) -> String {
    let alias = table.split_whitespace().last().unwrap_or(table);
    let condition = text_clause(column, value, params);
    let mut sql =
        format!("EXISTS (SELECT 1 FROM {table} WHERE {alias}.citekey = d.citekey AND {condition}");
    if let Some((col, want)) = extra {
        params.push(SqlValue::Text(want.to_owned()));
        sql.push_str(&format!(" AND {col} = ?"));
    }
    sql.push(')');
    sql
}

/// Escape `LIKE` wildcards so a DOI containing `_` is not a single-character
/// wildcard, then wrap in `%`.
fn like_pattern(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len() + 2);
    escaped.push('%');
    for c in raw.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push('%');
    escaped
}

/// Build an FTS5 match expression.
///
/// The text is always wrapped in double quotes so FTS5 reads it as a literal
/// string: without this, a search for `AND`, `foo*bar` or `(x` is either an
/// operator or a parse error rather than the words the user typed. Inner quotes
/// are doubled, which is how FTS5 escapes them.
fn fts_expression(text: &Text) -> String {
    let (body, prefix) = match text {
        Text::Prefix(t) => (t, true),
        Text::Phrase(t) => (t, false),
    };
    let quoted = format!("\"{}\"", body.replace('"', "\"\""));
    if prefix { format!("{quoted}*") } else { quoted }
}
