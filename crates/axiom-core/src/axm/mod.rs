//! Zero-dependency domain-model engine for the `.axm` DSL.
//!
//! This module parses `.axm` files into a typed AST, resolves imports and
//! model references across files, and generates strongly typed TypeScript
//! and Rust model code with runtime validation. It intentionally avoids
//! third-party validation crates at runtime; only the standard library
//! (plus `serde` for Rust) is emitted.

pub mod ast;
pub mod codegen;
pub mod parser;
pub mod resolver;

pub use ast::{AxmFile, FieldDecl, ModelDecl, QueryDecl, Rule, Transform, TypeRef};
pub use codegen::{generate_rust_models, generate_typescript_models};
pub use parser::parse_axm_file;
pub use resolver::{ModelRegistry, query_catalog, query_definition, resolve_models, type_ref_name};

/// Compile the query declarations of a single parsed file into a catalog.
pub fn queries_in_file(file: &AxmFile) -> crate::query::QueryCatalog<'static> {
    crate::query::QueryCatalog {
        queries: file
            .queries
            .iter()
            .map(crate::axm::query_definition)
            .collect(),
    }
}
