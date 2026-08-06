//! Go-to-definition, returning a [`LocationLink`] with an origin range.

use std::path::Path;

use axiom_analysis::AnalysisDatabase;
use tower_lsp::lsp_types::{GotoDefinitionResponse, LocationLink, Url};

use crate::handlers::{byte_offset, lsp_range};

pub fn lsp_definition(
    db: &mut AnalysisDatabase,
    path: &Path,
    position: tower_lsp::lsp_types::Position,
) -> Option<GotoDefinitionResponse> {
    let offset = byte_offset(db, path, position);
    let definition = db.definition(path, offset)?;
    let target_uri = Url::from_file_path(&definition.file).ok()?;
    let target_range = lsp_range(db, &definition.file, definition.span);
    let origin = byte_offset(db, path, position);

    let link = LocationLink {
        target_uri,
        target_range,
        target_selection_range: target_range,
        origin_selection_range: Some(lsp_range(db, path, axiom_analysis::Span::new(origin, origin))),
    };
    Some(GotoDefinitionResponse::Link(vec![link]))
}
