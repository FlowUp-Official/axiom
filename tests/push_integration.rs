//! Integration tests for `axiom push`: database URL resolution order and
//! (ignored by default) live Postgres schema push.

use std::path::PathBuf;

use axiom::db;

#[test]
fn cli_url_wins_over_env_url() {
    assert_eq!(
        db::resolve_db_url_from(
            Some("postgres://cli.example/db".to_string()),
            Some("postgres://env.example/db".to_string()),
        )
        .unwrap(),
        "postgres://cli.example/db"
    );
}

#[test]
fn env_url_is_fallback_when_cli_url_missing() {
    assert_eq!(
        db::resolve_db_url_from(None, Some("postgres://env.example/db".to_string())).unwrap(),
        "postgres://env.example/db"
    );
}

#[test]
fn missing_url_is_an_error_mentioning_both_sources() {
    let err = db::resolve_db_url_from(None, None).unwrap_err();
    assert!(err.contains("--db-url"));
    assert!(err.contains("DATABASE_URL"));
}

#[test]
fn blank_cli_url_is_still_authoritative() {
    assert_eq!(
        db::resolve_db_url_from(Some(String::new()), Some("postgres://env.example/db".to_string()))
            .unwrap(),
        ""
    );
}

/// Live Postgres check. Requires `DATABASE_URL` pointing at a test database:
///
/// ```sh
/// docker run --name axiom-test-postgres -e POSTGRES_PASSWORD=postgres \
///   -e POSTGRES_DB=axiom_test -p 5432:5432 -d postgres:16
/// DATABASE_URL=postgres://postgres:postgres@localhost:5432/axiom_test \
///   cargo test --test push_integration -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore]
async fn pushes_schema_batch_to_live_postgres() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let client = db::connect(&url).await.expect("failed to connect");

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test_fixtures")
        .join("run_push_ddl");
    std::fs::create_dir_all(&dir).unwrap();

    let ddl = dir.join("schema.sql");
    std::fs::write(
        &ddl,
        "DROP TABLE IF EXISTS push_probe;\nCREATE TABLE push_probe (id BIGSERIAL PRIMARY KEY, note TEXT NOT NULL);\n",
    )
    .unwrap();

    let count = db::push_schema(&client, &[ddl]).await.expect("push_schema failed");
    assert_eq!(count, 1);

    let row = client
        .query_one("SELECT COUNT(*)::bigint FROM push_probe", &[])
        .await
        .expect("push_probe should exist after push");
    assert_eq!(row.get::<_, i64>(0), 0);

    client
        .batch_execute("DROP TABLE push_probe")
        .await
        .expect("cleanup drop");
}
