//! AST-based deterministic formatters for `.axm` models and SQL inputs.
//!
//! Both formatters are canonical: `format(format(x)) == format(x)`. The `.axm`
//! formatter pretty-prints the parsed AST; the SQL formatter re-renders the
//! sqlparser token stream (uppercasing keywords, normalizing whitespace, and
//! stripping trailing whitespace) rather than using regex rewriting.

pub mod axm;
pub mod printer;
pub mod sql;

pub use axm::format_axm;
pub use sql::format_sql;

/// Format `src` based on the file's extension. Returns `None` for file types
/// the toolchain does not format.
pub fn format_source(name: &str, src: &str) -> Option<Result<String, String>> {
    if name.ends_with(".axm") {
        Some(format_axm(src))
    } else if name.ends_with(".sql") {
        Some(Ok(format_sql(src)))
    } else {
        None
    }
}
