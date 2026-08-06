//! Reference graph: where workspace symbols are used, with byte spans.
//!
//! References are resolved against the shared symbol table (built from the
//! real catalog and AXM registry) plus a best-effort scan of query files. The
//! scan never parses anything itself — it walks the token layer and asks the
//! symbol table whether each identifier is meaningful.

use std::path::PathBuf;

use axiom_core::query::QueryCatalog;

use crate::symbols::{is_axm_primitive, Span, SymbolTable};
use crate::token::{PositionIndex, Token, TokenKind};

/// SQL keywords that are never identifiers. Kept deliberately small: anything
/// not in this set is a candidate column reference and is later filtered by the
/// symbol table.
const SQL_KEYWORDS: &[&str] = &[
    "select", "from", "where", "join", "inner", "left", "right", "full", "outer", "cross",
    "on", "as", "and", "or", "not", "in", "is", "null", "like", "ilike", "between", "exists",
    "insert", "into", "values", "update", "set", "delete", "create", "table", "alter", "add",
    "drop", "order", "group", "by", "having", "limit", "offset", "union", "all", "distinct",
    "returning", "primary", "key", "references", "constraint", "unique", "check", "default",
    "index", "with", "case", "when", "then", "else", "end", "cast", "count", "sum", "avg",
    "min", "max", "desc", "asc", "true", "false", "using", "collate", "nulls", "first", "last",
    "window", "over", "partition", "row", "rows", "fetch", "next", "only", "offset", "conflict",
    "do", "nothing", "return", "execute", "function", "begin", "commit", "rollback", "to",
];

fn is_keyword(word: &str) -> bool {
    SQL_KEYWORDS
        .binary_search(&word.to_ascii_lowercase().as_str())
        .is_ok()
}

/// SQL references discovered in a query file.
#[derive(Debug, Clone)]
pub struct SqlRef {
    pub table: String,
    pub column: Option<String>,
    pub file: PathBuf,
    pub span: Span,
    /// Human label, e.g. `users` or `users.email`.
    pub label: String,
}

/// AXM references discovered in a model or query file.
#[derive(Debug, Clone)]
pub struct AxmRef {
    pub name: String,
    pub file: PathBuf,
    pub span: Span,
    /// The file that declares the referenced model, when resolvable.
    pub resolved_file: Option<PathBuf>,
    /// True for `import ... from` clauses, false for field type uses.
    pub is_import: bool,
}

/// Reference sets for one query file.
#[derive(Debug, Clone, Default)]
pub struct QueryRefs {
    pub tables: Vec<SqlRef>,
    pub columns: Vec<SqlRef>,
    /// (alias, table) pairs from FROM/JOIN clauses.
    pub aliases: Vec<(String, String)>,
}

/// A table mention discovered in a query file.
struct TableUse {
    name: String,
    alias: Option<String>,
    span: Span,
}

/// Resolve references in a query file against the workspace symbol table.
pub fn resolve_query_refs(
    file: &std::path::Path,
    _src: &str,
    index: &PositionIndex,
    symbols: &SymbolTable,
) -> QueryRefs {
    let tokens = &index.tokens;
    let mut refs = QueryRefs::default();

    let table_uses = find_table_uses(file, tokens);

    // Alias -> table name map (case-insensitive aliases).
    for table in &table_uses {
        if let Some(alias) = &table.alias {
            refs.aliases.push((alias.to_lowercase(), table.name.clone()));
        }
    }

    for table in &table_uses {
        refs.tables.push(SqlRef {
            table: table.name.clone(),
            column: None,
            file: file.to_path_buf(),
            span: table.span,
            label: table.name.clone(),
        });
    }

    let table_names: Vec<String> = table_uses.iter().map(|t| t.name.to_lowercase()).collect();

    for (i, token) in tokens.iter().enumerate() {
        if !token.is_word() || is_keyword(token.ident_value()) {
            continue;
        }
        let lower = token.ident_value().to_lowercase();
        if table_names.contains(&lower) || refs.aliases.iter().any(|(a, _)| *a == lower) {
            continue;
        }

        // Dotted reference: `alias.column` or `table.column`.
        if let Some(_dot) = tokens.get(i + 1).filter(|t| t.text == ".")
            && let Some(col_tok) = tokens.get(i + 2).filter(|t| t.is_word())
            && let Some(table_name) = resolve_qualifier(&lower, &refs.aliases, symbols)
            && symbols.column(&table_name, col_tok.ident_value()).is_some()
        {
            refs.columns.push(SqlRef {
                table: table_name.clone(),
                column: Some(col_tok.ident_value().to_string()),
                file: file.to_path_buf(),
                span: Span::new(col_tok.start, col_tok.end),
                label: format!("{}.{}", table_name, col_tok.ident_value()),
            });
            continue;
        }

        // Bare identifier: resolve only when exactly one referenced table has
        // a matching column. Avoids renaming unrelated words.
        let matching: Vec<&str> = table_names
            .iter()
            .filter(|name| {
                symbols
                    .table(name)
                    .is_some_and(|t| t.columns.iter().any(|c| c.name.eq_ignore_ascii_case(token.ident_value())))
            })
            .map(String::as_str)
            .collect();
        if matching.len() == 1 {
            let table_name = matching[0].to_string();
            if symbols.column(&table_name, token.ident_value()).is_some() {
                refs.columns.push(SqlRef {
                    table: table_name.clone(),
                    column: Some(token.ident_value().to_string()),
                    file: file.to_path_buf(),
                    span: Span::new(token.start, token.end),
                    label: format!("{}.{}", table_name, token.ident_value()),
                });
            }
        }
    }

    refs
}

fn find_table_uses(_file: &std::path::Path, tokens: &[Token]) -> Vec<TableUse> {
    let mut uses = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if !token.is_word() {
            continue;
        }
        let kw = token.ident_value().to_ascii_lowercase();
        let follows_table = matches!(kw.as_str(), "from" | "join" | "update" | "into" | "table");
        // `DELETE FROM x` — the name follows FROM, already handled above.
        if !follows_table {
            continue;
        }
        // Skip "join" without a following name handled by taking next word.
        let mut j = i + 1;
        while j < tokens.len() && !tokens[j].is_word() {
            j += 1;
        }
        let Some(name_tok) = tokens.get(j) else {
            continue;
        };
        if is_keyword(name_tok.ident_value()) {
            continue;
        }
        let name = name_tok.ident_value().to_string();
        // Optional alias: next word that is not a keyword, not a comma-listed
        // next table, and not a clause keyword.
        let alias = tokens
            .get(j + 1)
            .filter(|t| t.is_word())
            .map(|t| t.ident_value())
            .filter(|a| !is_keyword(a) && !matches!(a.to_ascii_lowercase().as_str(), "where" | "on" | "order" | "group" | "limit" | "offset" | "having" | "union" | "set" | "returning" | "values" | "join" | "left" | "right" | "inner" | "full" | "cross"))
            .map(|a| a.to_string());

        uses.push(TableUse {
            name,
            alias,
            span: Span::new(name_tok.start, name_tok.end),
        });
    }
    uses
}

fn resolve_qualifier(
    qualifier: &str,
    alias_map: &[(String, String)],
    symbols: &SymbolTable,
) -> Option<String> {
    if let Some((_, table)) = alias_map.iter().find(|(alias, _)| alias == qualifier) {
        return Some(table.clone());
    }
    if symbols.table(qualifier).is_some() {
        return Some(qualifier.to_string());
    }
    None
}

/// Collect every field type reference (`address: Address`) and import name in
/// a model file, resolved against the symbol table.
pub fn resolve_axm_refs(
    file: &std::path::Path,
    src: &str,
    index: &PositionIndex,
    axm: &axiom_core::axm::ast::AxmFile,
    symbols: &SymbolTable,
) -> Vec<AxmRef> {
    let mut refs = Vec::new();
    let tokens = &index.tokens;

    for model in &axm.models {
        for field in &model.fields {
            if let Some(name) = named_type(&field.ty) {
                // Locate the field's name span via the symbol table so we know
                // where its type starts.
                let field_start = symbols
                    .model(&model.name)
                    .and_then(|m| m.fields.iter().find(|f| f.name == field.name))
                    .map(|f| f.span.end)
                    .unwrap_or(0);
                let span = field_type_span(tokens, src, field_start, name);
                if let Some(span) = span {
                    refs.push(AxmRef {
                        name: name.to_string(),
                        file: file.to_path_buf(),
                        span,
                        resolved_file: symbols.model(name).map(|m| m.file.clone()),
                        is_import: false,
                    });
                }
            }
        }
    }

    for import in &axm.imports {
        for name in &import.names {
            let span = tokens
                .iter()
                .find(|t| t.is_word() && t.ident_value() == name)
                .map(|t| Span::new(t.start, t.end))
                .or_else(|| index.find_word_any(name).map(|t| Span::new(t.start, t.end)));
            if let Some(span) = span {
                refs.push(AxmRef {
                    name: name.clone(),
                    file: file.to_path_buf(),
                    span,
                    resolved_file: symbols.model(name).map(|m| m.file.clone()),
                    is_import: true,
                });
            }
        }
    }

    refs
}

/// Whether a field type is a named (non-primitive) reference.
fn named_type(ty: &axiom_core::axm::ast::TypeRef) -> Option<&str> {
    use axiom_core::axm::ast::TypeRef;
    match ty {
        TypeRef::Named(name) if !is_axm_primitive(name) => Some(name),
        TypeRef::Array(inner) => named_type(inner),
        _ => None,
    }
}

/// Locate the span of a named type reference in a field.
fn field_type_span(
    tokens: &[Token],
    _src: &str,
    field_end: usize,
    type_name: &str,
) -> Option<Span> {
    let colon = tokens.iter().find(|t| {
        t.start >= field_end && t.kind == TokenKind::Punct && t.text == ":"
    })?;
    let word = tokens.iter().find(|t| {
        t.start >= colon.end && t.is_word() && t.ident_value().eq_ignore_ascii_case(type_name)
    })?;
    Some(Span::new(word.start, word.end))
}

/// Collect `-- @fn name(...) : ReturnType` return-type references. These live
/// in comment lines, so they are found by scanning lines rather than tokens.
pub fn query_return_type_refs(
    file: &std::path::Path,
    src: &str,
    queries: &QueryCatalog,
) -> Vec<AxmRef> {
    let mut refs = Vec::new();
    for query in &queries.queries {
        let (return_name, span) = match &query.return_type {
            axiom_core::query::QueryReturnType::Single(name)
            | axiom_core::query::QueryReturnType::Many(name) => {
                let name = name.trim().to_string();
                match find_annotation_name(src, &name, &query.name) {
                    Some(span) => (name, span),
                    None => continue,
                }
            }
            axiom_core::query::QueryReturnType::Exec => continue,
        };
        refs.push(AxmRef {
            name: return_name,
            file: file.to_path_buf(),
            span,
            resolved_file: None,
            is_import: false,
        });
    }
    refs
}

/// Find the byte span of `name` within the `-- @fn <query.name> ... : name`
/// annotation line.
fn find_annotation_name(src: &str, name: &str, query_name: &str) -> Option<Span> {
    let needle = format!("@{fn}", fn = "fn");
    for (line_no, line) in src.lines().enumerate() {
        if !line.contains(&needle) || !line.contains(query_name) {
            continue;
        }
        let line_start = line_start_of(src, line_no);
        if let Some(colon) = line.rfind(':') {
            let after = &line[colon + 1..];
            let trimmed = after.trim();
            let target = trimmed
                .trim_end_matches(']')
                .trim_end_matches("[]")
                .trim();
            if target.eq_ignore_ascii_case(name) {
                let name_offset = line_start + colon + 1 + (after.len() - after.trim_start().len());
                let end = name_offset + target.len();
                return Some(Span::new(name_offset, end));
            }
        }
    }
    None
}

fn line_start_of(src: &str, line_no: usize) -> usize {
    src.lines()
        .take(line_no)
        .map(|l| l.len() + 1)
        .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiom_core::catalog::{ColumnSchema, TableCatalog, TableSchema};
    use axiom_core::axm::parser::parse_axm_file;
    use crate::symbols::{build_model_symbols, build_table_symbols, SymbolTable};

    fn make_symbols(schema: &str, axm: Option<&str>) -> SymbolTable {
        let mut symbols = SymbolTable::default();
        let schema_index = PositionIndex::new_sql(schema);
        let catalog = parse_catalog(schema);
        for t in build_table_symbols(std::path::Path::new("schema.sql"), &catalog, &schema_index) {
            symbols.tables.push(t);
        }
        if let Some(axm) = axm {
            let index = PositionIndex::new_axm(axm);
            let parsed = parse_axm_file(axm).unwrap();
            for m in build_model_symbols(std::path::Path::new("models/user.axm"), &parsed, &index) {
                symbols.models.push(m);
            }
        }
        // Rebuild lookup indexes (tables pushed directly above bypass add_table).
        for (i, t) in symbols.tables.iter().enumerate() {
            symbols.table_index.insert(t.name.to_lowercase(), i);
        }
        for (i, m) in symbols.models.iter().enumerate() {
            symbols.model_index.insert(m.name.to_lowercase(), i);
        }
        symbols
    }

    fn parse_catalog(src: &str) -> TableCatalog<'static> {
        let catalog = axiom_core::catalog::parse_sql_catalog(src).unwrap();
        let mut tables = Vec::new();
        for t in catalog.tables {
            let columns = t
                .columns
                .iter()
                .map(|c| ColumnSchema {
                    name: c.name.to_string().into(),
                    data_type: c.data_type.to_string().into(),
                    nullable: c.nullable,
                    primary_key: c.primary_key,
                    rules: Vec::new(),
                })
                .collect();
            tables.push(TableSchema {
                name: t.name.to_string().into(),
                columns,
            });
        }
        TableCatalog { tables }
    }

    #[test]
    fn resolves_dotted_and_bare_columns() {
        let schema = "CREATE TABLE users (id serial, email varchar);";
        let query = "SELECT u.email, u.id FROM users u WHERE email IS NOT NULL;";
        let symbols = make_symbols(schema, None);
        let index = PositionIndex::new_sql(query);
        let refs = resolve_query_refs(std::path::Path::new("q.sql"), query, &index, &symbols);
        assert_eq!(refs.tables.len(), 1);
        assert_eq!(refs.tables[0].table, "users");
        assert_eq!(refs.tables[0].label, "users");
        assert_eq!(refs.columns.len(), 3);
        assert!(refs.columns.iter().any(|c| c.label == "users.email"));
        assert!(refs.columns.iter().any(|c| c.label == "users.id"));
        assert!(refs.columns.iter().any(|c| c.label == "users.email" && c.span.start > 10));
    }

    #[test]
    fn axm_refs_pick_named_types() {
        let axm = "model User { email: string }\nmodel Account {\n  owner: User\n}";
        let symbols = make_symbols("", Some(axm));
        let index = PositionIndex::new_axm(axm);
        let parsed = parse_axm_file(axm).unwrap();
        let refs = resolve_axm_refs(std::path::Path::new("models/a.axm"), axm, &index, &parsed, &symbols);
        let user_refs: Vec<_> = refs.iter().filter(|r| r.name == "User").collect();
        assert_eq!(user_refs.len(), 1);
        assert!(user_refs[0].resolved_file.is_some());
    }

    #[test]
    fn return_type_refs_found_in_comments() {
        let src = "-- @fn get_user(email: String) : User\nSELECT id FROM users WHERE email = $1;";
        let queries = axiom_core::query::parse_query_file(src).unwrap();
        let refs = query_return_type_refs(std::path::Path::new("q.sql"), src, &queries);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "User");
        assert_eq!(&src[refs[0].span.start..refs[0].span.end], "User");
    }
}
