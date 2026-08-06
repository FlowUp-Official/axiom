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

pub use ast::{FieldDecl, ModelDecl, Rule, Transform, TypeRef};
pub use codegen::{generate_rust_models, generate_typescript_models};
pub use parser::parse_axm_file;
pub use resolver::{resolve_models, ModelRegistry};
