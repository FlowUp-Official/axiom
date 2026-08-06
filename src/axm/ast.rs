//! The `.axm` domain-model AST.
//!
//! A `.axm` file describes application models: fields, their types, validation
//! rules, value transformations, and defaults. The AST is language-independent
//! and strongly typed: rules and transformations are dedicated enum variants
//! rather than generic string lists, so code generators can rely on structure
//! instead of parsing names.

/// A parsed `.axm` source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxmFile {
    pub imports: Vec<ImportStmt>,
    pub models: Vec<ModelDecl>,
}

impl AxmFile {
    pub fn model_by_name(&self, name: &str) -> Option<&ModelDecl> {
        self.models.iter().find(|m| m.name == name)
    }
}

/// A single `import { A, B } from "source"` statement.
///
/// The `names` are the models brought into scope; `source` is a relative
/// `.axm` file reference (without the extension), e.g. `"address"`. The
/// resolver is responsible for turning it into a concrete file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportStmt {
    pub names: Vec<String>,
    pub source: String,
}

/// A top-level `model` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDecl {
    /// Whether the model is `export`ed (public API) or file-private.
    pub exported: bool,
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

/// A single field declaration within a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeRef,
    /// `age?` — the field may be absent.
    pub optional: bool,
    /// `country = "US"` — applied only when the field is missing, never
    /// replacing an explicit `null`.
    pub default: Option<Literal>,
    /// Value transformations, in declared order, applied before validation.
    pub transformations: Vec<Transform>,
    /// Validation rules, in declared order.
    pub validations: Vec<Rule>,
}

/// A type reference. Arrays are modelled recursively as
/// [`TypeRef::Array`] wrapping the element type, so constructs such as
/// `string[]`, `Address[]`, and (future) `(User | Admin)[]` share one shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    String,
    Int,
    Float,
    Boolean,
    Json,
    Timestamp,
    /// A reference to a model, e.g. `Address`.
    Named(String),
    /// A homogenous array of `inner`, e.g. `Address[]`.
    Array(Box<TypeRef>),
}

/// A literal value used as a field default.
///
/// Equality is manual because `Float` wraps `f64`, which cannot be `Eq`.
#[derive(Debug, Clone)]
pub enum Literal {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl PartialEq for Literal {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Literal::String(a), Literal::String(b)) => a == b,
            (Literal::Int(a), Literal::Int(b)) => a == b,
            (Literal::Float(a), Literal::Float(b)) => a.to_bits() == b.to_bits(),
            (Literal::Bool(a), Literal::Bool(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Literal {}

impl Literal {
    pub fn rust_type(&self) -> &'static str {
        match self {
            Literal::String(_) => "String",
            Literal::Int(_) => "i64",
            Literal::Float(_) => "f64",
            Literal::Bool(_) => "bool",
        }
    }
}

/// A value transformation applied to a field before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transform {
    Trim,
    Lowercase,
    Uppercase,
}

/// A strongly typed validation rule. Rule names are never kept as strings:
/// each variant carries its typed payload so generators can branch directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `.min(n)` — numeric lower bound.
    Min(i64),
    /// `.max(n)` — numeric upper bound.
    Max(i64),
    /// `.min_len(n)` — minimum character length.
    MinLen(usize),
    /// `.max_len(n)` — maximum character length.
    MaxLen(usize),
    /// `.regex("...")` — full-string regular expression match.
    Regex(String),
    /// `.email()`
    Email,
    /// `.url()`
    Url,
    /// `.uuid()`
    Uuid,
    /// `.alphanumeric()`
    Alphanumeric,
    /// `.nonempty()`
    NonEmpty,
}
