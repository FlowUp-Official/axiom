//! AST-based canonical formatter for `.axm` model files.
//!
//! The formatter parses the file with the real `.axm` parser and re-prints the
//! AST, so it cannot mangle syntax it does not understand. Invalid input is
//! returned as an error and left untouched by callers.

use axiom_core::axm::ast::{FieldDecl, ImportStmt, Literal, ModelDecl, Rule, Transform, TypeRef};
use axiom_core::axm::parser::parse_axm_file;

use crate::printer::{fits_inline, indent, Lines, MAX_INLINE_WIDTH};

/// Format a `.axm` source file to canonical form.
///
/// Returns an `Err(message)` when the source does not parse.
pub fn format_axm(src: &str) -> Result<String, String> {
    let file = parse_axm_file(src).map_err(|e| e.to_string())?;
    let mut lines = Lines::new();

    let mut first_item = true;
    for import in &file.imports {
        if !first_item {
            lines.push_blank();
        }
        lines.push(format_import(import));
        first_item = false;
    }
    for model in &file.models {
        if !first_item {
            lines.push_blank();
        }
        format_model(model, &mut lines);
        first_item = false;
    }

    Ok(lines.finish())
}

fn format_import(import: &ImportStmt) -> String {
    format!(
        "import {{ {} }} from \"{}\"",
        import.names.join(", "),
        import.source
    )
}

fn format_model(model: &ModelDecl, lines: &mut Lines) {
    let export = if model.exported { "export " } else { "" };
    lines.push(format!("{export}model {} {{", model.name));
    for field in &model.fields {
        lines.push(format!("{}{}", indent(1), format_field(field)));
    }
    lines.push("}");
}

/// Render a field, breaking long rule chains onto continuation lines.
fn format_field(field: &FieldDecl) -> String {
    let header = format!("{}: {}", field_name(field), format_type(&field.ty));
    let calls = format_calls(field);

    let inline = if calls.is_empty() {
        header.clone()
    } else {
        format!("{header}{}", calls.join(""))
    };
    let inline = match &field.default {
        Some(lit) => format!("{inline} = {}", format_literal(lit)),
        None => inline,
    };

    if calls.len() <= 3 && fits_inline(&inline, MAX_INLINE_WIDTH) {
        return inline;
    }

    // Break the chain: one rule per continuation line, indented one level past
    // the field. The default (if any) rides on the final continuation line.
    let mut out = header;
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
fn format_calls(field: &FieldDecl) -> Vec<String> {
    let mut calls = Vec::new();
    for t in &field.transformations {
        calls.push(format_transform(t));
    }
    for r in &field.validations {
        calls.push(format_rule(r));
    }
    calls
}

fn format_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::String => "string".to_string(),
        TypeRef::Int => "int".to_string(),
        TypeRef::Float => "float".to_string(),
        TypeRef::Boolean => "boolean".to_string(),
        TypeRef::Json => "json".to_string(),
        TypeRef::Timestamp => "timestamp".to_string(),
        TypeRef::Named(name) => name.clone(),
        TypeRef::Array(inner) => format!("{}[]", format_type(inner)),
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
        Rule::Min(n) => format!(".min({n})"),
        Rule::Max(n) => format!(".max({n})"),
        Rule::MinLen(n) => format!(".min_len({n})"),
        Rule::MaxLen(n) => format!(".max_len({n})"),
        Rule::Regex(pattern) => format!(".regex({})", format_literal(&Literal::String(pattern.clone()))),
        Rule::Email => ".email()".to_string(),
        Rule::Url => ".url()".to_string(),
        Rule::Uuid => ".uuid()".to_string(),
        Rule::Alphanumeric => ".alphanumeric()".to_string(),
        Rule::NonEmpty => ".nonempty()".to_string(),
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
export model User {
      name : string
      age: int .min(18)
}
"#;
        assert_eq!(
            fmt(src),
            "import { Address } from \"address\"\n\
             \nexport model User {\n\
             \x20 name: string\n\
             \x20 age: int.min(18)\n\
             }\n"
        );
    }

    #[test]
    fn breaks_long_rule_chains() {
        let src = "model User {\n  username: string .alphanumeric() .min(3) .max(20) .nonempty()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  username: string\n    .alphanumeric()\n    .min(3)\n    .max(20)\n    .nonempty()\n}\n"
        );
    }

    #[test]
    fn keeps_short_chains_inline() {
        let src = "model User {\n  email: string .trim() .lowercase() .email()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  email: string.trim().lowercase().email()\n}\n"
        );
    }

    #[test]
    fn formats_defaults_and_optional_fields() {
        let src = "model User { country : string = \"US\"\n  age?: int.min(18) }";
        assert_eq!(
            fmt(src),
            "model User {\n  country: string = \"US\"\n  age?: int.min(18)\n}\n"
        );
    }

    #[test]
    fn transforms_precede_validations() {
        let src = "model User {\n  email: string .email() .trim() .lowercase()\n}";
        let out = fmt(src);
        assert!(out.contains("email: string.trim().lowercase().email()"), "{out}");
    }

    #[test]
    fn escaping_round_trips_string_literals() {
        let src = r#"model User { slug: string .regex("^[a-z0-9-\"\\n]+$") }"#;
        let out = fmt(src);
        assert!(out.contains(".regex(\"^[a-z0-9-\\\"\\\\n]+$\")"), "{out}");
    }

    #[test]
    fn floats_keep_fractional_marker() {
        let src = "model M { ratio: float = 0.5\n  whole: float = 2.0 }";
        let out = fmt(src);
        assert!(out.contains("whole: float = 2.0"), "{out}");
    }

    #[test]
    fn output_is_idempotent() {
        let messy = r#"
import {A,B} from "geo";
model User {
  email : string .trim() .email()
  address: Address[]
  tags: string[] .nonempty()
  username: string .alphanumeric() .min(3) .max(20) .lowercase()
  created: timestamp = "2024-01-01T00:00:00Z"
  private: boolean
}
"#;
        let once = fmt(messy);
        assert_eq!(once, fmt(&once), "formatting must be idempotent");
    }

    #[test]
    fn empty_models_format_to_brace_pair() {
        assert_eq!(fmt("export model Empty { }"), "export model Empty {\n}\n");
    }

    #[test]
    fn invalid_axm_is_reported() {
        let err = format_axm("model {").expect_err("should fail to parse");
        assert!(!err.is_empty());
    }
}
