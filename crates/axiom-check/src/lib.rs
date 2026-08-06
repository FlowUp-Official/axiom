//! Workspace correctness verification: parsing, reference resolution, and
//! generated-output synchronization.
//!
//! [`check_workspace`] is the entry point. It performs, per configured input:
//!
//! * syntax checking (SQL schemas, query files, `.axm` models),
//! * reference checks (imports, duplicate models, unknown types, regexes),
//! * query↔schema consistency (missing tables/columns, bad return types),
//! * generated-output synchronization (missing / outdated / different files).
//!
//! Results are surfaced as [`axiom_diagnostics::Diagnostic`] values; nothing is
//! printed here, so the CLI owns presentation and exit codes.

pub mod diagnostics;
pub mod synchronization;
pub mod workspace;

use std::path::{Path, PathBuf};

use axiom_core::cache::ToolCache;
use axiom_core::config::AxiomConfig;
use axiom_core::errors::AxiomError;
use axiom_diagnostics::Diagnostic;

pub use diagnostics::{line_of_offset, span_for_line};
pub use synchronization::{write_fixed_outputs, SyncCheck};
pub use workspace::{
    check_models, check_queries, check_schemas, collect_declared_models,
    collect_referenced_models, resolve_inputs, Workspace,
};

/// The outcome of a full `axiom check` run.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub diagnostics: Vec<Diagnostic>,
    /// Outputs rewritten by `--fix`.
    pub fixed: Vec<PathBuf>,
}

/// Run every check phase over a resolved workspace.
///
/// `cache` is the shared content-addressed [`ToolCache`]; when `None` the
/// analysis phases are recomputed from scratch. When `fix` is set, out-of-sync
/// generated outputs are rewritten and reported via [`CheckReport::fixed`].
pub fn check_workspace(
    mut cache: Option<&mut ToolCache>,
    workspace: &Workspace,
    config: &AxiomConfig,
    base: &Path,
    fix: bool,
) -> Result<CheckReport, AxiomError> {
    let (catalog, schema_diags) = check_schemas(&workspace.schema_files);
    let schema_hash = workspace::aggregate_hash(&workspace.schema_files);
    let declared_models = collect_declared_models(&workspace.model_files);
    let (query_catalog, query_diags) = check_queries(
        cache.as_deref_mut(),
        &schema_hash,
        &catalog,
        &declared_models,
        &workspace.query_files,
    );
    let (registry, model_diags) = check_models(cache, &workspace.model_files);

    let mut sync = synchronization::check_synchronization(
        config,
        base,
        &catalog,
        &query_catalog,
        registry.as_ref(),
    );

    if fix && !sync.problems.is_empty() {
        let written = synchronization::write_fixed_outputs(
            config,
            base,
            &catalog,
            &query_catalog,
            registry.as_ref(),
        )?;
        sync.fixed = written;
        sync.problems.clear();
    }

    let mut diagnostics = Vec::new();
    diagnostics.extend(schema_diags);
    diagnostics.extend(query_diags);
    diagnostics.extend(model_diags);
    diagnostics.extend(sync.problems);

    Ok(CheckReport {
        diagnostics,
        fixed: sync.fixed,
    })
}
