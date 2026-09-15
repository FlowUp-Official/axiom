//! Zero-copy schema catalog.
//!
//! Column metadata (names, data types, nullability, primary keys) is read from
//! `CREATE TABLE` statements. Validation rules live in `.axm` model files, not
//! in SQL comments, so no annotation parsing exists here.
//!
//! Everything that can borrow from the input is kept as a `Cow<'a, str>` so
//! parsing allocates only when a value must be synthesized (e.g. the
//! normalized `DataType` display string).

use std::borrow::Cow;

use sqlparser::ast::{ColumnDef, ColumnOption, ObjectName, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span, Token, TokenWithSpan, Tokenizer};

use crate::errors::AxiomError;

/// A single column extracted from a `CREATE TABLE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSchema<'a> {
    /// Column name, borrowed from the SQL source when possible.
    pub name: Cow<'a, str>,
    /// Canonicalized column data type, e.g. `VARCHAR(255)`.
    pub data_type: Cow<'a, str>,
    /// Whether the column may contain `NULL`.
    pub nullable: bool,
    /// Whether the column is (part of) the table's primary key.
    pub primary_key: bool,
}

/// A single table and its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema<'a> {
    /// Fully qualified table name, e.g. `public.users`.
    pub name: Cow<'a, str>,
    /// The columns of the table, in source order.
    pub columns: Vec<ColumnSchema<'a>>,
}

/// A parsed catalog of one or more `CREATE TABLE` statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableCatalog<'a> {
    /// Tables in the order they were declared.
    pub tables: Vec<TableSchema<'a>>,
}

impl<'a> TableCatalog<'a> {
    /// Return the table with the given (suffix) name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn table_by_name(&self, name: &str) -> Option<&TableSchema<'a>> {
        self.tables
            .iter()
            .find(|t| t.name == name || t.name.ends_with(&format!(".{name}")))
    }
}

/// Parse SQL source into a zero-copy [`TableCatalog`].
pub fn parse_sql_catalog<'a>(sql: &'a str) -> Result<TableCatalog<'a>, AxiomError> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql)?;
    let tokens = Tokenizer::new(&dialect, sql).tokenize_with_location()?;
    let line_starts = compute_line_starts(sql);

    let mut catalog = TableCatalog::default();

    for stmt in &statements {
        let Statement::CreateTable(create) = stmt else {
            continue;
        };

        let located = locate_column_lines(&create.columns, &tokens, sql, &line_starts);

        let mut columns = Vec::with_capacity(create.columns.len());

        for (col, loc) in create.columns.iter().zip(located.iter()) {
            let Some((_line, name)) = loc else {
                continue;
            };

            columns.push(ColumnSchema {
                name: name.clone(),
                data_type: Cow::Owned(col.data_type.to_string()),
                nullable: column_nullable(col),
                primary_key: column_primary_key(col),
            });
        }

        catalog.tables.push(TableSchema {
            name: Cow::Owned(object_name_to_string(&create.name)),
            columns,
        });
    }

    Ok(catalog)
}

fn column_nullable(col: &ColumnDef) -> bool {
    col.options.iter().all(|o| {
        !matches!(o.option, ColumnOption::NotNull | ColumnOption::PrimaryKey(_))
    })
}

/// Format an object name (e.g. `"public"."users"`) as `public.users`,
/// dropping any quote characters.
fn object_name_to_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
        .collect::<Vec<_>>()
        .join(".")
}

fn column_primary_key(col: &ColumnDef) -> bool {
    col.options
        .iter()
        .any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)))
}

fn compute_line_starts(sql: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in sql.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn offset_at(line_starts: &[usize], loc: Location) -> usize {
    let line = loc.line as usize;
    let col = loc.column as usize;
    line_starts.get(line - 1).copied().unwrap_or(0) + (col - 1)
}

/// Borrow the exact source text covered by a token span.
fn slice_span<'a>(sql: &'a str, line_starts: &[usize], span: &Span) -> &'a str {
    let start = offset_at(line_starts, span.start);
    let end = offset_at(line_starts, span.end);
    &sql[start..end]
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'`' && last == b'`') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Words that begin a table-level constraint rather than a column definition.
fn is_constraint_start(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "primary"
            | "key"
            | "unique"
            | "constraint"
            | "foreign"
            | "check"
            | "references"
            | "exclude"
            | "index"
    )
}

/// Locate the source line and borrowed name of each column definition.
///
/// Returns one entry per [`ColumnDef`], in order. An entry is `None` when the
/// column could not be located in the token stream (should not happen for
/// well-formed DDL).
fn locate_column_lines<'a>(
    columns: &[ColumnDef],
    tokens: &[TokenWithSpan],
    sql: &'a str,
    line_starts: &[usize],
) -> Vec<Option<(u64, Cow<'a, str>)>> {
    let significant: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| !matches!(&t.token, Token::Whitespace(_)))
        .collect();

    // Candidate column starts: identifier tokens at paren-depth 1 that directly
    // follow `(` or `,`.
    let mut candidates: Vec<(u64, String, Cow<'a, str>)> = Vec::new();
    let mut depth: i64 = 0;

    for (idx, tws) in significant.iter().enumerate() {
        match &tws.token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }

        if depth != 1 {
            continue;
        }
        let Token::Word(word) = &tws.token else {
            continue;
        };
        let prev = idx
            .checked_sub(1)
            .and_then(|i| significant.get(i))
            .map(|t| &t.token);
        if !matches!(prev, Some(Token::LParen) | Some(Token::Comma)) {
            continue;
        }
        if is_constraint_start(&word.value) {
            continue;
        }

        let borrowed = Cow::Borrowed(strip_quotes(slice_span(sql, line_starts, &tws.span)));
        candidates.push((tws.span.start.line, word.value.clone(), borrowed));
    }

    // Match candidates to the parsed columns by name (case-insensitive).
    let mut used = vec![false; candidates.len()];
    let mut out = Vec::with_capacity(columns.len());

    for col in columns {
        let needle = col.name.value.to_ascii_lowercase();
        let mut found = None;
        for (i, (line, value, name)) in candidates.iter().enumerate() {
            if !used[i] && value.to_ascii_lowercase() == needle {
                used[i] = true;
                found = Some((*line, name.clone()));
                break;
            }
        }
        out.push(found);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SQL: &str = r#"
CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    external_id UUID,
    name TEXT
);

CREATE TABLE sessions (
    ip INET,
    created_at TIMESTAMP NOT NULL
);
"#;

    #[test]
    fn parses_ddl_into_catalog() {
        let catalog = parse_sql_catalog(SAMPLE_SQL).expect("parse sql");
        assert_eq!(catalog.tables.len(), 2);

        let users = catalog.table_by_name("users").expect("users table");
        assert_eq!(users.columns.len(), 4);

        let id = &users.columns[0];
        assert_eq!(id.name.as_ref(), "id");
        assert!(id.primary_key);
        assert!(!id.nullable);

        let email = &users.columns[1];
        assert_eq!(email.name.as_ref(), "email");
        assert_eq!(email.data_type.as_ref(), "VARCHAR(255)");
        assert!(!email.nullable);

        let external_id = &users.columns[2];
        assert_eq!(external_id.name.as_ref(), "external_id");

        let name = &users.columns[3];
        assert_eq!(name.name.as_ref(), "name");

        let sessions = catalog.table_by_name("sessions").expect("sessions table");
        let ip = &sessions.columns[0];
        assert_eq!(ip.name.as_ref(), "ip");
    }

    #[test]
    fn non_create_statements_are_ignored() {
        let sql = "SELECT 1;\nCREATE TABLE t (a TEXT);";
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        assert_eq!(catalog.tables.len(), 1);
        assert_eq!(catalog.table_by_name("t").expect("table").columns.len(), 1);
    }

    #[test]
    fn zero_copy_names_borrow_from_input() {
        let catalog = parse_sql_catalog(SAMPLE_SQL).expect("parse sql");
        let users = catalog.table_by_name("users").expect("users table");
        let email = &users.columns[1];
        assert!(matches!(email.name, Cow::Borrowed(_)));
    }

    #[test]
    fn quoted_identifiers_are_supported() {
        let sql = r#"
CREATE TABLE "mixed" (
    "weird col" TEXT
);
"#;
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        let table = catalog.table_by_name("mixed").expect("table");
        assert_eq!(table.columns[0].name.as_ref(), "weird col");
    }
}