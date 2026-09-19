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

pub use ast::{
    AxmFile, FieldDecl, ModelDecl, ModelOverride, QueryDecl, Rule, SafeParseMode, Target,
    Transform, TypeRef,
};
pub use codegen::{
    generate_rust_models, generate_rust_models_with_options, generate_typescript_models,
    generate_typescript_models_with_options, ValidationOptions,
};
pub use parser::parse_axm_file;
pub use resolver::{ModelRegistry, query_catalog, query_definition, resolve_models, type_ref_name};

/// Whether `name` matches Axiom's bare-identifier grammar: an ASCII letter or
/// `_`, followed by zero or more ASCII alphanumerics or `_`.
///
/// This is exactly the grammar the `.axm` parser accepts for a bare `ident`.
/// Quoted names (e.g. `"first-name"`) intentionally escape this grammar and
/// are therefore not identifiers.
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

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
