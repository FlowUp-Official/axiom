//! Symbol tables built from the shared compiler structures.
//!
//! Symbols associate the owned [`TableCatalog`], parsed [`AxmFile`]s, and the
//! byte-position layer with concrete spans so the editor can navigate, rename,
//! and complete against them. No parsing happens here — only attribution of
//! positions to already-resolved names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axiom_core::axm::ast::{AxmFile, FieldDecl, ModelDecl};
use axiom_core::catalog::{TableCatalog, TableSchema};

use crate::position::TextPosition;
use crate::token::{PositionIndex, TokenKind};

/// A byte range within a file's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Merge two spans into the range that covers both.
    pub fn cover(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }

    pub fn to_positions(&self, index: &PositionIndex, text: &str) -> (TextPosition, TextPosition) {
        (
            index.lines.position(text, self.start),
            index.lines.position(text, self.end),
        )
    }
}

impl From<axiom_diagnostics::Span> for Span {
    fn from(s: axiom_diagnostics::Span) -> Self {
        Span::new(s.start, s.end)
    }
}

#[derive(Debug, Clone)]
pub struct TableSym {
    pub name: String,
    pub file: PathBuf,
    /// Span of the table name in its schema file.
    pub span: Span,
    pub columns: Vec<ColumnSym>,
}

#[derive(Debug, Clone)]
pub struct ColumnSym {
    pub name: String,
    pub file: PathBuf,
    /// Span of the column name in its schema file.
    pub span: Span,
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone)]
pub struct ModelSym {
    pub name: String,
    pub file: PathBuf,
    /// Span of the model name in its `.axm` file.
    pub span: Span,
    pub exported: bool,
    pub fields: Vec<FieldSym>,
}

#[derive(Debug, Clone)]
pub struct FieldSym {
    pub name: String,
    pub file: PathBuf,
    /// Span of the field name in its `.axm` file.
    pub span: Span,
    pub type_name: String,
    pub optional: bool,
}

/// The full workspace symbol table. Lookup is case-insensitive, matching
/// PostgreSQL's unquoted-identifier folding.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    pub tables: Vec<TableSym>,
    pub models: Vec<ModelSym>,
    pub(crate) table_index: HashMap<String, usize>,
    pub(crate) model_index: HashMap<String, usize>,
}

impl SymbolTable {
    pub fn table(&self, name: &str) -> Option<&TableSym> {
        self.table_index
            .get(&name.to_lowercase())
            .and_then(|&i| self.tables.get(i))
    }

    pub fn column(&self, table: &str, column: &str) -> Option<&ColumnSym> {
        self.table(table)?.columns.iter().find(|c| c.name.eq_ignore_ascii_case(column))
    }

    pub fn model(&self, name: &str) -> Option<&ModelSym> {
        self.model_index
            .get(&name.to_lowercase())
            .and_then(|&i| self.models.get(i))
    }

    pub fn table_names(&self) -> impl Iterator<Item = &str> {
        self.tables.iter().map(|t| t.name.as_str())
    }

    pub fn model_names(&self) -> impl Iterator<Item = &str> {
        self.models.iter().map(|m| m.name.as_str())
    }

    /// All symbols whose name starts with `prefix` (case-insensitive),
    /// preferring exact case matches first.
    pub fn starts_with(&self, prefix: &str) -> Vec<&str> {
        let lower = prefix.to_lowercase();
        let mut names: Vec<&str> = self
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.models.iter().map(|m| m.name.as_str()))
            .filter(|n| n.to_lowercase().starts_with(&lower))
            .collect();
        names.sort_by_key(|n| (n.to_lowercase() != lower, n.to_lowercase()));
        names
    }

    pub(crate) fn add_table(&mut self, table: TableSym) {
        if !self.table_index.contains_key(&table.name.to_lowercase()) {
            self.table_index.insert(table.name.to_lowercase(), self.tables.len());
            self.tables.push(table);
        }
    }

    pub(crate) fn add_model(&mut self, model: ModelSym) {
        if !self.model_index.contains_key(&model.name.to_lowercase()) {
            self.model_index.insert(model.name.to_lowercase(), self.models.len());
            self.models.push(model);
        }
    }
}

const AXM_PRIMITIVES: &[&str] = &["string", "int", "float", "boolean", "json", "timestamp"];

pub fn is_axm_primitive(name: &str) -> bool {
    AXM_PRIMITIVES
        .iter()
        .any(|p| p.eq_ignore_ascii_case(name))
}

/// Build the SQL side of the symbol table from one schema file's owned
/// catalog slice and its position index.
pub fn build_table_symbols(
    file: &Path,
    catalog: &TableCatalog,
    index: &PositionIndex,
) -> Vec<TableSym> {
    catalog
        .tables
        .iter()
        .map(|table| table_symbol(file, table, index))
        .collect()
}

fn table_symbol(file: &Path, table: &TableSchema, index: &PositionIndex) -> TableSym {
    let name_span = index
        .find_word_any(&table.name)
        .map(|t| Span::new(t.start, t.end))
        .unwrap_or_else(|| Span::new(0, 0));

    // The table body starts at the first `(` after the name and ends at its
    // matching `)`.
    let (body_start, body_end) = table_body_range(index, name_span.end);

    let columns = table
        .columns
        .iter()
        .map(|column| {
            let span = index
                .find_word(&column.name, body_start)
                .filter(|t| t.end <= body_end)
                .map(|t| Span::new(t.start, t.end))
                .unwrap_or_else(|| Span::new(name_span.end, name_span.end));
            ColumnSym {
                name: column.name.to_string(),
                file: file.to_path_buf(),
                span,
                type_name: column.data_type.to_string(),
                nullable: column.nullable,
                primary_key: column.primary_key,
            }
        })
        .collect();

    TableSym {
        name: table.name.to_string(),
        file: file.to_path_buf(),
        span: name_span,
        columns,
    }
}

/// Byte range of a table's column list, derived from token structure.
fn table_body_range(index: &PositionIndex, name_end: usize) -> (usize, usize) {
    let mut depth: i32 = 0;
    let mut start = name_end;
    let mut end = name_end;
    for token in &index.tokens {
        if token.start < name_end {
            continue;
        }
        match token.text.as_str() {
            "(" => {
                if depth == 0 {
                    start = token.end;
                }
                depth += 1;
            }
            ")" => {
                depth -= 1;
                if depth == 0 {
                    end = token.start;
                    break;
                }
            }
            _ => {}
        }
    }
    (start, end)
}

/// Build the `.axm` side of the symbol table from one parsed model file and
/// its position index.
pub fn build_model_symbols(
    file: &Path,
    axm: &AxmFile,
    index: &PositionIndex,
) -> Vec<ModelSym> {
    axm.models
        .iter()
        .map(|model| model_symbol(file, model, index))
        .collect()
}

fn model_symbol(file: &Path, model: &ModelDecl, index: &PositionIndex) -> ModelSym {
    let span = index
        .find_word_any(&model.name)
        .map(|t| Span::new(t.start, t.end))
        .unwrap_or_else(|| Span::new(0, 0));

    let fields = model
        .fields
        .iter()
        .map(|field| field_symbol(file, field, index))
        .collect();

    ModelSym {
        name: model.name.clone(),
        file: file.to_path_buf(),
        span,
        exported: model.exported,
        fields,
    }
}

fn field_symbol(file: &Path, field: &FieldDecl, index: &PositionIndex) -> FieldSym {
    let span = index
        .find_word_any(&field.name)
        .map(|t| Span::new(t.start, t.end))
        .unwrap_or_else(|| Span::new(0, 0));
    FieldSym {
        name: field.name.clone(),
        file: file.to_path_buf(),
        span,
        type_name: type_name_of(field, index, span),
        optional: field.optional,
    }
}

/// The display type of a field: its raw [`TypeRef`] spelled from the source,
/// falling back to a normalized name.
fn type_name_of(field: &FieldDecl, index: &PositionIndex, field_span: Span) -> String {
    if let Some(colon) = index
        .tokens
        .iter()
        .find(|t| t.kind == TokenKind::Punct && t.text == ":" && t.start >= field_span.end)
        .map(|t| t.end)
        && let Some(word) = index.tokens.iter().find(|t| {
            t.kind == TokenKind::Word && t.start >= colon && !t.text.contains('.')
        })
    {
        return word.text.clone();
    }
    format!("{:?}", field.ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiom_core::catalog::ColumnSchema;
    use axiom_core::axm::parser::parse_axm_file;

    fn catalog_with(columns: &[(&str, &str, bool)]) -> TableCatalog<'static> {
        let mut table = TableSchema {
            name: "users".into(),
            columns: Vec::new(),
        };
        for (name, ty, nullable) in columns {
            table.columns.push(ColumnSchema {
                name: (*name).to_string().into(),
                data_type: (*ty).to_string().into(),
                nullable: *nullable,
                primary_key: false,
                rules: Vec::new(),
            });
        }
        TableCatalog {
            tables: vec![table],
        }
    }

    #[test]
    fn builds_table_symbols_with_spans() {
        let src = "CREATE TABLE users (\n  id BIGSERIAL PRIMARY KEY,\n  email VARCHAR(255) NOT NULL\n);";
        let index = PositionIndex::new_sql(src);
        let catalog = catalog_with(&[("id", "BIGSERIAL", false), ("email", "VARCHAR(255)", false)]);
        let tables = build_table_symbols(Path::new("schema.sql"), &catalog, &index);
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(&src[table.span.start..table.span.end], "users");
        assert_eq!(table.columns.len(), 2);
        assert_eq!(&src[table.columns[0].span.start..table.columns[0].span.end], "id");
        assert_eq!(&src[table.columns[1].span.start..table.columns[1].span.end], "email");
        assert_eq!(table.columns[1].type_name, "VARCHAR(255)");
    }

    #[test]
    fn builds_model_symbols_with_spans() {
        let src = "export model User {\n  email: string.email().trim()\n  address: Address\n}";
        let axm = parse_axm_file(src).unwrap();
        let index = PositionIndex::new_axm(src);
        let models = build_model_symbols(Path::new("models/user.axm"), &axm, &index);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(&src[model.span.start..model.span.end], "User");
        assert!(model.exported);
        assert_eq!(model.fields.len(), 2);
        assert_eq!(&src[model.fields[0].span.start..model.fields[0].span.end], "email");
        assert_eq!(model.fields[0].type_name, "string");
        assert_eq!(model.fields[1].type_name, "Address");
    }

    #[test]
    fn symbol_table_lookup_is_case_insensitive() {
        let src = "CREATE TABLE users (id serial)";
        let index = PositionIndex::new_sql(src);
        let tables = build_table_symbols(Path::new("schema.sql"), &catalog_with(&[("id", "serial", false)]), &index);
        let mut st = SymbolTable::default();
        for t in tables {
            st.add_table(t);
        }
        assert!(st.table("USERS").is_some());
        assert!(st.column("Users", "ID").is_some());
    }
}
