//! Convert `axiom-analysis` completion results into LSP completion items.

use std::path::Path;

use axiom_analysis::{AnalysisDatabase, CompletionKind};
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

use crate::handlers::byte_offset;

pub fn lsp_completion(
    db: &mut AnalysisDatabase,
    path: &Path,
    position: tower_lsp::lsp_types::Position,
) -> Vec<CompletionItem> {
    let offset = byte_offset(db, path, position);
    db.completion(path, offset)
        .into_iter()
        .map(|item| CompletionItem {
            label: item.label,
            kind: Some(completion_kind(item.kind)),
            detail: Some(item.detail),
            insert_text: Some(item.insert_text),
            ..CompletionItem::default()
        })
        .collect()
}

fn completion_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Field => CompletionItemKind::FIELD,
        CompletionKind::Method => CompletionItemKind::METHOD,
        CompletionKind::Type => CompletionItemKind::CLASS,
        CompletionKind::Table => CompletionItemKind::STRUCT,
        CompletionKind::Model => CompletionItemKind::INTERFACE,
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
    }
}
