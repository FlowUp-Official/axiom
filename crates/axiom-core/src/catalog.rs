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
    /// Whether the column has an explicit `DEFAULT` or is otherwise
    /// database-supplied on insert (e.g. `DEFAULT now()`, `DEFAULT false`,
    /// `DEFAULT gen_random_uuid()`). Such columns are optional for
    /// `infers insert` and `infers update`.
    pub has_default: bool,
    /// Whether the column is `GENERATED ALWAYS AS (...) STORED` (or VIRTUAL).
    /// Generated columns are omitted from `infers insert`/`infers update`.
    pub is_generated: bool,
    /// Whether the column is an identity column
    /// (`GENERATED ... AS IDENTITY` / `GENERATED ... AS IDENTITY ...`).
    /// Identity columns are omitted from `infers insert`/`infers update`.
    pub is_identity: bool,
}

/// A single table and its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema<'a> {
    /// Fully qualified table name, e.g. `public.users`.
    pub name: Cow<'a, str>,
    /// The columns of the table, in source order.
    pub columns: Vec<ColumnSchema<'a>>,
}

/// A PostgreSQL `CREATE TYPE ... AS ENUM ('a', 'b', ...)` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumSchema<'a> {
    /// Type name, e.g. `post_visibility`.
    pub name: Cow<'a, str>,
    /// Enum label values, in declaration order.
    pub values: Vec<Cow<'a, str>>,
}

/// A parsed catalog of one or more `CREATE TABLE` statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableCatalog<'a> {
    /// Tables in the order they were declared.
    pub tables: Vec<TableSchema<'a>>,
    /// Enum types declared with `CREATE TYPE ... AS ENUM (...)`, in order.
    pub enums: Vec<EnumSchema<'a>>,
}

impl<'a> TableCatalog<'a> {
    /// Return the table with the given (suffix) name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn table_by_name(&self, name: &str) -> Option<&TableSchema<'a>> {
        self.tables
            .iter()
            .find(|t| t.name == name || t.name.ends_with(&format!(".{name}")))
    }

    /// Return the enum type with the given (suffix) name, if declared.
    pub fn enum_by_name(&self, name: &str) -> Option<&EnumSchema<'a>> {
        self.enums
            .iter()
            .find(|e| e.name == name || e.name.ends_with(&format!(".{name}")))
    }

    /// Return the enum whose canonical (PascalCase) generated name matches
    /// `canonical`. This lets codegen resolve enum references that were written
    /// using the generated PascalCase name (e.g. `visibility: PostVisibility`)
    /// back to the PostgreSQL-declared `post_visibility` type.
    pub fn enum_by_canonical_name(&self, canonical: &str) -> Option<&EnumSchema<'a>> {
        self.enums.iter().find(|e| {
            crate::codegen::util::pascal_case(e.name.trim_start_matches("public.")) == canonical
        })
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
        match stmt {
            Statement::CreateTable(create) => {
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
                        has_default: column_has_default(col),
                        is_generated: column_is_generated(col),
                        is_identity: column_is_identity(col),
                    });
                }

                catalog.tables.push(TableSchema {
                    name: Cow::Owned(object_name_to_string(&create.name)),
                    columns,
                });
            }
            Statement::CreateType { name, representation } => {
                if let Some(repr) = representation {
                    if let sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels } = repr {
                        catalog.enums.push(EnumSchema {
                            name: Cow::Owned(object_name_to_string(name)),
                            values: labels
                                .iter()
                                .map(|l| Cow::Owned(unquote_enum_label(&l.to_string())))
                                .collect(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(catalog)
}

fn column_nullable(col: &ColumnDef) -> bool {
    col.options.iter().all(|o| {
        !matches!(
            o.option,
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_)
        )
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

/// Strip the surrounding quote characters that sqlparser preserves on enum
/// label `Ident`s (e.g. `'happy'` from `ENUM ('happy', ...)`). Both single
/// and double quotes are handled; bare identifiers are returned unchanged.
fn unquote_enum_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.len() >= 2 {
        let (first, last) = (trimmed.chars().next().unwrap(), trimmed.chars().last().unwrap());
        if (first == '\'' && last == '\'') || (first == '"' && last == '"') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn column_primary_key(col: &ColumnDef) -> bool {
    col.options
        .iter()
        .any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)))
}

/// Whether the column has a `DEFAULT`, a `GENERATED ... AS IDENTITY`, or any
/// other database-supplied value (so the caller may omit it on insert).
fn column_has_default(col: &ColumnDef) -> bool {
    col.options.iter().any(|o| matches!(
        o.option,
        ColumnOption::Default(_) | ColumnOption::Identity(_)
    ))
}

/// Whether the column is `GENERATED ALWAYS AS (...) STORED` / `VIRTUAL`.
fn column_is_generated(col: &ColumnDef) -> bool {
    col.options
        .iter()
        .any(|o| matches!(o.option, ColumnOption::Generated { .. }))
}

/// Whether the column is a SQL standard identity column
/// (`GENERATED ... AS IDENTITY`).
fn column_is_identity(col: &ColumnDef) -> bool {
    col.options
        .iter()
        .any(|o| matches!(o.option, ColumnOption::Identity(_)))
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

    #[test]
    fn parses_create_type_enum_into_catalog() {
        let sql = "CREATE TYPE mood AS ENUM ('happy', 'sad', 'ok');";
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        assert_eq!(catalog.tables.len(), 0);
        assert_eq!(catalog.enums.len(), 1);
        let r#enum = catalog.enum_by_name("mood").expect("mood enum");
        assert_eq!(r#enum.values.len(), 3);
        assert_eq!(r#enum.values[0].as_ref(), "happy");
        assert_eq!(r#enum.values[1].as_ref(), "sad");
        assert_eq!(r#enum.values[2].as_ref(), "ok");
    }

    #[test]
    fn enum_values_are_unquoted() {
        let sql = r#"CREATE TYPE "public"."status" AS ENUM ('active', "inactive");"#;
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        let r#enum = catalog.enum_by_name("status").expect("status enum");
        assert_eq!(r#enum.values[0].as_ref(), "active");
        assert_eq!(r#enum.values[1].as_ref(), "inactive");
    }
}
