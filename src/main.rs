mod catalog;
mod config;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use catalog::{parse_sql_catalog, TableCatalog};
use config::AxiomConfig;

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

    let mut catalog = TableCatalog::default();
    for (_, sql) in &sources {
        let parsed = parse_sql_catalog(sql)?;
        catalog.tables.extend(parsed.tables);
    }

    println!("parsed {} table(s) from schema inputs:", catalog.tables.len());
    for table in &catalog.tables {
        println!("  {} ({} columns)", table.name, table.columns.len());
        for column in &table.columns {
            println!(
                "    {} {} [{}{}] -> {} rule(s)",
                column.name,
                column.data_type,
                if column.primary_key { "PK " } else { "" },
                if column.nullable { "nullable" } else { "not null" },
                column.rules.len()
            );
        }
    }

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
