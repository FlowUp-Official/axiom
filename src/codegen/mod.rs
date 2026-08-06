//! Code generation for TypeScript and Rust outputs.

pub mod rust;
pub mod typescript;
pub(crate) mod util;

pub use rust::generate_rust;
pub use typescript::generate_typescript;
