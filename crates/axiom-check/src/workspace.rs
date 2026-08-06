//! Input resolution and the per-input check phases.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sqlparser::ast::{Expr, ObjectName, Visit, Visitor};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use axiom_core::axm::ast::TypeRef;
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::axm::resolver::{resolve_models, ModelRegistry};
use axiom_core::cache::{compute_content_hash, ToolCache};
use axiom_core::catalog::{parse_sql_catalog, TableCatalog};
use axiom_core::config::{resolve_glob_paths, AxiomConfig};
use axiom_core::errors::AxiomError;
use axiom_core::query::{parse_query_file, QueryCatalog, QueryReturnType};
use axiom_diagnostics::Diagnostic;

use crate::diagnostics::{line_of_offset, parse_error};

/// All resolved input sources, with their file contents.
pub struct Workspace {
    pub schema_files: Vec<(PathBuf, String)>,
    pub query_files: Vec<(PathBuf, String)>,
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
        schema_files: read(&config.inputs.schema)?,
        query_files: read(&config.inputs.queries)?,
        model_files: read(&config.inputs.models)?,
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

/// Parse every query file and verify each query against the schema catalog:
/// referenced tables must exist, column references must resolve, and the
/// declared return type must match a table or model.
///
/// Per-file results are cached in the [`ToolCache`] keyed by the file's content
/// hash plus the aggregate schema hash, so schema edits invalidate stale
/// results while untouched query files stay cached.
pub fn check_queries<'a>(
    mut cache: Option<&mut ToolCache>,
    schema_hash: &[u8; 32],
    catalog: &TableCatalog<'_>,
    declared_models: &BTreeSet<String>,
    files: &'a [(PathBuf, String)],
) -> (QueryCatalog<'a>, Vec<Diagnostic>) {
    let mut query_catalog = QueryCatalog::default();
    let mut diags = Vec::new();

    for (path, src) in files {
        let file_hash = compute_content_hash(src.as_bytes());
        let key = format!(
            "check:query:{}:{}",
            hex(schema_hash),
            hex(&file_hash)
        );

        let file_diags = if let Some(cache) = cache.as_deref()
            && let Some(payload) = cache.get(&key)
            && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
        {
            cached
        } else {
            let computed = check_query_file(path, src, catalog, declared_models);
            if let Some(cache) = cache.as_mut()
                && let Ok(payload) = serde_json::to_vec(&computed)
            {
                cache.insert(key, payload);
            }
            computed
        };
        diags.extend(file_diags);

        // The query catalog is always rebuilt: synchronization needs it.
        if let Ok(parsed) = parse_query_file(src) {
            query_catalog.queries.extend(parsed.queries);
        }
    }
    (query_catalog, diags)
}

fn check_query_file(
    path: &Path,
    src: &str,
    catalog: &TableCatalog<'_>,
    declared_models: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    let parsed = match parse_query_file(src) {
        Ok(parsed) => parsed,
        Err(AxiomError::QueryAnnotationError { message, span, .. }) => {
            diags.push(
                parse_error(path, "check.query-annotation", message)
                    .with_span(line_of_offset(src, span.offset())),
            );
            return diags;
        }
        Err(_) => return diags,
    };

    for query in &parsed.queries {
        diags.extend(check_query_body(path, query.sql.trim(), catalog));

        match &query.return_type {
            QueryReturnType::Single(name) | QueryReturnType::Many(name) => {
                let name_str = name.trim();
                let known_table = catalog.table_by_name(name_str).is_some();
                let known_model = declared_models.contains(name_str);
                if !known_table && !known_model {
                    diags.push(
                        Diagnostic::error(
                            path,
                            "check.query-return-type",
                            format!(
                                "query `{}` returns `{name_str}`, but no such table or model exists",
                                query.name
                            ),
                        )
                        .with_help("declare the table in a schema file or the model in a `.axm` file"),
                    );
                }
            }
            QueryReturnType::Exec => {}
        }
    }
    diags
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
                    format!("query references table `{relation}`, which is not defined in the schema"),
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

    (registry, diags)
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
                for rule in &field.validations {
                    let axiom_core::axm::ast::Rule::Regex(pattern) = rule else {
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
        AxiomError::ModelDuplicate { name, first, second } => Diagnostic::error(
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
        AxiomError::ModelResolutionError { message, .. } => Diagnostic::error(
            &path,
            "check.model-resolution",
            message,
        )
        .with_help("make sure imports resolve and every referenced model is in scope"),
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
            referenced.extend(import.names.clone());
        }
        for model in &file.models {
            for field in &model.fields {
                collect_type_names(&field.ty, &mut referenced);
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
        _ => {}
    }
}

/// The set of model names declared anywhere in the workspace.
pub fn collect_declared_models(
    model_files: &[(PathBuf, String)],
) -> std::collections::BTreeSet<String> {
    let mut declared = std::collections::BTreeSet::new();
    for (_, src) in model_files {
        if let Ok(file) = parse_axm_file(src) {
            for model in &file.models {
                declared.insert(model.name.clone());
            }
        }
    }
    declared
}

fn hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
