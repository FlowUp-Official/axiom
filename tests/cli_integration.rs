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
    std::fs::write(
        dir.join("axiom.json"),
        r#"{
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "inputs": { "schema": ["schema.sql"], "queries": [] },
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

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(rs.contains("pub struct Users {"));
    assert!(rs.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"));
    assert!(rs.contains("if !is_email(&email) {"));
    assert!(rs.contains("message: \"Bad Email\".to_string()"));
    assert!(rs.contains("if let Some(value) = &self.external_id {"));
    assert!(rs.contains("fn is_uuid(value: &str) -> bool {"));
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
