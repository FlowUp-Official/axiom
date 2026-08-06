//! Hover content for SQL and `.axm` positions.

use std::path::Path;

use axiom_core::axm::ast::Rule;

use crate::database::{AnalysisDatabase, Lang, Role};
use crate::symbols::Span;
use crate::HoverInfo;

impl AnalysisDatabase {
    pub fn hover(&mut self, path: &Path, offset: usize) -> Option<HoverInfo> {
        match self.file_lang(path) {
            Some(Lang::Sql) => self.sql_hover(path, offset),
            Some(Lang::Axm) => self.axm_hover(path, offset),
            None => None,
        }
    }

    // ------------------------------------------------------------------
    // SQL
    // ------------------------------------------------------------------

    fn sql_hover(&mut self, path: &Path, offset: usize) -> Option<HoverInfo> {
        let index = self.position_index(path)?.clone();
        let token = index.token_at(offset)?;
        if !token.is_word() {
            return None;
        }
        let word = token.ident_value();
        let span = Span::new(token.start, token.end);

        let symbols = crate::symbols::SymbolTable::clone(self.symbol_table());

        // Query file: column references via the reference graph.
        if self.file_role(path) == Role::Query {
            let refs = self.query_refs(path)?;
            if let Some(r) = refs.columns.iter().find(|r| r.span.start == token.start)
                && let Some(column) = symbols.column(&r.table, word)
            {
                return Some(column_hover(word, &r.table, column, span));
            }
            if let Some(r) = refs.tables.iter().find(|r| r.span.start == token.start)
                && let Some(table) = symbols.table(&r.table)
            {
                return Some(table_hover(table, span));
            }
        }

        // Schema file: table and column definitions directly.
        if self.file_role(path) == Role::Schema {
            for table in &symbols.tables {
                if table.span.start == token.start {
                    return Some(table_hover(table, span));
                }
                for column in &table.columns {
                    if column.span.start == token.start {
                        return Some(column_hover(&column.name, &table.name, column, span));
                    }
                }
            }
        }

        None
    }

    // ------------------------------------------------------------------
    // AXM
    // ------------------------------------------------------------------

    fn axm_hover(&mut self, path: &Path, offset: usize) -> Option<HoverInfo> {
        let index = self.position_index(path)?.clone();
        let token = index.token_at(offset)?;
        if !token.is_word() {
            return None;
        }
        let span = Span::new(token.start, token.end);
        let symbols = crate::symbols::SymbolTable::clone(self.symbol_table());

        // Model name (declaration or use).
        if let Some(model) = symbols.models.iter().find(|m| m.span.start == token.start) {
            return Some(model_hover(model, span));
        }
        let refs = self.axm_refs(path);
        if let Some(r) = refs.iter().find(|r| r.span.start == token.start)
            && let Some(model) = symbols.model(&r.name)
        {
            return Some(model_hover(model, span));
        }

        // Field name: pull rules from the parsed AST.
        let axm = self.axm_file(path)?;
        // Locate the field whose name span matches the token.
        let fields: Vec<(&str, String, Vec<String>)> = axm
            .models
            .iter()
            .flat_map(|m| m.fields.iter())
            .filter_map(|f| {
                let field_index = index.find_word_any(&f.name)?;
                (field_index.start == token.start).then(|| {
                    let rules: Vec<String> = f
                        .validations
                        .iter()
                        .map(rule_label)
                        .chain(f.transformations.iter().map(transform_label))
                        .collect();
                    (f.name.as_str(), format!("{:?}", f.ty), rules)
                })
            })
            .collect();
        if let Some((name, ty, rules)) = fields.first() {
            let mut lines = vec![format!("Type: {ty}")];
            if !rules.is_empty() {
                lines.push("Rules:".to_string());
                for rule in rules {
                    lines.push(format!("  ✓ {rule}"));
                }
            }
            lines.push(format!("Generated: {ty}"));
            return Some(HoverInfo {
                title: format!("Field: {name}"),
                lines,
                range: span,
            });
        }

        None
    }
}

fn column_hover(
    name: &str,
    table: &str,
    column: &crate::symbols::ColumnSym,
    span: Span,
) -> HoverInfo {
    HoverInfo {
        title: format!("{table}.{name}"),
        lines: vec![
            format!("Type: {}", column.type_name),
            format!("Nullable: {}", column.nullable),
            format!("Primary key: {}", column.primary_key),
        ],
        range: span,
    }
}

fn table_hover(table: &crate::symbols::TableSym, span: Span) -> HoverInfo {
    let mut lines = vec![format!("{} column(s)", table.columns.len())];
    for column in &table.columns {
        let mut parts = vec![
            column.name.clone(),
            column.type_name.clone(),
            if column.nullable { "NULL" } else { "NOT NULL" }.to_string(),
        ];
        if column.primary_key {
            parts.push("PK".to_string());
        }
        lines.push(parts.join(" "));
    }
    HoverInfo {
        title: format!("Table: {}", table.name),
        lines,
        range: span,
    }
}

fn model_hover(model: &crate::symbols::ModelSym, span: Span) -> HoverInfo {
    let mut lines = vec![
        format!("{} field(s)", model.fields.len()),
        format!("exported: {}", model.exported),
    ];
    for field in &model.fields {
        lines.push(format!("  {}: {}", field.name, field.type_name));
    }
    HoverInfo {
        title: format!("Model: {}", model.name),
        lines,
        range: span,
    }
}

fn rule_label(rule: &Rule) -> String {
    match rule {
        Rule::Email => "email validation".to_string(),
        Rule::Url => "url validation".to_string(),
        Rule::Uuid => "uuid validation".to_string(),
        Rule::Alphanumeric => "alphanumeric validation".to_string(),
        Rule::NonEmpty => "nonempty validation".to_string(),
        Rule::Min(n) => format!("min {n} validation"),
        Rule::Max(n) => format!("max {n} validation"),
        Rule::MinLen(n) => format!("min length {n} validation"),
        Rule::MaxLen(n) => format!("max length {n} validation"),
        Rule::Regex(p) => format!("regex validation ({p})"),
    }
}

fn transform_label(transform: &axiom_core::axm::ast::Transform) -> String {
    match transform {
        axiom_core::axm::ast::Transform::Trim => "trim transform".to_string(),
        axiom_core::axm::ast::Transform::Lowercase => "lowercase transform".to_string(),
        axiom_core::axm::ast::Transform::Uppercase => "uppercase transform".to_string(),
    }
}
