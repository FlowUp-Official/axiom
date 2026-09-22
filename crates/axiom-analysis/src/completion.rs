//! Text completion for `.axm` sources.

use std::path::Path;

use axiom_core::axm::ast::{ModelOverride, Rule, Transform, TypeRef};

use crate::database::{AnalysisDatabase, Lang};
use crate::token::TokenKind;
use crate::{CompletionItem, CompletionKind};

impl AnalysisDatabase {
    pub fn completion(&mut self, path: &Path, offset: usize) -> Vec<CompletionItem> {
        match self.file_lang(path) {
            // Standalone `.sql` schema/query files surface no completions: the
            // schema is authored, and query sources live inside `.axm` files.
            Some(Lang::Sql) => Vec::new(),
            Some(Lang::Axm) => self.axm_completion(path, offset),
            None => Vec::new(),
        }
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
        if let Some(dot) = tokens.iter().find(|t| {
            t.kind == TokenKind::Punct && t.text == "." && t.start <= offset && offset <= t.end + 1
        }) {
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
            for call in Rule::CALLS {
                items.push(CompletionItem {
                    label: format!("{call}()"),
                    detail: "validator".to_string(),
                    kind: CompletionKind::Method,
                    insert_text: format!("{call}()"),
                });
            }
            for call in Transform::CALLS {
                items.push(CompletionItem {
                    label: format!("{call}()"),
                    detail: "transform".to_string(),
                    kind: CompletionKind::Method,
                    insert_text: format!("{call}()"),
                });
            }
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
            let mut items: Vec<CompletionItem> = TypeRef::PRIMITIVES
                .iter()
                .filter(|p| p.to_lowercase().starts_with(&prefix.to_lowercase()))
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

        // After `@` — decorators. Offer the same decorators the parser
        // accepts, so completion can never insert an invalid spelling.
        if tokens
            .iter()
            .rev()
            .find(|t| t.kind != TokenKind::Comment && t.end <= word_start)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text == "@")
        {
            return ModelOverride::ALL
                .iter()
                .filter(|o| o.name().to_lowercase().starts_with(&prefix.to_lowercase()))
                .map(|o| CompletionItem {
                    label: o.name().to_string(),
                    detail: decorator_detail(o.name()).to_string(),
                    kind: CompletionKind::Keyword,
                    insert_text: o.name().to_string(),
                })
                .collect();
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

/// Short description for a decorator, one per `ModelOverride` variant.
fn decorator_detail(name: &str) -> &'static str {
    match name {
        "target" => "restrict codegen to the listed targets",
        "no_codegen" => "never emit the standalone validation API",
        "no_types_codegen" => "never emit the generated public model type",
        "no_validation_codegen" => "never emit public validation generation",
        "parse" => "keep the standalone validation API",
        "safeParse" => "set the safeParse error aggregation mode",
        _ => unreachable!("unknown decorator {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_from(src: &str) -> AnalysisDatabase {
        let mut db = AnalysisDatabase::new();
        db.open(Path::new("models/a.axm"), src.to_string());
        db
    }

    fn labels(items: Vec<CompletionItem>) -> Vec<String> {
        items.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn suggests_canonical_primitives_at_type_position() {
        let src = "model User { email: X }";
        let offset = src.find('X').unwrap();
        let labels = labels(db_from(src).completion(Path::new("models/a.axm"), offset));
        for p in TypeRef::PRIMITIVES {
            assert!(
                labels.iter().any(|l| l == p),
                "missing canonical primitive {p}"
            );
        }
        for stale in ["string", "int", "timestamp"] {
            assert!(
                !labels.iter().any(|l| l == stale),
                "stale primitive {stale} suggested"
            );
        }
    }

    #[test]
    fn suggests_canonical_validators_at_dot_position() {
        let src = "model User { email: String. }";
        let offset = src.find('.').unwrap() + 1;
        let labels = labels(db_from(src).completion(Path::new("models/a.axm"), offset));
        for call in Rule::CALLS {
            assert!(
                labels.iter().any(|l| l == &format!("{call}()")),
                "missing canonical validator {call}()"
            );
        }
        for stale in ["minLen()", "maxLen()", "min_len()", "max_len()"] {
            assert!(
                !labels.iter().any(|l| l == stale),
                "stale validator {stale} suggested"
            );
        }
    }

    #[test]
    fn suggests_model_decorators_after_at() {
        let src = "@\nmodel User { id: UUID }";
        let offset = src.find('@').unwrap() + 1;
        let labels = labels(db_from(src).completion(Path::new("models/a.axm"), offset));
        let expect = [
            "target",
            "no_codegen",
            "no_types_codegen",
            "no_validation_codegen",
            "parse",
            "safeParse",
        ];
        for name in expect {
            let matches = labels.iter().filter(|l| l == &name).count();
            assert_eq!(
                matches, 1,
                "expected exactly one suggestion for {name}, got {labels:?}"
            );
        }
        assert_eq!(
            labels.len(),
            expect.len(),
            "unexpected extra suggestions: {labels:?}"
        );
    }

    #[test]
    fn filters_model_decorators_by_typed_prefix() {
        let src = "@no_\nmodel User { id: UUID }";
        let offset = src.find('@').unwrap() + 4;
        let labels = labels(db_from(src).completion(Path::new("models/a.axm"), offset));
        assert_eq!(
            labels,
            vec!["no_codegen", "no_types_codegen", "no_validation_codegen"]
        );
    }

    #[test]
    fn annotates_model_decorators_as_keywords() {
        let src = "@\nmodel User { id: UUID }";
        let offset = src.find('@').unwrap() + 1;
        let items = db_from(src).completion(Path::new("models/a.axm"), offset);
        assert!(!items.is_empty());
        assert!(
            items.iter().all(|i| { i.kind == CompletionKind::Keyword }),
            "expected all decorators to be keyword completions, got {items:?}"
        );
    }

    #[test]
    fn does_not_suggest_decorators_away_from_at() {
        let src = "model User { email: X }";
        let offset = src.find('X').unwrap();
        let labels = labels(db_from(src).completion(Path::new("models/a.axm"), offset));
        for stale in ["no_codegen", "no_types_codegen", "no_validation_codegen", "safeParse"] {
            assert!(
                !labels.iter().any(|l| l == stale),
                "decorator {stale} suggested away from `@`"
            );
        }
    }
}
