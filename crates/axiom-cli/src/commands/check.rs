//! `axiom check` — workspace verification and generated-output synchronization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axiom_core::cache::ToolCache;
use axiom_core::config::AxiomConfig;
use axiom_core::errors::AxiomError;
use axiom_core::paths::resolve_path;
use axiom_diagnostics::{render_all, Renderer, summary};
use owo_colors::{OwoColorize, Stream};

use crate::commands::base_dir;
use crate::commands::tty::is_tty;
use crate::CheckArgs;

/// Run every check phase over the workspace. With `--fix`, out-of-sync
/// generated outputs are rewritten before reporting.
pub fn run(args: CheckArgs, config: &AxiomConfig, config_path: &Path) -> Result<i32, AxiomError> {
    let base = base_dir(config_path);
    let workspace = axiom_check::resolve_inputs(config, &base)?;

    let cache_path = resolve_path(&base, &config.cache.path);

    let mut cache: Option<ToolCache> = None;
    if config.cache.enabled {
        cache = Some(ToolCache::open(&cache_path));
    }

    let report = axiom_check::check_workspace(cache.as_mut(), &workspace, config, &base, args.fix)?;

    if let Some(cache) = cache.as_mut()
        && let Err(_) = cache.save(&cache_path)
    {
        // Cache is an optimization; a failed write is not fatal.
    }

    let renderer = Renderer::new(is_tty());
    if !report.diagnostics.is_empty() {
        let sources = source_map(&workspace);
        eprint!(
            "{}",
            render_all(&renderer, &report.diagnostics, |path| sources
                .get(path)
                .cloned())
        );
    }

    let (errors, warnings) = summary(&report.diagnostics);
    if !report.fixed.is_empty() {
        let verb = if report.fixed.len() == 1 { "file" } else { "files" };
        println!(
            "{} {}",
            format!("fixed {} output {}", report.fixed.len(), verb).if_supports_color(
                Stream::Stdout,
                |s| s.green().bold().to_string()
            ),
            "(run `axiom generate` next)".if_supports_color(Stream::Stdout, |s| s.dimmed().to_string()),
        );
    }

    if errors > 0 {
        return Ok(1);
    }
    if !report.fixed.is_empty() {
        // Fixes were applied and nothing is left broken.
        return Ok(2);
    }
    if warnings > 0 {
        let w = if warnings == 1 { "warning" } else { "warnings" };
        eprintln!(
            "{}",
            format!("{warnings} {w} found")
                .if_supports_color(Stream::Stderr, |s| s.yellow().to_string()),
        );
        return Ok(0);
    }
    println!(
        "{}",
        "All checks passed".if_supports_color(Stream::Stdout, |s| {
            s.green().bold().to_string()
        }),
    );
    Ok(0)
}

fn source_map(workspace: &axiom_check::Workspace) -> BTreeMap<PathBuf, String> {
    let mut map = BTreeMap::new();
    for (path, src) in &workspace.schema_files {
        map.insert(path.clone(), src.clone());
    }
    for (path, src) in &workspace.query_files {
        map.insert(path.clone(), src.clone());
    }
    for (path, src) in &workspace.model_files {
        map.insert(path.clone(), src.clone());
    }
    map
}
