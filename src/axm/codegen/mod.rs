//! Code generation for `.axm` models.
//!
//! Both targets share the same AST traversal and validation semantics; only
//! the emitted surface differs. Generated validators compose recursively so a
//! `User` validator can validate a nested `Address` and arrays of models.

pub mod rust;
pub mod typescript;

pub use rust::generate_rust_models;
pub use typescript::generate_typescript_models;

use crate::axm::ast::{ModelDecl, Rule, TypeRef};
use crate::axm::resolver::ModelRegistry;

/// Which optional helpers a generated output needs, so only used helpers are
/// emitted (mirroring the SQL generators' behavior).
#[derive(Debug, Default)]
pub(crate) struct Uses {
    pub string: bool,
    pub int: bool,
    pub float: bool,
    pub boolean: bool,
    pub json: bool,
    pub array: bool,
    pub timestamp: bool,
    pub email: bool,
    pub url: bool,
    pub uuid: bool,
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
            Rule::Email => self.email = true,
            Rule::Url => self.url = true,
            Rule::Uuid => self.uuid = true,
            Rule::Alphanumeric => self.alphanumeric = true,
            Rule::NonEmpty => self.nonempty = true,
            Rule::Min(_) => self.min = true,
            Rule::Max(_) => self.max = true,
            Rule::MinLen(_) => self.min_len = true,
            Rule::MaxLen(_) => self.max_len = true,
            Rule::Regex(_) => self.regex = true,
        }
    }

    fn add_type(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::String => self.string = true,
            TypeRef::Int => self.int = true,
            TypeRef::Float => self.float = true,
            TypeRef::Boolean => self.boolean = true,
            TypeRef::Json => self.json = true,
            TypeRef::Timestamp => self.timestamp = true,
            TypeRef::Named(_) => {}
            TypeRef::Array(inner) => {
                self.array = true;
                self.add_type(inner);
            }
        }
    }
}

pub(crate) fn collect_uses(registry: &ModelRegistry) -> Uses {
    let mut uses = Uses::default();
    for resolved in &registry.models {
        for field in &resolved.model.fields {
            uses.add_type(&field.ty);
            for rule in &field.validations {
                uses.add_rule(rule);
            }
        }
    }
    uses
}

pub(crate) fn rule_message(rule: &Rule) -> String {
    match rule {
        Rule::Email => "must be a valid email address".to_string(),
        Rule::Url => "must be a valid URL".to_string(),
        Rule::Uuid => "must be a valid UUID".to_string(),
        Rule::Alphanumeric => "must be alphanumeric".to_string(),
        Rule::NonEmpty => "must not be empty".to_string(),
        Rule::Min(n) => format!("must be >= {n}"),
        Rule::Max(n) => format!("must be <= {n}"),
        Rule::MinLen(n) => format!("must be at least {n} characters"),
        Rule::MaxLen(n) => format!("must be at most {n} characters"),
        Rule::Regex(_) => "must match the expected pattern".to_string(),
    }
}

pub(crate) fn model_name(model: &ModelDecl) -> String {
    crate::codegen::util::pascal_case(&model.name)
}
