//! Workspace rename across SQL and `.axm` files.
//!
//! Rename reuses the same reference graph as go-to-definition: renaming a
//! symbol updates its declaration plus every resolved use, so `User.email`
//! updates `schema.sql`, `queries/*.sql`, and `models/*.axm` in one pass.

use std::path::{Path, PathBuf};

use crate::database::{AnalysisDatabase, Role};
use crate::{Rename, SymbolKind, TextEdit};

impl AnalysisDatabase {
    pub fn rename(&mut self, path: &Path, offset: usize, new_name: &str) -> Option<Rename> {
        let sym = self.symbol_at(path, offset)?;
        if sym.name.eq_ignore_ascii_case(new_name) {
            return Some(Rename { edits: Vec::new() });
        }

        let mut grouped: Vec<(PathBuf, Vec<TextEdit>)> = Vec::new();
        let mut push = |file: &Path, span| {
            if let Some((_, edits)) = grouped.iter_mut().find(|(f, _)| f == file) {
                edits.push(TextEdit {
                    span,
                    new_text: new_name.to_string(),
                });
            } else {
                grouped.push((
                    file.to_path_buf(),
                    vec![TextEdit {
                        span,
                        new_text: new_name.to_string(),
                    }],
                ));
            }
        };

        match sym.kind {
            SymbolKind::Table => {
                push(&sym.file, sym.span);
                let files: Vec<PathBuf> = self.file_paths().map(Path::to_path_buf).collect();
                for file in files {
                    if self.file_role(&file) == Role::Query {
                        let refs = self.query_refs(&file).unwrap_or_default();
                        for r in refs.tables {
                            if r.table.eq_ignore_ascii_case(&sym.name) {
                                push(&file, r.span);
                            }
                        }
                    }
                    for r in self.return_type_refs(&file) {
                        if r.name.eq_ignore_ascii_case(&sym.name) {
                            push(&file, r.span);
                        }
                    }
                }
            }
            SymbolKind::Column => {
                push(&sym.file, sym.span);
                let parent = sym.parent.clone().unwrap_or_default();
                let files: Vec<PathBuf> = self.file_paths().map(Path::to_path_buf).collect();
                for file in files {
                    if self.file_role(&file) == Role::Query {
                        let refs = self.query_refs(&file).unwrap_or_default();
                        for r in refs.columns {
                            if r.table.eq_ignore_ascii_case(&parent)
                                && r.column.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(&sym.name))
                            {
                                push(&file, r.span);
                            }
                        }
                    }
                }
            }
            SymbolKind::Model => {
                push(&sym.file, sym.span);
                let files: Vec<PathBuf> = self.file_paths().map(Path::to_path_buf).collect();
                for file in files {
                    let refs = self.axm_refs(&file);
                    for r in refs {
                        if r.name.eq_ignore_ascii_case(&sym.name) {
                            push(&file, r.span);
                        }
                    }
                    for r in self.return_type_refs(&file) {
                        if r.name.eq_ignore_ascii_case(&sym.name) {
                            push(&file, r.span);
                        }
                    }
                }
            }
            SymbolKind::Field => {
                // Fields are only referenced at their declaration today.
                push(&sym.file, sym.span);
            }
        }

        Some(Rename {
            edits: grouped,
        })
    }
}
