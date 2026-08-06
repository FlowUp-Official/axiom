//! Text completion for SQL and `.axm` sources.

use std::path::Path;

use crate::database::{AnalysisDatabase, Lang};
use crate::token::TokenKind;
use crate::{CompletionItem, CompletionKind};

/// All `.axm` validator and transform callables.
const AXM_CALLABLES: &[&str] = &[
    "email()",
    "url()",
    "uuid()",
    "min()",
    "max()",
    "minLen()",
    "maxLen()",
    "regex()",
    "alphanumeric()",
    "nonempty()",
    "trim()",
    "lowercase()",
    "uppercase()",
];

impl AnalysisDatabase {
    pub fn completion(&mut self, path: &Path, offset: usize) -> Vec<CompletionItem> {
        match self.file_lang(path) {
            Some(Lang::Sql) => self.sql_completion(path, offset),
            Some(Lang::Axm) => self.axm_completion(path, offset),
            None => Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // SQL
    // ------------------------------------------------------------------

    fn sql_completion(&mut self, path: &Path, offset: usize) -> Vec<CompletionItem> {
        let Some(index) = self.position_index(path).cloned() else {
            return Vec::new();
        };
        let tokens = &index.tokens;

        // `alias.` or `table.` — column completion.
        if let Some(dot) = tokens
            .iter()
            .find(|t| t.kind == TokenKind::Punct && t.text == "." && t.start <= offset && offset <= t.end + 1)
            && let Some(qualifier) = tokens
                .iter()
                .rev()
                .find(|t| t.kind != TokenKind::Comment && t.end <= dot.start)
                .filter(|t| t.is_word())
            && let Some(columns) = self.columns_for_qualifier(path, qualifier.ident_value())
        {
            return columns
                .into_iter()
                .map(|(name, type_name)| CompletionItem {
                    label: name.clone(),
                    detail: type_name,
                    kind: CompletionKind::Field,
                    insert_text: name,
                })
                .collect();
        }

        // Word being typed — compute the prefix from the source slice.
        let word_start = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Comment)
            .find(|t| t.start <= offset && offset <= t.end)
            .filter(|t| t.is_word())
            .map(|t| t.start)
            .unwrap_or(offset);
        let src = self.file_text(path).unwrap_or("");
        let prefix = src[word_start..offset.min(src.len())].to_string();

        // After FROM/JOIN/UPDATE/INTO -> table completion.
        let after_table_kw = tokens
            .iter()
            .rev()
            .find(|t| t.kind != TokenKind::Comment && t.end <= word_start)
            .is_some_and(|t| {
                matches!(
                    t.ident_value().to_ascii_lowercase().as_str(),
                    "from" | "join" | "update" | "into" | "table"
                )
            });
        if after_table_kw {
            return self.table_completions(&prefix);
        }

        // Generic schema completion in a SELECT list: tables plus the columns
        // of the single table in scope, so `SELECT i|` suggests `id`.
        let single_table = {
            let refs = self.query_refs(path).unwrap_or_default();
            refs.tables.first().map(|r| r.table.clone())
        };
        let mut items = self.table_completions(&prefix);
        let symbols = self.symbol_table();
        if let Some(name) = single_table
            && let Some(table) = symbols.table(&name)
        {
            for column in &table.columns {
                if column.name.to_lowercase().starts_with(&prefix.to_lowercase()) {
                    items.push(CompletionItem {
                        label: column.name.clone(),
                        detail: column.type_name.clone(),
                        kind: CompletionKind::Field,
                        insert_text: column.name.clone(),
                    });
                }
            }
        }
        items
    }

    fn columns_for_qualifier(
        &mut self,
        path: &Path,
        qualifier: &str,
    ) -> Option<Vec<(String, String)>> {
        let refs = self.query_refs(path)?;
        let table_name = refs
            .aliases
            .iter()
            .find(|(alias, _)| alias == &qualifier.to_lowercase())
            .map(|(_, table)| table.clone())
            .or_else(|| {
                let symbols = self.symbol_table();
                if symbols.table(qualifier).is_some() {
                    Some(qualifier.to_string())
                } else {
                    None
                }
            })?;
        let table = self.symbol_table().table(&table_name)?;
        Some(
            table
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.type_name.clone()))
                .collect(),
        )
    }

    fn table_completions(&mut self, prefix: &str) -> Vec<CompletionItem> {
        let symbols = self.symbol_table();
        symbols
            .table_names()
            .filter(|n| n.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(|name| CompletionItem {
                label: name.to_string(),
                detail: "table".to_string(),
                kind: CompletionKind::Table,
                insert_text: name.to_string(),
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // AXM
    // ------------------------------------------------------------------

    fn axm_completion(&mut self, path: &Path, offset: usize) -> Vec<CompletionItem> {
        let Some(index) = self.position_index(path).cloned() else {
            return Vec::new();
        };
        let tokens = &index.tokens;

        // `x.` — model field completion and/or validator callables.
        if let Some(dot) = tokens
            .iter()
            .find(|t| t.kind == TokenKind::Punct && t.text == "." && t.start <= offset && offset <= t.end + 1)
        {
            let qualifier = tokens
                .iter()
                .rev()
                .find(|t| t.kind != TokenKind::Comment && t.end <= dot.start)
                .filter(|t| t.is_word())
                .map(|t| t.ident_value().to_string());
            let mut items = Vec::new();
            if let Some(q) = &qualifier
                && let Some(model) = self.symbol_table().model(q)
            {
                items.extend(model.fields.iter().map(|f| CompletionItem {
                    label: f.name.clone(),
                    detail: f.type_name.clone(),
                    kind: CompletionKind::Field,
                    insert_text: f.name.clone(),
                }));
            }
            items.extend(AXM_CALLABLES.iter().map(|c| CompletionItem {
                label: (*c).to_string(),
                detail: "validator".to_string(),
                kind: CompletionKind::Method,
                insert_text: (*c).to_string(),
            }));
            return items;
        }

        let src = self.file_text(path).unwrap_or("");
        let word_start = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Comment)
            .find(|t| t.start <= offset && offset <= t.end)
            .filter(|t| t.is_word())
            .map(|t| t.start)
            .unwrap_or(offset);
        let prefix = src[word_start..offset.min(src.len())].to_string();

        // After `:` — type position: primitives + models.
        if tokens
            .iter()
            .rev()
            .find(|t| t.kind != TokenKind::Comment && t.end <= word_start)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text == ":")
        {
            let mut items: Vec<CompletionItem> = [
                "string", "int", "float", "boolean", "json", "timestamp",
            ]
            .iter()
            .filter(|p| p.starts_with(&prefix.to_lowercase()))
            .map(|p| CompletionItem {
                label: (*p).to_string(),
                detail: "primitive".to_string(),
                kind: CompletionKind::Type,
                insert_text: (*p).to_string(),
            })
            .collect();
            items.extend(self.model_completions(&prefix));
            return items;
        }

        // After `import` — model names.
        if tokens
            .iter()
            .rev()
            .find(|t| t.kind != TokenKind::Comment && t.end <= word_start)
            .is_some_and(|t| t.is_word() && t.ident_value() == "import")
        {
            return self.model_completions(&prefix);
        }

        self.model_completions(&prefix)
    }

    fn model_completions(&mut self, prefix: &str) -> Vec<CompletionItem> {
        let symbols = self.symbol_table();
        symbols
            .model_names()
            .filter(|n| n.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(|name| CompletionItem {
                label: name.to_string(),
                detail: "model".to_string(),
                kind: CompletionKind::Model,
                insert_text: name.to_string(),
            })
            .collect()
    }
}
