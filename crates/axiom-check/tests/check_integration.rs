//! Tests for the check crate's phases.

use std::path::PathBuf;

use axiom_core::cache::ToolCache;

use axiom_check::workspace::aggregate_hash;
use axiom_check::{
    check_models, check_queries, check_schemas, collect_declared_models,
    collect_referenced_models, Workspace,
};

fn file(path: &str, src: &str) -> (PathBuf, String) {
    (PathBuf::from(path), src.to_string())
}

fn codes(diags: &[axiom_diagnostics::Diagnostic]) -> Vec<&str> {
    diags.iter().map(|d| d.code.as_str()).collect()
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
    let query = vec![file(
        "queries/q.sql",
        "-- @fn get_order() : Orders[]\nSELECT * FROM orders WHERE id = $1",
    )];
    let (catalog, _) = check_schemas(&schema);
    let declared = collect_declared_models(&[]);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &declared, &query);
    assert!(
        codes(&diags).contains(&"check.missing-table"),
        "{diags:?}"
    );
}

#[test]
fn query_return_type_must_exist() {
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let query = vec![file(
        "queries/q.sql",
        "-- @fn list_users() : Users[]\nSELECT id FROM users",
    )];
    let (catalog, _) = check_schemas(&schema);
    let declared = collect_declared_models(&[]);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &declared, &query);
    assert!(
        codes(&diags).contains(&"check.query-return-type"),
        "{diags:?}"
    );
}

#[test]
fn model_duplicates_are_reported() {
    let files = vec![
        file("models/a.axm", "export model User { a: string }"),
        file("models/b.axm", "export model User { b: string }"),
    ];
    let (_, diags) = check_models(None, &files);
    assert!(codes(&diags).contains(&"check.duplicate-model"), "{diags:?}");
}

#[test]
fn broken_import_is_reported() {
    let files = vec![file(
        "models/user.axm",
        "import { Missing } from \"nowhere\"\nexport model User { x: Missing }",
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
        "export model User { slug: string .regex(\"[unclosed\") }",
    )];
    let (_, diags) = check_models(None, &files);
    assert!(codes(&diags).contains(&"check.regex-invalid"), "{diags:?}");
}

#[test]
fn query_results_are_cached_by_content() {
    let mut cache = ToolCache::default();
    let schema = vec![file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")];
    let query = file(
        "queries/q.sql",
        "-- @fn list_users() : Users[]\nSELECT id FROM users",
    );
    let (catalog, _) = check_schemas(&schema);
    let declared = collect_declared_models(&[]);
    let schema_hash =
        aggregate_hash(&[file("schema.sql", "CREATE TABLE users (id INT PRIMARY KEY);")]);

    let (_, first) = check_queries(
        Some(&mut cache),
        &schema_hash,
        &catalog,
        &declared,
        std::slice::from_ref(&query),
    );
    assert!(!first.is_empty(), "first run computes diagnostics");

    // Second run with identical content hits the cache and skips re-analysis.
    let (_, second) = check_queries(Some(&mut cache), &schema_hash, &catalog, &declared, &[query]);
    assert_eq!(second, first, "cached run yields identical diagnostics");
}

#[test]
fn referenced_models_include_field_types_and_imports() {
    let files = vec![file(
        "models/user.axm",
        "import { Address } from \"address\"\nexport model User {\n  billing: Address\n}",
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
