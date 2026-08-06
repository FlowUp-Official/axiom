//! Static analysis engine for `.axm` models and SQL inputs.
//!
//! Rules are plain structs implementing [`LintRule`]; a [`LintRunner`] batches
//! them over a workspace. Results are content-addressed in the shared
//! [`axiom_core::cache::ToolCache`] so unchanged files are not re-analysed.

pub mod runner;
pub mod rules;

pub use runner::{
    build_context, lint_sources, hex, word_span, LintContext, LintOptions, LintRule,
    LintRunner, WorkspaceView,
};
pub use rules::axm::{DeadModel, RedundantValidator, UnusedImport};
pub use rules::sql::{MissingWhereClause, SelectStar, UnindexedForeignKey};
