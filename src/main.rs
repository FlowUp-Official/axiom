use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use axiom::cache::{compute_file_hash, is_cache_valid, write_cache_atomically};
use axiom::catalog::{parse_sql_catalog, TableCatalog};
use axiom::codegen::{generate_rust, generate_typescript};
use axiom::config::{AxiomConfig, OutputConfig};
use axiom::db;
use axiom::errors::AxiomError;
use axiom::query::{parse_query_file, QueryCatalog};
use clap::{Parser, Subcommand};
use owo_colors::{OwoColorize, Stream};

#[derive(Debug, Parser)]
#[command(
    name = "axiom",
    version,
    about = "Code generator for SQL schemas and queries"
)]
struct Cli {
    /// Path to the `axiom.json` configuration file. Auto-detected in the current
    /// directory when omitted.
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print the JSON schema for `axiom.json` files to stdout.
    Schema,
    /// Generate typed output from the configured inputs.
    Generate(GenerateArgs),
    /// Push generated output to a target database.
    Push(PushArgs),
}

#[derive(Debug, clap::Args)]
struct GenerateArgs {
    /// Database URL, used when generation needs to inspect a live database.
    #[arg(long, env = "DATABASE_URL")]
    db_url: Option<String>,

    /// Load environment variables from the given file.
    #[arg(long, value_name = "FILE")]
    env_file: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct PushArgs {
    /// Database URL to push to.
    #[arg(long, env = "DATABASE_URL")]
    db_url: Option<String>,

    /// Load environment variables from the given file.
    #[arg(long, value_name = "FILE")]
    env_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    miette::set_panic_hook();

    if let Err(err) = run().await {
        eprintln!("{:?}", miette::Report::new(err));
        std::process::exit(1);
    }
    Ok(())
}

async fn run() -> Result<(), AxiomError> {
    let cli = Cli::parse();

    if matches!(cli.command, Commands::Schema) {
        println!("{}", AxiomConfig::generate_json_schema());
        return Ok(());
    }

    let (config, config_path) = AxiomConfig::find_and_load(cli.config.as_deref())?;

    let targets = config.target_types();
    println!(
        "loaded configuration `{}` for project `{}` (targets: {})",
        config_path
            .display()
            .to_string()
            .if_supports_color(Stream::Stdout, |s| s.cyan().to_string()),
        config
            .project
            .name
            .if_supports_color(Stream::Stdout, |s| s.bold().to_string()),
        if targets.is_empty() {
            "none".to_string()
        } else {
            targets.join(", ")
        }
    );

    match cli.command {
        Commands::Generate(args) => run_generate(args, &config, &config_path).await,
        Commands::Push(args) => run_push(args, &config, &config_path).await,
        Commands::Schema => unreachable!("handled before config loading"),
    }
}

fn load_env(env_file: Option<&PathBuf>) -> Result<(), AxiomError> {
    if let Some(path) = env_file {
        dotenvy::from_path(path)?;
    }
    Ok(())
}

/// Resolve the configured glob patterns into ordered file paths, relative to
/// the directory containing the config file.
fn resolve_glob_paths(
    patterns: &[String],
    base: &Path,
) -> Result<Vec<PathBuf>, AxiomError> {
    let mut paths = Vec::new();
    for pattern in patterns {
        let pattern_path = Path::new(pattern);
        let joined = if pattern_path.is_absolute() {
            pattern.to_string()
        } else {
            base.join(pattern).to_string_lossy().into_owned()
        };
        for path in glob::glob(&joined)? {
            paths.push(path?);
        }
    }
    Ok(paths)
}

async fn run_generate(
    args: GenerateArgs,
    config: &AxiomConfig,
    config_path: &Path,
) -> Result<(), AxiomError> {
    load_env(args.env_file.as_ref())?;

    let base = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    for path in resolve_glob_paths(&config.inputs.schema, base)? {
        let sql = std::fs::read_to_string(&path)?;
        sources.push((path, sql));
    }

    let mut query_sources: Vec<(PathBuf, String)> = Vec::new();
    for path in resolve_glob_paths(&config.inputs.queries, base)? {
        let sql = std::fs::read_to_string(&path)?;
        query_sources.push((path, sql));
    }

    // BLAKE3 digests of the config file and every resolved input file (schema
    // and query sources alike, so edits to queries invalidate the cache).
    let config_hash = compute_file_hash(config_path)?;
    let mut file_hashes: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    for (path, _) in sources.iter().chain(&query_sources) {
        file_hashes.insert(path.to_string_lossy().into_owned(), compute_file_hash(path)?);
    }

    let cache_path = if config.cache.path.is_absolute() {
        config.cache.path.clone()
    } else {
        base.join(&config.cache.path)
    };

    if config.cache.enabled && is_cache_valid(&cache_path, &config_hash, &file_hashes) {
        println!(
            "{} {}",
            "Everything up to date".if_supports_color(Stream::Stdout, |s| {
                s.green().bold().to_string()
            }),
            "(<0.5ms)".if_supports_color(Stream::Stdout, |s| s.dimmed().to_string()),
        );
        return Ok(());
    }

    let mut catalog = TableCatalog::default();
    for (_, sql) in &sources {
        catalog.tables.extend(parse_sql_catalog(sql)?.tables);
    }

    let mut query_catalog = QueryCatalog::default();
    for (_, sql) in &query_sources {
        query_catalog.queries.extend(parse_query_file(sql)?.queries);
    }

    let generated: Vec<(String, String)> = config
        .outputs
        .iter()
        .map(|(name, output)| match output {
            OutputConfig::TypeScript(_) => {
                (name.clone(), generate_typescript(&catalog, &query_catalog))
            }
            OutputConfig::Rust(_) => (name.clone(), generate_rust(&catalog, &query_catalog)),
        })
        .collect();

    for (name, contents) in &generated {
        let path = match &config.outputs[name] {
            OutputConfig::TypeScript(ts) => &ts.path,
            OutputConfig::Rust(rust) => &rust.path,
        };
        let output_path = if path.is_absolute() {
            path.clone()
        } else {
            base.join(path)
        };
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output_path, contents)?;
    }

    write_cache_atomically(&cache_path, config_hash, file_hashes)?;

    let query_count = query_catalog.queries.len();
    let query_part = if query_count == 0 {
        String::new()
    } else {
        let word = if query_count == 1 { "query" } else { "queries" };
        format!(" and {query_count} {word}")
    };
    let summary = format!(
        "generated {} target(s) from {} table(s){}:",
        generated.len(),
        catalog.tables.len(),
        query_part,
    );
    let tables = catalog
        .tables
        .iter()
        .map(|t| t.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "{} {}",
        summary.if_supports_color(Stream::Stdout, |s| s.green().bold().to_string()),
        tables.if_supports_color(Stream::Stdout, |s| s.cyan().to_string()),
    );

    Ok(())
}

async fn run_push(
    args: PushArgs,
    config: &AxiomConfig,
    config_path: &Path,
) -> Result<(), AxiomError> {
    let base = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let db_url = db::resolve_db_url(args.db_url, args.env_file.as_deref())?;
    let schema_files = resolve_glob_paths(&config.inputs.schema, base)?;
    if schema_files.is_empty() {
        let notice = format!(
            "[axiom] no schema files matched `{}`; nothing to push",
            config.inputs.schema.join(", ")
        );
        println!(
            "{}",
            notice.if_supports_color(Stream::Stdout, |s| s.yellow().to_string())
        );
        return Ok(());
    }

    let client = db::connect(&db_url).await?;
    let started = Instant::now();
    let count = db::push_schema(&client, &schema_files).await?;
    let timing = format!(
        "({} file{} in {}ms)",
        count,
        if count == 1 { "" } else { "s" },
        started.elapsed().as_millis(),
    );
    println!(
        "{} {}",
        "[axiom] Database schema push complete!".if_supports_color(
            Stream::Stdout,
            |s| s.green().bold().to_string()
        ),
        timing.if_supports_color(Stream::Stdout, |s| s.dimmed().to_string()),
    );
    Ok(())
}
