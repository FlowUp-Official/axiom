//! End-to-end CLI integration tests.
//!
//! Each test runs the compiled `axiom` binary against a self-contained fixture
//! directory under `target/test_fixtures/` (never system `/tmp`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCHEMA_SQL: &str = r#"
CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    external_id UUID,
    username VARCHAR(32)
);
"#;

const MODELS_AXM: &str = r#"
query get_user($email: String) -> Users {
  SELECT id, email FROM users WHERE email = $email
}

query get_users($limit: Int) -> Users[] {
  SELECT id, email FROM users ORDER BY id LIMIT $1
}

query delete_user($id: Int) {
  DELETE FROM users WHERE id = $1
}
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
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("axiom.json"),
        r#"{
  "$schema": "https://github.com/FlowUp-Official/axiom/releases/download/v0.7.0/axiom.schema.json",
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "source": { "schema": ["schema.sql"], "axm": ["models/models.axm"] },
  "codegen": {
    "validation": {
      "apis": ["safeParse", "parse"],
      "safeParse": { "errors": "all" }
    }
  },
  "outputs": {
    "api": { "type": "typescript", "path": "gen/api.ts" },
    "core": { "type": "rust", "path": "gen/core.rs" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("schema.sql"), schema).unwrap();
    std::fs::write(dir.join("models/models.axm"), MODELS_AXM).unwrap();
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

fn run_init(dir: &Path, force: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_axiom"));
    cmd.current_dir(dir).arg("init");
    if force {
        cmd.arg("--force");
    }
    cmd.output().expect("failed to run axiom binary")
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
    assert!(ts.contains("export function validateUsers(input: Users): ValidationError[] {"));
    assert!(
        !ts.contains("Bad Email"),
        "column annotations were removed from schema"
    );
    assert!(
        !ts.contains("input.username"),
        "column transforms no longer emitted"
    );
    assert!(
        !ts.contains("UUID_RE"),
        "column uuid rule no longer emitted"
    );
    assert!(
        !ts.contains("const IPV6_RE ="),
        "unused preset should not be emitted"
    );

    assert!(ts.contains("import type { Sql } from 'postgres';"));
    assert!(ts.contains("export interface GetUserParams {"));
    assert!(ts.contains("  email: string;"));
    assert!(ts.contains("  limit: number;"));
    assert!(ts.contains("export async function getUser("));
    assert!(ts.contains("  sql: Sql,"));
    assert!(ts.contains("  params: GetUserParams"));
    assert!(ts.contains("): Promise<Users> {"));
    assert!(ts.contains("SELECT id, email FROM users WHERE email = ${params.email}"));
    assert!(ts.contains("export async function getUsers("));
    assert!(ts.contains("): Promise<Users[]> {"));
    assert!(ts.contains("export async function deleteUser("));
    assert!(ts.contains("): Promise<void> {"));

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(rs.contains("pub struct Users {"));
    assert!(rs.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"));
    assert!(rs.contains("pub fn validate(&self) -> Result<(), Vec<ValidationError>>"));
    assert!(rs.contains("let _ = self;"));
    assert!(
        !rs.contains("Bad Email"),
        "column annotations were removed from schema"
    );
    assert!(
        !rs.contains("if let Some(value)"),
        "nullable column rules no longer emitted"
    );
    assert!(
        !rs.contains("fn is_uuid"),
        "column uuid rule no longer emitted"
    );

    assert!(rs.contains("pub struct GetUserParams {"));
    assert!(rs.contains("pub email: String,"));
    assert!(rs.contains("pub async fn get_user("));
    assert!(rs.contains("client: &tokio_postgres::Client,"));
    assert!(rs.contains(") -> Result<Users, Box<dyn std::error::Error>> {"));
    assert!(rs.contains(
        "params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;"
    ));
    assert!(rs.contains("let bind0 = params.email.to_axm_text();"));
    assert!(rs.contains("let binds: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&bind0];"));
    assert!(rs.contains("row_to_json(axm_q)::text AS axm_row"));
    assert!(rs.contains("serde_json::from_value::<Users>(value)?"));
    assert!(rs.contains("pub async fn get_users("));
    assert!(rs.contains(") -> Result<Vec<Users>, Box<dyn std::error::Error>> {"));
    assert!(rs.contains("let rows = client.query("));
    assert!(rs.contains("pub async fn delete_user("));
    assert!(rs.contains("client.execute("));
    assert!(rs.contains("impl ToSql for AxmTextValue {"));
}

#[test]
fn query_declarations_are_emitted_exactly_once() {
    let dir = fixture_dir("run_single_emission");
    write_fixture(&dir, SCHEMA_SQL);

    let output = run_generate(&dir);
    assert!(
        output.status.success(),
        "generate failed: {}",
        stdout(&output)
    );

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).expect("api.ts should exist");
    for symbol in [
        "export async function getUser(",
        "export async function getUsers(",
        "export async function deleteUser(",
    ] {
        assert_eq!(
            ts.matches(symbol).count(),
            1,
            "`{symbol}` must appear exactly once in api.ts"
        );
    }

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    for symbol in [
        "pub async fn get_user(",
        "pub async fn get_users(",
        "pub async fn delete_user(",
    ] {
        assert_eq!(
            rs.matches(symbol).count(),
            1,
            "`{symbol}` must appear exactly once in core.rs"
        );
    }
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
    assert!(
        stdout(&first).contains("3 queries"),
        "summary should count queries"
    );

    let second = run_generate(&dir);
    assert!(stdout(&second).contains("Everything up to date (<0.5ms)"));

    // Edit a query in the `.axm` source: the BLAKE3 cache must miss and codegen
    // reruns.
    let queries = dir.join("models/models.axm");
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

#[test]
fn init_creates_valid_config_file() {
    let dir = fixture_dir("run_init_create");
    std::fs::write(dir.join("placeholder.txt"), "").unwrap();

    let output = run_init(&dir, false);
    assert!(output.status.success(), "init failed: {}", stdout(&output));

    let path = dir.join("axiom.json");
    assert!(path.exists(), "axiom.json should be created");
    let contents = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&contents).unwrap();

    // Test binaries are development builds (no `AXIOM_RELEASE` set), so they
    // must NOT emit a version-pinned `$schema`: the running code may have
    // diverged from the nearest published release, and an emitted URL could
    // promise a schema that this binary did not generate. Release builds
    // embed the URL via `schema_version_url` (covered by axiom-core unit
    // tests).
    assert!(
        value.get("$schema").is_none(),
        "development builds must omit `$schema`, got: {value}"
    );

    assert!(stdout(&output).contains("Initialized new axiom.json configuration file"));
}

#[test]
fn init_refuses_overwrite_then_force_overwrites() {
    let dir = fixture_dir("run_init_force");
    std::fs::write(dir.join("placeholder.txt"), "").unwrap();

    let first = run_init(&dir, false);
    assert!(first.status.success(), "{}", stdout(&first));

    let second = run_init(&dir, false);
    assert!(
        !second.status.success(),
        "second init without --force should fail"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already exists"),
        "stderr should mention the existing file"
    );

    let third = run_init(&dir, true);
    assert!(
        third.status.success(),
        "init with --force should succeed: {}",
        stdout(&third)
    );
}

#[test]
fn lint_flags_sql_rules_in_axm_query_bodies() {
    let dir = fixture_dir("run_lint_axm_query_bodies");
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("axiom.json"),
        r#"{
  "$schema": "https://github.com/FlowUp-Official/axiom/releases/download/v0.7.0/axiom.schema.json",
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "source": { "schema": ["schema.sql"], "axm": ["models/models.axm"] },
  "codegen": {
    "validation": {
      "apis": ["safeParse", "parse"],
      "safeParse": { "errors": "all" }
    }
  },
  "outputs": {
    "api": { "type": "typescript", "path": "gen/api.ts" },
    "core": { "type": "rust", "path": "gen/core.rs" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("schema.sql"), SCHEMA_SQL).unwrap();
    std::fs::write(
        dir.join("models/models.axm"),
        "model User { id: UUID }\n\nquery delete_all() {\n  DELETE FROM users\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(&dir)
        .arg("--config")
        .arg("axiom.json")
        .arg("lint")
        .output()
        .expect("failed to run axiom binary");
    assert!(
        !output.status.success(),
        "lint should fail on a DELETE without WHERE"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lint.missing-where-clause"),
        "stderr should report missing-where-clause: {stderr}"
    );
}

#[test]
fn lint_flags_unused_query_param_and_type_alias() {
    let dir = fixture_dir("run_lint_unused_param_and_alias");
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(
        dir.join("axiom.json"),
        r#"{
  "$schema": "https://github.com/FlowUp-Official/axiom/releases/download/v0.7.0/axiom.schema.json",
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "source": { "schema": ["schema.sql"], "axm": ["models/models.axm"] },
  "codegen": {
    "validation": {
      "apis": ["safeParse", "parse"],
      "safeParse": { "errors": "all" }
    }
  },
  "outputs": {
    "api": { "type": "typescript", "path": "gen/api.ts" },
    "core": { "type": "rust", "path": "gen/core.rs" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("schema.sql"), SCHEMA_SQL).unwrap();
    std::fs::write(
        dir.join("models/models.axm"),
        "model User { id: UUID }\n\nmodel Unused = String\n\nquery get_user($id: UUID, $ghost: String) -> User? {\n  SELECT id FROM users WHERE id = $id\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(&dir)
        .arg("--config")
        .arg("axiom.json")
        .arg("lint")
        .output()
        .expect("failed to run axiom binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "lint should fail on unused query params and aliases: {stderr}"
    );
    assert!(
        stderr.contains("lint.unused-query-param"),
        "stderr should report unused-query-param: {stderr}"
    );
    assert!(
        stderr.contains("lint.dead-model"),
        "stderr should report dead-model: {stderr}"
    );
}

const LINT_CONFIG_JSON: &str = r#"{
  "$schema": "https://github.com/FlowUp-Official/axiom/releases/download/v0.7.0/axiom.schema.json",
  "project": { "name": "fixture", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "source": { "schema": ["schema.sql"], "axm": ["models/models.axm"] },
  "codegen": {
    "validation": {
      "apis": ["safeParse", "parse"],
      "safeParse": { "errors": "all" }
    }
  },
  "outputs": {
    "api": { "type": "typescript", "path": "gen/api.ts" },
    "core": { "type": "rust", "path": "gen/core.rs" }
  }
}
"#;

fn write_lint_fixture(dir: &Path, models: &str) {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(dir.join("axiom.json"), LINT_CONFIG_JSON).unwrap();
    std::fs::write(dir.join("schema.sql"), SCHEMA_SQL).unwrap();
    std::fs::write(dir.join("models/models.axm"), models).unwrap();
}

fn run_lint(dir: &Path, rules: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_axiom"));
    cmd.current_dir(dir)
        .arg("--config")
        .arg("axiom.json")
        .arg("lint");
    for rule in rules {
        cmd.arg("--rules").arg(rule);
    }
    cmd.output().expect("failed to run axiom binary")
}

#[test]
fn lint_dead_model_treats_query_referenced_model_as_live() {
    let dir = fixture_dir("run_lint_dead_model_query_ref");
    write_lint_fixture(
        &dir,
        "model User { id: UUID }\n\nquery get_user($id: UUID) -> User? {\n  SELECT id FROM users WHERE id = $id\n}\n",
    );

    let output = run_lint(&dir, &["dead-model"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "expected no dead-model: {stderr}");
    assert!(!stderr.contains("lint.dead-model"), "{stderr}");
}

#[test]
fn lint_select_star_reports_each_query_once() {
    let dir = fixture_dir("run_lint_select_star_counts");
    write_lint_fixture(
        &dir,
        "model User { id: UUID }\n\nquery A() -> User[] {\n  SELECT * FROM users\n}\n\nquery B() -> User[] {\n  SELECT * FROM users\n}\n",
    );

    let output = run_lint(&dir, &["select-star"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("lint.select-star").count(),
        2,
        "each query must be reported once: {stderr}"
    );
}

#[test]
fn lint_naming_convention_flags_full_identifier_violations() {
    let dir = fixture_dir("run_lint_naming_convention");
    write_lint_fixture(
        &dir,
        "model user_name = String\nmodel User {\n  id: UUID\n  user_Name: String\n}\n",
    );

    let output = run_lint(&dir, &["naming-convention"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("lint.naming-convention"), "{stderr}");
    assert!(stderr.contains("user_name"), "{stderr}");
    assert!(stderr.contains("user_Name"), "{stderr}");
}

const APIS_MODELS_AXM: &str = r#"
model User {
  email: String .email()
  age: Int .min(0)
}
"#;

fn write_apis_fixture(dir: &Path, config_json: &str) {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::write(dir.join("axiom.json"), config_json).unwrap();
    std::fs::write(dir.join("schema.sql"), SCHEMA_SQL).unwrap();
    std::fs::write(dir.join("models/models.axm"), APIS_MODELS_AXM).unwrap();
}

fn config_with_apis(apis: &str) -> String {
    format!(
        r#"{{
  "$schema": "https://github.com/FlowUp-Official/axiom/releases/download/v0.7.0/axiom.schema.json",
  "project": {{ "name": "fixture", "dialect": "postgres" }},
  "cache": {{ "enabled": false, "path": ".axiom.cache" }},
  "source": {{ "schema": ["schema.sql"], "axm": ["models/models.axm"] }},
  "codegen": {{
    "validation": {{
      "apis": {apis}
    }}
  }},
  "outputs": {{
    "api": {{ "type": "typescript", "path": "gen/api.ts" }},
    "core": {{ "type": "rust", "path": "gen/core.rs" }}
  }}
}}
"#,
    )
}

#[test]
fn safe_parse_only_emits_no_parse() {
    let dir = fixture_dir("run_apis_safe_parse_only");
    write_apis_fixture(&dir, &config_with_apis(r#"["safeParse"]"#));
    // safeParse requires its config object since it's in apis.
    let config_path = dir.join("axiom.json");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config = config.replace(
        r#""validation": {
      "apis": ["safeParse"]
    }"#,
        r#""validation": {
      "apis": ["safeParse"],
      "safeParse": { "errors": "all" }
    }"#,
    );
    std::fs::write(&config_path, config).unwrap();

    let output = run_generate(&dir);
    assert!(output.status.success(), "generate failed: {}", stdout(&output));

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).expect("api.ts should exist");
    assert!(ts.contains("export function safeParseUser("));
    assert!(!ts.contains("export function parseUser("), "parse should not be emitted");

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(rs.contains("pub fn safe_parse("));
    assert!(!rs.contains("pub fn parse("), "parse should not be emitted");
}

#[test]
fn parse_only_emits_standalone_parse() {
    let dir = fixture_dir("run_apis_parse_only");
    write_apis_fixture(&dir, &config_with_apis(r#"["parse"]"#));

    let output = run_generate(&dir);
    assert!(output.status.success(), "generate failed: {}", stdout(&output));

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).expect("api.ts should exist");
    assert!(ts.contains("export function parseUser("));
    assert!(!ts.contains("function safeParse"), "safeParse should not be emitted");
    assert!(
        !ts.contains("const result = safeParseUser"),
        "standalone parse should not delegate to safeParse"
    );

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(rs.contains("pub fn parse("));
    assert!(!rs.contains("pub fn safe_parse("), "safe_parse should not be emitted");
}

#[test]
fn safe_parse_first_errors_mode_drives_fail_fast_default() {
    let dir = fixture_dir("run_apis_safe_parse_first");
    write_apis_fixture(&dir, &config_with_apis(r#"["safeParse"]"#));
    let config_path = dir.join("axiom.json");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config = config.replace(
        r#""validation": {
      "apis": ["safeParse"]
    }"#,
        r#""validation": {
      "apis": ["safeParse"],
      "safeParse": { "errors": "first" }
    }"#,
    );
    std::fs::write(&config_path, config).unwrap();

    let output = run_generate(&dir);
    assert!(output.status.success(), "generate failed: {}", stdout(&output));

    let ts = std::fs::read_to_string(dir.join("gen/api.ts")).expect("api.ts should exist");
    assert!(
        ts.contains("AXM_STOP"),
        "first mode should emit fail-fast scaffolding in TS"
    );
    assert!(ts.contains("_axm_fail_fast"));

    let rs = std::fs::read_to_string(dir.join("gen/core.rs")).expect("core.rs should exist");
    assert!(
        rs.contains("AXM_FAIL_FAST"),
        "first mode should emit fail-fast scaffolding in Rust"
    );
}

fn run_help(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_axiom")).args(args).output().expect("run axiom --help")
}

#[test]
fn help_auto_piped_is_plain() {
    let out = run_help(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "piped help should still print usage");
    assert!(!stdout.contains("\x1b["), "piped auto help must NOT contain ANSI");
}

#[test]
fn help_color_always_piped_is_styled() {
    let out = run_help(&["--color", "always", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\x1b["), "--color always piped help MUST contain ANSI");
}

#[test]
fn help_color_never_is_plain() {
    let out = run_help(&["--color", "never", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "never help should still print usage");
    assert!(!stdout.contains("\x1b["), "--color never help must be plain");
}

#[test]
fn subcommand_help_color_always_is_styled() {
    let out = run_help(&["--color", "always", "check", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\x1b["), "subcommand help with --color always must contain ANSI");
}

#[test]
fn subcommand_help_auto_piped_is_plain() {
    let out = run_help(&["check", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("\x1b["), "piped auto subcommand help must NOT contain ANSI");
}

/// Returns true if the given SGR parameter bytes are a full reset (`\x1b[0m`).
fn is_reset(params: &[u8]) -> bool {
    params.iter().all(|&b| b == b'0')
}

/// Returns true if the given SGR parameter bytes set a foreground color
/// (SGR 30-37, 90-97, or 38 for extended). Clap's styling renderer (anstyle)
/// emits effects like bold (`1`) and colors as *separate* escape sequences, so a
/// bold region appears as `\x1b[1m\x1b[36m` — two sequences. We therefore
/// cannot judge "bold" in isolation; we scan whole styled regions instead.
fn is_fg_color_params(params: &[u8]) -> bool {
    let s = std::str::from_utf8(params).unwrap_or("");
    for part in s.split(';') {
        match part {
            "30" | "31" | "32" | "33" | "34" | "35" | "36" | "37" | "38" | "90" | "91"
            | "92" | "93" | "94" | "95" | "96" | "97" => return true,
            _ => {}
        }
    }
    false
}

/// An "active region" is the span from a non-reset SGR escape up to (but not
/// including) the next reset `\x1b[0m`. A region is "colorless bold" when it
/// contains a bold SGR (`1`) but no foreground-color SGR — exactly the pattern
/// that Clap's default `Styles::styled()` produced (e.g. `\x1b[1maxiom\x1b[0m`),
/// which terminals render as bold white.
fn has_colorless_bold_region(out: &str) -> bool {
    let bytes = out.as_bytes();
    let mut i = 0;
    let mut in_region = false;
    let mut region_has_bold = false;
    let mut region_has_fg = false;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Collect param bytes up to 'm'.
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'm' {
                end += 1;
            }
            if end >= bytes.len() {
                break;
            }
            let params = &bytes[start..end];
            if is_reset(params) {
                // End of region: flag if it was a colorless-bold region.
                if in_region && region_has_bold && !region_has_fg {
                    return true;
                }
                in_region = false;
                region_has_bold = false;
                region_has_fg = false;
            } else {
                if is_fg_color_params(params) {
                    region_has_fg = true;
                }
                let s = std::str::from_utf8(params).unwrap_or("");
                for part in s.split(';') {
                    if part == "1" {
                        region_has_bold = true;
                    }
                }
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
     // Region never closed by reset — treat trailing state too.
    in_region && region_has_bold && !region_has_fg
}

/// The styled help must never produce a "colorless bold" region (bold SGR with
/// no foreground color in the same region), which is what rendered the `axiom`
/// command name, headings, and flags as bold white under Clap's default
/// `Styles::styled()`. The intentional palette always pairs bold with a
/// foreground color.
#[test]
fn help_color_always_piped_has_no_colorless_bold_region() {
    let out = run_help(&["--color", "always", "--help"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(
        !has_colorless_bold_region(&text),
        "styled help contains a colorless-bold region (bold without fg color) — \
         reproduces the bold-white regression:\n{}",
        text
    );
}

/// The `axiom` command name in the usage line must be rendered with an explicit
/// foreground color (green in the intentional palette), never as bare bold.
#[test]
fn help_color_always_piped_renders_command_name_with_fg_color() {
    let out = run_help(&["--color", "always", "--help"]);
    assert!(out.status.success());
    let text = stdout(&out);
    // The intentional palette renders the bin name in bold green. An explicit
    // green foreground escape (\x1b[32m or \x1b[1;32m) must be present, and
    // the bin name must never appear as bare bold without a color escape.
    assert!(
        text.as_bytes().windows(5).any(|w| w == b"\x1b[32m" || w == b"\x1b[1;3"),
        "expected an explicit foreground color (green SGR 32) so the command name \
         is not bold-white; got:\n{}",
        text
    );
    // The exact bare-bold-no-color regression pattern is a hard failure.
    assert!(
        !text.contains("\x1b[1maxiom\x1b[0m"),
        "command name rendered as bare bold (bold white): {}",
        text
    );
}

/// Subcommand styled help must also avoid colorless-bold regions.
#[test]
fn subcommand_help_color_always_has_no_colorless_bold_region() {
    let out = run_help(&["--color", "always", "check", "--help"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("Usage:"));
    assert!(
        !has_colorless_bold_region(&text),
        "subcommand styled help contains a colorless-bold region — \
         reproduces the bold-white regression:\n{}",
        text
    );
}

/// `auto` over a pipe (the default test environment: stdout is not a TTY) must
/// stay plain — the palette change must not force color when piped.
#[test]
fn help_auto_piped_contains_no_ansi_at_all() {
    let out = run_help(&["--help"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(
        !text.contains("\x1b["),
        "auto help over a pipe must contain zero ANSI bytes"
    );
    assert!(text.contains("Usage:"));
}
