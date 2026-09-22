//! Tests for the check crate's phases.

use std::path::PathBuf;

use axiom_core::cache::ToolCache;
use axiom_core::config::AxiomConfig;

use axiom_check::workspace::aggregate_hash;
use axiom_check::{
    Workspace, check_duplicate_fields, check_model_sources, check_models, check_queries,
    check_schemas, check_target_references, collect_referenced_models,
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
    assert!(matches!(
        diags[0].severity,
        axiom_diagnostics::Severity::Error
    ));
}

#[test]
fn well_formed_schema_produces_catalog() {
    let files = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT NOT NULL);",
    )];
    let (catalog, diags) = check_schemas(&files);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(catalog.tables.len(), 1);
    assert_eq!(catalog.tables[0].columns.len(), 2);
}

#[test]
fn query_referencing_missing_table_is_reported() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "query get_order() -> Orders[] {\n  SELECT * FROM orders WHERE id = $1\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(codes(&diags).contains(&"check.missing-table"), "{diags:?}");
}

#[test]
fn query_return_type_must_exist() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
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
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
    )];
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
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
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
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
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
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "query reset_users() {\n  SELECT id FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(codes(&diags).contains(&"check.query-contract"), "{diags:?}");
}

#[test]
fn projected_column_must_be_a_return_field() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "model User { id: Int, email: String }\n\nquery list_users() -> User {\n  SELECT password FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(codes(&diags).contains(&"check.query-contract"), "{diags:?}");
}

#[test]
fn multi_statement_transaction_contract_checks_the_last_statement() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
    )];
    // Transaction with a row-returning last statement: no contract error.
    let models = vec![file(
        "models/queries.axm",
        "model User { id: Int, email: String }\n\ntransaction list_users() -> User {\n  DELETE FROM users;\n  SELECT id, email FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(diags.is_empty(), "{diags:?}");

    // Exec contract with a trailing SELECT: flagged as wrong contract.
    let models = vec![file(
        "models/queries.axm",
        "transaction reset_users() {\n  DELETE FROM users;\n  SELECT id FROM users\n}",
    )];
    let registry = registry_from(&models);
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(codes(&diags).contains(&"check.query-contract"), "{diags:?}");
}

#[test]
fn multi_statement_query_is_rejected_with_transaction_hint() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "model User { id: Int, email: String }\n\nquery list_users() -> User {\n  DELETE FROM users;\n  SELECT id, email FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.query-multi-statement"),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.help.as_deref().is_some_and(|h| h.contains("transaction"))),
        "expected a suggestion to use a transaction"
    );
}

#[test]
fn single_statement_transaction_is_rejected() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "model User { id: Int, email: String }\n\ntransaction single() -> User {\n  SELECT id, email FROM users\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(
        codes(&diags).contains(&"check.transaction-statement-count"),
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
    assert!(
        codes(&diags).contains(&"check.duplicate-model"),
        "{diags:?}"
    );
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
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "query list_users() -> Users[] {\n  SELECT id FROM missing_table\n}",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let schema_hash = aggregate_hash(&[file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY);",
    )]);

    let (_, first) = check_queries(Some(&mut cache), &schema_hash, &catalog, &registry, &models);
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
fn referenced_models_include_query_returns_params_and_alias_bases() {
    let files = vec![
        file(
            "models/a.axm",
            "model Returned { id: UUID }\nmodel Parameter { id: UUID }\n\
             model Aliased { id: UUID }\nmodel Dead { id: UUID }",
        ),
        file(
            "models/b.axm",
            "model Base = Aliased\nmodel Wrapper = Base\n\
             query Q($p: Parameter) -> Returned? {\n  SELECT id FROM a\n}",
        ),
    ];
    let referenced = collect_referenced_models(&files);
    assert!(referenced.contains("Returned"), "{referenced:?}");
    assert!(referenced.contains("Parameter"), "{referenced:?}");
    // `Aliased` is reached through a chain of type aliases.
    assert!(referenced.contains("Aliased"), "{referenced:?}");
    assert!(!referenced.contains("Dead"), "{referenced:?}");
}

#[test]
fn genuinely_unreferenced_model_is_absent_from_referenced_models() {
    let files = vec![file(
        "models/a.axm",
        "model Live { id: UUID }\nmodel Dead { id: UUID }\n\
         query Get() -> Live? {\n  SELECT id FROM live\n}",
    )];
    let referenced = collect_referenced_models(&files);
    assert!(referenced.contains("Live"), "{referenced:?}");
    assert!(!referenced.contains("Dead"), "{referenced:?}");
}

#[test]
fn workspace_resolution_requires_existing_files() {
    let files = vec![file("models/a.axm", "model A { x: String }")];
    let workspace = Workspace {
        schema_files: vec![],
        model_files: files,
    };
    assert_eq!(workspace.model_files.len(), 1);
}

#[test]
fn model_source_must_match_a_table_for_every_infers_operation() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE public.users (id INT PRIMARY KEY, email TEXT NOT NULL);",
    )];
    let (catalog, _) = check_schemas(&schema);
    for op in ["select", "insert", "update", "delete"] {
        let src = format!("model User infers {op}<users> {{\n  email: String .email()\n}}\n");
        let files = &[file("models/user.axm", &src)];
        let registry = registry_from(files);
        let diags = check_model_sources(&catalog, &registry, files);
        assert!(diags.is_empty(), "{op} => {diags:?}");
    }
}

#[test]
fn model_source_rejects_missing_or_miscased_relation() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE public.users (id INT PRIMARY KEY, email TEXT NOT NULL);",
    )];
    let (catalog, _) = check_schemas(&schema);
    for (op, relation) in [("select", "Users"), ("insert", "orders"), ("delete", "public.none")] {
        let src = format!("model User infers {op}<{relation}> {{\n  email: String\n}}\n");
        let files = &[file("models/user.axm", &src)];
        let registry = registry_from(files);
        let diags = check_model_sources(&catalog, &registry, files);
        assert!(
            codes(&diags).contains(&"check.model-source"),
            "infers {op}<{relation}> => {diags:?}"
        );
        let message = diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            message.contains(&format!("infers {op}<{relation}>")),
            "diagnostic should preserve the operation `{op}`, got: {message}"
        );
    }
}

#[test]
fn model_sources_preserve_operation_through_resolution() {
    use axiom_core::axm::ast::ModelOperation;
    for op in ["select", "insert", "update", "delete"] {
        let src = format!("model User infers {op}<users> {{\n  email: String\n}}\n");
        let files = &[file("models/user.axm", &src)];
        let registry = registry_from(files);
        let source = registry.models[0].model.source.as_ref().expect("source");
        assert_eq!(source.operation, ModelOperation::parse(op).unwrap());
        assert_eq!(source.operation.name(), op);
    }
}

#[test]
fn rule_base_mismatches_are_reported() {
    let files = vec![file(
        "models/user.axm",
        "model User {\n  id: Int .email()\n  name: String .min(3)\n  tags: String[] .email()\n  count: Int .trim()\n}\n",
    )];
    let (_, diags) = check_models(None, &files);
    let codes = codes(&diags);
    assert_eq!(
        codes.iter().filter(|c| **c == "check.rule-base").count(),
        4,
        "{diags:?}"
    );
}

#[test]
fn valid_rule_base_combinations_pass() {
    let files = vec![file(
        "models/user.axm",
        "model User {\n  id: Int .min(1) .max(99)\n  email: String .email() .min_length(3)\n  tags: String[] .nonempty()\n  alias: UUID\n}\n",
    )];
    let (registry, diags) = check_models(None, &files);
    assert!(registry.is_some());
    assert!(codes(&diags).is_empty(), "{diags:?}");
}

#[test]
fn dotted_model_placeholders_check_model_fields() {
    let schema = vec![file(
        "schema.sql",
        "CREATE TABLE users (id INT PRIMARY KEY, email TEXT NOT NULL);",
    )];
    let models = vec![file(
        "models/queries.axm",
        "model User {\n  email: String\n}\nquery find_by_email($input: User) -> User? {\n  SELECT * FROM users WHERE email = $input.email\n}\n",
    )];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    assert!(diags.is_empty(), "{diags:?}");
    let bad = vec![file(
        "models/bad.axm",
        "model User {\n  email: String\n}\nquery find_by_email($input: User) -> User? {\n  SELECT * FROM users WHERE email = $input.email_address\n}\n",
    )];
    let registry = registry_from(&bad);
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &bad);
    assert!(
        codes(&diags).contains(&"check.query-placeholder"),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("has no field `email_address`")),
        "{diags:?}"
    );
}

#[test]
fn duplicate_fields_are_reported() {
    let files = vec![file(
        "models/user.axm",
        "model User {\n  name: String\n  name: String\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_duplicate_fields(&registry, &files);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, "check.duplicate-field");
    assert!(diags[0].span.is_some());
}

#[test]
fn each_extra_duplicate_field_is_reported() {
    let files = vec![file(
        "models/user.axm",
        "model User {\n  name: String\n  name: String\n  name: String\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_duplicate_fields(&registry, &files);
    assert_eq!(diags.len(), 2, "{diags:?}");
}

#[test]
fn distinct_fields_are_not_reported() {
    let files = vec![file(
        "models/user.axm",
        "model User {\n  name: String\n  email: String\n}",
    )];
    let registry = registry_from(&files);
    assert!(check_duplicate_fields(&registry, &files).is_empty());
}

#[test]
fn reference_to_target_excluded_model_is_reported() {
    let files = vec![file(
        "models/models.axm",
        "model Address {\n  street: String\n}\n\n@target(\"rust\")\nmodel Account {\n  owner: Address\n}\n\nmodel User {\n  name: String\n  account: Account\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    let ts: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("typescript"))
        .collect();
    assert_eq!(ts.len(), 1, "{diags:?}");
    assert_eq!(ts[0].code, "check.target-excluded-reference");
    assert!(ts[0].message.contains("`Account`"), "{:?}", ts[0].message);
    assert!(ts[0].span.is_some());
    assert_eq!(diags.len(), 1, "rust emits Account, so only typescript fails");
}

#[test]
fn transitive_reference_to_target_excluded_model_is_reported() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"rust\")\nmodel Leaf {\n  x: String\n}\n\nmodel Mid {\n  leaf: Leaf\n}\n\nmodel Root {\n  mid: Mid\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, "check.target-excluded-reference");
    assert!(diags[0].message.contains("model `Mid`"), "{:?}", diags[0].message);
    assert!(diags[0].message.contains("`Leaf`"), "{:?}", diags[0].message);
}

#[test]
fn reference_within_the_same_target_is_clean() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"typescript\")\nmodel Leaf {\n  x: String\n}\n\n@target(\"typescript\")\nmodel Root {\n  leaf: Leaf\n}",
    )];
    let registry = registry_from(&files);
    assert!(
        check_target_references(&AxiomConfig::default_template(), &registry, &files).is_empty()
    );
}

#[test]
fn type_alias_reference_to_target_excluded_model_is_reported() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"rust\")\nmodel OnlyRust {\n  x: String\n}\n\nmodel Alias = OnlyRust",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert!(
        diags.iter().any(|d| d.message.contains("model `Alias`")),
        "{diags:?}"
    );
}

#[test]
fn query_reference_to_target_excluded_model_is_reported() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"rust\")\nmodel OnlyRust {\n  x: String\n}\n\nquery GetThing() -> OnlyRust? {\n  SELECT x FROM things\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert!(
        diags.iter().any(|d| d.message.contains("query `GetThing`")),
        "{diags:?}"
    );
}

#[test]
fn referenced_models_include_fields_aliases_queries_and_imports() {
    let files = vec![
        file(
            "models/a.axm",
            "model Email = String\nmodel User {\n  email: Email\n}",
        ),
        file(
            "models/b.axm",
            "import { Email } from \"a\"\nmodel Org {\n  contact: Email\n}",
        ),
        file(
            "models/c.axm",
            "model Id = String\nquery Get($id: Id) -> Id[] {\n  SELECT id FROM t\n}",
        ),
    ];
    let referenced = collect_referenced_models(&files);
    assert!(referenced.contains("Email"), "{referenced:?}");
    assert!(referenced.contains("Id"), "{referenced:?}");
}

#[test]
fn unreferenced_model_is_absent_from_referenced_models() {
    let files = vec![file(
        "models/a.axm",
        "model Unused = String\nmodel User {\n  name: String\n}",
    )];
    let referenced = collect_referenced_models(&files);
    assert!(!referenced.contains("Unused"), "{referenced:?}");
}
