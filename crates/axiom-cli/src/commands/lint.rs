//! `axiom lint` — static analysis over `.axm` models and SQL inputs.

use std::path::Path;

use axiom_core::cache::ToolCache;
use axiom_core::config::AxiomConfig;
use axiom_core::errors::AxiomError;
use axiom_core::paths::resolve_path;
use axiom_diagnostics::{render_all, Renderer, summary};
use axiom_lint::{lint_sources, LintOptions, WorkspaceView};
use owo_colors::{OwoColorize, Stream};

use crate::commands::base_dir;
use crate::commands::tty::is_tty;
use crate::LintArgs;

/// Run the configured lint rules over every input file, honoring
/// `--rules` selection and the content-addressed [`ToolCache`].
pub fn run(args: LintArgs, config: &AxiomConfig, config_path: &Path) -> Result<i32, AxiomError> {
    let base = base_dir(config_path);
    let workspace = axiom_check::resolve_inputs(config, &base)?;

    let mut files = Vec::with_capacity(
        workspace.schema_files.len() + workspace.query_files.len() + workspace.model_files.len(),
    );
    files.extend(workspace.schema_files.iter().cloned());
    files.extend(workspace.query_files.iter().cloned());
    files.extend(workspace.model_files.iter().cloned());

    let mut view = WorkspaceView::empty();
    view.referenced_models = axiom_check::collect_referenced_models(&workspace.model_files);

    let cache_path = resolve_path(&base, &config.cache.path);

    let mut cache: Option<ToolCache> = None;
    if config.cache.enabled {
        cache = Some(ToolCache::open(&cache_path));
    }

    let options = LintOptions {
        rules: args.rules.clone(),
    };
    let diagnostics = lint_sources(cache.as_mut(), &files, &view, &options);

    // Persist new cache entries; failures degrade to recomputation next run.
    if let Some(cache) = cache.as_mut()
        && let Err(_) = cache.save(&cache_path)
    {
        // Cache is an optimization; a failed write is not fatal.
    }

    let renderer = Renderer::new(is_tty());
    if !diagnostics.is_empty() {
        eprint!("{}", render_all(&renderer, &diagnostics, |_| None));
    }

    let (errors, warnings) = summary(&diagnostics);
    if errors + warnings == 0 {
        println!(
            "{}",
            "No lint issues found".if_supports_color(Stream::Stdout, |s| {
                s.green().bold().to_string()
            }),
        );
        return Ok(0);
    }

    let w = if warnings == 1 { "warning" } else { "warnings" };
    let e = if errors == 1 { "error" } else { "errors" };
    eprintln!(
        "{}",
        format!("{warnings} {w}, {errors} {e} found")
            .if_supports_color(Stream::Stderr, |s| s.yellow().bold().to_string()),
    );
    Ok(1)
}
