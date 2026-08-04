//! End-to-end CLI integration tests.
//!
//! Each test runs the compiled `axiom` binary against a self-contained fixture
//! directory under `target/test_fixtures/` (never system `/tmp`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCHEMA_SQL: &str = r#"
CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    -- @validate email[msg="Bad Email"], min_len=3[msg="Too short"], trim, lower
    email VARCHAR(255) NOT NULL,
    -- @validate uuid
    external_id UUID,
    -- @validate trim, lower, alphanumeric
    username VARCHAR(32)
);
"#;

const QUERIES_SQL: &str = r#"
-- @fn get_user(email: String) : Users
SELECT id, email FROM users WHERE email = $1

-- @validate email(email, trim, lower)
-- @fn get_users(limit: Int) : Users[]
SELECT id, email FROM users ORDER BY id LIMIT $1

-- @fn delete_user(id: BigInt) : Exec
DELETE FROM users WHERE id = $1
"#;

fn fixture_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test_fixtures")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_fixture(dir: &Path, schema: &str) {
    std::fs::create_dir_all(dir.join("queries")).unwrap();
    std::fs::write(
        dir.join("axiom.json"),
        r#"{
  "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.5.0/schemas/axiom.schema.json",
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "inputs": { "schema": ["schema.sql"], "queries": ["queries/accounts.sql"] },
  "validation": { "on_error": "fail" },
  "outputs": {
    "api": { "type": "typescript", "path": "gen/api.ts" },
    "core": { "type": "rust", "path": "gen/core.rs" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("schema.sql"), schema).unwrap();
    std::fs::write(dir.join("queries/accounts.sql"), QUERIES_SQL).unwrap();
}

fn run_generate(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir)
        .arg("--config")
        .arg("axiom.json")
        .arg("generate")
        .output()
        .expect("failed to run axiom binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn generates_typescript_and_rust_outputs() {
    let dir = fixture_dir("run_generate_outputs");
    write_fixture(&dir, SCHEMA_SQL);

    let output = run_generate(&dir);
    assert!(
        output.status.success(),
        "generate failed: {}",
        stdout(&output)
    );
    assert!(stdout(&output).contains("generated 2 target(s)"));

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).expect("api.ts should exist");
    assert!(ts.contains("export interface Users {"));
    assert!(ts.contains("EMAIL_RE.test(email)"));
    assert!(ts.contains("errors.push({ path: \"email\", message: \"Bad Email\" });"));
    assert!(ts.contains("const username = input.username.trim().toLowerCase();"));
    assert!(ts.contains("UUID_RE.test(input.externalId)"));
    assert!(!ts.contains("const IPV6_RE ="), "unused preset should not be emitted");

    assert!(ts.contains("import type { Sql } from 'postgres';"));
    assert!(ts.contains("export interface GetUserParams {"));
    assert!(ts.contains("  email: string;"));
    assert!(ts.contains("  limit: number;"));
    assert!(ts.contains("export async function getUser("));
    assert!(ts.contains("  sql: Sql,"));
    assert!(ts.contains("  params: GetUserParams"));
    assert!(ts.contains("): Promise<Users | null> {"));
    assert!(ts.contains("SELECT id, email FROM users WHERE email = ${params.email}"));
    assert!(ts.contains("const email = params.email.trim().toLowerCase();"));
    assert!(ts.contains("errors.push({ path: \"email\", message: \"must be a valid email address\" });"));
    assert!(ts.contains("export async function getUsers("));
    assert!(ts.contains("): Promise<Users[]> {"));
    assert!(ts.contains("export async function deleteUser("));
    assert!(ts.contains("): Promise<void> {"));

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(rs.contains("pub struct Users {"));
    assert!(rs.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"));
    assert!(rs.contains("if !is_email(&email) {"));
    assert!(rs.contains("message: \"Bad Email\".to_string()"));
    assert!(rs.contains("if let Some(value) = &self.external_id {"));
    assert!(rs.contains("fn is_uuid(value: &str) -> bool {"));

    assert!(rs.contains("pub struct GetUserParams {"));
    assert!(rs.contains("pub email: String,"));
    assert!(rs.contains("pub async fn get_user("));
    assert!(rs.contains("pool: &sqlx::PgPool,"));
    assert!(rs.contains(") -> Result<Option<Users>, Box<dyn std::error::Error>> {"));
    assert!(rs.contains("params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;"));
    assert!(rs.contains("sqlx::query_as!("));
    assert!(rs.contains(".fetch_optional(pool)"));
    assert!(rs.contains("pub async fn get_users("));
    assert!(rs.contains(") -> Result<Vec<Users>, Box<dyn std::error::Error>> {"));
    assert!(rs.contains(".fetch_all(pool)"));
    assert!(rs.contains("pub async fn delete_user("));
    assert!(rs.contains("sqlx::query("));
    assert!(rs.contains(".bind(params.id)"));
    assert!(rs.contains(".execute(pool)"));
    assert!(rs.contains("fn is_email(value: &str) -> bool {"));
}

#[test]
fn cache_hit_skips_codegen_and_reports_up_to_date() {
    let dir = fixture_dir("run_cache_hit");
    write_fixture(&dir, SCHEMA_SQL);

    let first = run_generate(&dir);
    assert!(first.status.success(), "{}", stdout(&first));
    assert!(stdout(&first).contains("generated 2 target(s)"));
    assert!(dir.join(".axiom.cache").exists(), "cache file should exist");

    let second = run_generate(&dir);
    assert!(second.status.success(), "{}", stdout(&second));
    assert!(
        stdout(&second).contains("Everything up to date (<0.5ms)"),
        "expected cache hit, got: {}",
        stdout(&second)
    );
    assert!(
        !stdout(&second).contains("generated 2 target(s)"),
        "codegen should have been skipped"
    );
}

#[test]
fn schema_change_invalidates_cache_and_regenerates() {
    let dir = fixture_dir("run_invalidation");
    write_fixture(&dir, SCHEMA_SQL);

    let first = run_generate(&dir);
    assert!(first.status.success());
    assert!(stdout(&first).contains("generated 2 target(s)"));

    let second = run_generate(&dir);
    assert!(stdout(&second).contains("Everything up to date (<0.5ms)"));

    // Touch the schema: cache must now miss.
    let modified = SCHEMA_SQL.replace("VARCHAR(32)", "VARCHAR(64)");
    std::fs::write(dir.join("schema.sql"), modified).unwrap();

    let third = run_generate(&dir);
    assert!(third.status.success());
    assert!(
        stdout(&third).contains("generated 2 target(s)"),
        "expected regeneration after schema change, got: {}",
        stdout(&third)
    );

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).unwrap();
    assert!(ts.contains("VARCHAR(64)") || ts.contains("string;"));
}

#[test]
fn malformed_schema_errors_cleanly() {
    let dir = fixture_dir("run_bad_schema");
    write_fixture(&dir, "THIS IS NOT VALID SQL ###");
    // Ensure the cache can't short-circuit the parse failure.
    let _ = std::fs::remove_file(dir.join(".axiom.cache"));

    let output = run_generate(&dir);
    assert!(
        !output.status.success(),
        "expected failure for malformed SQL, got success: {}",
        stdout(&output)
    );
}

#[test]
fn query_change_invalidates_cache_and_regenerates() {
    let dir = fixture_dir("run_query_invalidation");
    write_fixture(&dir, SCHEMA_SQL);

    let first = run_generate(&dir);
    assert!(first.status.success(), "{}", stdout(&first));
    assert!(stdout(&first).contains("generated 2 target(s)"));
    assert!(stdout(&first).contains("3 queries"), "summary should count queries");

    let second = run_generate(&dir);
    assert!(stdout(&second).contains("Everything up to date (<0.5ms)"));

    // Edit a query file: the BLAKE3 cache must miss and codegen reruns.
    let queries = dir.join("queries/accounts.sql");
    let contents = std::fs::read_to_string(&queries).unwrap();
    std::fs::write(&queries, contents.replace("LIMIT $1", "LIMIT $1::int")).unwrap();

    let third = run_generate(&dir);
    assert!(third.status.success(), "{}", stdout(&third));
    assert!(
        stdout(&third).contains("generated 2 target(s)"),
        "expected regeneration after query change, got: {}",
        stdout(&third)
    );

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).unwrap();
    assert!(
        ts.contains("LIMIT ${params.limit}::int"),
        "query body should be regenerated"
    );
}
