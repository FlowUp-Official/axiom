//! Helpers for turning parse/link failures into [`Diagnostic`]s.

use axiom_diagnostics::{Diagnostic, Span};
use std::path::Path;

/// A `Diagnostic` for a file that failed to parse, without a precise span.
pub fn parse_error(file: impl Into<std::path::PathBuf>, code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(file, code, message)
}

/// Byte span of the whole line `line_no` (1-based) in `source`. Falls back to
/// a zero-width span at the start of the file.
pub fn span_for_line(source: &str, line_no: usize) -> Span {
    if line_no <= 1 {
        return Span::new(0, 0);
    }
    let mut seen = 1;
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line_no {
                return Span::new(i + 1, i + 1);
            }
        }
    }
    Span::new(source.len(), source.len())
}

/// A zero-width span at the start of the line containing byte `offset`.
pub fn line_of_offset(source: &str, offset: usize) -> Span {
    let offset = offset.min(source.len());
    let start = source[..offset]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    Span::new(start, start)
}

/// Find a byte span in `source` for `needle` appearing at or after `from`.
pub fn find_span(source: &str, needle: &str, from: usize) -> Option<Span> {
    let rel = source[from.min(source.len())..].find(needle)?;
    let start = from + rel;
    Some(Span::new(start, start + needle.len()))
}

/// A `Path` converted for use in diagnostics.
pub fn as_path(path: &Path) -> &Path {
    path
}
