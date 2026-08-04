//! Database connection and schema synchronization for `axiom push`.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Pure URL-resolution precedence: an explicit CLI URL wins, otherwise the
/// `DATABASE_URL` found in the environment (populated from `.env`) is used.
pub fn resolve_db_url_from(
    cli_url: Option<String>,
    env_url: Option<String>,
) -> Result<String, String> {
    if let Some(url) = cli_url {
        return Ok(url);
    }
    if let Some(url) = env_url
        && !url.trim().is_empty()
    {
        return Ok(url);
    }
    Err(
        "no database URL found: pass `--db-url <URL>` or set DATABASE_URL in your \
         environment or .env file"
            .to_string(),
    )
}

/// Resolve the database URL for a push: explicit `--db-url`, then a custom
/// `--env-file` (or the default `.env`), then the process environment.
pub fn resolve_db_url(
    cli_url: Option<String>,
    env_file: Option<&Path>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(url) = cli_url {
        return Ok(url);
    }

    match env_file {
        Some(path) => {
            if !path.exists() {
                return Err(
                    format!("env file `{}` does not exist", path.display()).into(),
                );
            }
            dotenvy::from_path(path)?;
        }
        None if Path::new(".env").exists() => {
            dotenvy::from_filename(".env")?;
        }
        None => {}
    }

    resolve_db_url_from(None, std::env::var("DATABASE_URL").ok()).map_err(Into::into)
}

/// Establish an asynchronous connection and spawn the connection upkeep task.
pub async fn connect(
    db_url: &str,
) -> Result<tokio_postgres::Client, Box<dyn std::error::Error>> {
    let (client, connection) = tokio_postgres::connect(db_url, tokio_postgres::NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[axiom] database connection error: {e}");
        }
    });
    Ok(client)
}

/// Read and batch-execute each DDL file, logging per-file progress.
///
/// Returns the number of schema files successfully executed.
pub async fn push_schema(
    client: &tokio_postgres::Client,
    schema_files: &[PathBuf],
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut executed = 0;
    for path in schema_files {
        let sql = std::fs::read_to_string(path)?;
        let started = Instant::now();
        client.batch_execute(&sql).await?;
        println!(
            "[axiom] synced `{}` ({} statements, {:?})",
            path.display(),
            sql.split(';').filter(|s| !s.trim().is_empty()).count(),
            started.elapsed(),
        );
        executed += 1;
    }
    Ok(executed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_url_takes_precedence_over_env() {
        assert_eq!(
            resolve_db_url_from(
                Some("postgres://cli.example".to_string()),
                Some("postgres://env.example".to_string()),
            )
            .unwrap(),
            "postgres://cli.example"
        );
    }

    #[test]
    fn env_url_is_used_when_cli_absent() {
        assert_eq!(
            resolve_db_url_from(None, Some("postgres://env.example".to_string())).unwrap(),
            "postgres://env.example"
        );
    }

    #[test]
    fn empty_env_url_is_rejected() {
        assert!(resolve_db_url_from(None, Some("   ".to_string())).is_err());
    }

    #[test]
    fn missing_both_sources_is_an_actionable_error() {
        let err = resolve_db_url_from(None, None).unwrap_err();
        assert!(err.contains("--db-url"), "error should hint at --db-url: {err}");
        assert!(err.contains("DATABASE_URL"), "error should mention DATABASE_URL: {err}");
    }
}
