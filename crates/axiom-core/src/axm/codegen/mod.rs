//! Code generation for `.axm` models, type aliases, and queries.
//!
//! Both targets share the same AST traversal and validation semantics; only
//! the emitted surface differs. Generated validators compose recursively so a
//! `User` validator can validate a nested `Address` and arrays of models.
//!
//! Resolution concerns (imports, alias mapping) live in the resolver; the
//! generators reach back into the [`ModelRegistry`] to emit *canonical* names
//! in each file's scope and to inline type-alias methods into validation code.

pub mod rust;
pub mod typescript;

pub use rust::generate_rust_models;
pub use typescript::generate_typescript_models;

use std::path::Path;

use crate::axm::ast::{AnnotatedType, Literal, ModelDecl, QueryReturn, Rule, TypeRef};
use crate::axm::resolver::ModelRegistry;
use crate::catalog::TableCatalog;
use crate::codegen::util;

/// Which optional helpers a generated output needs, so only the used helpers
/// are emitted (mirroring the SQL generators' behavior).
#[derive(Debug, Default)]
pub(crate) struct Uses {
    pub string: bool,
    pub int: bool,
    pub bigint: bool,
    pub float: bool,
    pub boolean: bool,
    pub json: bool,
    pub date: bool,
    pub datetime: bool,
    pub bytes: bool,
    pub array: bool,
    pub email: bool,
    pub url: bool,
    pub uuid: bool,
    pub ulid: bool,
    pub ipv4: bool,
    pub ipv6: bool,
    pub isodate: bool,
    pub alphanumeric: bool,
    pub nonempty: bool,
    pub min: bool,
    pub max: bool,
    pub min_len: bool,
    pub max_len: bool,
    pub regex: bool,
}

impl Uses {
    fn add_rule(&mut self, rule: &Rule) {
        match rule {
            Rule::Email(_) => self.email = true,
            Rule::Url(_) => self.url = true,
            Rule::Uuid(_) => self.uuid = true,
            Rule::Ulid(_) => self.ulid = true,
            Rule::Ipv4(_) => self.ipv4 = true,
            Rule::Ipv6(_) => self.ipv6 = true,
            Rule::IsoDate(_) => self.isodate = true,
            Rule::Alphanumeric(_) => self.alphanumeric = true,
            Rule::NonEmpty(_) => self.nonempty = true,
            Rule::Min(..) => self.min = true,
            Rule::Max(..) => self.max = true,
            Rule::MinLength(..) => self.min_len = true,
            Rule::MaxLength(..) => self.max_len = true,
            Rule::Regex(_, _) => self.regex = true,
        }
    }

    fn add_type(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::String => self.string = true,
            TypeRef::Int => self.int = true,
            TypeRef::BigInt => self.bigint = true,
            TypeRef::Float => self.float = true,
            TypeRef::Boolean => self.boolean = true,
            TypeRef::Json => self.json = true,
            TypeRef::Date => self.date = true,
            TypeRef::DateTime => self.datetime = true,
            TypeRef::Bytes => self.bytes = true,
            TypeRef::Uuid => self.uuid = true,
            TypeRef::Named(_) => {}
            TypeRef::Nullable(inner) => self.add_type(inner),
            TypeRef::Array(inner) => {
                self.array = true;
                self.add_type(inner);
            }
        }
    }

    fn add_annotated(&mut self, ann: &AnnotatedType) {
        self.add_type(&ann.base);
        for rule in &ann.rules {
            self.add_rule(rule);
        }
    }
}

/// Collect the set of helpers needed to emit the whole registry, including
/// database-backed columns (which bring primitive types) and queries.
pub(crate) fn collect_uses(registry: &ModelRegistry, catalog: &TableCatalog) -> Uses {
    let mut uses = Uses::default();
    for resolved in &registry.models {
        for field in effective_fields(registry, catalog, &resolved.path, &resolved.model) {
            uses.add_annotated(&field.annotated);
        }
    }
    for resolved in &registry.types {
        uses.add_annotated(&inline_annotated(registry, &resolved.path, &resolved.ty.ty));
    }
    for resolved in &registry.queries {
        for param in &resolved.query.params {
            uses.add_type(&param.ty);
        }
        match &resolved.query.return_type {
            QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
                uses.add_type(ty);
            }
            QueryReturn::Exec => {}
        }
    }
    uses
}

/// The name a model is emitted under (already PascalCase as declared).
pub(crate) fn model_name(model: &ModelDecl) -> String {
    util::pascal_case(&model.name)
}

/// The canonical PascalCase name a referenced model/type is emitted under,
/// accounting for import aliases (`DbUser` -> `User`).
pub(crate) fn canonical_name(registry: &ModelRegistry, path: &Path, written: &str) -> String {
    util::pascal_case(registry.effective_name(path, written))
}

/// The default message for a rule, honoring a user-supplied override.
pub(crate) fn rule_message(rule: &Rule) -> String {
    match rule.message() {
        Some(m) => m.to_string(),
        None => match rule {
            Rule::Email(_) => "must be a valid email address".to_string(),
            Rule::Url(_) => "must be a valid URL".to_string(),
            Rule::Uuid(_) => "must be a valid UUID".to_string(),
            Rule::Ulid(_) => "must be a valid ULID".to_string(),
            Rule::Ipv4(_) => "must be a valid IPv4 address".to_string(),
            Rule::Ipv6(_) => "must be a valid IPv6 address".to_string(),
            Rule::IsoDate(_) => "must be a valid ISO 8601 date".to_string(),
            Rule::Alphanumeric(_) => "must be alphanumeric".to_string(),
            Rule::NonEmpty(_) => "must not be empty".to_string(),
            Rule::Min(n, _) => format!("must be >= {n}"),
            Rule::Max(n, _) => format!("must be <= {n}"),
            Rule::MinLength(n, _) => format!("must be at least {n} characters"),
            Rule::MaxLength(n, _) => format!("must be at most {n} characters"),
            Rule::Regex(_, _) => "must match the expected pattern".to_string(),
        },
    }
}

/// A fully resolved field, ready for emission. `annotated` has every type
/// alias chain inlined (its base is never a *pure* alias, and rules/transforms
/// include those inherited from aliases).
#[derive(Debug, Clone)]
pub(crate) struct EffectiveField {
    /// The key used in the emitted interface/struct and the `record[key]`
    /// lookup. For declared fields this is the declared name; for unrefined
    /// database columns it is the camelCase form.
    pub emitted_name: String,
    pub annotated: AnnotatedType,
    pub optional: bool,
    pub default: Option<Literal>,
}

/// Compute the effective fields for a model declared in `path`.
///
/// A bare model uses its declared fields. A database-backed model
/// (`model X extends select<t>`) merges the table columns with its declared
/// fields: every column becomes a field (unrefined columns get a
/// database-derived type), and declared fields refine their matching column or
/// add application-only fields.
pub(crate) fn effective_fields(
    registry: &ModelRegistry,
    catalog: &TableCatalog,
    path: &Path,
    model: &ModelDecl,
) -> Vec<EffectiveField> {
    if let Some(source) = &model.source {
        if let Some(table) = catalog.tables.iter().find(|t| t.name == source.relation) {
            let mut used = vec![false; model.fields.len()];
            let mut out = Vec::with_capacity(table.columns.len() + model.fields.len());

            for column in &table.columns {
                let db_name = util::ts_field_name(&column.name);
                let declared = model
                    .fields
                    .iter()
                    .zip(&mut used)
                    .find(|(field, matched)| {
                        !**matched && (field.name == column.name || field.name == db_name)
                    });

                if let Some((field, matched)) = declared {
                    *matched = true;
                    let mut annotated = inline_annotated(registry, path, &field.ty);
                    if column.nullable
                        && field.default.is_none()
                        && !matches!(annotated.base, TypeRef::Nullable(_))
                    {
                        annotated.base =
                            TypeRef::Nullable(Box::new(std::mem::replace(&mut annotated.base, TypeRef::String)));
                    }
                    out.push(EffectiveField {
                        emitted_name: field.name.clone(),
                        annotated,
                        optional: field.optional || column.nullable,
                        default: field.default.clone(),
                    });
                } else {
                    let base = sql_type_to_type_ref(&column.data_type);
                    let base = if column.nullable {
                        TypeRef::Nullable(Box::new(base))
                    } else {
                        base
                    };
                    out.push(EffectiveField {
                        emitted_name: db_name,
                        annotated: AnnotatedType::new(base),
                        optional: column.nullable,
                        default: None,
                    });
                }
            }

            for (field, matched) in model.fields.iter().zip(&used) {
                if *matched {
                    continue;
                }
                out.push(EffectiveField {
                    emitted_name: field.name.clone(),
                    annotated: inline_annotated(registry, path, &field.ty),
                    optional: field.optional,
                    default: field.default.clone(),
                });
            }
            return out;
        }
    }

    model
        .fields
        .iter()
        .map(|field| EffectiveField {
            emitted_name: field.name.clone(),
            annotated: inline_annotated(registry, path, &field.ty),
            optional: field.optional,
            default: field.default.clone(),
        })
        .collect()
}

/// How deep a chain of type aliases may be inlined before a generator gives up
/// and falls back to the written reference. Genuine alias cycles are rejected
/// by the semantic verifier, so this only guards against pathological inputs.
const MAX_ALIAS_DEPTH: usize = 32;

/// Inline type-alias chains into an annotation: the emitted `base` is never a
/// *pure* alias, and rules/transforms inherited from aliases are merged in
/// (alias rules apply before usage-site rules).
pub(crate) fn inline_annotated(
    registry: &ModelRegistry,
    path: &Path,
    ann: &AnnotatedType,
) -> AnnotatedType {
    inline_alias(registry, path, ann, 0)
}

fn inline_alias(registry: &ModelRegistry, path: &Path, ann: &AnnotatedType, depth: usize) -> AnnotatedType {
    match &ann.base {
        TypeRef::Named(name) => {
            let effective = registry.effective_name(path, name);
            if let Some(resolved) = registry.type_by_name(effective) {
                if depth >= MAX_ALIAS_DEPTH {
                    return ann.clone();
                }
                let inner = inline_alias(registry, &resolved.path, &resolved.ty.ty, depth + 1);
                let mut transforms = inner.transforms;
                transforms.extend(ann.transforms.iter().cloned());
                let mut rules = inner.rules;
                rules.extend(ann.rules.iter().cloned());
                AnnotatedType {
                    base: inner.base,
                    transforms,
                    rules,
                }
            } else {
                ann.clone()
            }
        }
        TypeRef::Array(inner) => {
            let mut cloned = ann.clone();
            cloned.base = TypeRef::Array(Box::new(inline_element(registry, path, inner, depth)));
            cloned
        }
        TypeRef::Nullable(inner) => {
            let mut cloned = ann.clone();
            cloned.base = TypeRef::Nullable(Box::new(inline_element(registry, path, inner, depth)));
            cloned
        }
        _ => ann.clone(),
    }
}

fn inline_element(registry: &ModelRegistry, path: &Path, ty: &TypeRef, depth: usize) -> TypeRef {
    match ty {
        TypeRef::Named(name) => {
            let effective = registry.effective_name(path, name);
            match registry.type_by_name(effective) {
                Some(resolved) if depth < MAX_ALIAS_DEPTH => {
                    let inner = inline_alias(registry, &resolved.path, &resolved.ty.ty, depth + 1);
                    if inner.transforms.is_empty() && inner.rules.is_empty() {
                        inner.base
                    } else {
                        // Rules live on the element; a runtime coerce function
                        // is generated for the alias, so keep the reference.
                        ty.clone()
                    }
                }
                _ => ty.clone(),
            }
        }
        TypeRef::Array(inner) => TypeRef::Array(Box::new(inline_element(registry, path, inner, depth))),
        TypeRef::Nullable(inner) => {
            TypeRef::Nullable(Box::new(inline_element(registry, path, inner, depth)))
        }
        _ => ty.clone(),
    }
}

/// The runtime target of a `Named` reference, used by both emitters.
/// Either a model's `coerce` function, a type alias with methods (its own
/// `coerce` function), or a pure alias that folds into its base type.
pub(crate) enum NamedKind {
    /// The canonical model name.
    Model(String),
    /// The canonical alias name (its `coerce{Name}` function is emitted).
    AliasFun(String),
    /// A pure alias: fully resolve to its concrete base type.
    Pure(TypeRef),
    /// A name that resolves to neither model nor type (semantics will reject).
    Unknown(String),
}

/// Classify a written `Named` reference for code emission.
pub(crate) fn named_kind(registry: &ModelRegistry, path: &Path, name: &str) -> NamedKind {
    let effective = registry.effective_name(path, name);
    if let Some(resolved) = registry.type_by_name(effective) {
        let inlined = inline_alias(registry, &resolved.path, &resolved.ty.ty, 0);
        if inlined.transforms.is_empty() && inlined.rules.is_empty() {
            let base = if let TypeRef::Named(inline_name) = &inlined.base {
                // The alias chains to another alias; resolve one final step.
                resolve_pure(registry, path, inline_name, inlined.base.clone(), 0)
            } else {
                inlined.base
            };
            NamedKind::Pure(base)
        } else {
            NamedKind::AliasFun(effective.to_string())
        }
    } else if registry.model_by_name(effective).is_some() {
        NamedKind::Model(effective.to_string())
    } else {
        NamedKind::Unknown(effective.to_string())
    }
}

fn resolve_pure(registry: &ModelRegistry, path: &Path, written: &str, fallback: TypeRef, depth: usize) -> TypeRef {
    let effective = registry.effective_name(path, written);
    if let Some(resolved) = registry.type_by_name(effective) {
        if depth >= MAX_ALIAS_DEPTH {
            return fallback;
        }
        let inlined = inline_alias(registry, &resolved.path, &resolved.ty.ty, 0);
        if inlined.transforms.is_empty() && inlined.rules.is_empty() {
            match &inlined.base {
                TypeRef::Named(next) => {
                    return resolve_pure(registry, &resolved.path, next, inlined.base.clone(), depth + 1);
                }
                _ => return inlined.base,
            }
        }
    }
    fallback
}

/// Map a SQL data type to the closest Axiom primitive for unrefined
/// database-backed columns.
pub(crate) fn sql_type_to_type_ref(data_type: &str) -> TypeRef {
    match core_type(data_type).as_str() {
        "BIGINT" | "BIGSERIAL" | "INT8" => TypeRef::BigInt,
        "INT" | "INT2" | "INT4" | "INTEGER" | "SMALLINT" | "SERIAL" | "SMALLSERIAL" => {
            TypeRef::Int
        }
        "FLOAT" | "FLOAT4" | "REAL" => TypeRef::Float,
        "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" | "DECIMAL" | "DEC" | "NUMERIC" => {
            TypeRef::Float
        }
        "BOOL" | "BOOLEAN" => TypeRef::Boolean,
        "UUID" => TypeRef::Uuid,
        "JSON" | "JSONB" => TypeRef::Json,
        "DATE" => TypeRef::Date,
        "TIMESTAMP" | "TIMESTAMPTZ" | "DATETIME" | "SMALLDATETIME" => TypeRef::DateTime,
        "BYTEA" | "BYTES" | "BLOB" => TypeRef::Bytes,
        _ => TypeRef::String,
    }
}

fn core_type(data_type: &str) -> String {
    data_type
        .split('(')
        .next()
        .unwrap_or(data_type)
        .trim()
        .to_ascii_uppercase()
}