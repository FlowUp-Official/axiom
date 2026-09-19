//! Lint rules over SQL: schema files and the SQL body of every `.axm` `query`
//! declaration.

use std::collections::BTreeMap;

use axiom_diagnostics::{Diagnostic, Span};
use sqlparser::ast::{
    ColumnOption, CreateIndex, CreateTable, Expr, IndexColumn, Statement, TableConstraint,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::tokenizer::{Location, Token, TokenWithSpan, Tokenizer};

use crate::runner::{LintContext, LintRule, word_span};

/// Flags `DELETE` and `UPDATE` statements that omit a `WHERE` clause, which
/// would affect every row in the table.
#[derive(Debug)]
pub struct MissingWhereClause;

impl LintRule for MissingWhereClause {
    fn name(&self) -> &'static str {
        "missing-where-clause"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(statements) = &ctx.statements else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for stmt in statements {
            let (keyword, target) = match stmt {
                Statement::Delete(d) if d.selection.is_none() => ("delete", None),
                Statement::Update(u) if u.selection.is_none() => {
                    ("update", Some(u.table.to_string()))
                }
                _ => continue,
            };
            let message = match target {
                Some(table) => format!(
                    "`{keyword}` without a `WHERE` clause will modify every row of `{table}`"
                ),
                None => "`delete` without a `WHERE` clause will delete every row".to_string(),
            };
            let span = keyword_span(ctx.source, keyword);
            let mut diag = Diagnostic::error(ctx.file, "lint.missing-where-clause", message)
                .with_help(
                    "add a `WHERE` clause, or explicitly guard it with `WHERE true` if intended",
                );
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            out.push(diag);
        }
        out
    }
}

/// Flags `SELECT *` projections; explicit column lists are more stable against
/// schema changes.
///
/// The rule scans a context only when it carries a parsed SQL statement list.
/// The whole-file `.axm` context has no statements, so each `query` body is
/// analyzed exactly once (from its own body context) instead of once for the
/// body and once for the enclosing file; `.axm` syntax is never read as SQL,
/// and standalone `.sql` files are scanned once.
#[derive(Debug)]
pub struct SelectStar;

impl LintRule for SelectStar {
    fn name(&self) -> &'static str {
        "select-star"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        if ctx.statements.is_none() {
            return Vec::new();
        }

        let Ok(tokens) = Tokenizer::new(&GenericDialect {}, ctx.source).tokenize_with_location()
        else {
            return Vec::new();
        };

        let line_starts = line_starts(ctx.source);
        let mut out = Vec::new();
        // Whether the previous significant token opens a select item: `SELECT`,
        // a projection comma, or a `DISTINCT`/`ALL` modifier.
        let mut select_item = false;
        for token in tokens {
            match &token.token {
                Token::Mul => {
                    if select_item {
                        let span = token_location_span(ctx.source, &line_starts, &token);
                        let mut diag = Diagnostic::warning(
                            ctx.file,
                            "lint.select-star",
                            "`SELECT *` selects every column; list columns explicitly",
                        )
                        .with_help("enumerate the columns instead of `*`");
                        if let Some(span) = span {
                            diag = diag.with_span(span);
                        }
                        out.push(diag);
                    }
                    select_item = false;
                }
                Token::Whitespace(_) | Token::EOF => {}
                other => {
                    let upper = other.to_string().to_uppercase();
                    select_item =
                        matches!(upper.as_str(), "SELECT" | "," | "DISTINCT" | "ALL");
                }
            }
        }
        out
    }
}

/// Flags foreign-key columns that are not covered by any `CREATE INDEX`,
/// since writes to the referenced table then force full-table scans.
#[derive(Debug)]
pub struct UnindexedForeignKey;

impl LintRule for UnindexedForeignKey {
    fn name(&self) -> &'static str {
        "unindexed-foreign-key"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(statements) = &ctx.statements else {
            return Vec::new();
        };

        let mut foreign_keys: Vec<(String, String)> = Vec::new();
        let mut indexes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for stmt in statements {
            match stmt {
                Statement::CreateTable(create) => {
                    collect_table_fks(create, &mut foreign_keys);
                    collect_table_keys(create, &mut keys);
                }
                Statement::CreateIndex(index) => collect_index(index, &mut indexes),
                _ => {}
            }
        }

        let mut out = Vec::new();
        for (table, column) in foreign_keys {
            let col = column.to_lowercase();
            let covered = indexes
                .get(&table.to_lowercase())
                .is_some_and(|cols| cols.iter().any(|c| c == &col))
                // A column-level `PRIMARY KEY`/`UNIQUE` (or a table-level
                // constraint over this column) also creates an index that
                // backs the foreign key, so such columns need no extra index.
                || keys
                    .get(&table.to_lowercase())
                    .is_some_and(|cols| cols.iter().any(|c| c == &col));
            if covered {
                continue;
            }
            let span = word_span(ctx.source, 0, &column);
            let mut diag = Diagnostic::warning(
                ctx.file,
                "lint.unindexed-foreign-key",
                format!("foreign-key column `{column}` on `{table}` is not indexed"),
            )
            .with_help(format!(
                "add `CREATE INDEX ON {table} ({column})` to speed up referential lookups"
            ));
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            out.push(diag);
        }
        out
    }
}

/// Flags `CREATE TABLE` statements that declare no primary key or unique
/// constraint, leaving rows with no stable identity.
#[derive(Debug)]
pub struct MissingPrimaryKey;

impl LintRule for MissingPrimaryKey {
    fn name(&self) -> &'static str {
        "missing-primary-key"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(statements) = &ctx.statements else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for stmt in statements {
            let Statement::CreateTable(create) = stmt else {
                continue;
            };
            if table_has_key(create) {
                continue;
            }
            let table = create.name.to_string();
            let mut diag = Diagnostic::warning(
                ctx.file,
                "lint.missing-primary-key",
                format!("table `{table}` has no primary key; rows have no stable identity"),
            )
            .with_help("add a PRIMARY KEY (or a UNIQUE constraint) to the table");
            if let Some(span) = word_span(ctx.source, 0, &table) {
                diag = diag.with_span(span);
            }
            out.push(diag);
        }
        out
    }
}

/// Whether a `CREATE TABLE` declares a row key: a column-level `PRIMARY KEY`
/// or `UNIQUE`, or a table-level `PRIMARY KEY`/`UNIQUE` constraint.
fn table_has_key(create: &CreateTable) -> bool {
    let column_key = create.columns.iter().any(|column| {
        column.options.iter().any(|option| {
            matches!(
                &option.option,
                ColumnOption::PrimaryKey(_) | ColumnOption::Unique(_)
            )
        })
    });
    let table_key = create.constraints.iter().any(|constraint| {
        matches!(
            constraint,
            TableConstraint::PrimaryKey(_) | TableConstraint::Unique(_)
        )
    });
    column_key || table_key
}

fn collect_table_fks(create: &CreateTable, out: &mut Vec<(String, String)>) {
    let table = create.name.to_string();
    let table_lc = table.to_lowercase();
    for column in &create.columns {
        for option in &column.options {
            if let ColumnOption::ForeignKey(fk) = &option.option {
                let cols = if fk.columns.is_empty() {
                    vec![column.name.clone()]
                } else {
                    fk.columns.clone()
                };
                for col in cols {
                    out.push((table_lc.clone(), col.to_string()));
                }
            }
        }
    }
    for constraint in &create.constraints {
        if let TableConstraint::ForeignKey(fk) = constraint {
            for col in &fk.columns {
                out.push((table_lc.clone(), col.to_string()));
            }
        }
    }
}

fn collect_index(index: &CreateIndex, out: &mut BTreeMap<String, Vec<String>>) {
    let table = index.table_name.to_string().to_lowercase();
    let cols: Vec<String> = index
        .columns
        .iter()
        .filter_map(index_column_name)
        .map(|c| c.to_lowercase())
        .collect();
    out.entry(table).or_default().extend(cols);
}

/// Collect every column that the database backs with an index via a
/// `PRIMARY KEY` or `UNIQUE` constraint, so foreign keys on those columns are
/// considered covered. Both column-level options and table-level constraints
/// are handled.
fn collect_table_keys(create: &CreateTable, out: &mut BTreeMap<String, Vec<String>>) {
    let table = create.name.to_string().to_lowercase();
    let entry = out.entry(table).or_default();
    for column in &create.columns {
        let is_key = column.options.iter().any(|o| {
            matches!(
                &o.option,
                ColumnOption::PrimaryKey(_) | ColumnOption::Unique(_)
            )
        });
        if is_key {
            entry.push(column.name.to_string().to_lowercase());
        }
    }
    for constraint in &create.constraints {
        match constraint {
            TableConstraint::PrimaryKey(c) => push_constraint_columns(entry, &c.columns),
            TableConstraint::Unique(c) => push_constraint_columns(entry, &c.columns),
            _ => {}
        }
    }
}

fn push_constraint_columns(entry: &mut Vec<String>, columns: &[IndexColumn]) {
    for col in columns {
        if let Some(name) = index_column_name(col) {
            entry.push(name.to_lowercase());
        }
    }
}

fn index_column_name(column: &IndexColumn) -> Option<String> {
    match &column.column.expr {
        Expr::Identifier(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Byte span of the first occurrence of `keyword` (case-insensitive, word
/// boundaries) in `source`.
fn keyword_span(source: &str, keyword: &str) -> Option<Span> {
    let lower = source.to_ascii_lowercase();
    let target = keyword.to_ascii_lowercase();
    let mut offset = 0;
    while offset <= lower.len() {
        let Some(rel) = lower[offset..].find(&target) else {
            break;
        };
        let abs = offset + rel;
        let before = lower[..abs].chars().last();
        let after = lower[abs + target.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if boundary(before) && boundary(after) {
            return Some(Span::new(abs, abs + target.len()));
        }
        offset = abs + 1;
    }
    None
}

/// Byte offset of the start of every line in `source` (the first is always 0).
fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Byte span of a token, using the tokenizer's line/column locations.
fn token_location_span(source: &str, line_starts: &[usize], token: &TokenWithSpan) -> Option<Span> {
    let start = location_offset(source, line_starts, token.span.start)?;
    let end = location_offset(source, line_starts, token.span.end).unwrap_or(start + 1);
    Some(Span::new(start, end.max(start + 1)))
}

/// Convert a tokenizer [`Location`] (1-based line/column, columns counted in
/// characters) into a byte offset into `source`.
fn location_offset(source: &str, line_starts: &[usize], loc: Location) -> Option<usize> {
    if loc.line == 0 || loc.column == 0 {
        return None;
    }
    let line_start = *line_starts.get((loc.line - 1) as usize)?;
    let line_end = source[line_start..]
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let offset = line
        .char_indices()
        .nth((loc.column - 1) as usize)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    Some(line_start + offset)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::runner::WorkspaceView;

    fn ctx<'a>(source: &'a str, workspace: &'a WorkspaceView) -> LintContext<'a> {
        LintContext {
            file: Path::new("schema.sql"),
            source,
            origin: 0,
            axm: None,
            statements: sqlparser::parser::Parser::parse_sql(&GenericDialect {}, source).ok(),
            workspace,
        }
    }

    #[test]
    fn delete_without_where_is_flagged() {
        let source = "DELETE FROM users;";
        let ws = WorkspaceView::empty();
        let diags = MissingWhereClause.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.missing-where-clause");
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn delete_with_where_is_fine() {
        let source = "DELETE FROM users WHERE id = 5;";
        let ws = WorkspaceView::empty();
        assert!(MissingWhereClause.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn update_without_where_is_flagged() {
        let source = "UPDATE users SET email = NULL;";
        let ws = WorkspaceView::empty();
        assert_eq!(MissingWhereClause.check(&ctx(source, &ws)).len(), 1);
    }

    #[test]
    fn select_star_is_flagged() {
        let source = "SELECT * FROM users;";
        let ws = WorkspaceView::empty();
        let diags = SelectStar.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.select-star");
    }

    #[test]
    fn count_star_is_not_flagged() {
        let source = "SELECT COUNT(*) FROM users;";
        let ws = WorkspaceView::empty();
        assert!(
            SelectStar.check(&ctx(source, &ws)).is_empty(),
            "{:?}",
            SelectStar.check(&ctx(source, &ws))
        );
    }

    #[test]
    fn distinct_select_star_is_flagged() {
        let source = "SELECT DISTINCT * FROM users;";
        let ws = WorkspaceView::empty();
        let diags = SelectStar.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn qualified_select_star_is_not_flagged() {
        let source = "SELECT users.* FROM users;";
        let ws = WorkspaceView::empty();
        assert!(SelectStar.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn select_star_after_comma_is_flagged() {
        let source = "SELECT id, * FROM users;";
        let ws = WorkspaceView::empty();
        let diags = SelectStar.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn select_star_inside_comment_is_not_flagged() {
        let source = "-- SELECT * FROM users\nSELECT id FROM users;";
        let ws = WorkspaceView::empty();
        assert!(SelectStar.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn select_star_inside_string_literal_is_not_flagged() {
        let source = "SELECT 'SELECT * FROM users' AS note FROM users;";
        let ws = WorkspaceView::empty();
        assert!(SelectStar.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn select_star_span_points_at_star() {
        let source = "SELECT id, * FROM users;";
        let ws = WorkspaceView::empty();
        let diags = SelectStar.check(&ctx(source, &ws));
        let star = source.find('*').unwrap();
        assert_eq!(diags[0].span, Some(Span::new(star, star + 1)));
    }

    #[test]
    fn select_star_is_skipped_without_parsed_statements() {
        let ws = WorkspaceView::empty();
        let ctx = LintContext {
            file: Path::new("models/user.axm"),
            source: "query List() -> User[] {\n  SELECT * FROM users\n}",
            origin: 0,
            axm: None,
            statements: None,
            workspace: &ws,
        };
        assert!(SelectStar.check(&ctx).is_empty());
    }

    #[test]
    fn unindexed_foreign_key_is_flagged() {
        let source = "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT REFERENCES users(id));";
        let ws = WorkspaceView::empty();
        let diags = UnindexedForeignKey.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.unindexed-foreign-key");
    }

    #[test]
    fn indexed_foreign_key_is_fine() {
        let source = "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT REFERENCES users(id));\nCREATE INDEX ON orders (user_id);";
        let ws = WorkspaceView::empty();
        assert!(UnindexedForeignKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn non_foreign_keys_are_ignored() {
        let source = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);";
        let ws = WorkspaceView::empty();
        assert!(UnindexedForeignKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn primary_key_foreign_key_is_fine() {
        let source = "CREATE TABLE posts (id INT PRIMARY KEY);\nCREATE TABLE post_analytics (post_id TEXT PRIMARY KEY REFERENCES posts(id));";
        let ws = WorkspaceView::empty();
        assert!(
            UnindexedForeignKey.check(&ctx(source, &ws)).is_empty(),
            "{:?}",
            UnindexedForeignKey.check(&ctx(source, &ws))
        );
    }

    #[test]
    fn table_level_unique_foreign_key_is_fine() {
        let source = "CREATE TABLE posts (id INT PRIMARY KEY);\nCREATE TABLE post_analytics (post_id TEXT, UNIQUE (post_id), FOREIGN KEY (post_id) REFERENCES posts(id));";
        let ws = WorkspaceView::empty();
        assert!(UnindexedForeignKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn composite_unique_still_requires_indexed_fk_column() {
        let source = "CREATE TABLE posts (id INT PRIMARY KEY);\nCREATE TABLE post_analytics (post_id TEXT, slug TEXT, UNIQUE (slug), FOREIGN KEY (post_id) REFERENCES posts(id));";
        let ws = WorkspaceView::empty();
        let diags = UnindexedForeignKey.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("post_id"));
    }

    #[test]
    fn table_level_primary_key_covers_fk_column() {
        let source = "CREATE TABLE posts (id INT PRIMARY KEY);\nCREATE TABLE post_analytics (post_id TEXT, PRIMARY KEY (post_id), FOREIGN KEY (post_id) REFERENCES posts(id));";
        let ws = WorkspaceView::empty();
        assert!(UnindexedForeignKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn missing_primary_key_is_flagged() {
        let source = "CREATE TABLE users (id INT, email TEXT);";
        let ws = WorkspaceView::empty();
        let diags = MissingPrimaryKey.check(&ctx(source, &ws));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.missing-primary-key");
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn column_primary_key_is_fine() {
        let source = "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);";
        let ws = WorkspaceView::empty();
        assert!(MissingPrimaryKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn table_level_primary_key_is_fine() {
        let source = "CREATE TABLE users (id INT, email TEXT, PRIMARY KEY (id));";
        let ws = WorkspaceView::empty();
        assert!(MissingPrimaryKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn unique_constraint_satisfies_missing_primary_key() {
        let source = "CREATE TABLE users (id INT, email TEXT UNIQUE);";
        let ws = WorkspaceView::empty();
        assert!(MissingPrimaryKey.check(&ctx(source, &ws)).is_empty());
    }

    #[test]
    fn table_level_unique_is_fine() {
        let source = "CREATE TABLE users (id INT, email TEXT, UNIQUE (email));";
        let ws = WorkspaceView::empty();
        assert!(MissingPrimaryKey.check(&ctx(source, &ws)).is_empty());
    }
}
