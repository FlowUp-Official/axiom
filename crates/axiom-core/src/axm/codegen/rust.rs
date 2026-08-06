//! Rust code generation for `.axm` models.
//!
//! Emits serde structs, reusable helper functions, and per-model `coerce`
//! validators that compose recursively. Every model exposes `safe_parse()` and
//! `parse()` (panics on failure). Only `std` and `serde` are used.
//!
//! Error paths are threaded as `Vec<PathSegment>` and rendered to strings only
//! inside `push_error()`, at reporting time.

use std::fmt::Write;

use crate::axm::ast::{FieldDecl, Literal, ModelDecl, Rule, Transform, TypeRef};
use crate::axm::codegen::{collect_uses, model_name, rule_message, Uses};
use crate::axm::resolver::ModelRegistry;
use crate::codegen::rust::REGEX_MATCHER;
use crate::codegen::util;

/// Generate the Rust module body for a set of models. Assumes the surrounding
/// output already defines `ValidationError` (which the SQL generator always
/// does), so it is reused rather than redefined.
pub fn generate_rust_models(registry: &ModelRegistry) -> String {
    if registry.is_empty() {
        return String::new();
    }

    let uses = collect_uses(registry);
    let mut out = String::new();
    out.push_str("\n// ---------------------------------------------------------------------------\n");
    out.push_str("// .axm models\n");
    out.push_str("// ---------------------------------------------------------------------------\n\n");

    emit_helpers(&mut out, &uses);

    for resolved in &registry.models {
        emit_struct(&mut out, &resolved.model);
    }
    for resolved in &registry.models {
        emit_impl_and_coerce(&mut out, &resolved.model);
    }

    if uses.regex {
        out.push('\n');
        out.push_str(&REGEX_MATCHER.replace("fn regex_is_match", "fn axm_regex_is_match"));
        out.push('\n');
    }

    out
}

fn emit_helpers(out: &mut String, uses: &Uses) {
    out.push_str("#[derive(Debug, Clone)]\n");
    out.push_str("pub enum PathSegment {\n");
    out.push_str("    Field(String),\n");
    out.push_str("    Index(usize),\n");
    out.push_str("}\n\n");

    out.push_str("fn render_path(path: &[PathSegment]) -> String {\n");
    out.push_str("    let mut out = String::new();\n");
    out.push_str("    for segment in path {\n");
    out.push_str("        match segment {\n");
    out.push_str("            PathSegment::Field(name) => {\n");
    out.push_str("                if out.is_empty() {\n");
    out.push_str("                    out.push_str(name);\n");
    out.push_str("                } else {\n");
    out.push_str("                    out.push('.');\n");
    out.push_str("                    out.push_str(name);\n");
    out.push_str("                }\n");
    out.push_str("            }\n");
    out.push_str("            PathSegment::Index(index) => {\n");
    out.push_str("                out.push('[');\n");
    out.push_str("                out.push_str(&index.to_string());\n");
    out.push_str("                out.push(']');\n");
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    out\n");
    out.push_str("}\n\n");

    out.push_str("fn push_error(errors: &mut Vec<ValidationError>, path: &[PathSegment], message: &str) {\n");
    out.push_str("    errors.push(ValidationError {\n");
    out.push_str("        path: render_path(path),\n");
    out.push_str("        message: message.to_string(),\n");
    out.push_str("    });\n");
    out.push_str("}\n\n");

    if uses.string {
        out.push_str("fn coerce_string(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> String {\n");
        out.push_str("    match value.as_str() {\n");
        out.push_str("        Some(s) => s.to_string(),\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected a string\");\n");
        out.push_str("            String::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.int {
        out.push_str("fn coerce_int(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> i64 {\n");
        out.push_str("    match value.as_i64() {\n");
        out.push_str("        Some(n) => n,\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected an integer\");\n");
        out.push_str("            0\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.float {
        out.push_str("fn coerce_float(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> f64 {\n");
        out.push_str("    match value.as_f64() {\n");
        out.push_str("        Some(n) => n,\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected a number\");\n");
        out.push_str("            0.0\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.boolean {
        out.push_str("fn coerce_boolean(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> bool {\n");
        out.push_str("    match value.as_bool() {\n");
        out.push_str("        Some(b) => b,\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected a boolean\");\n");
        out.push_str("            false\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.json {
        out.push_str("fn coerce_json(value: &serde_json::Value, _path: &mut Vec<PathSegment>, _errors: &mut Vec<ValidationError>) -> serde_json::Value {\n");
        out.push_str("    value.clone()\n");
        out.push_str("}\n\n");
    }

    if uses.array {
        out.push_str("fn coerce_array(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> Vec<serde_json::Value> {\n");
        out.push_str("    match value.as_array() {\n");
        out.push_str("        Some(arr) => arr.clone(),\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected an array\");\n");
        out.push_str("            Vec::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    if uses.timestamp {
        out.push_str("fn is_iso_timestamp(value: &str) -> bool {\n");
        out.push_str("    let bytes = value.as_bytes();\n");
        out.push_str("    if bytes.len() < 19 {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    for (i, &b) in bytes.iter().enumerate().take(19) {\n");
        out.push_str("        if matches!(i, 4 | 7 | 10 | 13 | 16) {\n");
        out.push_str("            continue;\n");
        out.push_str("        }\n");
        out.push_str("        if !b.is_ascii_digit() {\n");
        out.push_str("            return false;\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("    let mut i = 19;\n");
        out.push_str("    if bytes.get(i) == Some(&b'.') {\n");
        out.push_str("        i += 1;\n");
        out.push_str("        let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();\n");
        out.push_str("        if digits == 0 {\n");
        out.push_str("            return false;\n");
        out.push_str("        }\n");
        out.push_str("        i += digits;\n");
        out.push_str("    }\n");
        out.push_str("    match bytes.get(i) {\n");
        out.push_str("        None => return true,\n");
        out.push_str("        Some(b'Z') | Some(b'z') => i += 1,\n");
        out.push_str("        Some(b'+') | Some(b'-') => {\n");
        out.push_str("            i += 1;\n");
        out.push_str("            let rest = &bytes[i..];\n");
        out.push_str("            if rest.len() != 5 || rest[2] != b':' {\n");
        out.push_str("                return false;\n");
        out.push_str("            }\n");
        out.push_str("            if !rest.iter().enumerate().all(|(j, &b)| j == 2 || b.is_ascii_digit()) {\n");
        out.push_str("                return false;\n");
        out.push_str("            }\n");
        out.push_str("            i += 5;\n");
        out.push_str("        }\n");
        out.push_str("        _ => return false,\n");
        out.push_str("    }\n");
        out.push_str("    i == bytes.len()\n");
        out.push_str("}\n\n");

        out.push_str("fn coerce_timestamp(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> String {\n");
        out.push_str("    match value.as_str() {\n");
        out.push_str("        Some(s) if is_iso_timestamp(s) => s.to_string(),\n");
        out.push_str("        _ => {\n");
        out.push_str("            push_error(errors, path, \"expected an ISO 8601 timestamp\");\n");
        out.push_str("            String::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    if uses.email {
        out.push_str("fn check_email(value: &str) -> bool {\n");
        out.push_str("    if value.contains(char::is_whitespace) {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    let mut parts = value.split('@');\n");
        out.push_str("    let Some(local) = parts.next() else { return false; };\n");
        out.push_str("    let Some(domain) = parts.next() else { return false; };\n");
        out.push_str("    if parts.next().is_some() {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    !local.is_empty() && !domain.is_empty() && domain.contains('.')\n");
        out.push_str("}\n\n");
    }
    if uses.url {
        out.push_str("fn check_url(value: &str) -> bool {\n");
        out.push_str("    if value.contains(char::is_whitespace) {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    let rest = value\n");
        out.push_str("        .strip_prefix(\"http://\")\n");
        out.push_str("        .or_else(|| value.strip_prefix(\"https://\"));\n");
        out.push_str("    match rest {\n");
        out.push_str("        Some(rest) => !rest.is_empty() && (rest.contains('.') || rest.starts_with(\"localhost\")),\n");
        out.push_str("        None => false,\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.uuid {
        out.push_str("fn check_uuid(value: &str) -> bool {\n");
        out.push_str("    let bytes = value.as_bytes();\n");
        out.push_str("    if bytes.len() != 36 {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    for (i, &b) in bytes.iter().enumerate() {\n");
        out.push_str("        match i {\n");
        out.push_str("            8 | 13 | 18 | 23 => {\n");
        out.push_str("                if b != b'-' {\n");
        out.push_str("                    return false;\n");
        out.push_str("                }\n");
        out.push_str("            }\n");
        out.push_str("            _ => {\n");
        out.push_str("                if !b.is_ascii_hexdigit() {\n");
        out.push_str("                    return false;\n");
        out.push_str("                }\n");
        out.push_str("            }\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("    true\n");
        out.push_str("}\n\n");
    }
    if uses.alphanumeric {
        out.push_str("fn check_alphanumeric(value: &str) -> bool {\n");
        out.push_str("    !value.is_empty() && value.chars().all(|c| c.is_alphanumeric())\n");
        out.push_str("}\n\n");
    }
    if uses.nonempty {
        out.push_str("fn check_nonempty(value: &str) -> bool {\n");
        out.push_str("    !value.is_empty()\n");
        out.push_str("}\n\n");
    }
}

fn emit_struct(out: &mut String, model: &ModelDecl) {
    let type_name = model_name(model);
    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]"
    );
    let _ = writeln!(out, "pub struct {type_name} {{");
    for field in &model.fields {
        let rust_name = util::rust_field_name(&field.name);
        let ty = rust_field_type(&field.ty, field.optional);
        let _ = writeln!(out, "    pub {rust_name}: {ty},");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_impl_and_coerce(out: &mut String, model: &ModelDecl) {
    let type_name = model_name(model);
    let coerce_name = coerce_fn_name(&model.name);

    let _ = writeln!(out, "impl {type_name} {{");
    let _ = writeln!(
        out,
        "    pub fn safe_parse(value: &serde_json::Value) -> Result<{type_name}, Vec<ValidationError>> {{"
    );
    let _ = writeln!(out, "        let mut errors: Vec<ValidationError> = Vec::new();");
    let _ = writeln!(out, "        let mut path: Vec<PathSegment> = Vec::new();");
    let _ = writeln!(out, "        let out = {coerce_name}(value, &mut path, &mut errors);");
    let _ = writeln!(out, "        if errors.is_empty() {{");
    let _ = writeln!(out, "            Ok(out)");
    let _ = writeln!(out, "        }} else {{");
    let _ = writeln!(out, "            Err(errors)");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}\n");
    let _ = writeln!(out, "    pub fn parse(value: &serde_json::Value) -> {type_name} {{");
    let _ = writeln!(out, "        match Self::safe_parse(value) {{");
    let _ = writeln!(out, "            Ok(value) => value,");
    let _ = writeln!(
        out,
        "            Err(errors) => panic!(\"{type_name} validation failed: {{errors:?}}\"),"
    );
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}\n");

    let _ = writeln!(
        out,
        "fn {coerce_name}(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> {type_name} {{"
    );
    let _ = writeln!(out, "    let mut out = {type_name}::default();");
    let _ = writeln!(out, "    let Some(record) = value.as_object() else {{");
    let _ = writeln!(out, "        push_error(errors, path, \"expected an object\");");
    let _ = writeln!(out, "        return out;");
    let _ = writeln!(out, "    }};");
    for field in &model.fields {
        emit_field(out, field, 4);
    }
    let _ = writeln!(out, "    out");
    let _ = writeln!(out, "}}\n");
}

fn emit_field(out: &mut String, field: &FieldDecl, base_indent: usize) {
    let pad = " ".repeat(base_indent);
    let key = &field.name;

    match &field.default {
        Some(literal) => {
            let _ = writeln!(
                out,
                "{pad}let raw = record.get(\"{key}\").cloned().unwrap_or_else(|| {});",
                rust_json_literal(literal)
            );
            let _ = writeln!(out, "{pad}{{");
            emit_field_body(out, field, base_indent + 4);
            let _ = writeln!(out, "{pad}}}");
        }
        None if field.optional => {
            let _ = writeln!(out, "{pad}if let Some(raw) = record.get(\"{key}\") {{");
            emit_field_body(out, field, base_indent + 4);
            let _ = writeln!(out, "{pad}}}");
        }
        None => {
            let _ = writeln!(out, "{pad}match record.get(\"{key}\") {{");
            let _ = writeln!(out, "{pad}    Some(raw) => {{");
            emit_field_body(out, field, base_indent + 8);
            let _ = writeln!(out, "{pad}    }}");
            let _ = writeln!(out, "{pad}    None => {{");
            let _ = writeln!(
                out,
                "{pad}        path.push(PathSegment::Field(\"{key}\".to_string()));"
            );
            let _ = writeln!(out, "{pad}        push_error(errors, path, \"field is required\");");
            let _ = writeln!(out, "{pad}        path.pop();");
            let _ = writeln!(out, "{pad}    }}");
            let _ = writeln!(out, "{pad}}}");
        }
    }
}

fn emit_field_body(out: &mut String, field: &FieldDecl, base_indent: usize) {
    let pad = " ".repeat(base_indent);
    let key = &field.name;
    let rust_name = util::rust_field_name(&field.name);
    let optional = field.optional;
    let raw_expr = if field.default.is_some() { "&raw" } else { "raw" };

    let _ = writeln!(
        out,
        "{pad}path.push(PathSegment::Field(\"{key}\".to_string()));"
    );

    match &field.ty {
        TypeRef::Array(inner) => {
            let _ = writeln!(out, "{pad}let base = coerce_array({raw_expr}, path, errors);");
            let _ = writeln!(out, "{pad}let mut items = Vec::with_capacity(base.len());");
            let _ = writeln!(out, "{pad}for (index, entry) in base.into_iter().enumerate() {{");
            let _ = writeln!(out, "{pad}    path.push(PathSegment::Index(index));");
            let _ = writeln!(
                out,
                "{pad}    items.push({});",
                rust_coerce_value_expr(inner, "&entry")
            );
            let _ = writeln!(out, "{pad}    path.pop();");
            let _ = writeln!(out, "{pad}}}");
            let assign = if optional {
                "Some(items)".to_string()
            } else {
                "items".to_string()
            };
            let _ = writeln!(out, "{pad}out.{rust_name} = {assign};");
        }
        _ => {
            let _ = writeln!(
                out,
                "{pad}let base = {};",
                rust_coerce_value_expr(&field.ty, raw_expr)
            );
            if field.transformations.is_empty() {
                let _ = writeln!(out, "{pad}let value = base;");
            } else {
                let chain: String = field
                    .transformations
                    .iter()
                    .map(rust_transform_op)
                    .collect();
                let _ = writeln!(out, "{pad}let value = base{chain}.to_string();");
            }
            for rule in &field.validations {
                let msg = util::escape_rust(&rule_message(rule));
                let condition = rust_rule_condition(rule, "value", &field.ty);
                let _ = writeln!(
                    out,
                    "{pad}if {condition} {{ push_error(errors, path, \"{msg}\"); }}"
                );
            }
            let assign = if optional {
                "Some(value)".to_string()
            } else {
                "value".to_string()
            };
            let _ = writeln!(out, "{pad}out.{rust_name} = {assign};");
        }
    }

    let _ = writeln!(out, "{pad}path.pop();");
}

fn rust_transform_op(transform: &Transform) -> &'static str {
    match transform {
        Transform::Trim => ".trim()",
        Transform::Lowercase => ".to_lowercase()",
        Transform::Uppercase => ".to_uppercase()",
    }
}

fn rust_rule_condition(rule: &Rule, value: &str, ty: &TypeRef) -> String {
    match rule {
        Rule::Email => format!("!check_email(&{value})"),
        Rule::Url => format!("!check_url(&{value})"),
        Rule::Uuid => format!("!check_uuid(&{value})"),
        Rule::Alphanumeric => format!("!check_alphanumeric(&{value})"),
        Rule::NonEmpty => format!("!check_nonempty(&{value})"),
        Rule::Min(n) => {
            let lit = if matches!(ty, TypeRef::Float) {
                format!("{n}.0")
            } else {
                n.to_string()
            };
            format!("{value} < {lit}")
        }
        Rule::Max(n) => {
            let lit = if matches!(ty, TypeRef::Float) {
                format!("{n}.0")
            } else {
                n.to_string()
            };
            format!("{value} > {lit}")
        }
        Rule::MinLen(n) => format!("{value}.chars().count() < {n}"),
        Rule::MaxLen(n) => format!("{value}.chars().count() > {n}"),
        Rule::Regex(pattern) => format!(
            "!axm_regex_is_match(\"{}\", &{value})",
            util::escape_rust(pattern)
        ),
    }
}

fn coerce_fn_name(model_name: &str) -> String {
    format!("coerce_{}", util::rust_field_name(model_name))
}

fn rust_field_type(ty: &TypeRef, optional: bool) -> String {
    let base = match ty {
        TypeRef::String | TypeRef::Timestamp => "String".to_string(),
        TypeRef::Int => "i64".to_string(),
        TypeRef::Float => "f64".to_string(),
        TypeRef::Boolean => "bool".to_string(),
        TypeRef::Json => "serde_json::Value".to_string(),
        TypeRef::Named(name) => util::pascal_case(name),
        TypeRef::Array(inner) => format!("Vec<{}>", rust_field_type(inner, false)),
    };
    if optional {
        format!("Option<{base}>")
    } else {
        base
    }
}

fn rust_coerce_value_expr(ty: &TypeRef, value: &str) -> String {
    match ty {
        TypeRef::String => format!("coerce_string({value}, path, errors)"),
        TypeRef::Int => format!("coerce_int({value}, path, errors)"),
        TypeRef::Float => format!("coerce_float({value}, path, errors)"),
        TypeRef::Boolean => format!("coerce_boolean({value}, path, errors)"),
        TypeRef::Json => format!("coerce_json({value}, path, errors)"),
        TypeRef::Timestamp => format!("coerce_timestamp({value}, path, errors)"),
        TypeRef::Named(name) => {
            format!("coerce_{}({value}, path, errors)", util::rust_field_name(name))
        }
        TypeRef::Array(_) => unreachable!("array coercion is emitted by the field body"),
    }
}

fn rust_json_literal(literal: &Literal) -> String {
    match literal {
        Literal::String(s) => format!("serde_json::json!({})", format_rust_string(s)),
        Literal::Int(n) => format!("serde_json::json!({n})"),
        Literal::Float(f) => format!("serde_json::json!({f})"),
        Literal::Bool(b) => format!("serde_json::json!({b})"),
    }
}

fn format_rust_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axm::parser::parse_axm_file;

    fn registry(src: &str) -> ModelRegistry {
        let file = parse_axm_file(src).expect("parse");
        ModelRegistry {
            models: vec![crate::axm::resolver::ResolvedModel {
                path: std::path::PathBuf::from("models/test.axm"),
                model: file.models[0].clone(),
            }],
            index: [(file.models[0].name.clone(), 0usize)].into_iter().collect(),
        }
    }

    #[test]
    fn emits_struct_with_serde_and_helpers() {
        let out = generate_rust_models(&registry(
            "export model User {\n  email: string .email()\n  age: int\n}",
        ));
        assert!(out.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]"));
        assert!(out.contains("pub struct User {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub age: i64,"));
        assert!(out.contains("fn coerce_string("));
        assert!(out.contains("fn check_email("));
        assert!(out.contains("fn coerce_user("));
    }

    #[test]
    fn emits_safe_parse_and_parse() {
        let out = generate_rust_models(&registry(
            "export model User {\n  email: string\n}",
        ));
        assert!(out.contains("pub fn safe_parse(value: &serde_json::Value) -> Result<User, Vec<ValidationError>>"));
        assert!(out.contains("pub fn parse(value: &serde_json::Value) -> User {"));
        assert!(out.contains("panic!(\"User validation failed: {errors:?}\")"));
    }

    #[test]
    fn emits_transforms_before_validation() {
        let out = generate_rust_models(&registry(
            "export model User {\n  email: string .trim() .lowercase() .email()\n}",
        ));
        assert!(out.contains("let value = base.trim().to_lowercase().to_string();"));
        assert!(out.contains("if !check_email(&value) { push_error(errors, path, \"must be a valid email address\"); }"));
    }

    #[test]
    fn optional_fields_are_option() {
        let out = generate_rust_models(&registry(
            "export model User {\n  age?: int .min(18)\n  country: string = \"US\"\n}",
        ));
        assert!(out.contains("pub age: Option<i64>,"));
        assert!(out.contains("if let Some(raw) = record.get(\"age\")"));
        assert!(out.contains("out.age = Some(value);"));
        assert!(out.contains("let raw = record.get(\"country\").cloned().unwrap_or_else(|| serde_json::json!(\"US\"));"));
    }

    #[test]
    fn required_fields_report_missing() {
        let out = generate_rust_models(&registry(
            "export model User {\n  email: string\n}",
        ));
        assert!(out.contains("push_error(errors, path, \"field is required\");"));
    }

    #[test]
    fn recursive_array_validation() {
        let out = generate_rust_models(&registry(
            "export model User {\n  history: Address[]\n}",
        ));
        assert!(out.contains("pub history: Vec<Address>,"));
        assert!(out.contains("path.push(PathSegment::Index(index));"));
        assert!(out.contains("items.push(coerce_address(&entry, path, errors));"));
    }

    #[test]
    fn error_paths_render_arrays_and_fields() {
        let out = generate_rust_models(&registry(
            "export model User {\n  history: Address[]\n  email: string .email()\n}",
        ));
        assert!(out.contains("pub enum PathSegment {"));
        assert!(out.contains("Field(String),"));
        assert!(out.contains("Index(usize),"));
        assert!(out.contains("path: render_path(path),"));
    }

    #[test]
    fn unused_helpers_are_not_emitted() {
        let out = generate_rust_models(&registry(
            "export model User {\n  name: string .nonempty()\n}",
        ));
        assert!(out.contains("fn check_nonempty("));
        assert!(!out.contains("fn check_uuid("));
        assert!(!out.contains("fn coerce_timestamp("));
        assert!(!out.contains("fn coerce_int("));
        assert!(!out.contains("fn coerce_boolean("));
        assert!(!out.contains("fn coerce_array("));
    }

    #[test]
    fn empty_registry_generates_nothing() {
        assert_eq!(generate_rust_models(&ModelRegistry::default()), "");
    }
}
