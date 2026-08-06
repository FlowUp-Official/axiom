//! Hover content, rendered as markdown.

use std::path::Path;

use axiom_analysis::AnalysisDatabase;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use crate::handlers::{byte_offset, lsp_range};

pub fn lsp_hover(
    db: &mut AnalysisDatabase,
    path: &Path,
    position: tower_lsp::lsp_types::Position,
) -> Option<Hover> {
    let offset = byte_offset(db, path, position);
    let info = db.hover(path, offset)?;

    let mut markdown = format!("**{}**\n", info.title);
    if !info.lines.is_empty() {
        markdown.push('\n');
        markdown.push_str(&info.lines.join("  \n"));
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(lsp_range(db, path, info.range)),
    })
}
