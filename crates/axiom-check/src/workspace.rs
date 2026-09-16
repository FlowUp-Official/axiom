//! Input resolution and the per-input check phases.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqlparser::ast::{Expr, ObjectName, Select, SelectItem, SetExpr, Statement, Visit, Visitor};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use axiom_core::axm::ast::{AnnotatedType, QueryDecl, QueryReturn, Rule, Transform, TypeRef};
use axiom_core::axm::codegen::resolve_relation;
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::axm::resolver::{ModelRegistry, resolve_models};
use axiom_core::cache::{ToolCache, compute_content_hash};
use axiom_core::catalog::{TableCatalog, parse_sql_catalog};
use axiom_core::config::{AxiomConfig, resolve_glob_paths};
use axiom_core::errors::AxiomError;
use axiom_core::query::{Placeholder, QueryCatalog, scan_dotted_placeholders, scan_placeholders};
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

/// Compile every `query` declaration in the linked registry into the shared
/// query catalog and verify each one against the schema catalog: referenced
/// tables must exist, column references must resolve, the declared return
/// type must match a table, model, or type alias, and the SQL body must honor
/// the declared return contract (rows vs. execution).
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

    let mut per_file: BTreeMap<PathBuf, Vec<&QueryDecl>> = BTreeMap::new();
    for resolved in &registry.queries {
        per_file
            .entry(resolved.path.clone())
            .or_default()
            .push(&resolved.query);
    }

    for (path, queries) in &per_file {
        let src = src_by_path.get(path).map(String::as_str).unwrap_or("");
        let file_hash = compute_content_hash(src.as_bytes());
        let key = format!("check:query:{}:{}", hex(schema_hash), hex(&file_hash));

        let file_diags = if let Some(cache) = cache.as_deref()
            && let Some(payload) = cache.get(&key)
            && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
        {
            cached
        } else {
            let computed: Vec<Diagnostic> = queries
                .iter()
                .flat_map(|q| check_declared_query(path, src, q, catalog, registry))
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

/// Validate a single `query` declaration: placeholders must resolve to a
/// declared parameter, the return type must refer to a known table, model, or
/// type alias, and the SQL body must match the declared return contract.
fn check_declared_query(
    path: &Path,
    src: &str,
    query: &QueryDecl,
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    diags.extend(check_query_body(path, query.sql.trim(), catalog));

    let body_start = src.find(&query.sql).unwrap_or(0);
    for (start, _, kind) in scan_placeholders(&query.sql) {
        let span = line_of_offset(src, body_start + start);
        match kind {
            Placeholder::Positional(n) if n > query.params.len() => {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-placeholder",
                        format!(
                            "query `{}` uses placeholder `${n}`, but only {} parameter{} are declared",
                            query.name,
                            query.params.len(),
                            if query.params.len() == 1 { "is" } else { "s" },
                        ),
                    )
                    .with_span(span),
                );
            }
            Placeholder::Named(name) if query.params.iter().all(|p| p.name != name) => {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-placeholder",
                        format!(
                            "query `{}` uses placeholder `${name}`, which is not declared in the `query` signature",
                            query.name
                        ),
                    )
                    .with_help("add the parameter to the `query` declaration, or fix the placeholder")
                    .with_span(span),
                );
            }
            _ => {}
        }
    }

    for (start, _, dotted) in scan_dotted_placeholders(&query.sql) {
        let span = line_of_offset(src, body_start + start);
        let Some((base, field)) = dotted.split_once('.') else {
            continue;
        };
        let Some(param) = query.params.iter().find(|p| p.name == base) else {
            // An undeclared base parameter is already reported above.
            continue;
        };
        let TypeRef::Named(model_name) = &param.ty else {
            diags.push(
                Diagnostic::error(
                    path,
                    "check.query-placeholder",
                    format!(
                        "query `{}` uses placeholder `${dotted}` to address fields of `${base}`, but `${base}` is not a model parameter (its type is `{}`)",
                        query.name,
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
                        "query `{}` uses placeholder `${dotted}`, but model `{model_name}` has no field `{field}`",
                        query.name
                    ),
                )
                .with_help("add the field to the model or fix the placeholder")
                .with_span(span),
            );
        }
    }

    match &query.return_type {
        QueryReturn::Exec => {}
        QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
            let name = axiom_core::axm::type_ref_name(ty);
            if !known_return_type(catalog, registry, &name) {
                diags.push(
                    Diagnostic::error(
                        path,
                        "check.query-return-type",
                        format!(
                            "query `{}` returns `{name}`, but no such table, model, or type exists",
                            query.name
                        ),
                    )
                    .with_help(
                        "declare the table in a schema file or the model/type in a `.axm` file",
                    ),
                );
            }
        }
    }

    diags.extend(check_return_contract(path, query, catalog, registry));
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
    query: &QueryDecl,
    catalog: &TableCatalog<'_>,
    registry: &ModelRegistry,
) -> Vec<Diagnostic> {
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, &query.sql) else {
        return Vec::new(); // body parse errors are already reported
    };
    // For multi-statement query bodies the return contract describes the rows
    // a caller receives, which is the result of the LAST statement.
    let Some(statement) = statements.into_iter().next_back() else {
        return Vec::new();
    };

    let produces_rows = statement_produces_rows(&statement);
    let expects_rows = !matches!(query.return_type, QueryReturn::Exec);

    let prefix = format!("query `{}`", query.name);
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
        &query.return_type
    else {
        return Vec::new();
    };
    let row_name = axiom_core::axm::type_ref_name(ty);
    let Some(select) = single_select(&statement) else {
        return Vec::new();
    };
    let projected = projection_identifiers(select);
    if projected.is_empty() {
        return Vec::new();
    }

    if let Some(model) = registry.model_by_name(&row_name) {
        let field_names: Vec<String> = model.model.fields.iter().map(|f| f.name.clone()).collect();
        projection_vs_fields(path, query, &row_name, &projected, &field_names, true)
    } else if let Some(table) = catalog.table_by_name(&row_name) {
        let field_names: Vec<String> = table.columns.iter().map(|c| c.name.to_string()).collect();
        projection_vs_fields(path, query, &row_name, &projected, &field_names, false)
    } else {
        Vec::new() // unresolved row type already reported
    }
}

fn projection_vs_fields(
    path: &Path,
    query: &QueryDecl,
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
                        "query `{}` projects column `{ident}`, which is not a field of the declared `{row_name}` type",
                        query.name
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

/// The set of model names referenced (as field types or imports) across the
/// workspace. Used by the linter's `dead-model` rule.
pub fn collect_referenced_models(
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
        for model in &file.models {
            for field in &model.fields {
                collect_type_names(&field.ty.base, &mut referenced);
            }
        }
    }
    referenced
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

fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
