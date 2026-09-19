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
    pub transactions: Vec<TransactionDecl>,
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

    pub fn transaction_by_name(&self, name: &str) -> Option<&TransactionDecl> {
        self.transactions.iter().find(|t| t.name == name)
    }

    /// Every top-level declaration name in declaration order.
    pub fn declarations(&self) -> impl Iterator<Item = &str> {
        self.types
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.models.iter().map(|m| m.name.as_str()))
            .chain(self.queries.iter().map(|q| q.name.as_str()))
            .chain(self.transactions.iter().map(|t| t.name.as_str()))
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
///
/// A model may be preceded by `@` decorators that override code generation for
/// just that model, e.g. `@target("typescript")` or `@no_codegen`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDecl {
    pub name: String,
    /// The `select<...>` source expression, when the model is database-backed.
    pub source: Option<ModelSource>,
    pub fields: Vec<FieldDecl>,
    /// The `@...` decorators applied to this model, in source order.
    pub overrides: Vec<ModelOverride>,
}

impl ModelDecl {
    /// The explicit `@target(...)` restriction, if any.
    pub fn target_restriction(&self) -> Option<&[Target]> {
        target_restriction(&self.overrides)
    }

    /// Whether the `@no_codegen` decorator is applied.
    pub fn is_no_codegen(&self) -> bool {
        self.overrides
            .iter()
            .any(|o| matches!(o, ModelOverride::NoCodegen))
    }

    /// The explicit `@safeParse(...)` mode, if the model carries the decorator.
    pub fn safe_parse_override(&self) -> Option<SafeParseMode> {
        self.overrides.iter().find_map(|o| match o {
            ModelOverride::SafeParse(mode) => Some(*mode),
            _ => None,
        })
    }

    /// The `@safeParse(...)` error-aggregation mode, defaulting to `All`.
    pub fn safe_parse_mode(&self) -> SafeParseMode {
        self.safe_parse_override().unwrap_or(SafeParseMode::All)
    }
}

/// A single `@...` decorator on a model or query. The list is extensible: new
/// decorators become new variants parsed by the same `@ ident (...) ?` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOverride {
    /// `@target("typescript", "rust")` — restrict code generation for this
    /// model or query to the listed targets.
    Target(Vec<Target>),
    /// `@no_codegen` — never emit this model's standalone validation API.
    /// Its type and `coerce` are emitted only when another emitted declaration
    /// references the model. Only applicable to `model` declarations.
    NoCodegen,
    /// `@parse` — declare that this model keeps its standalone validation
    /// entry points (`Result`/`safeParse`/`parse`). Documentary: every
    /// model not marked `@no_codegen` gets them anyway. Only applicable to
    /// `model` declarations; mutually exclusive with `@no_codegen`.
    Parse,
    /// `@safeParse("first")` / `@safeParse("all")` — how `safeParse`/`parse`
    /// aggregate validation errors. `First` stops coercing at the first error
    /// and reports only it; `All` (the default) collects every error. Only
    /// applicable to `model` declarations; mutually exclusive with
    /// `@no_codegen`.
    SafeParse(SafeParseMode),
}

/// The error-aggregation mode named by `@safeParse(...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeParseMode {
    /// `@safeParse("first")` — stop validating at the first error.
    First,
    /// `@safeParse("all")` — collect every validation error (the default).
    All,
}

impl SafeParseMode {
    /// The recognized mode names, in canonical order.
    pub const ALL: [SafeParseMode; 2] = [SafeParseMode::First, SafeParseMode::All];

    /// The lowercase source spelling of the mode.
    pub fn name(&self) -> &'static str {
        match self {
            SafeParseMode::First => "first",
            SafeParseMode::All => "all",
        }
    }

    /// Parse a mode name; names are case-sensitive and lowercase.
    pub fn parse(name: &str) -> Option<SafeParseMode> {
        match name {
            "first" => Some(SafeParseMode::First),
            "all" => Some(SafeParseMode::All),
            _ => None,
        }
    }
}

/// The explicit `@target(...)` restriction in a decorator list, if any.
fn target_restriction(overrides: &[ModelOverride]) -> Option<&[Target]> {
    overrides.iter().find_map(|o| match o {
        ModelOverride::Target(targets) => Some(targets.as_slice()),
        ModelOverride::NoCodegen | ModelOverride::Parse | ModelOverride::SafeParse(_) => None,
    })
}

/// A codegen target named by `@target(...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    TypeScript,
    Rust,
}

impl Target {
    /// The recognized target names, in canonical order.
    pub const ALL: [Target; 2] = [Target::TypeScript, Target::Rust];

    /// The lowercase source spelling of the target.
    pub fn name(&self) -> &'static str {
        match self {
            Target::TypeScript => "typescript",
            Target::Rust => "rust",
        }
    }

    /// Parse a target name; names are case-sensitive and lowercase.
    pub fn parse(name: &str) -> Option<Target> {
        match name {
            "typescript" => Some(Target::TypeScript),
            "rust" => Some(Target::Rust),
            _ => None,
        }
    }
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
///
/// A query may be preceded by `@` decorators; `@target(...)` restricts which
/// codegen targets the query function is emitted for. `@no_codegen` does not
/// apply to queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: QueryReturn,
    pub sql: String,
    /// The `@...` decorators applied to this query, in source order.
    pub overrides: Vec<ModelOverride>,
}

impl QueryDecl {
    /// The explicit `@target(...)` restriction, if any. Queries cannot carry
    /// `@no_codegen` (rejected at parse time), so a `None` restriction means
    /// the query is emitted for every target.
    pub fn target_restriction(&self) -> Option<&[Target]> {
        target_restriction(&self.overrides)
    }
}

/// A transaction declaration: a first-class, atomic multi-statement database
/// contract.
///
/// ```text
/// transaction CreateOrder($userId: UUID, $items: Item[]) -> Order { ...SQL... }
/// ```
///
/// Syntax, parameter rules, decorators, and return types mirror [`QueryDecl`]
/// exactly; the semantic difference is that the SQL body holds **two or more**
/// statements that are executed atomically (committed together, or all rolled
/// back on any failure). The LAST statement drives the return value, exactly
/// as for a multi-statement query body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: QueryReturn,
    pub sql: String,
    /// The `@...` decorators applied to this transaction, in source order.
    pub overrides: Vec<ModelOverride>,
}

impl TransactionDecl {
    /// The explicit `@target(...)` restriction, if any. Transactions cannot
    /// carry `@no_codegen` (rejected at parse time), so a `None` restriction
    /// means the transaction is emitted for every target.
    pub fn target_restriction(&self) -> Option<&[Target]> {
        target_restriction(&self.overrides)
    }
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

impl QueryReturn {
    /// The `TypeRef` of the return value, if this is not an `Exec` query.
    pub fn ty_ref(&self) -> Option<&TypeRef> {
        match self {
            QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => Some(ty),
            QueryReturn::Exec => None,
        }
    }
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
    /// The canonical PascalCase primitive type names, in declaration order.
    ///
    /// The parser matches these exactly (case-sensitive): `string`, `uuid`,
    /// `timestamp`, ... are ordinary identifiers, not primitives.
    pub const PRIMITIVES: &'static [&'static str] = &[
        "String", "Int", "BigInt", "Float", "Boolean", "UUID", "Date", "DateTime", "Json", "Bytes",
    ];

    /// Whether `name` is a canonical primitive type name.
    pub fn is_primitive_name(name: &str) -> bool {
        Self::PRIMITIVES.contains(&name)
    }

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

impl Transform {
    /// The canonical transform call spellings (`.trim()`, `.lowercase()`,
    /// `.uppercase()`).
    pub const CALLS: &'static [&'static str] = &["trim", "lowercase", "uppercase"];
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
    /// The canonical validator call spellings, one per variant, in the same
    /// order as the enum definition.
    pub const CALLS: &'static [&'static str] = &[
        "email",
        "url",
        "uuid",
        "ulid",
        "ipv4",
        "ipv6",
        "isodate",
        "alphanumeric",
        "nonempty",
        "min",
        "max",
        "min_length",
        "max_length",
        "regex",
    ];

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

#[cfg(test)]
mod tests {
    use super::*;

    fn all_rules() -> Vec<Rule> {
        vec![
            Rule::Min(0, None),
            Rule::Max(0, None),
            Rule::MinLength(0, None),
            Rule::MaxLength(0, None),
            Rule::Regex(String::new(), None),
            Rule::Email(None),
            Rule::Url(None),
            Rule::Uuid(None),
            Rule::Ulid(None),
            Rule::Ipv4(None),
            Rule::Ipv6(None),
            Rule::IsoDate(None),
            Rule::Alphanumeric(None),
            Rule::NonEmpty(None),
        ]
    }

    #[test]
    fn rule_calls_are_the_canonical_names() {
        let mut canonical: Vec<&str> = all_rules().iter().map(Rule::name).collect();
        let mut calls = Rule::CALLS.to_vec();
        canonical.sort_unstable();
        calls.sort_unstable();
        assert_eq!(canonical, calls);
    }

    #[test]
    fn transform_calls_are_the_canonical_names() {
        assert_eq!(Transform::CALLS, &["trim", "lowercase", "uppercase"]);
        assert_eq!(Transform::CALLS.len(), 3);
    }

    #[test]
    fn primitives_are_case_sensitive_pascal_case() {
        assert!(TypeRef::is_primitive_name("String"));
        assert!(TypeRef::is_primitive_name("BigInt"));
        assert!(TypeRef::is_primitive_name("DateTime"));
        assert!(!TypeRef::is_primitive_name("string"));
        assert!(!TypeRef::is_primitive_name("timestamp"));
        assert!(!TypeRef::is_primitive_name("UUID?"));
    }
}
