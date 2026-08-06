//! Go-to-definition and symbol-under-cursor resolution.

use std::path::Path;

use crate::database::{AnalysisDatabase, Lang, Role};
use crate::symbols::SymbolTable;
use crate::{Definition, SymbolKind, SymbolRef};

impl AnalysisDatabase {
    pub fn definition(&mut self, path: &Path, offset: usize) -> Option<Definition> {
        let sym = self.symbol_at(path, offset)?;
        Some(Definition {
            file: sym.file,
            span: sym.span,
            label: sym.name,
        })
    }

    /// Resolve the symbol (and its declaration site) under a cursor.
    pub fn symbol_at(&mut self, path: &Path, offset: usize) -> Option<SymbolRef> {
        let lang = self.file_lang(path)?;
        let index = self.position_index(path)?.clone();
        let token = index.token_at(offset)?;
        if !token.is_word() {
            return None;
        }
        let word = token.ident_value();
        let symbols = SymbolTable::clone(self.symbol_table());

        match lang {
            Lang::Sql => {
                // Schema file: table and column declarations.
                if self.file_role(path) == Role::Schema {
                    for table in &symbols.tables {
                        if table.span.start == token.start {
                            return Some(SymbolRef {
                                kind: SymbolKind::Table,
                                name: table.name.clone(),
                                file: table.file.clone(),
                                span: table.span,
                                parent: None,
                            });
                        }
                        for column in &table.columns {
                            if column.span.start == token.start {
                                return Some(SymbolRef {
                                    kind: SymbolKind::Column,
                                    name: column.name.clone(),
                                    file: column.file.clone(),
                                    span: column.span,
                                    parent: Some(table.name.clone()),
                                });
                            }
                        }
                    }
                }

                // Query file: table and column uses resolve to their schema
                // declarations.
                let refs = self.query_refs(path).unwrap_or_default();
                if let Some(r) = refs.tables.iter().find(|r| r.span.start == token.start)
                    && let Some(table) = symbols.table(&r.table)
                {
                    return Some(SymbolRef {
                        kind: SymbolKind::Table,
                        name: table.name.clone(),
                        file: table.file.clone(),
                        span: table.span,
                        parent: None,
                    });
                }
                if let Some(r) = refs.columns.iter().find(|r| r.span.start == token.start)
                    && let Some(column) = symbols.column(&r.table, r.column.as_deref()?)
                {
                    return Some(SymbolRef {
                        kind: SymbolKind::Column,
                        name: column.name.clone(),
                        file: column.file.clone(),
                        span: column.span,
                        parent: Some(r.table.clone()),
                    });
                }

                // Return-type references in annotations.
                for r in self.return_type_refs(path) {
                    if r.span.start == token.start {
                        if let Some(model) = symbols.model(&r.name) {
                            return Some(SymbolRef {
                                kind: SymbolKind::Model,
                                name: model.name.clone(),
                                file: model.file.clone(),
                                span: model.span,
                                parent: None,
                            });
                        }
                        if let Some(table) = symbols.table(&r.name) {
                            return Some(SymbolRef {
                                kind: SymbolKind::Table,
                                name: table.name.clone(),
                                file: table.file.clone(),
                                span: table.span,
                                parent: None,
                            });
                        }
                    }
                }

                // Fallback: a bare word that names a table.
                if let Some(table) = symbols.table(word) {
                    return Some(SymbolRef {
                        kind: SymbolKind::Table,
                        name: table.name.clone(),
                        file: table.file.clone(),
                        span: table.span,
                        parent: None,
                    });
                }
            }

            Lang::Axm => {
                // Model declaration.
                if let Some(model) = symbols.models.iter().find(|m| m.span.start == token.start) {
                    return Some(SymbolRef {
                        kind: SymbolKind::Model,
                        name: model.name.clone(),
                        file: model.file.clone(),
                        span: model.span,
                        parent: None,
                    });
                }
                // Field declaration.
                if let Some(model) = symbols
                    .models
                    .iter()
                    .find(|m| m.fields.iter().any(|f| f.span.start == token.start))
                    && let Some(field) = model.fields.iter().find(|f| f.span.start == token.start)
                {
                    return Some(SymbolRef {
                        kind: SymbolKind::Field,
                        name: field.name.clone(),
                        file: field.file.clone(),
                        span: field.span,
                        parent: Some(model.name.clone()),
                    });
                }
                // Model type use.
                let refs = self.axm_refs(path);
                if let Some(r) = refs.iter().find(|r| r.span.start == token.start)
                    && let Some(model) = symbols.model(&r.name)
                {
                    return Some(SymbolRef {
                        kind: SymbolKind::Model,
                        name: model.name.clone(),
                        file: model.file.clone(),
                        span: model.span,
                        parent: None,
                    });
                }
            }
        }
        None
    }
}
