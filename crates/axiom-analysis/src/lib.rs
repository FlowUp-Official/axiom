//! Axiom analysis engine: incremental, editor-friendly semantic analysis
//! over SQL and `.axm` files, built on the shared compiler infrastructure.
//!
//! This crate owns workspace state, parsed-AST caching, symbol and reference
//! graphs, and the semantic queries (diagnostics, completion, hover, go-to
//! definition, rename) that power `axiom-lsp`. It is editor-agnostic: positions
//! are byte offsets and line/column pairs, never LSP types.

pub mod completion;
pub mod database;
pub mod definition;
pub mod diagnostics;
pub mod hover;
pub mod position;
pub mod references;
pub mod rename;
pub mod symbols;
pub mod token;

pub use database::{AnalysisDatabase, ChangeKind, FileId, Lang, Role};
pub use position::{LineIndex, TextPosition};
pub use symbols::Span;

use std::path::PathBuf;

/// Editor-neutral completion item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: String,
    pub kind: CompletionKind,
    pub insert_text: String,
}

/// Editor-neutral completion item kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Field,
    Method,
    Type,
    Table,
    Model,
    Keyword,
}

/// Hover content for one position.
#[derive(Debug, Clone)]
pub struct HoverInfo {
    /// First, prominent line (e.g. `users.email`).
    pub title: String,
    /// Remaining lines (plain text).
    pub lines: Vec<String>,
    /// The source range the hover applies to.
    pub range: Span,
}

/// A go-to-definition result.
#[derive(Debug, Clone)]
pub struct Definition {
    pub file: PathBuf,
    pub span: Span,
    pub label: String,
}

/// A single text edit within one file.
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub span: Span,
    pub new_text: String,
}

/// A rename across the workspace: edits grouped by file.
#[derive(Debug, Clone)]
pub struct Rename {
    pub edits: Vec<(PathBuf, Vec<TextEdit>)>,
}

/// The kind of symbol under a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Table,
    Column,
    Model,
    Field,
}

/// A resolved symbol under a cursor.
#[derive(Debug, Clone)]
pub struct SymbolRef {
    pub kind: SymbolKind,
    pub name: String,
    /// The file/span where the symbol is declared.
    pub file: PathBuf,
    pub span: Span,
    /// Parent symbol name (table for columns, model for fields).
    pub parent: Option<String>,
}
