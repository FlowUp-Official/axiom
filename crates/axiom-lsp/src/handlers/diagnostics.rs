//! Diagnostics, converted from the shared `axiom-diagnostics` model.
//!
//! The check functions in `axiom-check` are reused verbatim; this module only
//! maps their results (and the byte spans the analysis layer attaches) to LSP
//! ranges.

use std::path::Path;

use axiom_analysis::AnalysisDatabase;
use axiom_diagnostics::Severity;
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range, Url,
};

use crate::handlers::lsp_range;

/// LSP diagnostics for one file. `None` when the file is not tracked.
pub fn lsp_diagnostics(
    db: &mut AnalysisDatabase,
    path: &Path,
) -> Vec<(Url, Vec<Diagnostic>)> {
    let Some(text) = db.file_text(path).map(str::to_string) else {
        return Vec::new();
    };
    let diags = db.file_diagnostics(path);
    let uri = match Url::from_file_path(path) {
        Ok(uri) => uri,
        Err(_) => return Vec::new(),
    };

    let items: Vec<Diagnostic> = diags
        .into_iter()
        .map(|d| {
            let range = d
                .span
                .map(|s| lsp_range(db, path, axiom_analysis::Span::new(s.start, s.end)))
                .unwrap_or_else(|| {
                    // No span: flag the whole line, falling back to the first line.
                    let line = db
                        .position_index(path)
                        .map(|idx| {
                            let first = idx.to_text_position(&text, 0);
                            first.line
                        })
                        .unwrap_or(0);
                    Range::new(Position::new(line, 0), Position::new(line, 0))
                });
            Diagnostic {
                range,
                severity: Some(match d.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                }),
                code: Some(NumberOrString::String(d.code)),
                message: d.message,
                source: Some("axiom".to_string()),
                ..Diagnostic::default()
            }
        })
        .collect();

    vec![(uri, items)]
}
