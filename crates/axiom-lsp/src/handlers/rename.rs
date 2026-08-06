//! Rename, returned as a workspace edit covering every affected file.

use std::path::Path;

use axiom_analysis::AnalysisDatabase;
use tower_lsp::lsp_types::{TextEdit as LspTextEdit, Url, WorkspaceEdit};

use crate::handlers::{byte_offset, lsp_range};

pub fn lsp_rename(
    db: &mut AnalysisDatabase,
    path: &Path,
    position: tower_lsp::lsp_types::Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let offset = byte_offset(db, path, position);
    let rename = db.rename(path, offset, new_name)?;
    if rename.edits.is_empty() {
        return None;
    }

    let mut changes = std::collections::HashMap::new();
    for (file, edits) in rename.edits {
        let uri = Url::from_file_path(&file).ok()?;
        let edits: Vec<LspTextEdit> = edits
            .into_iter()
            .map(|e| LspTextEdit {
                range: lsp_range(db, &file, e.span),
                new_text: e.new_text,
            })
            .collect();
        changes.insert(uri, edits);
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        ..WorkspaceEdit::default()
    })
}
