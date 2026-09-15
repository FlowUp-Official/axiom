//! AST-based canonical formatter for `.axm` model files.
//!
//! The formatter parses the file with the real `.axm` parser and re-prints the
//! AST, so it cannot mangle syntax it does not understand. Invalid input is
//! returned as an error and left untouched by callers.

use axiom_core::axm::ast::{
    AnnotatedType, FieldDecl, ImportStmt, ImportedName, Literal, ModelDecl, ParamDecl, QueryDecl,
    QueryReturn, Rule, Transform, TypeDecl, TypeRef,
};
use axiom_core::axm::parser::parse_axm_file;

use crate::printer::{Lines, MAX_INLINE_WIDTH, fits_inline, indent};

/// Format a `.axm` source file to canonical form.
///
/// Returns an `Err(message)` when the source does not parse.
pub fn format_axm(src: &str) -> Result<String, String> {
    let file = parse_axm_file(src).map_err(|e| e.to_string())?;
    let mut lines = Lines::new();

    let mut first_item = true;
    for item in file
        .imports
        .iter()
        .map(|i| vec![format_import(i)])
        .chain(file.types.iter().map(|t| vec![format_type_decl(t)]))
        .chain(file.models.iter().map(format_model))
        .chain(file.queries.iter().map(format_query))
    {
        if !first_item {
            lines.push_blank();
        }
        for line in item {
            lines.push(line);
        }
        first_item = false;
    }

    Ok(lines.finish())
}

fn format_import(import: &ImportStmt) -> String {
    format!(
        "import {{ {} }} from \"{}\"",
        import
            .names
            .iter()
            .map(format_imported_name)
            .collect::<Vec<_>>()
            .join(", "),
        import.source
    )
}

fn format_imported_name(name: &ImportedName) -> String {
    match &name.alias {
        Some(alias) => format!("{} as {alias}", name.name),
        None => name.name.clone(),
    }
}

fn format_type_decl(decl: &TypeDecl) -> String {
    format!("type {} = {};", decl.name, format_annotated(&decl.ty))
}

/// Render a full top-level model including its `extends select<...>` source.
fn format_model(model: &ModelDecl) -> Vec<String> {
    let source = model
        .source
        .as_ref()
        .map(|s| format!(" extends select<{}>", s.relation))
        .unwrap_or_default();
    let mut out = vec![format!("model {}{source} {{", model.name)];
    for field in &model.fields {
        out.push(format!("{}{}", indent(1), format_field(field)));
    }
    out.push("}".to_string());
    out
}

/// Render a full query declaration.
fn format_query(query: &QueryDecl) -> Vec<String> {
    let params = query
        .params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match &query.return_type {
        QueryReturn::Exec => String::new(),
        QueryReturn::Single(ty) => format!(" -> {}", format_type(ty)),
        QueryReturn::Optional(ty) => format!(" -> {}?", format_type(ty)),
        QueryReturn::Many(ty) => format!(" -> {}[]", format_type(ty)),
    };
    let mut out = vec![format!("query {}({params}){ret} {{", query.name)];
    for body_line in query.sql.lines() {
        out.push(format!("  {body_line}"));
    }
    out.push("}".to_string());
    out
}

fn format_param(param: &ParamDecl) -> String {
    format!("${}: {}", param.name, format_type(&param.ty))
}

/// Render a field, breaking long rule chains onto continuation lines.
fn format_field(field: &FieldDecl) -> String {
    let base = format!("{}: {}", field_name(field), format_type(&field.ty.base));
    let calls = format_calls(&field.ty);
    let inline_full = format!("{}{}", base, calls.join(""));
    let inline = match &field.default {
        Some(lit) => format!("{inline_full} = {}", format_literal(lit)),
        None => inline_full.clone(),
    };

    if calls.len() <= 3 && fits_inline(&inline, MAX_INLINE_WIDTH) {
        return inline;
    }

    // Break the chain: one rule per continuation line, indented one level past
    // the field. The default (if any) rides on the final continuation line.
    let mut out = base;
    for (i, call) in calls.iter().enumerate() {
        out.push('\n');
        let last = i == calls.len() - 1;
        match &field.default {
            Some(lit) if last => {
                out.push_str(&format!("{}{call} = {}", indent(2), format_literal(lit)));
            }
            _ => {
                out.push_str(&format!("{}{call}", indent(2)));
            }
        }
    }
    out
}

fn field_name(field: &FieldDecl) -> String {
    if field.optional {
        format!("{}?", field.name)
    } else {
        field.name.clone()
    }
}

/// The `.transform().rule()` chain, transformations first (matching codegen's
/// transform-before-validation order), each call fully rendered.
fn format_calls(ty: &AnnotatedType) -> Vec<String> {
    let mut calls = Vec::new();
    for t in &ty.transforms {
        calls.push(format_transform(t));
    }
    for r in &ty.rules {
        calls.push(format_rule(r));
    }
    calls
}

/// The base type with its method chain, e.g. `String.email().min(3)`.
fn format_annotated(ty: &AnnotatedType) -> String {
    let calls = format_calls(ty);
    format!("{}{}", format_type(&ty.base), calls.join(""))
}

fn format_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::String => "String".to_string(),
        TypeRef::Int => "Int".to_string(),
        TypeRef::BigInt => "BigInt".to_string(),
        TypeRef::Float => "Float".to_string(),
        TypeRef::Boolean => "Boolean".to_string(),
        TypeRef::Uuid => "UUID".to_string(),
        TypeRef::Date => "Date".to_string(),
        TypeRef::DateTime => "DateTime".to_string(),
        TypeRef::Json => "Json".to_string(),
        TypeRef::Bytes => "Bytes".to_string(),
        TypeRef::Named(name) => name.clone(),
        TypeRef::Array(inner) => format!("{}[]", format_type(inner)),
        TypeRef::Nullable(inner) => format!("{}?", format_type(inner)),
    }
}

fn format_transform(transform: &Transform) -> String {
    match transform {
        Transform::Trim => ".trim()".to_string(),
        Transform::Lowercase => ".lowercase()".to_string(),
        Transform::Uppercase => ".uppercase()".to_string(),
    }
}

fn format_rule(rule: &Rule) -> String {
    match rule {
        Rule::Min(n, m) => format_message_call("min", &[n.to_string()], m),
        Rule::Max(n, m) => format_message_call("max", &[n.to_string()], m),
        Rule::MinLength(n, m) => format_message_call("min_length", &[n.to_string()], m),
        Rule::MaxLength(n, m) => format_message_call("max_length", &[n.to_string()], m),
        Rule::Regex(pattern, m) => format_message_call(
            "regex",
            &[format_literal(&Literal::String(pattern.clone()))],
            m,
        ),
        Rule::Email(m) => format_message_call("email", &[], m),
        Rule::Url(m) => format_message_call("url", &[], m),
        Rule::Uuid(m) => format_message_call("uuid", &[], m),
        Rule::Ulid(m) => format_message_call("ulid", &[], m),
        Rule::Ipv4(m) => format_message_call("ipv4", &[], m),
        Rule::Ipv6(m) => format_message_call("ipv6", &[], m),
        Rule::IsoDate(m) => format_message_call("isodate", &[], m),
        Rule::Alphanumeric(m) => format_message_call("alphanumeric", &[], m),
        Rule::NonEmpty(m) => format_message_call("nonempty", &[], m),
    }
}

/// Render `.name(args, "msg")` from a rule's argument list and optional message.
fn format_message_call(name: &str, args: &[String], message: &Option<String>) -> String {
    let head = args.join(", ");
    match message {
        Some(m) => format!(
            ".{name}({}{}{})",
            head,
            if head.is_empty() { "" } else { ", " },
            format_literal(&Literal::String(m.clone()))
        ),
        None if head.is_empty() => format!(".{name}()"),
        None => format!(".{name}({head})"),
    }
}

fn format_literal(literal: &Literal) -> String {
    match literal {
        Literal::String(s) => format!("\"{}\"", escape_string(s)),
        Literal::Int(n) => n.to_string(),
        Literal::Float(f) => format_float(*f),
        Literal::Bool(b) => b.to_string(),
    }
}

/// Render a float keeping a fractional marker so it round-trips as `float`.
fn format_float(f: f64) -> String {
    let mut text = f.to_string();
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        text.push_str(".0");
    }
    text
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_axm(src).expect("formats cleanly")
    }

    #[test]
    fn formats_simple_model_with_import() {
        let src = r#"
import { Address } from   "address";
model User {
      name : String
      age: Int .min(18)
}
"#;
        assert_eq!(
            fmt(src),
            "import { Address } from \"address\"\n\
             \nmodel User {\n\
             \x20 name: String\n\
             \x20 age: Int.min(18)\n\
             }\n"
        );
    }

    #[test]
    fn formats_extends_select_and_aliased_imports() {
        let src = r#"import { User as DbUser } from "users"
model UserView extends select<public.users> {
  id: UUID
  email: Email
}"#;
        assert_eq!(
            fmt(src),
            "import { User as DbUser } from \"users\"\n\
             \nmodel UserView extends select<public.users> {\n\
             \x20 id: UUID\n\
             \x20 email: Email\n\
             }\n"
        );
    }

    #[test]
    fn breaks_long_rule_chains() {
        let src =
            "model User {\n  username: String .alphanumeric() .min(3) .max(20) .nonempty()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  username: String\n    .alphanumeric()\n    .min(3)\n    .max(20)\n    .nonempty()\n}\n"
        );
    }

    #[test]
    fn keeps_short_chains_inline() {
        let src = "model User {\n  email: String .trim() .lowercase() .email()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  email: String.trim().lowercase().email()\n}\n"
        );
    }

    #[test]
    fn formats_defaults_and_optional_fields() {
        let src = "model User { country : String = \"US\"\n  age?: Int.min(18) }";
        assert_eq!(
            fmt(src),
            "model User {\n  country: String = \"US\"\n  age?: Int.min(18)\n}\n"
        );
    }

    #[test]
    fn formats_type_aliases() {
        let src = "type Email = String.email();\ntype UserId = BigInt\nmodel User {\n  email: Email\n  id: UserId\n}";
        let out = fmt(src);
        assert_eq!(
            out,
            "type Email = String.email();\n\
             \ntype UserId = BigInt;\n\
             \nmodel User {\n\
             \x20 email: Email\n\
             \x20 id: UserId\n\
             }\n"
        );
    }

    #[test]
    fn transforms_precede_validations() {
        let src = "model User {\n  email: String .email() .trim() .lowercase()\n}";
        let out = fmt(src);
        assert!(
            out.contains("email: String.trim().lowercase().email()"),
            "{out}"
        );
    }

    #[test]
    fn formats_query_declarations() {
        let src = r#"query GetUser($id: UUID) -> User? {
  SELECT * FROM users WHERE id = $id;
}
query CreateUser($input: NewUser) {
  INSERT INTO users (email) VALUES ($input.email);
}"#;
        let out = fmt(src);
        assert!(out.contains("query GetUser($id: UUID) -> User? {"), "{out}");
        assert!(out.contains("query CreateUser($input: NewUser) {"), "{out}");
    }

    #[test]
    fn escaping_round_trips_string_literals() {
        let src = r#"model User { slug: String .regex("^[a-z0-9-\"\\n]+$") }"#;
        let out = fmt(src);
        assert!(out.contains(".regex(\"^[a-z0-9-\\\"\\\\n]+$\")"), "{out}");
    }

    #[test]
    fn floats_keep_fractional_marker() {
        let src = "model M { ratio: Float = 0.5\n  whole: Float = 2.0 }";
        let out = fmt(src);
        assert!(out.contains("whole: Float = 2.0"), "{out}");
    }

    #[test]
    fn rules_with_messages_round_trip() {
        let src = "model User {\n  username: String .nonempty(\"required\") .min_length(3, \"too short\")\n}";
        let out = fmt(src);
        assert!(out.contains(".nonempty(\"required\")"), "{out}");
        assert!(out.contains(".min_length(3, \"too short\")"), "{out}");
    }

    #[test]
    fn output_is_idempotent() {
        let messy = r#"
import {A,B} from "geo";
type Email = String .email() .max_length(320)
model User {
  email : String .trim() .email()
  address: Address[]
  tags: String[] .nonempty()
  username: String .alphanumeric() .min(3) .max(20) .lowercase()
  created: DateTime = "2024-01-01T00:00:00Z"
  private: Boolean
  bio: String?
}
query ListUsers($limit: Int) -> User[] {
  SELECT * FROM users ORDER BY id LIMIT $limit;
}
"#;
        let once = fmt(messy);
        assert_eq!(once, fmt(&once), "formatting must be idempotent");
    }

    #[test]
    fn empty_models_format_to_brace_pair() {
        assert_eq!(fmt("model Empty { }"), "model Empty {\n}\n");
    }

    #[test]
    fn invalid_axm_is_reported() {
        let err = format_axm("model {").expect_err("should fail to parse");
        assert!(!err.is_empty());
    }
}
