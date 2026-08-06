//! LSP request handlers. Each module maps an `axiom-analysis` result to the
//! corresponding `lsp_types` value; the transport glue lives in `server.rs`.

pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod formatting;
pub mod hover;
pub mod rename;

use std::path::Path;

use axiom_analysis::{AnalysisDatabase, TextPosition};
use tower_lsp::lsp_types::{Position, Range};

/// Byte offset for an LSP position in `path`'s current buffer.
pub fn byte_offset(db: &AnalysisDatabase, path: &Path, pos: Position) -> usize {
    let text = db.file_text(path).unwrap_or("");
    db.position_index(path)
        .and_then(|idx| {
            idx.from_text_position(text, TextPosition::new(pos.line, pos.character))
        })
        .unwrap_or(0)
}

fn lsp_position(pos: TextPosition) -> Position {
    Position::new(pos.line, pos.character)
}

/// Convert a byte span into an LSP range over `path`'s current buffer.
pub fn lsp_range(db: &AnalysisDatabase, path: &Path, span: axiom_analysis::Span) -> Range {
    let text = db.file_text(path).unwrap_or("");
    let idx = db.position_index(path);
    let start = idx
        .map(|idx| idx.to_text_position(text, span.start))
        .unwrap_or_default();
    let end = idx
        .map(|idx| idx.to_text_position(text, span.end))
        .unwrap_or_default();
    Range::new(lsp_position(start), lsp_position(end))
}
