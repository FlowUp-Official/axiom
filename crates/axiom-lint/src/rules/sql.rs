//! Lint rules over SQL schema and query files.

use std::collections::BTreeMap;

use axiom_diagnostics::{Diagnostic, Span};
use sqlparser::ast::{
    ColumnOption, CreateIndex, CreateTable, Expr, IndexColumn, Statement, TableConstraint,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::runner::{word_span, LintContext, LintRule};

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
                .with_help("add a `WHERE` clause, or explicitly guard it with `WHERE true` if intended");
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
#[derive(Debug)]
pub struct SelectStar;

impl LintRule for SelectStar {
    fn name(&self) -> &'static str {
        "select-star"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Ok(tokens) = Tokenizer::new(&GenericDialect {}, ctx.source).tokenize() else {
            return Vec::new();
        };

        let mut out = Vec::new();
        let mut prev: Option<String> = None;
        let mut last_position: Option<(usize, usize)> = None;
        for token in tokens {
            match &token {
                Token::Mul => {
                    let is_star_item = prev.as_deref().is_some_and(|p| p == "SELECT" || p == ",");
                    if is_star_item {
                        let span = last_position
                            .map(|(s, e)| Span::new(s, e))
                            .unwrap_or_else(|| Span::new(0, 1));
                        out.push(
                            Diagnostic::warning(
                                ctx.file,
                                "lint.select-star",
                                "`SELECT *` selects every column; list columns explicitly",
                            )
                            .with_help("enumerate the columns instead of `*`")
                            .with_span(span),
                        );
                    }
                    prev = Some("*".to_string());
                }
                Token::Whitespace(_) | Token::EOF => {}
                other => {
                    prev = Some(other.to_string().to_uppercase());
                    last_position = token_span(&token, ctx.source);
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

        for stmt in statements {
            match stmt {
                Statement::CreateTable(create) => collect_table_fks(create, &mut foreign_keys),
                Statement::CreateIndex(index) => collect_index(index, &mut indexes),
                _ => {}
            }
        }

        let mut out = Vec::new();
        for (table, column) in foreign_keys {
            let covered = indexes
                .get(&table.to_lowercase())
                .is_some_and(|cols| cols.iter().any(|c| c == &column.to_lowercase()));
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

/// Best-effort byte span of a token in the source.
fn token_span(token: &Token, source: &str) -> Option<(usize, usize)> {
    let text = token.to_string();
    let lower = source.to_ascii_lowercase();
    let rel = lower.find(&text.to_ascii_lowercase())?;
    Some((rel, rel + text.len()))
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
        assert!(SelectStar.check(&ctx(source, &ws)).is_empty(), "{:?}", SelectStar.check(&ctx(source, &ws)));
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
}
