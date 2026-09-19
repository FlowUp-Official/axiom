//! Static analysis engine for `.axm` models and SQL inputs.
//!
//! Rules are plain structs implementing [`LintRule`]; a [`LintRunner`] batches
//! them over a workspace. Results are content-addressed in the shared
//! [`axiom_core::cache::ToolCache`] so unchanged files are not re-analysed.

pub mod rules;
pub mod runner;

pub use rules::axm::{
    DeadModel, NamingConvention, RedundantValidator, UnusedImport, UnusedQueryParam,
    UnusedTypeAlias, UnsatisfiableValidator,
};
pub use rules::sql::{
    MissingPrimaryKey, MissingWhereClause, SelectStar, UnindexedForeignKey,
};
pub use runner::{
    LintContext, LintOptions, LintRule, LintRunner, WorkspaceView, build_contexts, hex,
    lint_sources, word_span,
};
