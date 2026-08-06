//! Generated-output synchronization: recompute every output in memory and
//! compare it against the file on disk.

use std::path::{Path, PathBuf};

use axiom_core::axm::{generate_rust_models, generate_typescript_models, ModelRegistry};
use axiom_core::catalog::TableCatalog;
use axiom_core::codegen::{generate_rust, generate_typescript};
use axiom_core::config::{AxiomConfig, OutputConfig};
use axiom_core::paths::resolve_path;
use axiom_core::errors::AxiomError;
use axiom_core::query::QueryCatalog;
use axiom_diagnostics::Diagnostic;

/// The output of the synchronization phase.
#[derive(Debug, Default)]
pub struct SyncCheck {
    /// Diagnostics for outputs that are missing or out of sync.
    pub problems: Vec<Diagnostic>,
    /// Outputs rewritten by `--fix`.
    pub fixed: Vec<PathBuf>,
}

/// Recompute each configured output in memory and compare with the file on
/// disk. Outputs that are missing or differ from the recomputed contents are
/// reported as errors with a `--fix` hint.
pub fn check_synchronization(
    config: &AxiomConfig,
    base: &Path,
    catalog: &TableCatalog<'_>,
    query_catalog: &QueryCatalog<'_>,
    registry: Option<&ModelRegistry>,
) -> SyncCheck {
    let mut result = SyncCheck::default();

    for (name, output) in &config.outputs {
        let generated = render_output(output, catalog, query_catalog, registry);
        let output_path = output_path(base, output);

        if !output_path.exists() {
            result.problems.push(
                Diagnostic::error(
                    output_path.clone(),
                    "check.output-missing",
                    format!("generated output `{name}` is missing"),
                )
                .with_help("run `axiom generate`, or `axiom check --fix` to write it"),
            );
            continue;
        }

        match std::fs::read_to_string(&output_path) {
            Ok(existing) if existing != generated => {
                result.problems.push(
                    Diagnostic::error(
                        output_path.clone(),
                        "check.output-outdated",
                        format!("generated output `{name}` is out of date"),
                    )
                    .with_help("run `axiom generate`, or `axiom check --fix` to rewrite it"),
                );
            }
            Ok(_) => {}
            Err(_) => {
                result.problems.push(
                    Diagnostic::error(
                        output_path.clone(),
                        "check.output-unreadable",
                        format!("generated output `{name}` could not be read"),
                    )
                    .with_help("check file permissions on the output path"),
                );
            }
        }
    }

    result
}

/// Rewrite every output that differs from its recomputed contents. Returns the
/// paths written.
pub fn write_fixed_outputs(
    config: &AxiomConfig,
    base: &Path,
    catalog: &TableCatalog<'_>,
    query_catalog: &QueryCatalog<'_>,
    registry: Option<&ModelRegistry>,
) -> Result<Vec<PathBuf>, AxiomError> {
    let mut written = Vec::new();
    for output in config.outputs.values() {
        let generated = render_output(output, catalog, query_catalog, registry);
        let output_path = output_path(base, output);

        let current = std::fs::read_to_string(&output_path).unwrap_or_default();
        if current == generated {
            continue;
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output_path, generated)?;
        written.push(output_path);
    }
    Ok(written)
}

fn render_output(
    output: &OutputConfig,
    catalog: &TableCatalog<'_>,
    query_catalog: &QueryCatalog<'_>,
    registry: Option<&ModelRegistry>,
) -> String {
    match output {
        OutputConfig::TypeScript(_) => {
            let mut code = generate_typescript(catalog, query_catalog);
            if let Some(registry) = registry {
                code.push_str(&generate_typescript_models(registry));
            }
            code
        }
        OutputConfig::Rust(_) => {
            let mut code = generate_rust(catalog, query_catalog);
            if let Some(registry) = registry {
                code.push_str(&generate_rust_models(registry));
            }
            code
        }
    }
}

fn output_path(base: &Path, output: &OutputConfig) -> PathBuf {
    let path = match output {
        OutputConfig::TypeScript(ts) => &ts.path,
        OutputConfig::Rust(rust) => &rust.path,
    };
    resolve_path(base, path)
}
