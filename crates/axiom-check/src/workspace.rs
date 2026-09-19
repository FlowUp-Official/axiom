//! Input resolution and the per-input check phases.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sqlparser::ast::{Expr, ObjectName, Select, SelectItem, SetExpr, Statement, Visit, Visitor};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use axiom_core::axm::ast::{
    AnnotatedType, ParamDecl, QueryReturn, Rule, Target, Transform, TypeRef,
};
use axiom_core::axm::codegen::{emitted_model_names, query_emitted_for, transaction_emitted_for, resolve_relation};
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::axm::resolver::{ModelRegistry, resolve_models};
use axiom_core::cache::{ToolCache, compute_content_hash};
use axiom_core::catalog::{TableCatalog, parse_sql_catalog};
use axiom_core::config::{AxiomConfig, resolve_glob_paths};
use axiom_core::errors::AxiomError;
use axiom_core::query::{
    Placeholder, QueryCatalog, QueryDefinition, scan_dotted_placeholders, scan_placeholders,
};
use axiom_diagnostics::{Diagnostic, Span};

use crate::diagnostics::{find_span, line_of_offset, parse_error};

/// All resolved input sources, with their file contents.
pub struct Workspace {
    pub schema_files: Vec<(PathBuf, String)>,
    pub model_files: Vec<(PathBuf, String)>,
}

/// Resolve every configured input glob into an ordered list of `(path, src)`.
pub fn resolve_inputs(config: &AxiomConfig, base: &Path) -> Result<Workspace, AxiomError> {
    let read = |patterns: &[String]| -> Result<Vec<(PathBuf, String)>, AxiomError> {
        let mut out = Vec::new();
        for path in resolve_glob_paths(patterns, base)? {
            out.push((path.clone(), std::fs::read_to_string(&path)?));
        }
        Ok(out)
    };
    Ok(Workspace {
        schema_files: read(&config.source.schema)?,
        model_files: read(&config.source.axm)?,
    })
}

/// Parse every schema file into a shared catalog, reporting per-file syntax
/// failures. The catalog is always rebuilt: the synchronization check needs it.
pub fn check_schemas<'a>(files: &'a [(PathBuf, String)]) -> (TableCatalog<'a>, Vec<Diagnostic>) {
    let mut catalog = TableCatalog::default();
    let mut diags = Vec::new();
    for (path, src) in files {
        match parse_sql_catalog(src) {
            Ok(parsed) => catalog.tables.extend(parsed.tables),
            Err(err) => diags.push(parse_error(path, "check.sql-parse", err.to_string())),
        }
    }
    (catalog, diags)
}

/// Compile every `query` and `transaction` declaration in the linked registry
/// into the shared query catalog and verify each one against the schema
/// catalog: referenced tables must exist, column references must resolve, the
/// declared return type must match a table, model, or type alias, and the SQL
/// body must honor the declared return contract (rows vs. execution).
///
/// Queries are parsed once by [`resolve_models`]; this phase only validates
/// and compiles, so no per-file re-parsing happens. Per-file results are
/// cached keyed by the file's content hash plus the aggregate schema hash.
pub fn check_queries(
    mut cache: Option<&mut ToolCache>,
    schema_hash: &[u8; 32],
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
    model_files: &[(PathBuf, String)],
) -> (QueryCatalog<'static>, Vec<Diagnostic>) {
    let query_catalog = axiom_core::axm::query_catalog(registry);
    let mut diags = Vec::new();

    let src_by_path: BTreeMap<PathBuf, String> = model_files
        .iter()
        .map(|(p, s)| (p.clone(), s.clone()))
        .collect();

    let mut per_file: BTreeMap<PathBuf, Vec<DeclaredStmt>> = BTreeMap::new();
    for resolved in &registry.queries {
        per_file
            .entry(resolved.path.clone())
            .or_default()
            .push(DeclaredStmt {
                kind: DeclKind::Query,
                name: &resolved.query.name,
                params: &resolved.query.params,
                return_type: &resolved.query.return_type,
                sql: &resolved.query.sql,
            });
    }
    for resolved in &registry.transactions {
        per_file
            .entry(resolved.path.clone())
            .or_default()
            .push(DeclaredStmt {
                kind: DeclKind::Transaction,
                name: &resolved.transaction.name,
                params: &resolved.transaction.params,
                return_type: &resolved.transaction.return_type,
                sql: &resolved.transaction.sql,
            });
    }

    for (path, statements) in &per_file {
        let src = src_by_path.get(path).map(String::as_str).unwrap_or("");
        let file_hash = compute_content_hash(src.as_bytes());
        let key = format!("check:query:{}:{}", hex(schema_hash), hex(&file_hash));

        let file_diags = if let Some(cache) = cache.as_deref()
            && let Some(payload) = cache.get(&key)
            && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
        {
            cached
        } else {
            let computed: Vec<Diagnostic> = statements
                .iter()
                .flat_map(|stmt| check_declared_statement(path, src, stmt, catalog, registry))
                .collect();
            if let Some(cache) = cache.as_mut()
                && let Ok(payload) = serde_json::to_vec(&computed)
            {
                cache.insert(key, payload);
            }
            computed
        };
        diags.extend(file_diags);
    }

    (query_catalog, diags)
}

/// Validate every `model ... extends select<relation>` source against the
/// schema catalog. The relation is a database identifier: it must match a
/// declared table exactly (for qualified names) or by its last segment (for
/// unqualified names), always case-sensitively.
pub fn check_model_sources(
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
    model_files: &[(PathBuf, String)],
) -> Vec<Diagnostic> {
    let src_by_path: BTreeMap<PathBuf, String> = model_files
        .iter()
        .map(|(p, s)| (p.clone(), s.clone()))
        .collect();
    let mut diags = Vec::new();
    for resolved in &registry.models {
        let Some(source) = &resolved.model.source else {
            continue;
        };
        let relation = &source.relation;
        if resolve_relation(catalog, relation).is_some() {
            continue;
        }
        let src = src_by_path
            .get(&resolved.path)
            .map(String::as_str)
            .unwrap_or("");
        let span = model_source_relation_span(src, relation);
        diags.push(
            Diagnostic::error(
                &resolved.path,
                "check.model-source",
                format!(
                    "model `{}` extends select<{relation}>, which does not match any table in the schema",
                    resolved.model.name
                ),
            )
            .with_help(
                "select<...> names a database relation; use the exact (case-sensitive) table \
                 name from a schema file, or the unqualified name to match the last segment \
                 of a qualified table",
            )
            .with_span(span),
        );
    }
    diags
}

/// Prefer a span covering the relation text inside `select<...>`; falls back
/// to the start of the declaring line.
fn model_source_relation_span(src: &str, relation: &str) -> Span {
    let rel_pos = find_span(src, "select<", 0).map(|s| s.end).unwrap_or(0);
    if let Some(rel_end) = src[rel_pos..].find('>')
        && src[rel_pos..rel_pos + rel_end].trim() == relation
    {
        let raw = &src[rel_pos..rel_pos + rel_end];
        let trimmed = raw.trim();
        let start = rel_pos + raw.len() - trimmed.len();
        Span::new(start, start + trimmed.len())
    } else {
        line_of_offset(src, rel_pos)
    }
}

/// Report fields declared more than once within the same model.
///
/// The parser and resolver reject duplicate *declaration* names (models and
/// type aliases share one namespace), but a model's field list is a
/// model-local namespace the resolver never inspects. Duplicate fields resolve
/// without error and generate ambiguous or non-compiling output, so they are a
/// correctness error.
pub fn check_duplicate_fields(
    registry: &ModelRegistry,
    model_files: &[(PathBuf, String)],
) -> Vec<Diagnostic> {
    let src_by_path: BTreeMap<PathBuf, String> = model_files.iter().cloned().collect();
    let mut diags = Vec::new();
    for resolved in &registry.models {
        let src = src_by_path
            .get(&resolved.path)
            .map(String::as_str)
            .unwrap_or("");
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for field in &resolved.model.fields {
            let count = seen.entry(field.name.as_str()).or_insert(0);
            *count += 1;
            if *count < 2 {
                continue;
            }
            let span = field_decl_span(src, &field.name, *count - 1);
            let mut diag = Diagnostic::error(
                &resolved.path,
                "check.duplicate-field",
                format!(
                    "model `{}` declares field `{}` more than once",
                    resolved.model.name, field.name
                ),
            )
            .with_help(
                "remove or rename the duplicate field; generated code shares one field namespace",
            );
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            diags.push(diag);
        }
    }
    diags
}

/// Report references from a declaration emitted for a codegen target to a
/// model that target's `@target(...)` excludes.
///
/// `emit_plan` already decides which models each target emits; a reference from
/// an emitted declaration to an omitted model produces output that names a
/// type/struct the target never generated. Type aliases are emitted for every
/// target, so their bases are checked too. A reference hidden behind a pure
/// alias is caught when the alias itself is checked, so aliases are not
/// followed from their referencing declarations.
pub fn check_target_references(
    config: &AxiomConfig,
    registry: &ModelRegistry,
    model_files: &[(PathBuf, String)],
) -> Vec<Diagnostic> {
    let src_by_path: BTreeMap<PathBuf, String> = model_files.iter().cloned().collect();
    let mut diags = Vec::new();
    let mut seen_targets = BTreeSet::new();

    for name in config.target_types() {
        let Some(target) = Target::parse(name) else {
            continue;
        };
        if !seen_targets.insert(target.name()) {
            continue;
        }
        let emitted = emitted_model_names(registry, target);

        for resolved in &registry.models {
            if !emitted.contains(&resolved.model.name) {
                continue;
            }
            let src = src_by_path
                .get(&resolved.path)
                .map(String::as_str)
                .unwrap_or("");
            let owner = format!("model `{}`", resolved.model.name);
            for field in &resolved.model.fields {
                let mut refs = Vec::new();
                collect_excluded_refs(registry, &resolved.path, &field.ty.base, &emitted, &mut refs);
                for reference in refs {
                    let span = decl_keyword_start(src, "model", &resolved.model.name)
                        .and_then(|from| word_span_from(src, &field.name, from))
                        .and_then(|field| word_span_from(src, &reference.written, field.end));
                    push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
                }
            }
        }

        for resolved in &registry.types {
            let src = src_by_path
                .get(&resolved.path)
                .map(String::as_str)
                .unwrap_or("");
            let owner = format!("type `{}`", resolved.ty.name);
            let mut refs = Vec::new();
            collect_excluded_refs(
                registry,
                &resolved.path,
                &resolved.ty.ty.base,
                &emitted,
                &mut refs,
            );
            for reference in refs {
                let span = decl_keyword_start(src, "type", &resolved.ty.name)
                    .and_then(|from| word_span_from(src, &reference.written, from));
                push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
            }
        }

        for resolved in &registry.queries {
            if !query_emitted_for(&resolved.query, target) {
                continue;
            }
            let src = src_by_path
                .get(&resolved.path)
                .map(String::as_str)
                .unwrap_or("");
            let owner = format!("query `{}`", resolved.query.name);
            let query_start = decl_keyword_start(src, "query", &resolved.query.name);
            for param in &resolved.query.params {
                let mut refs = Vec::new();
                collect_excluded_refs(registry, &resolved.path, &param.ty, &emitted, &mut refs);
                for reference in refs {
                    let span = query_start
                        .and_then(|from| param_type_span(src, from, &param.name, &reference.written));
                    push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
                }
            }
            let return_ty = match &resolved.query.return_type {
                QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
                    Some(ty)
                }
                QueryReturn::Exec => None,
            };
            if let Some(ty) = return_ty {
                let mut refs = Vec::new();
                collect_excluded_refs(registry, &resolved.path, ty, &emitted, &mut refs);
                for reference in refs {
                    let span = query_start
                        .and_then(|from| word_span_from(src, &reference.written, from));
                    push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
                }
            }
        }

        for resolved in &registry.transactions {
            if !transaction_emitted_for(&resolved.transaction, target) {
                continue;
            }
            let src = src_by_path
                .get(&resolved.path)
                .map(String::as_str)
                .unwrap_or("");
            let owner = format!("transaction `{}`", resolved.transaction.name);
            let txn_start = decl_keyword_start(src, "transaction", &resolved.transaction.name);
            for param in &resolved.transaction.params {
                let mut refs = Vec::new();
                collect_excluded_refs(registry, &resolved.path, &param.ty, &emitted, &mut refs);
                for reference in refs {
                    let span = txn_start
                        .and_then(|from| param_type_span(src, from, &param.name, &reference.written));
                    push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
                }
            }
            let return_ty = match &resolved.transaction.return_type {
                QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
                    Some(ty)
                }
                QueryReturn::Exec => None,
            };
            if let Some(ty) = return_ty {
                let mut refs = Vec::new();
                collect_excluded_refs(registry, &resolved.path, ty, &emitted, &mut refs);
                for reference in refs {
                    let span = txn_start
                        .and_then(|from| word_span_from(src, &reference.written, from));
                    push_excluded_reference(&mut diags, &resolved.path, target, &owner, span, &reference);
                }
            }
        }
    }

    diags
}

/// A reference from a checked declaration to a model omitted from a target.
struct ExcludedReference {
    /// The spelling at the reference site (`DbUser`), used for the span.
    written: String,
    /// The canonical declaration name (`User`), used for the message.
    canonical: String,
}

/// Collect references to models that are absent from `emitted`. Type aliases
/// are skipped: they are emitted for every target and checked where declared.
fn collect_excluded_refs(
    registry: &ModelRegistry,
    path: &Path,
    ty: &TypeRef,
    emitted: &BTreeSet<String>,
    out: &mut Vec<ExcludedReference>,
) {
    match ty {
        TypeRef::Named(written) => {
            let canonical = registry.effective_name(path, written);
            if registry.type_by_name(canonical).is_some() {
                return;
            }
            if registry.model_by_name(canonical).is_some() && !emitted.contains(canonical) {
                out.push(ExcludedReference {
                    written: written.clone(),
                    canonical: canonical.to_string(),
                });
            }
        }
        TypeRef::Array(inner) | TypeRef::Nullable(inner) => {
            collect_excluded_refs(registry, path, inner, emitted, out);
        }
        _ => {}
    }
}

fn push_excluded_reference(
    out: &mut Vec<Diagnostic>,
    path: &Path,
    target: Target,
    owner: &str,
    span: Option<Span>,
    reference: &ExcludedReference,
) {
    let canonical = &reference.canonical;
    let target_name = target.name();
    let mut diag = Diagnostic::error(
        path,
        "check.target-excluded-reference",
        format!(
            "{owner} references model `{canonical}`, which `@target` excludes from target `{target_name}`"
        ),
    )
    .with_help(format!(
        "emit `{canonical}` for `{target_name}` (widen its `@target`) or stop referencing it from {owner}"
    ));
    if let Some(span) = span {
        diag = diag.with_span(span);
    }
    out.push(diag);
}

/// Byte span of the `name` in the first `keyword name` declaration at or after
/// the start of `source`.
fn decl_keyword_start(source: &str, keyword: &str, name: &str) -> Option<usize> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for (start, _) in source.match_indices(keyword) {
        if start > 0 && source[..start].chars().next_back().is_some_and(is_word) {
            continue;
        }
        let after_kw = &source[start + keyword.len()..];
        let trimmed = after_kw.trim_start();
        if !trimmed.starts_with(name) {
            continue;
        }
        let name_at = start + keyword.len() + (after_kw.len() - trimmed.len());
        let after = source[name_at + name.len()..].chars().next();
        if !after.is_some_and(|c| c.is_ascii_whitespace() || matches!(c, '{' | '(' | '=')) {
            continue;
        }
        return Some(name_at);
    }
    None
}

/// Byte span of `$param`'s type token within the query whose declaration starts
/// at `from`.
fn param_type_span(source: &str, from: usize, param: &str, ty: &str) -> Option<Span> {
    let needle = format!("${param}:");
    let start = source[from..].find(&needle).map(|i| from + i + needle.len())?;
    word_span_from(source, ty, start)
}

/// Byte span of `name` as a whole word at or after `from`.
fn word_span_from(source: &str, name: &str, from: usize) -> Option<Span> {
    let from = from.min(source.len());
    let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut offset = from;
    while offset <= source.len() {
        let rel = source[offset..].find(name)?;
        let start = offset + rel;
        let before = source[..start].chars().next_back();
        let after = source[start + name.len()..].chars().next();
        if boundary(before) && boundary(after) {
            return Some(Span::new(start, start + name.len()));
        }
        offset = start + name.len();
    }
    None
}

/// Byte span of the `occurrence`th (0-based) field declaration of `name`, where
/// a field declaration is a line whose trimmed text starts with `name` followed
/// by `?` or `:`.
fn field_decl_span(source: &str, name: &str, occurrence: usize) -> Option<Span> {
    let mut offset = 0;
    let mut seen = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(name)
            && (rest.starts_with('?') || rest.starts_with(':'))
        {
            if seen == occurrence {
                let leading = line.len() - trimmed.len();
                return Some(Span::new(offset + leading, offset + leading + name.len()));
            }
            seen += 1;
        }
        offset += line.len() + 1;
    }
    None
}

/// Which statement-style declaration is being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Query,
    Transaction,
}

impl DeclKind {
    fn keyword(self) -> &'static str {
        match self {
            DeclKind::Query => "query",
            DeclKind::Transaction => "transaction",
        }
    }
}

/// A resolved `query` or `transaction` declaration, normalized for checking.
struct DeclaredStmt<'a> {
    kind: DeclKind,
    name: &'a str,
    params: &'a [ParamDecl],
    return_type: &'a QueryReturn,
    sql: &'a str,
}

/// Validate a single `query`/`transaction` declaration: placeholders must
/// resolve to a declared parameter, the return type must refer to a known
/// table, model, or type alias, and the SQL body must match the declared
/// return contract. `query` bodies must stay a single statement while
/// `transaction` bodies must contain at least two.
fn check_declared_statement(
    path: &Path,
    src: &str,
    statement: &DeclaredStmt,
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    let body_statements = QueryDefinition::split_statements(statement.sql);
    match statement.kind {
        DeclKind::Query if body_statements.len() > 1 => {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.query-multi-statement",
                    format!(
                        "query `{}` contains {} statements, but a `query` may only contain one",
                        statement.name,
                        body_statements.len()
                    ),
                )
                .with_help(
                    "split each statement into its own `query`, or use a transaction for a multi-statement body",
                ),
            );
        }
        DeclKind::Transaction if body_statements.len() < 2 => {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.transaction-statement-count",
                    format!(
                        "transaction `{}` contains {} statement{}, but a transaction requires at least 2",
                        statement.name,
                        body_statements.len(),
                        if body_statements.len() == 1 { "" } else { "s" },
                    ),
                )
                .with_help(
                    "a transaction groups multiple statements that run atomically; use `query` for a single statement",
                ),
            );
        }
        _ => {}
    }

    diags.extend(check_query_body(path, statement.sql.trim(), catalog));

    let body_start = src.find(statement.sql).unwrap_or(0);
    for (start, _, kind) in scan_placeholders(statement.sql) {
        let span = line_of_offset(src, body_start + start);
        match kind {
            Placeholder::Positional(n) if n > statement.params.len() => {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-placeholder",
                        format!(
                            "{} `{}` uses placeholder `${n}`, but only {} parameter{} are declared",
                            statement.kind.keyword(),
                            statement.name,
                            statement.params.len(),
                            if statement.params.len() == 1 { "is" } else { "s" },
                        ),
                    )
                    .with_span(span),
                );
            }
            Placeholder::Named(name) if statement.params.iter().all(|p| p.name != name) => {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-placeholder",
                        format!(
                            "{} `{}` uses placeholder `${name}`, which is not declared in the `{}` signature",
                            statement.kind.keyword(),
                            statement.name,
                            statement.kind.keyword(),
                        ),
                    )
                    .with_help(
                        "add the parameter to the declaration, or fix the placeholder",
                    )
                    .with_span(span),
                );
            }
            _ => {}
        }
    }

    for (start, _, dotted) in scan_dotted_placeholders(statement.sql) {
        let span = line_of_offset(src, body_start + start);
        let Some((base, field)) = dotted.split_once('.') else {
            continue;
        };
        let Some(param) = statement.params.iter().find(|p| p.name == base) else {
            // An undeclared base parameter is already reported above.
            continue;
        };
        let TypeRef::Named(model_name) = &param.ty else {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.query-placeholder",
                    format!(
                        "{} `{}` uses placeholder `${dotted}` to address fields of `${base}`, but `${base}` is not a model parameter (its type is `{}`)",
                        statement.kind.keyword(),
                        statement.name,
                        axiom_core::axm::type_ref_name(&param.ty),
                    ),
                )
                .with_help("only parameters typed with a model can be accessed with `$param.field`")
                .with_span(span),
            );
            continue;
        };
        let Some(model) = registry.model_by_name(model_name) else {
            continue;
        };
        if !model.model.fields.iter().any(|f| f.name == field) {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.query-placeholder",
                    format!(
                        "{} `{}` uses placeholder `${dotted}`, but model `{model_name}` has no field `{field}`",
                        statement.kind.keyword(),
                        statement.name
                    ),
                )
                .with_help("add the field to the model or fix the placeholder")
                .with_span(span),
            );
        }
    }

    match &statement.return_type {
        QueryReturn::Exec => {}
        QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
            let name = axiom_core::axm::type_ref_name(ty);
            if !known_return_type(catalog, registry, &name) {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-return-type",
                        format!(
                            "{} `{}` returns `{name}`, but no such table, model, or type exists",
                            statement.kind.keyword(),
                            statement.name
                        ),
                    )
                    .with_help(
                        "declare the table in a schema file or the model/type in a `.axm` file",
                    ),
                );
            }
        }
    }

    diags.extend(check_return_contract(path, statement, catalog, registry));
    diags
}

/// Whether a return type name resolves to a catalog table (case-insensitive)
/// or a linked model or type alias (case-sensitive).
fn known_return_type(catalog: &TableCatalog<'_>, registry: &ModelRegistry, name: &str) -> bool {
    let table = catalog.tables.iter().any(|t| {
        t.name
            .rsplit('.')
            .next()
            .unwrap_or(&t.name)
            .eq_ignore_ascii_case(name)
    });
    table || registry.index.contains_key(name) || registry.type_index.contains_key(name)
}

/// Verify the declared return contract against the SQL body: a row-returning
/// contract needs a statement that produces rows, `Exec` must not be a plain
/// `SELECT`, and bare projection identifiers must be fields of the declared
/// row type.
fn check_return_contract(
    path: &Path,
    statement: &DeclaredStmt,
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
) -> Vec<Diagnostic> {
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, statement.sql) else {
        return Vec::new(); // body parse errors are already reported
    };
    // The return contract describes the rows a caller receives, which is the
    // result of the LAST statement of the body.
    let Some(statement_ref) = statements.into_iter().next_back() else {
        return Vec::new();
    };

    let produces_rows = statement_produces_rows(&statement_ref);
    let expects_rows = !matches!(statement.return_type, QueryReturn::Exec);

    let prefix = format!("{} `{}`", statement.kind.keyword(), statement.name);
    if expects_rows && !produces_rows {
        return vec![
            Diagnostic::error(
                path,
                "check.query-contract",
                format!(
                    "{prefix} declares a row return type, but the SQL body does not return any rows"
                ),
            )
            .with_help(
                "declare `-> Type`, `-> Type?`, or `-> Type[]` only for row-returning statements",
            ),
        ];
    }
    if !expects_rows && produces_rows {
        return vec![
            Diagnostic::error(
                path,
                "check.query-contract",
                format!("{prefix} is declared as `Exec` (no `->`), but the SQL body returns rows"),
            )
            .with_help(
                "add a `-> Type` return contract, or use a statement that does not select rows",
            ),
        ];
    }

    let (QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty)) =
        &statement.return_type
    else {
        return Vec::new();
    };
    let row_name = axiom_core::axm::type_ref_name(ty);
    let Some(select) = single_select(&statement_ref) else {
        return Vec::new();
    };
    let projected = projection_identifiers(select);
    if projected.is_empty() {
        return Vec::new();
    }

    if let Some(model) = registry.model_by_name(&row_name) {
        let field_names: Vec<String> = model.model.fields.iter().map(|f| f.name.clone()).collect();
        projection_vs_fields(path, statement.name, &row_name, &projected, &field_names, true)
    } else if let Some(table) = catalog.table_by_name(&row_name) {
        let field_names: Vec<String> = table.columns.iter().map(|c| c.name.to_string()).collect();
        projection_vs_fields(path, statement.name, &row_name, &projected, &field_names, false)
    } else {
        Vec::new() // unresolved row type already reported
    }
}

fn projection_vs_fields(
    path: &Path,
    name: &str,
    row_name: &str,
    projected: &[String],
    field_names: &[String],
    case_sensitive: bool,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for ident in projected {
        let matched = if case_sensitive {
            field_names.iter().any(|f| f == ident)
        } else {
            field_names.iter().any(|f| f.eq_ignore_ascii_case(ident))
        };
        if !matched {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.query-contract",
                    format!(
                        "`{name}` projects column `{ident}`, which is not a field of the declared `{row_name}` type"
                    ),
                )
                .with_help("align the SQL projection with the declared return type's fields"),
            );
        }
    }
    diags
}

/// Does this statement produce rows to callers?
fn statement_produces_rows(statement: &Statement) -> bool {
    match statement {
        Statement::Query(_) => true,
        Statement::Insert(insert) => insert.returning.is_some(),
        Statement::Update(update) => update.returning.is_some(),
        Statement::Delete(delete) => delete.returning.is_some(),
        _ => false,
    }
}

/// The top-level `SELECT` of a plain (non-composed) query statement, if any.
fn single_select(statement: &Statement) -> Option<&Select> {
    let Statement::Query(query) = statement else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    Some(select.as_ref())
}

/// Bare (unqualified) identifiers in a `SELECT` projection. Qualified refs,
/// expressions, and wildcards are skipped so joins do not produce false
/// positives.
fn projection_identifiers(select: &Select) -> Vec<String> {
    select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::UnnamedExpr(Expr::Identifier(id))
            | SelectItem::ExprWithAlias {
                expr: Expr::Identifier(id),
                ..
            } => Some(id.value.clone()),
            _ => None,
        })
        .collect()
}

/// Verify a query body's tables and columns against the catalog. Returns the
/// collected diagnostics; a body that does not parse yields a single
/// `check.query-sql` error.
fn check_query_body(path: &Path, sql: &str, catalog: &TableCatalog<'_>) -> Vec<Diagnostic> {
    let statements = match Parser::parse_sql(&GenericDialect {}, sql) {
        Ok(stmts) => stmts,
        Err(err) => {
            return vec![parse_error(
                path,
                "check.query-sql",
                format!("failed to parse query SQL: {err}"),
            )];
        }
    };

    let mut refs = QueryRefs::default();
    let _ = statements.visit(&mut refs);

    let mut diags = Vec::new();
    let mut known_columns: Vec<String> = Vec::new();

    for relation in &refs.relations {
        if let Some(table) = catalog.table_by_name(relation) {
            known_columns.extend(
                table
                    .columns
                    .iter()
                    .map(|c| c.name.to_string().to_lowercase()),
            );
        } else {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.missing-table",
                    format!(
                        "query references table `{relation}`, which is not defined in the schema"
                    ),
                )
                .with_help("add the table to a schema input, or fix the query"),
            );
        }
    }

    for ident in &refs.identifiers {
        let lower = ident.to_lowercase();
        if !known_columns.is_empty() && !known_columns.contains(&lower) {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.missing-column",
                    format!("column `{ident}` is not defined on any table referenced by the query"),
                )
                .with_help("fix the column name, or qualify it with a table"),
            );
        }
    }
    diags
}

/// Collect table relations and bare column identifiers from a SQL statement
/// list. This is a best-effort reference scan (aliases and subqueries can
/// produce extra identifiers).
#[derive(Default)]
struct QueryRefs {
    relations: Vec<String>,
    identifiers: Vec<String>,
}

impl Visitor for QueryRefs {
    type Break = ();

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> std::ops::ControlFlow<()> {
        self.relations.push(relation.to_string());
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
        if let Expr::Identifier(id) = expr {
            self.identifiers.push(id.to_string());
        }
        std::ops::ControlFlow::Continue(())
    }
}

/// Parse every model file, check `.regex()` patterns, and link the workspace.
/// Linking failures (duplicates, unresolvable imports, cycles, unknown types)
/// are reported as diagnostics while still yielding a usable registry when
/// possible.
pub fn check_models(
    mut cache: Option<&mut ToolCache>,
    files: &[(PathBuf, String)],
) -> (Option<ModelRegistry>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut all_parsed = true;

    for (path, src) in files {
        if let Err(err) = parse_axm_file(src) {
            all_parsed = false;
            diags.push(parse_error(path, "check.axm-parse", err.to_string()));
        }
    }

    let regex_key = format!("check:regex:{}", hex(&aggregate_hash(files)));
    let regex_diags = if let Some(cache) = cache.as_deref()
        && let Some(payload) = cache.get(&regex_key)
        && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
    {
        cached
    } else {
        let computed = check_regexes(files);
        if let Some(cache) = cache.as_mut()
            && let Ok(payload) = serde_json::to_vec(&computed)
        {
            cache.insert(regex_key, payload);
        }
        computed
    };
    diags.extend(regex_diags);

    let registry = if all_parsed {
        match resolve_models(files) {
            Ok(registry) => Some(registry),
            Err(err) => {
                diags.push(link_error(files.first().map(|(p, _)| p.as_path()), err));
                None
            }
        }
    } else {
        None
    };

    if let Some(registry) = &registry {
        diags.extend(check_rule_base_compat(registry, files));
    }

    (registry, diags)
}

/// The broad base-class of a type, after expanding named aliases.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BaseClass {
    Str,
    Num,
    Collection,
    Other,
}

fn base_class(registry: &ModelRegistry, ty: &TypeRef, depth: usize) -> BaseClass {
    if depth > 16 {
        return BaseClass::Other;
    }
    match ty {
        TypeRef::String | TypeRef::Uuid => BaseClass::Str,
        TypeRef::Int | TypeRef::BigInt | TypeRef::Float => BaseClass::Num,
        TypeRef::Array(_) => BaseClass::Collection,
        TypeRef::Nullable(inner) => base_class(registry, inner, depth + 1),
        TypeRef::Named(name) => match registry.type_index.get(name) {
            Some(&i) => base_class(registry, &registry.types[i].ty.ty.base, depth + 1),
            None => BaseClass::Other,
        },
        _ => BaseClass::Other,
    }
}

/// The canonical spelling of a transform for diagnostics.
fn transform_call_name(transform: &Transform) -> &'static str {
    match transform {
        Transform::Trim => "trim",
        Transform::Lowercase => "lowercase",
        Transform::Uppercase => "uppercase",
    }
}

/// The canonical spelling of a rule for diagnostics, e.g. `.min_length(3)`.
fn rule_call_name(rule: &Rule) -> String {
    match rule {
        Rule::Min(_, _) => ".min(...)".to_string(),
        Rule::Max(_, _) => ".max(...)".to_string(),
        Rule::MinLength(_, _) => ".min_length(...)".to_string(),
        Rule::MaxLength(_, _) => ".max_length(...)".to_string(),
        Rule::Regex(_, _) => ".regex(...)".to_string(),
        Rule::Email(_) => ".email()".to_string(),
        Rule::Url(_) => ".url()".to_string(),
        Rule::Uuid(_) => ".uuid()".to_string(),
        Rule::Ulid(_) => ".ulid()".to_string(),
        Rule::Ipv4(_) => ".ipv4()".to_string(),
        Rule::Ipv6(_) => ".ipv6()".to_string(),
        Rule::IsoDate(_) => ".isodate()".to_string(),
        Rule::Alphanumeric(_) => ".alphanumeric()".to_string(),
        Rule::NonEmpty(_) => ".nonempty()".to_string(),
    }
}

/// Whether `rule` may be applied to a value of the given base class, per the
/// rule/base table: numeric rules on numbers, string rules on strings,
/// collection rules on strings and arrays. Everything else is rejected.
fn rule_class_compatible(class: BaseClass, rule: &Rule) -> bool {
    match rule {
        Rule::Min(..) | Rule::Max(..) => class == BaseClass::Num,
        Rule::Email(_)
        | Rule::Url(_)
        | Rule::Uuid(_)
        | Rule::Ulid(_)
        | Rule::Ipv4(_)
        | Rule::Ipv6(_)
        | Rule::IsoDate(_)
        | Rule::Alphanumeric(_)
        | Rule::Regex(..) => class == BaseClass::Str,
        Rule::NonEmpty(_) | Rule::MinLength(..) | Rule::MaxLength(..) => {
            matches!(class, BaseClass::Str | BaseClass::Collection)
        }
    }
}

/// Validate the rule/base compatibility table over every model field and type
/// alias (transforms only on strings; rules matched by base class).
fn check_rule_base_compat(
    registry: &ModelRegistry,
    files: &[(PathBuf, String)],
) -> Vec<Diagnostic> {
    let src_by_path: BTreeMap<PathBuf, String> = files.iter().cloned().collect();
    let mut diags = Vec::new();
    for resolved in &registry.models {
        let src = src_by_path
            .get(&resolved.path)
            .map(String::as_str)
            .unwrap_or("");
        for field in &resolved.model.fields {
            check_type_rules(
                &mut diags,
                &resolved.path,
                src,
                &resolved.model.name,
                &field.name,
                &field.ty,
                registry,
            );
        }
    }
    for resolved in &registry.types {
        let src = src_by_path
            .get(&resolved.path)
            .map(String::as_str)
            .unwrap_or("");
        check_type_rules(
            &mut diags,
            &resolved.path,
            src,
            &resolved.ty.name,
            &resolved.ty.name,
            &resolved.ty.ty,
            registry,
        );
    }
    diags
}

fn check_type_rules(
    diags: &mut Vec<Diagnostic>,
    path: &Path,
    src: &str,
    owner: &str,
    field: &str,
    ty: &AnnotatedType,
    registry: &ModelRegistry,
) {
    let class = base_class(registry, &ty.base, 0);
    for transform in &ty.transforms {
        if class != BaseClass::Str {
            let span = line_of_offset(src, find_span(src, field, 0).map(|s| s.start).unwrap_or(0));
            diags.push(
                Diagnostic::error(
                    path,
                    "check.rule-base",
                    format!(
                        "field `{field}` of model `{owner}` applies `.{}`, which is only valid on `String` fields",
                        transform_call_name(transform),
                    ),
                )
                .with_help("move the transform to a `String`-typed field or remove it")
                .with_span(span),
            );
        }
    }
    for rule in &ty.rules {
        if rule_class_compatible(class, rule) {
            continue;
        }
        let span = line_of_offset(src, find_span(src, field, 0).map(|s| s.start).unwrap_or(0));
        let allowed = if matches!(rule, Rule::Min(..) | Rule::Max(..)) {
            "`Int`, `BigInt`, or `Float`"
        } else if matches!(
            rule,
            Rule::NonEmpty(_) | Rule::MinLength(..) | Rule::MaxLength(..)
        ) {
            "`String` fields or arrays (`T[]`)"
        } else {
            "`String` (or a string-based type alias)"
        };
        diags.push(
            Diagnostic::error(
                path,
                "check.rule-base",
                format!(
                    "field `{field}` of model `{owner}` applies {}, which is only valid on {}",
                    rule_call_name(rule),
                    allowed,
                ),
            )
            .with_help("choose a rule valid for this field's type, or change the field's type")
            .with_span(span),
        );
    }
}

/// Validate every `.regex("...")` pattern in every model file.
fn check_regexes(files: &[(PathBuf, String)]) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for (path, src) in files {
        let Ok(file) = parse_axm_file(src) else {
            continue;
        };
        for model in &file.models {
            for field in &model.fields {
                for rule in &field.ty.rules {
                    let axiom_core::axm::ast::Rule::Regex(pattern, _) = rule else {
                        continue;
                    };
                    if let Err(err) = regex::Regex::new(pattern) {
                        diags.push(
                            Diagnostic::error(
                                path,
                                "check.regex-invalid",
                                format!(
                                    "regex `{pattern}` on `{}` does not compile: {err}",
                                    field.name
                                ),
                            )
                            .with_help("fix the regular expression so it compiles"),
                        );
                    }
                }
            }
        }
    }
    diags
}

/// Turn a linking [`AxiomError`] into a diagnostic attached to `fallback_path`.
fn link_error(fallback: Option<&Path>, err: AxiomError) -> Diagnostic {
    let path = match &err {
        AxiomError::ModelParseError { path, .. }
        | AxiomError::ModelResolutionError { path, .. } => path.clone(),
        _ => fallback
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("models")),
    };
    match err {
        AxiomError::ModelDuplicate {
            name,
            first,
            second,
        } => Diagnostic::error(
            &path,
            "check.duplicate-model",
            format!("duplicate model `{name}` in `{first}` and `{second}`"),
        )
        .with_help("rename one of the models; generated code shares one namespace"),
        AxiomError::ModelImportCycle { chain } => Diagnostic::error(
            &path,
            "check.import-cycle",
            format!("import cycle detected: {chain}"),
        )
        .with_help("remove one import in the cycle to break it"),
        AxiomError::ModelResolutionError { message, .. } => {
            Diagnostic::error(&path, "check.model-resolution", message)
                .with_help("make sure imports resolve and every referenced model is in scope")
        }
        AxiomError::ModelParseError { message, .. } => {
            Diagnostic::error(&path, "check.axm-parse", message)
        }
        other => Diagnostic::error(&path, "check.model", other.to_string()),
    }
}

/// A deterministic aggregate digest over every `(path, content)` pair.
pub fn aggregate_hash(files: &[(PathBuf, String)]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for (path, src) in files {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(src.as_bytes());
    }
    hasher.finalize().into()
}

/// The set of model names referenced by any legitimate declaration site across
/// the workspace: an import, a type-alias base, a model field type, or a query
/// parameter/return type. Used by the linter's `dead-model` rule.
pub fn collect_referenced_models(
    model_files: &[(PathBuf, String)],
) -> std::collections::BTreeSet<String> {
    collect_referenced_names(model_files)
}

fn collect_type_names(ty: &TypeRef, out: &mut std::collections::BTreeSet<String>) {
    match ty {
        TypeRef::Named(name) => {
            out.insert(name.clone());
        }
        TypeRef::Array(inner) => collect_type_names(inner, out),
        TypeRef::Nullable(inner) => collect_type_names(inner, out),
        _ => {}
    }
}

/// The set of type names referenced anywhere across the workspace: imports,
/// type-alias bases, model fields, and query parameters and return types. Used
/// by the linter's `unused-type-alias` rule.
pub fn collect_referenced_types(
    model_files: &[(PathBuf, String)],
) -> std::collections::BTreeSet<String> {
    collect_referenced_names(model_files)
}

/// Every written name referenced by a `.axm` file, from every declaration site
/// that can name another declaration: imports (original and aliased names),
/// type-alias bases, model field types, and query parameter and return types.
///
/// This is the single scan behind both [`collect_referenced_models`] (the
/// `dead-model` liveness set) and [`collect_referenced_types`] (the
/// `unused-type-alias` liveness set). It is a written-name scan, not a
/// resolution graph: a name counts as referenced whenever it appears at one of
/// these sites, even if the referring declaration is itself unreferenced.
fn collect_referenced_names(
    model_files: &[(PathBuf, String)],
) -> std::collections::BTreeSet<String> {
    let mut referenced = std::collections::BTreeSet::new();
    for (_, src) in model_files {
        let Ok(file) = parse_axm_file(src) else {
            continue;
        };
        for import in &file.imports {
            for name in &import.names {
                referenced.insert(name.name.clone());
                if let Some(alias) = &name.alias {
                    referenced.insert(alias.clone());
                }
            }
        }
        for ty in &file.types {
            collect_type_names(&ty.ty.base, &mut referenced);
        }
        for model in &file.models {
            for field in &model.fields {
                collect_type_names(&field.ty.base, &mut referenced);
            }
        }
        for query in &file.queries {
            for param in &query.params {
                collect_type_names(&param.ty, &mut referenced);
            }
            match &query.return_type {
                QueryReturn::Exec => {}
                QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
                    collect_type_names(ty, &mut referenced)
                }
            }
        }
    }
    referenced
}

fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
