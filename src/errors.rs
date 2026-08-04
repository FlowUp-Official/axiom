//! Rich, developer-facing diagnostics for Axiom.
//!
//! Every user-facing failure is a typed [`miette::Diagnostic`] with a stable
//! diagnostic `code` and, where useful, an actionable `help` hint. CLI errors
//! therefore render as colored reports with source snippets instead of bare
//! strings, while programmatic callers can match on the enum variants.

use std::path::PathBuf;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

/// The unified error type for all user-facing Axiom failures.
#[derive(Debug, Error, Diagnostic)]
pub enum AxiomError {
    /// The default `axiom.json` was not found in the current directory.
    #[error("Configuration file `axiom.json` not found in current directory.")]
    #[diagnostic(
        code(axiom::config::missing),
        help("Create an `axiom.json` file in the project root, or specify an explicit path using `--config <PATH>`.")
    )]
    MissingConfig,

    /// An explicitly requested config file does not exist.
    #[error("Configuration file `{0}` does not exist.")]
    #[diagnostic(
        code(axiom::config::not_found),
        help("Double-check the path passed to `--config <PATH>`.")
    )]
    ConfigNotFound(PathBuf),

    /// The config file could not be parsed as JSON.
    #[error("Failed to parse configuration JSON: {0}")]
    #[diagnostic(code(axiom::config::invalid_json))]
    ConfigJson(#[from] serde_json::Error),

    /// A `-- @fn` annotation line does not follow the expected signature.
    #[error("Invalid annotation syntax: {message}")]
    #[diagnostic(
        code(axiom::query::annotation_error),
        help("Ensure the function definition follows the format: `-- @fn func_name(param: Type) : ReturnType`")
    )]
    QueryAnnotationError {
        message: String,
        #[source_code]
        src: String,
        #[label("Syntax error near this line")]
        span: SourceSpan,
    },

    /// A push operation failed, e.g. no database URL could be resolved.
    #[error("Database migration error: {details}")]
    #[diagnostic(code(axiom::push::db_error))]
    DatabaseError { details: String },

    /// An explicitly requested env file does not exist.
    #[error("Environment file `{0}` does not exist.")]
    #[diagnostic(
        code(axiom::push::env_file_missing),
        help("Point `--env-file` at a valid dotenv file, or omit it to fall back to `.env` and the process environment.")
    )]
    EnvFileMissing(PathBuf),

    /// A filesystem operation failed.
    #[error("I/O error: {0}")]
    #[diagnostic(code(axiom::io::error))]
    Io(#[from] std::io::Error),

    /// A configured input glob pattern is invalid.
    #[error("Invalid glob pattern `{0}`")]
    #[diagnostic(
        code(axiom::io::glob_pattern),
        help("Check the `inputs.schema` / `inputs.queries` patterns in `axiom.json`.")
    )]
    Glob(#[from] glob::PatternError),

    /// A glob match failed to expand.
    #[error("Glob expansion failed for `{0}`")]
    #[diagnostic(code(axiom::io::glob_match))]
    GlobMatch(#[from] glob::GlobError),

    /// Loading a dotenv file failed.
    #[error("Failed to load environment file: {0}")]
    #[diagnostic(code(axiom::config::env_file))]
    Dotenv(#[from] dotenvy::Error),

    /// The SQL input could not be tokenized.
    #[error("Failed to tokenize SQL: {0}")]
    #[diagnostic(code(axiom::schema::tokenize))]
    Tokenize(#[from] sqlparser::tokenizer::TokenizerError),

    /// The SQL input could not be parsed into statements.
    #[error("Failed to parse SQL: {0}")]
    #[diagnostic(code(axiom::schema::parse))]
    SqlParse(#[from] sqlparser::parser::ParserError),

    /// A database connection or statement failed.
    #[error("Database error: {0}")]
    #[diagnostic(code(axiom::push::connection))]
    Postgres(#[from] tokio_postgres::Error),

    /// Serializing the rkyv cache manifest failed.
    #[error("Failed to serialize cache manifest: {0}")]
    #[diagnostic(code(axiom::cache::archive))]
    Archive(#[from] rkyv::rancor::Error),
}
