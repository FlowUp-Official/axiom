//! The `.axm` language AST.
//!
//! An `.axm` file is a typed application contract layered on top of a database
//! schema. It may contain `import`s, `type` declarations, `model`
//! declarations, and `query` declarations. The AST is language-independent and
//! strongly typed: rules and transformations are dedicated enum variants
//! rather than generic string lists, so code generators can rely on structure
//! instead of parsing names.
//!
//! Axiom identifiers are strictly case-sensitive. Primitive type names use
//! `PascalCase` (`String`, `UUID`, `Int`, ...); model, type, and query names
//! use `PascalCase`; query parameters and fields use `camelCase`. The parser
//! never canonicalizes identifiers.

/// A parsed `.axm` source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxmFile {
    pub imports: Vec<ImportStmt>,
    pub types: Vec<TypeDecl>,
    pub models: Vec<ModelDecl>,
    pub queries: Vec<QueryDecl>,
}

impl AxmFile {
    pub fn type_by_name(&self, name: &str) -> Option<&TypeDecl> {
        self.types.iter().find(|t| t.name == name)
    }

    pub fn model_by_name(&self, name: &str) -> Option<&ModelDecl> {
        self.models.iter().find(|m| m.name == name)
    }

    pub fn query_by_name(&self, name: &str) -> Option<&QueryDecl> {
        self.queries.iter().find(|q| q.name == name)
    }

    /// Every top-level declaration name in declaration order.
    pub fn declarations(&self) -> impl Iterator<Item = &str> {
        self.types
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.models.iter().map(|m| m.name.as_str()))
            .chain(self.queries.iter().map(|q| q.name.as_str()))
    }
}

/// A single `import { A, B as C } from "source"` statement.
///
/// The `source` is a relative `.axm` file reference (without the extension),
/// e.g. `"users"`. The resolver is responsible for turning it into a concrete
/// file. Imported identifiers preserve exact capitalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportStmt {
    pub names: Vec<ImportedName>,
    pub source: String,
}

/// A single imported symbol, optionally aliased: `User as DbUser`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedName {
    pub name: String,
    pub alias: Option<String>,
}

/// A `type <Name> = <annotated type>;` declaration — a reusable, named
/// refinement of a primitive or existing type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    pub name: String,
    pub ty: AnnotatedType,
}

/// A base type plus the refinement/validation calls chained onto it, e.g.
/// `String.email().max_length(320)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatedType {
    pub base: TypeRef,
    /// Value transformations, in declared order, applied before validation.
    pub transforms: Vec<Transform>,
    /// Validation rules, in declared order.
    pub rules: Vec<Rule>,
}

impl AnnotatedType {
    pub fn new(base: TypeRef) -> Self {
        Self {
            base,
            transforms: Vec::new(),
            rules: Vec::new(),
        }
    }
}

/// A top-level `model` declaration.
///
/// The canonical database-backed form is:
///
/// ```text
/// model User extends select<users> { ... }
/// ```
///
/// where `select<users>` is a *database-derived source expression* (the
/// relation's DB identifier, not an Axiom type). Bare models (no `source`)
/// remain valid as pure application models with no database backing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDecl {
    pub name: String,
    /// The `select<...>` source expression, when the model is database-backed.
    pub source: Option<ModelSource>,
    pub fields: Vec<FieldDecl>,
}

/// A database-derived source expression: `select<users>`.
///
/// The payload is a database identifier, NOT an Axiom type identifier. It must
/// be resolved against the SQL schema and is subject to the configured SQL
/// dialect's identifier semantics, not Axiom capitalization rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSource {
    pub relation: String,
}

/// A single field declaration within a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    /// `age?` — the field may be absent.
    pub optional: bool,
    pub ty: AnnotatedType,
    /// `country = "US"` — applied only when the field is missing, never
    /// replacing an explicit `null`.
    pub default: Option<Literal>,
}

/// A query declaration: a first-class database query contract.
///
/// ```text
/// query GetUser($id: UUID) -> User? { ...SQL... }
/// ```
///
/// The missing return type (or `-> Exec`) marks an execution-only query. `-> T`
/// is exactly one value, `-> T?` zero or one, and `-> T[]` zero or more. The
/// SQL body is ordinary SQL and is stored verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: QueryReturn,
    pub sql: String,
}

/// A single query parameter, e.g. `$id: UUID`. Parameter names are `camelCase`
/// (the leading `$` is not part of the identifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDecl {
    pub name: String,
    pub ty: TypeRef,
}

/// The explicit public result contract of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryReturn {
    /// No row-return contract; the query performs an execution.
    Exec,
    /// `-> T` — exactly one value.
    Single(TypeRef),
    /// `-> T?` — zero or one value.
    Optional(TypeRef),
    /// `-> T[]` — zero or more values.
    Many(TypeRef),
}

/// A type reference. Primitives are strictly case-sensitive `PascalCase`
/// keywords; `string`, `uuid`, `int` are ordinary (and invalid) named
/// identifiers. `?` and `[]` are postfix operators: `User?[]` is an array of
/// nullable values, `User[]?` is a nullable array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    String,
    Int,
    BigInt,
    Float,
    Boolean,
    Uuid,
    Date,
    DateTime,
    Json,
    Bytes,
    /// A reference to a model or type alias, e.g. `Email` or `User`.
    Named(String),
    /// `T[]` — a homogenous array of `inner`.
    Array(Box<TypeRef>),
    /// `T?` — zero or one value of `inner`.
    Nullable(Box<TypeRef>),
}

impl TypeRef {
    /// The primitive name of this type, if it is a primitive.
    pub fn primitive_name(&self) -> Option<&'static str> {
        match self {
            TypeRef::String => Some("String"),
            TypeRef::Int => Some("Int"),
            TypeRef::BigInt => Some("BigInt"),
            TypeRef::Float => Some("Float"),
            TypeRef::Boolean => Some("Boolean"),
            TypeRef::Uuid => Some("UUID"),
            TypeRef::Date => Some("Date"),
            TypeRef::DateTime => Some("DateTime"),
            TypeRef::Json => Some("Json"),
            TypeRef::Bytes => Some("Bytes"),
            _ => None,
        }
    }
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

/// A strongly typed validation rule. Each variant carries its typed payload so
/// generators can branch directly; the trailing `Option<String>` is an optional
/// user-supplied message (e.g. `String.min_length(3, "too short")`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `.min(n)` — numeric lower bound.
    Min(i64, Option<String>),
    /// `.max(n)` — numeric upper bound.
    Max(i64, Option<String>),
    /// `.min_length(n)` — minimum character length.
    MinLength(usize, Option<String>),
    /// `.max_length(n)` — maximum character length.
    MaxLength(usize, Option<String>),
    /// `.regex("...")` — full-string regular expression match.
    Regex(String, Option<String>),
    /// `.email()`
    Email(Option<String>),
    /// `.url()`
    Url(Option<String>),
    /// `.uuid()`
    Uuid(Option<String>),
    /// `.ulid()`
    Ulid(Option<String>),
    /// `.ipv4()`
    Ipv4(Option<String>),
    /// `.ipv6()`
    Ipv6(Option<String>),
    /// `.isodate()`
    IsoDate(Option<String>),
    /// `.alphanumeric()`
    Alphanumeric(Option<String>),
    /// `.nonempty()`
    NonEmpty(Option<String>),
}

impl Rule {
    /// The human-readable rule name (the `.call()` spelling).
    pub fn name(&self) -> &'static str {
        match self {
            Rule::Min(..) => "min",
            Rule::Max(..) => "max",
            Rule::MinLength(..) => "min_length",
            Rule::MaxLength(..) => "max_length",
            Rule::Regex(..) => "regex",
            Rule::Email(..) => "email",
            Rule::Url(..) => "url",
            Rule::Uuid(..) => "uuid",
            Rule::Ulid(..) => "ulid",
            Rule::Ipv4(..) => "ipv4",
            Rule::Ipv6(..) => "ipv6",
            Rule::IsoDate(..) => "isodate",
            Rule::Alphanumeric(..) => "alphanumeric",
            Rule::NonEmpty(..) => "nonempty",
        }
    }

    /// The rule's custom message, if any.
    pub fn message(&self) -> Option<&str> {
        match self {
            Rule::Min(_, m)
            | Rule::Max(_, m)
            | Rule::MinLength(_, m)
            | Rule::MaxLength(_, m)
            | Rule::Regex(_, m)
            | Rule::Email(m)
            | Rule::Url(m)
            | Rule::Uuid(m)
            | Rule::Ulid(m)
            | Rule::Ipv4(m)
            | Rule::Ipv6(m)
            | Rule::IsoDate(m)
            | Rule::Alphanumeric(m)
            | Rule::NonEmpty(m) => m.as_deref(),
        }
    }

    /// Set the custom message, replacing any existing one.
    pub fn with_message(mut self, message: String) -> Self {
        match &mut self {
            Rule::Min(_, m)
            | Rule::Max(_, m)
            | Rule::MinLength(_, m)
            | Rule::MaxLength(_, m)
            | Rule::Regex(_, m)
            | Rule::Email(m)
            | Rule::Url(m)
            | Rule::Uuid(m)
            | Rule::Ulid(m)
            | Rule::Ipv4(m)
            | Rule::Ipv6(m)
            | Rule::IsoDate(m)
            | Rule::Alphanumeric(m)
            | Rule::NonEmpty(m) => *m = Some(message),
        }
        self
    }
}
