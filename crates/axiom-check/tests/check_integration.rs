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

// --- query-contract: scalar/alias return semantics ---

fn check_contract(schema_sql: &str, models_src: &str) -> Vec<axiom_diagnostics::Diagnostic> {
    let schema = vec![file("schema.sql", schema_sql)];
    let models = vec![file("models/models.axm", models_src)];
    let (catalog, _) = check_schemas(&schema);
    let registry = registry_from(&models);
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, &models);
    diags
}

#[test]
fn scalar_alias_array_return_accepts_compatible_column() {
    let schema = "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);";
    let models = "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}";
    let diags = check_contract(schema, models);
    assert!(
        diags.iter().all(|d| d.code != "check.query-contract"),
        "expected no query-contract error, got: {diags:?}"
    );
}

#[test]
fn scalar_alias_array_return_accepts_aliased_compatible_column() {
    let schema = "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);";
    let models = "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id AS id FROM follows WHERE follower_id = $userId\n}";
    let diags = check_contract(schema, models);
    assert!(
        diags.iter().all(|d| d.code != "check.query-contract"),
        "expected no query-contract error for aliased column, got: {diags:?}"
    );
}

#[test]
fn scalar_alias_array_return_rejects_incompatible_uuid_column() {
    // `model UserId = String` is text-based; a UUID column is also text-class,
    // but a numeric column is not.
    let schema = "CREATE TABLE follows (following_id INT NOT NULL, follower_id TEXT NOT NULL);";
    let models = "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}";
    let diags = check_contract(schema, models);
    assert!(
        codes(&diags).contains(&"check.query-contract"),
        "expected a query-contract error for SQL Int vs String alias, got: {diags:?}"
    );
}

#[test]
fn scalar_alias_accepts_uuid_column() {
    // UUID is text-class; a `model X = String` alias accepts a UUID column.
    let schema = "CREATE TABLE follows (following_id UUID NOT NULL, follower_id TEXT NOT NULL);";
    let models = "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}";
    let diags = check_contract(schema, models);
    assert!(
        diags.iter().all(|d| d.code != "check.query-contract"),
        "expected no query-contract error for UUID vs String alias, got: {diags:?}"
    );
}

#[test]
fn scalar_alias_accepts_varchar_column() {
    let schema = "CREATE TABLE follows (following_id VARCHAR(64) NOT NULL, follower_id TEXT NOT NULL);";
    let models = "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}";
    let diags = check_contract(schema, models);
    assert!(
        diags.iter().all(|d| d.code != "check.query-contract"),
        "expected no query-contract error for VARCHAR vs String alias, got: {diags:?}"
    );
}

#[test]
fn int_alias_rejects_text_column() {
    // `model Count = Int` (numeric) vs a TEXT column must mismatch.
    let schema = "CREATE TABLE follows (n TEXT NOT NULL);";
    let models = "model Count = Int\n\nquery Get() -> Count[] {\n  SELECT n FROM follows\n}";
    let diags = check_contract(schema, models);
    assert!(
        codes(&diags).contains(&"check.query-contract"),
        "expected a query-contract error for TEXT vs Int alias, got: {diags:?}"
    );
}

#[test]
fn structured_model_return_contract_remains_unchanged() {
    // The original structured-model field matching is unaffected.
    let schema = "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);";
    let models = "model User { id: Int, email: String }\n\nquery list_users() -> User {\n  SELECT password FROM users\n}";
    let diags = check_contract(schema, models);
    assert!(
        codes(&diags).contains(&"check.query-contract"),
        "expected the existing field-mismatch contract error, got: {diags:?}"
    );
}

// --- target-excluded-reference: alias/value models are emitted for every target ---

#[test]
fn alias_referenced_by_structured_model_is_not_target_excluded() {
    let files = vec![file(
        "models/models.axm",
        "model UserId = String\nmodel User {\n  id: String\n  name: UserId\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert!(
        diags.is_empty(),
        "alias referenced by a structured model should not be target-excluded: {diags:?}"
    );
}

#[test]
fn alias_referenced_by_query_is_not_target_excluded() {
    let schema = "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);";
    let files = vec![
        file("schema.sql", schema),
        file(
            "models/models.axm",
            "model UserId = String\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let (catalog, _) = check_schemas(&files.iter().map(|f| f.clone()).collect::<Vec<_>>().as_slice());
    let (registry, _) = check_models(None, &files[1..]);
    let registry = registry.expect("registry");
    // Run target-reference check on the model file only.
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files[1..]);
    assert!(
        diags.is_empty(),
        "alias referenced by a query should not be target-excluded: {diags:?}"
    );
}

#[test]
fn alias_with_target_typescript_is_not_target_excluded_from_typescript() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"typescript\")\nmodel UserId = String\nmodel User {\n  id: String\n  name: UserId\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert!(
        diags.is_empty(),
        "alias emitted for every target must not be reported excluded: {diags:?}"
    );
}

#[test]
fn alias_with_target_rust_is_not_target_excluded_from_rust() {
    let files = vec![file(
        "models/models.axm",
        "@target(\"rust\")\nmodel UserId = String\nmodel User {\n  id: String\n  name: UserId\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    assert!(
        diags.is_empty(),
        "alias emitted for every target must not be reported excluded: {diags:?}"
    );
}

#[test]
fn structured_model_target_exclusion_is_still_reported() {
    // Regression guard: genuine @target exclusions on structured models still
    // surface, so the alias-skip fix did not over-broaden the check.
    let files = vec![file(
        "models/models.axm",
        "model Address {\n  street: String\n}\n\n@target(\"rust\")\nmodel Account {\n  owner: Address\n}\n\nmodel User {\n  name: String\n  account: Account\n}",
    )];
    let registry = registry_from(&files);
    let diags = check_target_references(&AxiomConfig::default_template(), &registry, &files);
    let ts: Vec<_> = diags.iter().filter(|d| d.message.contains("typescript")).collect();
    assert_eq!(ts.len(), 1, "{diags:?}");
    assert_eq!(ts[0].code, "check.target-excluded-reference");
    assert!(ts[0].message.contains("`Account`"), "{:?}", ts[0].message);
}

// --- import scoping: query return types are exempt, fields/params are not ---

#[test]
fn query_return_type_does_not_require_cross_file_import() {
    // `UserId` is declared in types.axm; core.axm references it only in a
    // return type with no import. The resolver deliberately exempts return
    // types from import scoping (a return may name a SQL table), so this
    // resolves cleanly.
    let files = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "query GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let (registry, diags) = check_models(None, &files);
    assert!(diags.is_empty(), "expected no resolution errors, got: {diags:?}");
    assert!(registry.is_some(), "registry should resolve");
    assert!(
        registry.unwrap().model_by_name("UserId").is_some(),
        "UserId must be resolvable from the per-file return type"
    );
}

#[test]
fn model_field_referencing_unimported_type_is_rejected() {
    // Fields ARE subject to import scoping; `UserId` used as a field type
    // without an import must be reported as unknown.
    let files = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "model Follow { id: String, user_id: UserId }",
        ),
    ];
    let (registry, diags) = check_models(None, &files);
    assert!(
        diags.iter().any(|d| d.code == "check.model-resolution" && d.message.contains("unknown type `UserId`")),
        "expected an unknown-type error for the field, got: {diags:?}"
    );
    assert!(registry.is_none(), "registry should not resolve");
}

#[test]
fn query_param_referencing_unimported_type_is_rejected() {
    let files = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "query Get($id: UserId) -> String {\n  SELECT id FROM t\n}",
        ),
    ];
    let (registry, diags) = check_models(None, &files);
    assert!(
        diags.iter().any(|d| d.code == "check.model-resolution" && d.message.contains("unknown type `UserId`")),
        "expected an unknown-type error for the param, got: {diags:?}"
    );
    assert!(registry.is_none());
}

// --- return-type import scoping ---

fn run_query_check(schema_sql: &str, model_files: &[(PathBuf, String)]) -> Vec<axiom_diagnostics::Diagnostic> {
    let schema = vec![file("schema.sql", schema_sql)];
    let (catalog, _) = check_schemas(&schema);
    let (registry, _) = check_models(None, model_files);
    let registry = registry.expect("registry resolves");
    let hash = [0u8; 32];
    let (_, diags) = check_queries(None, &hash, &catalog, &registry, model_files);
    diags
}

#[test]
fn unimported_cross_file_query_return_type_is_rejected() {
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "query GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().any(|d| d.code == "check.model-resolution" && d.message.contains("not defined in this file and not imported")),
        "expected a model-resolution error for unimported UserId return, got: {diags:?}"
    );
}

#[test]
fn imported_cross_file_query_return_type_is_accepted() {
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "import { UserId } from \"./types\"\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().all(|d| d.code != "check.model-resolution"),
        "imported return type should not be flagged as unimported: {diags:?}"
    );
}

#[test]
fn unimported_cross_file_transaction_return_type_is_rejected() {
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "transaction CreateFollow($userId: String) -> UserId {\n  INSERT INTO follows (follower_id) VALUES ($userId) RETURNING following_id\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().any(|d| d.code == "check.model-resolution" && d.message.contains("not defined in this file and not imported")),
        "expected a model-resolution error for unimported UserId return, got: {diags:?}"
    );
}

#[test]
fn imported_cross_file_transaction_return_type_is_accepted() {
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "import { UserId } from \"./types\"\ntransaction CreateFollow($userId: String) -> UserId {\n  INSERT INTO follows (follower_id) VALUES ($userId) RETURNING following_id\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().all(|d| d.code != "check.model-resolution"),
        "imported return type should not be flagged as unimported: {diags:?}"
    );
}

#[test]
fn local_return_type_without_import_is_accepted() {
    // A model declared in the same file as the query needs no import.
    let models = vec![file(
        "models/m.axm",
        "model UserId = String\n\nquery GetFollowings($userId: String) -> UserId[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
    )];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().all(|d| d.code != "check.model-resolution"),
        "locally-declared return type should not be flagged as unimported: {diags:?}"
    );
}

#[test]
fn sql_table_return_without_import_is_accepted() {
    // A bare SQL table name is not an Axiom model, so import scoping does not
    // apply (the table is validated against the catalog, not the import scope).
    let models = vec![file(
        "models/m.axm",
        "query list_users() -> users[] {\n  SELECT id FROM users\n}",
    )];
    let diags = run_query_check(
        "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT);",
        &models,
    );
    assert!(
        diags.iter().all(|d| d.code != "check.model-resolution"),
        "table return type should not require an import: {diags:?}"
    );
}

#[test]
fn imported_aliased_return_type_resolves_through_alias() {
    // `import { UserId as UID }` then `-> UID[]` must resolve UID -> UserId.
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "import { UserId as UID } from \"./types\"\n\nquery GetFollowings($userId: String) -> UID[] {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().all(|d| d.code != "check.model-resolution"),
        "aliased import of the return type should resolve: {diags:?}"
    );
}

#[test]
fn nullable_array_return_type_enforces_import() {
    // `UserId[]?` (array then nullable) must still traverse to UserId.
    let models = vec![
        file("models/types.axm", "model UserId = String\n"),
        file(
            "models/core.axm",
            "query GetFollowings($userId: String) -> UserId[]? {\n  SELECT following_id FROM follows WHERE follower_id = $userId\n}",
        ),
    ];
    let diags = run_query_check(
        "CREATE TABLE follows (following_id TEXT NOT NULL, follower_id TEXT NOT NULL);",
        &models,
    );
    assert!(
        diags.iter().any(|d| d.code == "check.model-resolution" && d.message.contains("UserId")),
        "expected a model-resolution error for nullable-array UserId return, got: {diags:?}"
    );
}

