mod cache;
mod catalog;
mod codegen;
mod config;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use cache::{compute_file_hash, is_cache_valid, write_cache_atomically};
use catalog::{parse_sql_catalog, TableCatalog};
use codegen::{generate_rust, generate_typescript};
use config::{AxiomConfig, OutputConfig};

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let (config, config_path) = AxiomConfig::find_and_load(cli.config.as_deref())?;

    let targets = config.target_types();
    println!(
        "loaded configuration `{}` for project `{}` (targets: {})",
        config_path.display(),
        config.project.name,
        if targets.is_empty() {
            "none".to_string()
        } else {
            targets.join(", ")
        }
    );

    match cli.command {
        Commands::Generate(args) => run_generate(args, &config, &config_path).await,
        Commands::Push(args) => run_push(args).await,
    }
}

fn load_env(env_file: Option<&PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = env_file {
        dotenvy::from_path(path)?;
    }
    Ok(())
}

async fn run_generate(
    args: GenerateArgs,
    config: &AxiomConfig,
    config_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    load_env(args.env_file.as_ref())?;

    let base = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    for pattern in &config.inputs.schema {
        let pattern_path = Path::new(pattern);
        let joined = if pattern_path.is_absolute() {
            pattern.to_string()
        } else {
            base.join(pattern).to_string_lossy().into_owned()
        };
        for path in glob::glob(&joined)? {
            let path = path?;
            let sql = std::fs::read_to_string(&path)?;
            sources.push((path, sql));
        }
    }

    // BLAKE3 hashes of the config file and every resolved input file.
    let config_hash = compute_file_hash(config_path)?;
    let mut file_hashes = BTreeMap::new();
    for (path, _) in &sources {
        file_hashes.insert(path.to_string_lossy().into_owned(), compute_file_hash(path)?);
    }

    let cache_path = if config.cache.path.is_absolute() {
        config.cache.path.clone()
    } else {
        base.join(&config.cache.path)
    };

    if config.cache.enabled && is_cache_valid(&cache_path, &config_hash, &file_hashes) {
        println!("Everything up to date (<0.5ms)");
        return Ok(());
    }

    let mut catalog = TableCatalog::default();
    for (_, sql) in &sources {
        catalog.tables.extend(parse_sql_catalog(sql)?.tables);
    }

    let generated: Vec<(String, String)> = config
        .outputs
        .iter()
        .map(|(name, output)| match output {
            OutputConfig::TypeScript(_) => (name.clone(), generate_typescript(&catalog)),
            OutputConfig::Rust(_) => (name.clone(), generate_rust(&catalog)),
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

    println!(
        "generated {} target(s) from {} table(s): {}",
        generated.len(),
        catalog.tables.len(),
        catalog
            .tables
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(())
}

async fn run_push(args: PushArgs) -> Result<(), Box<dyn std::error::Error>> {
    load_env(args.env_file.as_ref())?;

    match args.db_url {
        Some(_) => println!("push: database push is not implemented yet"),
        None => println!("push: not implemented yet"),
    }
    Ok(())
}
