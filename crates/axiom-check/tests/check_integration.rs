//! Tests for the check crate's phases.

use std::path::PathBuf;

use axiom_core::cache::ToolCache;

use axiom_check::workspace::aggregate_hash;
use axiom_check::{
    check_models, check_queries, check_schemas, collect_referenced_models, Workspace,
};

fn file(path: &str, src: &str) -> (PathBuf, String) {
    (PathBuf::from(path), src.to_string())
}

fn codes(diags: &[axiom_diagnostics::Diagnostic]) -> Vec<&str> {
    diags.iter().map(|d| d.code.as_str()).collect()
}

/// Parse the given model files and hand the registry plus diagnostic output
/// back (panics when the files fail to resolve, since these fixtures are
/// expected to be valid).
fn registry_from(files: &[(PathBuf, String)]) -> axiom_core::axm::ModelRegistry {
    let (registry, diags) = check_models(None, files);
    match registry {
        Some(registry) => registry,
        None => panic!("fixture model files resolve cleanly: {diags:?}"),
    }
}

#[test]
fn schema_parse_error_is_reported() {
    let files = vec![file("schema.sql", "CREATE TABLE users (id INT")];
    let (_, diags) = check_schemas(&files);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, "check.sql-parse");
    assert!(matches!(diags[0].severity, axiom_diagnostics::Severity::Error));
}

#[test]
fn well_formed_schema_produces_catalog() {
    let files = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY, email TEXT NOT NULL);")];
    let (catalog, diags) = check_schemas(&files);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(catalog.tables.len(), 1);
    assert_eq!(catalog.tables[0].columns.len(), 2);
}

#[test]
fn query_referencing_missing_table_is_reported() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query get_order() -> Orders[] {\n  SELECT * FROM orders WHERE id = $1\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.missing-table"),
        "{diags:?}"
    );
}

#[test]
fn query_return_type_must_exist() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query list_users() -> Nope[] {\n  SELECT id FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-return-type"),
        "{diags:?}"
    );
}

#[test]
fn return_type_matches_table_case_insensitively() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);")];
    let models = vec![file(
        "models/queries.axm",
        "query list_users() -> Users[] {\n  SELECT id FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn named_placeholders_check_cleanly() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT NOT NULL);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "query get_user($email: String) -> users {\n  SELECT id, email FROM users WHERE email = $email\n}\n\nquery get_users($limit: Int, $email: String) -> users[] {\n  SELECT id, email FROM users\n  WHERE email = $email AND id < $limit\n  ORDER BY id\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn unknown_named_placeholder_is_reported() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query get_user($email: String) -> users {\n  SELECT id FROM users WHERE email = $nope\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-placeholder"),
        "{diags:?}"
    );
}

#[test]
fn positional_placeholder_beyond_declared_is_reported() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query get_user($email: String) -> users {\n  SELECT id FROM users WHERE id = $1 AND email = $2\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-placeholder"),
        "{diags:?}"
    );
}

#[test]
fn exec_contract_rejects_select() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query reset_users() {\n  SELECT id FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-contract"),
        "{diags:?}"
    );
}

#[test]
fn projected_column_must_be_a_return_field() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);")];
    let models = vec![file(
        "models/queries.axm",
        "model User { id: Int, email: String }\n\nquery list_users() -> User {\n  SELECT password FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-contract"),
        "{diags:?}"
    );
}

#[test]
fn model_duplicates_are_reported() {
    let files = vec![
        file("models/a.axm", "model User { a: String }"),
        file("models/b.axm", "model User { b: String }"),
    ];
    let (_, diags) = check_models(None, &files);
    assert!(codes(&diags).contains(&"check.duplicate-model"), "{diags:?}");
}

#[test]
fn broken_import_is_reported() {
    let files = vec![file(
        "models/user.axm",
        "import { Missing } from \"nowhere\"\nmodel User { x: Missing }",
    )];
    let (_, diags) = check_models(None, &files);
    assert!(
        codes(&diags).contains(&"check.model-resolution"),
        "{diags:?}"
    );
}

#[test]
fn invalid_regex_is_reported() {
    let files = vec![file(
        "models/user.axm",
        "model User { slug: String.regex(\"[unclosed\") }",
    )];
    let (_, diags) = check_models(None, &files);
    assert!(codes(&diags).contains(&"check.regex-invalid"), "{diags:?}");
}

#[test]
fn query_results_are_cached_by_content() {
    let mut cache = ToolCache::default();
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let models = vec![file(
        "models/queries.axm",
        "query list_users() -> Users[] {\n  SELECT id FROM missing_table\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let schema_hash =
        aggregate_hash(&[file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")]);

    let (_, first) = check_queries(
        Some(&mut cache),
        &schema_hash,
        &catalog,
        &registry,
        &models,
    );
    assert!(!first.is_empty(), "first run computes diagnostics");

    // Second run with identical content hits the cache and skips re-analysis.
    let (_, second) = check_queries(Some(&mut cache), &schema_hash, &catalog, &registry, &models);
    assert_eq!(second, first, "cached run yields identical diagnostics");
}

#[test]
fn referenced_models_include_field_types_and_imports() {
    let files = vec![file(
        "models/user.axm",
        "import { Address } from \"./address.axm\"\nmodel User {\n  billing: Address\n}",
    )];
    let referenced = collect_referenced_models(&files);
    assert!(referenced.contains("Address"));
}

#[test]
fn workspace_resolution_requires_existing_files() {
    let files = vec![file("models/a.axm", "export model A { x: string }")];
    let workspace = Workspace {
        schema_files: vec![],
        query_files: vec![],
        model_files: files,
    };
    assert_eq!(workspace.model_files.len(), 1);
}
