//! Format a document using the shared `axiom-format` engine.

use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// Full-document format edit for a SQL or `.axm` buffer. The edit range is
/// computed from the current text so it always covers the whole document.
pub fn lsp_formatting(path: &Path, text: &str) -> Vec<TextEdit> {
    let Some(result) = axiom_format::format_source(&path.to_string_lossy(), text) else {
        return Vec::new();
    };
    let Ok(formatted) = result else {
        return Vec::new();
    };
    if formatted == text {
        return Vec::new();
    }
    let line_count = text.lines().count().max(1) as u32;
    let last_line_len = text.lines().last().map(str::len).unwrap_or(0) as u32;
    vec![TextEdit {
        range: Range::new(Position::new(0, 0), Position::new(line_count, last_line_len)),
        new_text: formatted,
    }]
}
