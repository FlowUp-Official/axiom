//! Rust code generation for `.axm` models, type aliases, and queries.
//!
//! Emits serde structs, reusable helper functions, per-model `coerce`
//! validators that compose recursively, `coerce` validators for type aliases
//! that carry methods, and sqlx-backed query wrappers. Every model exposes
//! `safe_parse()` and `parse()` (panics on failure). Only `std`, `serde`, and
//! `sqlx` are used.
//!
//! Error paths are threaded as `Vec<PathSegment>` and rendered to strings only
//! inside `push_error()`, at reporting time.

use std::fmt::Write;
use std::path::Path;

use crate::axm::ast::{AnnotatedType, Literal, QueryReturn, Rule, Transform, TypeRef};
use crate::axm::codegen::{
    NamedKind, Uses, collect_uses, effective_fields, inline_annotated, model_name, named_kind,
    rule_message,
};
use crate::axm::resolver::ModelRegistry;
use crate::catalog::TableCatalog;
use crate::codegen::rust::REGEX_MATCHER;
use crate::codegen::util;

/// Generate the Rust module body for a registry. Assumes the surrounding
/// output already defines `ValidationError` (which the SQL generator always
/// does), so it is reused rather than redefined.
pub fn generate_rust_models(registry: &ModelRegistry, catalog: &TableCatalog) -> String {
    if registry.is_empty() {
        return String::new();
    }

    let uses = collect_uses(registry, catalog);
    let mut out = String::new();
    out.push_str(
        "\n// ---------------------------------------------------------------------------\n",
    );
    out.push_str("// .axm models\n");
    out.push_str(
        "// ---------------------------------------------------------------------------\n\n",
    );

    emit_helpers(&mut out, &uses);

    for resolved in &registry.types {
        emit_type_alias(
            &mut out,
            registry,
            &resolved.path,
            &resolved.ty.name,
            &resolved.ty.ty,
        );
    }
    for resolved in &registry.models {
        let fields = effective_fields(registry, catalog, &resolved.path, &resolved.model);
        emit_struct(&mut out, registry, &resolved.path, &resolved.model, &fields);
    }
    for resolved in &registry.models {
        let fields = effective_fields(registry, catalog, &resolved.path, &resolved.model);
        emit_impl_and_coerce(&mut out, registry, &resolved.path, &resolved.model, &fields);
    }
    for resolved in &registry.types {
        emit_alias_coerce(
            &mut out,
            registry,
            &resolved.path,
            &resolved.ty.name,
            &resolved.ty.ty,
        );
    }
    for resolved in &registry.queries {
        emit_query(&mut out, registry, &resolved.path, &resolved.query);
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

    out.push_str(
        "fn push_error(errors: &mut Vec<ValidationError>, path: &[PathSegment], message: &str) {\n",
    );
    out.push_str("    errors.push(ValidationError {\n");
    out.push_str("        path: render_path(path),\n");
    out.push_str("        message: message.to_string(),\n");
    out.push_str("    });\n");
    out.push_str("}\n\n");

    if uses.string {
        emit_coerce_helper(out, "coerce_string", "String", "expected a string", false);
    }
    if uses.int {
        emit_coerce_helper(out, "coerce_int", "i64", "expected an integer", false);
    }
    if uses.bigint {
        emit_coerce_helper(out, "coerce_bigint", "i64", "expected an integer", true);
    }
    if uses.float {
        emit_coerce_helper(out, "coerce_float", "f64", "expected a number", false);
    }
    if uses.boolean {
        emit_coerce_helper(out, "coerce_boolean", "bool", "expected a boolean", false);
    }
    if uses.json {
        out.push_str("fn coerce_json(value: &serde_json::Value, _path: &mut Vec<PathSegment>, _errors: &mut Vec<ValidationError>) -> serde_json::Value {\n");
        out.push_str("    value.clone()\n");
        out.push_str("}\n\n");
    }
    if uses.date {
        out.push_str("fn check_isodate(value: &str) -> bool {\n");
        out.push_str("    let bytes = value.as_bytes();\n");
        out.push_str("    bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-'\n");
        out.push_str("        && bytes.iter().enumerate().all(|(i, &b)| matches!(i, 4 | 7) || b.is_ascii_digit())\n");
        out.push_str("}\n\n");
        out.push_str("fn coerce_date(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> String {\n");
        out.push_str("    match value.as_str() {\n");
        out.push_str("        Some(s) if check_isodate(s) => s.to_string(),\n");
        out.push_str("        _ => {\n");
        out.push_str("            push_error(errors, path, \"expected an ISO 8601 date\");\n");
        out.push_str("            String::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.datetime {
        out.push_str("fn is_iso_timestamp(value: &str) -> bool {\n");
        let body = is_iso_timestamp_body();
        for line in body {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');

        out.push_str("fn coerce_datetime(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> String {\n");
        out.push_str("    match value.as_str() {\n");
        out.push_str("        Some(s) if is_iso_timestamp(s) => s.to_string(),\n");
        out.push_str("        _ => {\n");
        out.push_str("            push_error(errors, path, \"expected an ISO 8601 timestamp\");\n");
        out.push_str("            String::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    if uses.bytes {
        out.push_str("fn coerce_bytes(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> Vec<u8> {\n");
        out.push_str("    match value.as_array() {\n");
        out.push_str("        Some(arr) => {\n");
        out.push_str("            let mut out = Vec::with_capacity(arr.len());\n");
        out.push_str("            for item in arr {\n");
        out.push_str("                match item.as_u64() {\n");
        out.push_str("                    Some(n) if n <= 255 => out.push(n as u8),\n");
        out.push_str("                    _ => {\n");
        out.push_str(
            "                        push_error(errors, path, \"expected a byte sequence\");\n",
        );
        out.push_str("                        return Vec::new();\n");
        out.push_str("                    }\n");
        out.push_str("                }\n");
        out.push_str("            }\n");
        out.push_str("            out\n");
        out.push_str("        }\n");
        out.push_str("        None => {\n");
        out.push_str("            push_error(errors, path, \"expected a byte sequence\");\n");
        out.push_str("            Vec::new()\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
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
    if uses.ulid {
        out.push_str("fn check_ulid(value: &str) -> bool {\n");
        out.push_str("    let bytes = value.as_bytes();\n");
        out.push_str("    if bytes.len() != 26 {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    value.chars().all(|c| c.is_ascii_uppercase() && \"0123456789ABCDEFGHJKMNPQRSTVWXYZ\".contains(c))\n");
        out.push_str("}\n\n");
    }
    if uses.ipv4 {
        out.push_str("fn check_ipv4(value: &str) -> bool {\n");
        out.push_str("    let parts: Vec<&str> = value.split('.').collect();\n");
        out.push_str("    if parts.len() != 4 {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    parts.iter().all(|p| match p.parse::<u16>() {\n");
        out.push_str("        Ok(n) => n <= 255,\n");
        out.push_str("        Err(_) => false,\n");
        out.push_str("    })\n");
        out.push_str("}\n\n");
    }
    if uses.ipv6 {
        out.push_str("fn check_ipv6(value: &str) -> bool {\n");
        out.push_str("    if value.contains(\":::\") || value.starts_with(':') && !value.starts_with(\"::\") || value.ends_with(':') && !value.ends_with(\"::\") {\n");
        out.push_str("        return false;\n");
        out.push_str("    }\n");
        out.push_str("    value.split(\"::\").count() <= 2 && value\n");
        out.push_str("        .split([\":\", \"::\"].as_ref())\n");
        out.push_str("        .filter(|group| !group.is_empty())\n");
        out.push_str("        .all(|group| !group.contains('.') && group.len() <= 4 && group.chars().all(|c| c.is_ascii_hexdigit()))\n");
        out.push_str("}\n\n");
    }
    if uses.isodate {
        out.push_str("fn check_iso_date(value: &str) -> bool {\n");
        out.push_str("    let bytes = value.as_bytes();\n");
        out.push_str("    bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-'\n");
        out.push_str("        && bytes.iter().enumerate().all(|(i, &b)| matches!(i, 4 | 7) || b.is_ascii_digit())\n");
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

/// The body of the ISO timestamp validator, line by line (mirrors the old
/// SQL generator's `is_iso_timestamp`).
fn is_iso_timestamp_body() -> Vec<&'static str> {
    vec![
        "    let bytes = value.as_bytes();",
        "    if bytes.len() < 19 {",
        "        return false;",
        "    }",
        "    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {",
        "        return false;",
        "    }",
        "    for (i, &b) in bytes.iter().enumerate().take(19) {",
        "        if matches!(i, 4 | 7 | 10 | 13 | 16) {",
        "            continue;",
        "        }",
        "        if !b.is_ascii_digit() {",
        "            return false;",
        "        }",
        "    }",
        "    let mut i = 19;",
        "    if bytes.get(i) == Some(&b'.') {",
        "        i += 1;",
        "        let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();",
        "        if digits == 0 {",
        "            return false;",
        "        }",
        "        i += digits;",
        "    }",
        "    match bytes.get(i) {",
        "        None => return true,",
        "        Some(b'Z') | Some(b'z') => i += 1,",
        "        Some(b'+') | Some(b'-') => {",
        "            i += 1;",
        "            let rest = &bytes[i..];",
        "            if rest.len() != 5 || rest[2] != b':' {",
        "                return false;",
        "            }",
        "            if !rest.iter().enumerate().all(|(j, &b)| j == 2 || b.is_ascii_digit()) {",
        "                return false;",
        "            }",
        "            i += 5;",
        "        }",
        "        _ => return false,",
        "    }",
        "    i == bytes.len()",
        "}",
    ]
}

fn emit_coerce_helper(out: &mut String, name: &str, ty: &str, message: &str, allow_u64: bool) {
    let _ = writeln!(
        out,
        "fn {name}(value: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> {ty} {{"
    );
    match (name, allow_u64) {
        ("coerce_boolean", _) => {
            out.push_str("    match value.as_bool() {\n");
            out.push_str("        Some(b) => b,\n");
        }
        ("coerce_string", _) => {
            out.push_str("    match value.as_str() {\n");
            out.push_str("        Some(s) => s.to_string(),\n");
        }
        ("coerce_int", false) | ("coerce_bigint", _) => {
            let _ = writeln!(out, "    match value.as_i64() {{");
            out.push_str("        Some(n) => n,\n");
        }
        ("coerce_float", _) => {
            out.push_str("    match value.as_f64() {\n");
            out.push_str("        Some(n) => n,\n");
        }
        _ => unreachable!(),
    }
    let _ = writeln!(out, "        None => {{");
    let _ = writeln!(out, "            push_error(errors, path, \"{message}\");");
    let default = match ty {
        "String" => "String::new()",
        "i64" => "0",
        "f64" => "0.0",
        "bool" => "false",
        _ => unreachable!(),
    };
    let _ = writeln!(out, "            {default}");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}\n");
}

fn emit_type_alias(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    declared: &str,
    ann: &AnnotatedType,
) {
    let name = util::pascal_case(registry.effective_name(path, declared));
    let ty = rust_named_type(registry, path, &ann.base);
    let _ = writeln!(out, "pub type {name} = {ty};");
}

fn emit_alias_coerce(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    declared: &str,
    ann: &AnnotatedType,
) {
    let inlined = inline_annotated(registry, path, ann);
    if inlined.transforms.is_empty() && inlined.rules.is_empty() {
        return;
    }
    let result_ty = rust_named_type(registry, path, &inlined.base);
    let _ = writeln!(
        out,
        "fn coerce_{}(anchor: &serde_json::Value, path: &mut Vec<PathSegment>, errors: &mut Vec<ValidationError>) -> {result_ty} {{",
        util::rust_field_name(registry.effective_name(path, declared))
    );
    emit_annotated_value(out, registry, path, &inlined, "anchor", 4);
    let _ = writeln!(out, "    value");
    let _ = writeln!(out, "}}\n");
}

fn emit_struct(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    model: &crate::axm::ast::ModelDecl,
    fields: &[crate::axm::codegen::EffectiveField],
) {
    let type_name = model_name(model);
    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, sqlx::FromRow)]"
    );
    let _ = writeln!(out, "pub struct {type_name} {{");
    for field in fields {
        let rust_name = util::rust_field_name(&field.emitted_name);
        let ty = rust_field_type(registry, path, field);
        let _ = writeln!(out, "    pub {rust_name}: {ty},");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_impl_and_coerce(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    model: &crate::axm::ast::ModelDecl,
    fields: &[crate::axm::codegen::EffectiveField],
) {
    let type_name = model_name(model);
    let coerce_name = coerce_fn_name(&model.name);

    let _ = writeln!(out, "impl {type_name} {{");
    let _ = writeln!(
        out,
        "    pub fn safe_parse(value: &serde_json::Value) -> Result<{type_name}, Vec<ValidationError>> {{"
    );
    let _ = writeln!(
        out,
        "        let mut errors: Vec<ValidationError> = Vec::new();"
    );
    let _ = writeln!(out, "        let mut path: Vec<PathSegment> = Vec::new();");
    let _ = writeln!(
        out,
        "        let out = {coerce_name}(value, &mut path, &mut errors);"
    );
    let _ = writeln!(out, "        if errors.is_empty() {{");
    let _ = writeln!(out, "            Ok(out)");
    let _ = writeln!(out, "        }} else {{");
    let _ = writeln!(out, "            Err(errors)");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}\n");
    let _ = writeln!(
        out,
        "    pub fn parse(value: &serde_json::Value) -> {type_name} {{"
    );
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
    let _ = writeln!(
        out,
        "        push_error(errors, path, \"expected an object\");"
    );
    let _ = writeln!(out, "        return out;");
    let _ = writeln!(out, "    }};");
    for field in fields {
        emit_field(out, registry, path, field, 4);
    }
    let _ = writeln!(out, "    out");
    let _ = writeln!(out, "}}\n");
}

fn emit_field(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    field: &crate::axm::codegen::EffectiveField,
    base_indent: usize,
) {
    let pad = " ".repeat(base_indent);
    let key = util::escape_rust(&field.emitted_name);

    match &field.default {
        Some(literal) => {
            let _ = writeln!(
                out,
                "{pad}let raw = record.get(\"{key}\").cloned().unwrap_or_else(|| {});",
                rust_json_literal(literal)
            );
            let _ = writeln!(out, "{pad}{{");
            emit_field_body(out, registry, path, field, base_indent + 4);
            let _ = writeln!(out, "{pad}}}");
        }
        None if field.optional => {
            let _ = writeln!(out, "{pad}if let Some(raw) = record.get(\"{key}\") {{");
            emit_field_body(out, registry, path, field, base_indent + 4);
            let _ = writeln!(out, "{pad}}}");
        }
        None => {
            let _ = writeln!(out, "{pad}match record.get(\"{key}\") {{");
            let _ = writeln!(out, "{pad}    Some(raw) => {{");
            emit_field_body(out, registry, path, field, base_indent + 8);
            let _ = writeln!(out, "{pad}    }}");
            let _ = writeln!(out, "{pad}    None => {{");
            let _ = writeln!(
                out,
                "{pad}        path.push(PathSegment::Field(\"{key}\".to_string()));"
            );
            let _ = writeln!(
                out,
                "{pad}        push_error(errors, path, \"field is required\");"
            );
            let _ = writeln!(out, "{pad}        path.pop();");
            let _ = writeln!(out, "{pad}    }}");
            let _ = writeln!(out, "{pad}}}");
        }
    }
}

fn emit_field_body(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    field: &crate::axm::codegen::EffectiveField,
    base_indent: usize,
) {
    let pad = " ".repeat(base_indent);
    let key = util::escape_rust(&field.emitted_name);
    let rust_name = util::rust_field_name(&field.emitted_name);
    let optional = field.optional;
    let raw_expr = if field.default.is_some() {
        "&raw"
    } else {
        "raw"
    };

    let _ = writeln!(
        out,
        "{pad}path.push(PathSegment::Field(\"{key}\".to_string()));"
    );

    match &field.annotated.base {
        TypeRef::Nullable(inner) => {
            let _ = writeln!(out, "{pad}if {raw_expr}.is_null() {{");
            let _ = writeln!(out, "{pad}    out.{rust_name} = None;");
            let _ = writeln!(out, "{pad}}} else {{");
            let nested = AnnotatedType {
                base: (**inner).clone(),
                transforms: field.annotated.transforms.clone(),
                rules: field.annotated.rules.clone(),
            };
            emit_annotated_value(out, registry, path, &nested, raw_expr, base_indent + 4);
            let _ = writeln!(out, "{pad}    out.{rust_name} = Some(value);");
            let _ = writeln!(out, "{pad}}}");
            let _ = writeln!(out, "{pad}path.pop();");
        }
        TypeRef::Array(inner) => {
            let _ = writeln!(
                out,
                "{pad}let base = coerce_array({raw_expr}, path, errors);"
            );
            let _ = writeln!(out, "{pad}let mut items = Vec::with_capacity(base.len());");
            let _ = writeln!(
                out,
                "{pad}for (index, entry) in base.into_iter().enumerate() {{"
            );
            let _ = writeln!(out, "{pad}    path.push(PathSegment::Index(index));");
            let _ = writeln!(
                out,
                "{pad}    items.push({});",
                rust_coerce_value_expr(registry, path, inner, "&entry")
            );
            let _ = writeln!(out, "{pad}    path.pop();");
            let _ = writeln!(out, "{pad}}}");
            for rule in &field.annotated.rules {
                let msg = util::escape_rust(&rule_message(rule));
                let condition = rust_rule_condition(rule, "items", &field.annotated.base);
                let _ = writeln!(
                    out,
                    "{pad}if {condition} {{ push_error(errors, path, \"{msg}\"); }}"
                );
            }
            let assign = if optional {
                "Some(items)".to_string()
            } else {
                "items".to_string()
            };
            let _ = writeln!(out, "{pad}out.{rust_name} = {assign};");
            let _ = writeln!(out, "{pad}path.pop();");
        }
        _ => {
            emit_annotated_value(out, registry, path, &field.annotated, raw_expr, base_indent);
            let assign = if optional {
                "Some(value)".to_string()
            } else {
                "value".to_string()
            };
            let _ = writeln!(out, "{pad}out.{rust_name} = {assign};");
            let _ = writeln!(out, "{pad}path.pop();");
        }
    }
}

/// Emit statements that bind `value` from the expression `raw` (a
/// `&serde_json::Value`) applying the annotated type's transforms and rules.
fn emit_annotated_value(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    ann: &AnnotatedType,
    raw: &str,
    base_indent: usize,
) {
    let pad = " ".repeat(base_indent);
    let _ = writeln!(
        out,
        "{pad}let base = {};",
        rust_coerce_value_expr(registry, path, &ann.base, raw)
    );
    if ann.transforms.is_empty() {
        let _ = writeln!(out, "{pad}let value = base;");
    } else {
        let chain: String = ann.transforms.iter().map(rust_transform_op).collect();
        let _ = writeln!(out, "{pad}let value = base{chain}.to_string();");
    }
    for rule in &ann.rules {
        let msg = util::escape_rust(&rule_message(rule));
        let condition = rust_rule_condition(rule, "value", &ann.base);
        let _ = writeln!(
            out,
            "{pad}if {condition} {{ push_error(errors, path, \"{msg}\"); }}"
        );
    }
}

fn rust_transform_op(transform: &Transform) -> &'static str {
    match transform {
        Transform::Trim => ".trim()",
        Transform::Lowercase => ".to_lowercase()",
        Transform::Uppercase => ".to_uppercase()",
    }
}

fn rust_rule_condition(rule: &Rule, value: &str, ty: &TypeRef) -> String {
    let numeric = |n: i64| -> String {
        if matches!(ty, TypeRef::Float) {
            format!("{n}.0")
        } else {
            n.to_string()
        }
    };
    match rule {
        // Collection-level rules over arrays: `NonEmpty` / `MinLength` /
        // `MaxLength` inspect the built vector.
        Rule::NonEmpty(_) if matches!(ty, TypeRef::Array(_)) => {
            format!("{value}.is_empty()")
        }
        Rule::MinLength(n, _) if matches!(ty, TypeRef::Array(_)) => {
            format!("{value}.len() < {n}")
        }
        Rule::MaxLength(n, _) if matches!(ty, TypeRef::Array(_)) => {
            format!("{value}.len() > {n}")
        }
        Rule::Email(_) => format!("!check_email(&{value})"),
        Rule::Url(_) => format!("!check_url(&{value})"),
        Rule::Uuid(_) => format!("!check_uuid(&{value})"),
        Rule::Ulid(_) => format!("!check_ulid(&{value})"),
        Rule::Ipv4(_) => format!("!check_ipv4(&{value})"),
        Rule::Ipv6(_) => format!("!check_ipv6(&{value})"),
        Rule::IsoDate(_) => format!("!check_iso_date(&{value})"),
        Rule::Alphanumeric(_) => format!("!check_alphanumeric(&{value})"),
        Rule::NonEmpty(_) => format!("!check_nonempty(&{value})"),
        Rule::Min(n, _) => format!("{value} < {}", numeric(*n)),
        Rule::Max(n, _) => format!("{value} > {}", numeric(*n)),
        Rule::MinLength(n, _) => format!("{value}.chars().count() < {n}"),
        Rule::MaxLength(n, _) => format!("{value}.chars().count() > {n}"),
        Rule::Regex(pattern, _) => format!(
            "!axm_regex_is_match(\"{}\", &{value})",
            util::escape_rust(pattern)
        ),
    }
}

fn coerce_fn_name(model_name: &str) -> String {
    format!("coerce_{}", util::rust_field_name(model_name))
}

fn rust_field_type(
    registry: &ModelRegistry,
    path: &Path,
    field: &crate::axm::codegen::EffectiveField,
) -> String {
    let nullable_is_base = matches!(field.annotated.base, TypeRef::Nullable(_));
    let base = if nullable_is_base {
        match &field.annotated.base {
            TypeRef::Nullable(inner) => rust_named_type(registry, path, inner),
            _ => unreachable!(),
        }
    } else {
        rust_named_type(registry, path, &field.annotated.base)
    };
    if field.optional || nullable_is_base {
        format!("Option<{base}>")
    } else {
        base
    }
}

fn rust_named_type(registry: &ModelRegistry, path: &Path, ty: &TypeRef) -> String {
    match ty {
        TypeRef::String => "String".to_string(),
        TypeRef::Uuid => "String".to_string(),
        TypeRef::Int | TypeRef::BigInt => "i64".to_string(),
        TypeRef::Float => "f64".to_string(),
        TypeRef::Boolean => "bool".to_string(),
        TypeRef::Json => "serde_json::Value".to_string(),
        TypeRef::Date | TypeRef::DateTime => "String".to_string(),
        TypeRef::Bytes => "Vec<u8>".to_string(),
        TypeRef::Named(name) => match named_kind(registry, path, name) {
            NamedKind::Model(name) | NamedKind::AliasFun(name) | NamedKind::Unknown(name) => {
                util::pascal_case(&name)
            }
            NamedKind::Pure(base) => rust_named_type(registry, path, &base),
        },
        TypeRef::Array(inner) => format!("Vec<{}>", rust_named_type(registry, path, inner)),
        TypeRef::Nullable(inner) => format!("Option<{}>", rust_named_type(registry, path, inner)),
    }
}

fn rust_coerce_value_expr(
    registry: &ModelRegistry,
    path: &Path,
    ty: &TypeRef,
    value: &str,
) -> String {
    match ty {
        TypeRef::String => format!("coerce_string({value}, path, errors)"),
        TypeRef::Uuid => format!("coerce_string({value}, path, errors)"),
        TypeRef::Int => format!("coerce_int({value}, path, errors)"),
        TypeRef::BigInt => format!("coerce_bigint({value}, path, errors)"),
        TypeRef::Float => format!("coerce_float({value}, path, errors)"),
        TypeRef::Boolean => format!("coerce_boolean({value}, path, errors)"),
        TypeRef::Json => format!("coerce_json({value}, path, errors)"),
        TypeRef::Date => format!("coerce_date({value}, path, errors)"),
        TypeRef::DateTime => format!("coerce_datetime({value}, path, errors)"),
        TypeRef::Bytes => format!("coerce_bytes({value}, path, errors)"),
        TypeRef::Named(name) => match named_kind(registry, path, name) {
            NamedKind::Model(name) | NamedKind::AliasFun(name) | NamedKind::Unknown(name) => {
                format!(
                    "coerce_{}({value}, path, errors)",
                    util::rust_field_name(&name)
                )
            }
            NamedKind::Pure(base) => rust_coerce_value_expr(registry, path, &base, value),
        },
        TypeRef::Array(inner) => {
            let item = rust_coerce_value_expr(registry, path, inner, "&entry");
            format!(
                "{{ let base = coerce_array({value}, path, errors); let mut items = Vec::with_capacity(base.len()); for (index, entry) in base.into_iter().enumerate() {{ path.push(PathSegment::Index(index)); let item = {item}; path.pop(); items.push(item); }} items }}"
            )
        }
        TypeRef::Nullable(inner) => format!(
            "match {value} {{ n if n.is_null() => None, other => Some({}) }}",
            rust_coerce_value_expr(registry, path, inner, "other")
        ),
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

fn emit_query(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    query: &crate::axm::ast::QueryDecl,
) {
    let pascal = util::pascal_case(&query.name);
    let params_type = format!("{pascal}Params");
    let fn_name = util::rust_field_name(&query.name);

    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"
    );
    let _ = writeln!(out, "pub struct {params_type} {{");
    for param in &query.params {
        let field = util::rust_field_name(&param.name);
        let ty = rust_named_type(registry, path, &param.ty);
        let _ = writeln!(out, "    pub {field}: {ty},");
    }
    out.push_str("}\n\n");

    let _ = writeln!(out, "impl {params_type} {{");
    let _ = writeln!(
        out,
        "    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {{"
    );
    let has_checks = query.params.iter().any(|p| {
        let inlined = inline_annotated(registry, path, &AnnotatedType::new(p.ty.clone()));
        !inlined.rules.is_empty() || !inlined.transforms.is_empty()
    });
    if has_checks {
        out.push_str("        let mut errors: Vec<ValidationError> = Vec::new();\n");
        for param in &query.params {
            emit_param_validation(out, registry, path, param);
        }
        out.push_str("        if errors.is_empty() {\n");
        out.push_str("            Ok(())\n");
        out.push_str("        } else {\n");
        out.push_str("            Err(errors)\n");
        out.push_str("        }\n");
    } else {
        out.push_str("        let _ = self;\n");
        out.push_str("        Ok(())\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    let (sql, binds) = driver_sql(&query.sql, &query.params);
    let sql_lit = rust_raw_string(&sql);

    let ret_ty = match &query.return_type {
        QueryReturn::Many(ty_ref) => format!("Vec<{}>", rust_named_type(registry, path, ty_ref)),
        QueryReturn::Single(ty_ref) => rust_named_type(registry, path, ty_ref),
        QueryReturn::Optional(ty_ref) => {
            format!("Option<{}>", rust_named_type(registry, path, ty_ref))
        }
        QueryReturn::Exec => "()".to_string(),
    };

    let _ = writeln!(out, "pub async fn {fn_name}(");
    let _ = writeln!(out, "    pool: &sqlx::PgPool,");
    let _ = writeln!(out, "    params: {params_type},");
    let _ = writeln!(out, ") -> Result<{ret_ty}, Box<dyn std::error::Error>> {{");
    out.push_str(
        "    params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;\n",
    );

    match &query.return_type {
        QueryReturn::Many(ty_ref) | QueryReturn::Optional(ty_ref) => {
            let ty = rust_named_type(registry, path, ty_ref);
            let fetch = if matches!(query.return_type, QueryReturn::Many(_)) {
                "fetch_all(pool)"
            } else {
                "fetch_optional(pool)"
            };
            let _ = writeln!(out, "    let rows = sqlx::query_as!(");
            let _ = writeln!(out, "        {ty},");
            let _ = writeln!(out, "        {sql_lit},");
            for bind in &binds {
                let _ = writeln!(out, "        {bind},");
            }
            out.push_str("    )\n");
            let _ = writeln!(out, "    .{fetch}");
            out.push_str("    .await?;\n");
            let _ = writeln!(out, "    Ok(rows)");
        }
        QueryReturn::Single(ty_ref) => {
            let ty = rust_named_type(registry, path, ty_ref);
            let _ = writeln!(out, "    let rows = sqlx::query_as!(");
            let _ = writeln!(out, "        {ty},");
            let _ = writeln!(out, "        {sql_lit},");
            for bind in &binds {
                let _ = writeln!(out, "        {bind},");
            }
            out.push_str("    )\n");
            out.push_str("    .fetch_all(pool).await?;\n");
            let _ = writeln!(
                out,
                "    let row = rows.into_iter().next().ok_or_else(|| format!(\"{pascal} returned no rows\"))?;"
            );
            let _ = writeln!(out, "    Ok(row)");
        }
        QueryReturn::Exec => {
            let _ = writeln!(out, "    sqlx::query(");
            let _ = writeln!(out, "        {sql_lit},");
            out.push_str("    )\n");
            for bind in &binds {
                let _ = writeln!(out, "    .bind({bind})");
            }
            out.push_str("    .execute(pool)\n");
            out.push_str("    .await?;\n");
            out.push_str("    Ok(())\n");
        }
    }
    out.push_str("}\n\n");
}

fn emit_param_validation(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    param: &crate::axm::ast::ParamDecl,
) {
    let inlined = inline_annotated(registry, path, &AnnotatedType::new(param.ty.clone()));
    if inlined.rules.is_empty() && inlined.transforms.is_empty() {
        return;
    }
    let field = util::rust_field_name(&param.name);
    let chain: String = inlined.transforms.iter().map(rust_transform_op).collect();
    let value = if chain.is_empty() {
        format!("self.{field}")
    } else {
        let _ = writeln!(out, "        let {field} = self.{field}{chain};");
        field.to_string()
    };
    for rule in &inlined.rules {
        let msg = util::escape_rust(&rule_message(rule));
        let condition = rust_rule_condition(rule, &value, &inlined.base);
        let _ = writeln!(out, "        if {condition} {{");
        let _ = writeln!(out, "            errors.push(ValidationError {{");
        let _ = writeln!(out, "                path: \"{field}\".to_string(),");
        let _ = writeln!(out, "                message: \"{msg}\".to_string(),");
        let _ = writeln!(out, "            }});");
        let _ = writeln!(out, "        }}");
    }
}

/// Rewrite SQL placeholders into numbered `$n` placeholders (in bind order) and
/// produce the matching bind expressions. Markers are only substituted outside
/// string literals, quoted identifiers, and comments (see
/// [`scan_dotted_placeholders`]).
fn driver_sql(sql: &str, params: &[crate::axm::ast::ParamDecl]) -> (String, Vec<String>) {
    let hits = crate::query::scan_dotted_placeholders(sql);
    let mut out = String::with_capacity(sql.len());
    let mut binds = Vec::new();
    let mut next = 1usize;
    let mut last = 0usize;
    for (start, len, token) in hits {
        out.push_str(&sql[last..start]);
        last = start + len;
        let fields: Vec<&str> = token.split('.').collect();

        let mut bound = || -> Option<String> {
            if fields.len() > 1 && fields[0] == "input" {
                let input = params.iter().find(|p| p.name == "input")?;
                let sub = fields[1..].join(".");
                let mut field = util::rust_field_name(&input.name);
                field.push('.');
                field.push_str(&util::rust_field_name(&sub));
                return Some(format!("params.{field}"));
            }
            if token.bytes().all(|b| b.is_ascii_digit()) {
                let n: usize = token.parse().unwrap_or(1).max(1);
                let param = params.get(n - 1)?;
                next = next.max(n + 1);
                return Some(format!("params.{}", util::rust_field_name(&param.name)));
            }
            if fields.len() == 1 {
                let param = params.iter().find(|p| p.name == token)?;
                return Some(format!("params.{}", util::rust_field_name(&param.name)));
            }
            None
        };

        match bound() {
            Some(bind) => {
                let _ = write!(out, "${next}");
                binds.push(bind);
                next += 1;
            }
            None => {
                out.push_str(&sql[start..start + len]);
            }
        }
    }
    out.push_str(&sql[last..]);
    (out, binds)
}

/// Wrap SQL in a raw string literal, bumping the number of `#` delimiters if
/// the body contains a terminator.
fn rust_raw_string(sql: &str) -> String {
    let mut hashes = 1usize;
    loop {
        let close = format!("\"{}", "#".repeat(hashes));
        if !sql.contains(&close) {
            let hashes_str = "#".repeat(hashes);
            return format!("r{hashes_str}\"{sql}\"{hashes_str}");
        }
        hashes += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axm::resolver::resolve_models;
    use crate::catalog::TableCatalog;

    fn registry(src: &str) -> ModelRegistry {
        resolve_models(&[(std::path::PathBuf::from("models/test.axm"), src.to_string())])
            .expect("resolve")
    }

    fn no_catalog() -> TableCatalog<'static> {
        TableCatalog { tables: Vec::new() }
    }

    #[test]
    fn emits_struct_with_serde_and_helpers() {
        let out = generate_rust_models(
            &registry("model User {\n  email: String .email()\n  age: Int\n}"),
            &no_catalog(),
        );
        assert!(out.contains(
            "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, sqlx::FromRow)]"
        ));
        assert!(out.contains("pub struct User {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub age: i64,"));
        assert!(out.contains("fn coerce_string("));
        assert!(out.contains("fn check_email("));
        assert!(out.contains("fn coerce_user("));
    }

    #[test]
    fn emits_safe_parse_and_parse() {
        let out =
            generate_rust_models(&registry("model User {\n  email: String\n}"), &no_catalog());
        assert!(out.contains(
            "pub fn safe_parse(value: &serde_json::Value) -> Result<User, Vec<ValidationError>>"
        ));
        assert!(out.contains("pub fn parse(value: &serde_json::Value) -> User {"));
        assert!(out.contains("panic!(\"User validation failed: {errors:?}\")"));
    }

    #[test]
    fn emits_transforms_before_validation() {
        let out = generate_rust_models(
            &registry("model User {\n  email: String .trim() .lowercase() .email()\n}"),
            &no_catalog(),
        );
        assert!(out.contains("let value = base.trim().to_lowercase().to_string();"));
        assert!(out.contains("if !check_email(&value) { push_error(errors, path, \"must be a valid email address\"); }"));
    }

    #[test]
    fn optional_fields_are_option() {
        let out = generate_rust_models(
            &registry("model User {\n  age?: Int .min(18)\n  country: String = \"US\"\n}"),
            &no_catalog(),
        );
        assert!(out.contains("pub age: Option<i64>,"));
        assert!(out.contains("if let Some(raw) = record.get(\"age\")"));
        assert!(out.contains("out.age = Some(value);"));
        assert!(out.contains("let raw = record.get(\"country\").cloned().unwrap_or_else(|| serde_json::json!(\"US\"));"));
    }

    #[test]
    fn required_fields_report_missing() {
        let out =
            generate_rust_models(&registry("model User {\n  email: String\n}"), &no_catalog());
        assert!(out.contains("push_error(errors, path, \"field is required\");"));
    }

    #[test]
    fn recursive_array_validation() {
        let out = generate_rust_models(
            &registry("type Address = String .nonempty()\nmodel User {\n  history: Address[]\n}"),
            &no_catalog(),
        );
        assert!(out.contains("pub history: Vec<Address>,"));
        assert!(out.contains("path.push(PathSegment::Index(index));"));
        assert!(out.contains("items.push(coerce_address(&entry, path, errors));"));
    }

    #[test]
    fn error_paths_render_arrays_and_fields() {
        let out = generate_rust_models(
            &registry(
                "type Address = String .nonempty()\nmodel User {\n  history: Address[]\n  email: String .email()\n}",
            ),
            &no_catalog(),
        );
        assert!(out.contains("pub enum PathSegment {"));
        assert!(out.contains("Field(String),"));
        assert!(out.contains("Index(usize),"));
        assert!(out.contains("path: render_path(path),"));
    }

    #[test]
    fn unused_helpers_are_not_emitted() {
        let out = generate_rust_models(
            &registry("model User {\n  name: String .nonempty()\n}"),
            &no_catalog(),
        );
        assert!(out.contains("fn check_nonempty("));
        assert!(!out.contains("fn check_uuid("));
        assert!(!out.contains("fn coerce_datetime("));
        assert!(!out.contains("fn coerce_bigint("));
        assert!(!out.contains("fn coerce_boolean("));
        assert!(!out.contains("fn coerce_array("));
    }

    #[test]
    fn empty_registry_generates_nothing() {
        assert_eq!(
            generate_rust_models(&ModelRegistry::default(), &no_catalog()),
            ""
        );
    }

    #[test]
    fn type_aliases_fold_and_emit() {
        let src = "type Email = String .email() .max_length(320)\ntype UserId = BigInt\nmodel User {\n  email: Email\n  id: UserId\n}\n";
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains("pub type Email = String;"));
        assert!(out.contains("pub type UserId = i64;"));
        assert!(out.contains("fn coerce_email("));
        assert!(out.contains("check_email(&value)"));
        assert!(out.contains("pub id: i64,"));
        assert!(!out.contains("fn coerce_userid("));
    }

    #[test]
    fn nullable_fields_emit_option() {
        let out = generate_rust_models(&registry("model User {\n  bio: String?\n}"), &no_catalog());
        assert!(out.contains("pub bio: Option<String>,"));
        assert!(out.contains(".is_null()"));
        assert!(out.contains("out.bio = Some(value);"));
    }

    #[test]
    fn queries_emit_params_and_binding() {
        let src = r#"
model User { id: UUID }
query GetUser($id: UUID) -> User? {
  SELECT * FROM users WHERE id = $id;
}
query GetActiveUsers() -> User[] {
  SELECT * FROM users WHERE active = true;
}
"#;
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains("pub struct GetUserParams {"));
        assert!(out.contains("pub async fn get_user("));
        assert!(out.contains("params.id,"));
        assert!(out.contains("WHERE id = $1"));
        assert!(out.contains("fetch_optional(pool)"));
        assert!(out.contains("-> Result<Option<User>, Box<dyn std::error::Error>>"));
        assert!(out.contains("fetch_all(pool)"));
        assert!(out.contains("get_active_users"));
    }

    #[test]
    fn structured_input_params_bind_dotted_fields() {
        let src = r#"
model CreateUserInput { email: String .email() }
query CreateUser($input: CreateUserInput) {
  INSERT INTO users (email) VALUES ($input.email);
}
"#;
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains(".bind(params.input.email)") || out.contains("params.input.email,"));
        assert!(out.contains("VALUES ($1)"));
    }

    #[test]
    fn driver_sql_skips_placeholders_inside_literals() {
        use crate::axm::ast::ParamDecl;
        let params = vec![ParamDecl {
            name: "email".into(),
            ty: TypeRef::String,
        }];
        let sql = "SELECT '<cost is $5>', email\n".to_string()
            + "FROM users -- $email is a comment\n"
            + "WHERE status = '$$draft$$' AND email = $email";
        let (out, binds) = driver_sql(&sql, &params);
        assert!(out.contains("<cost is $5>"));
        assert!(out.contains("-- $email is a comment"));
        assert!(out.contains("$$draft$$"));
        assert!(out.contains("email = $1"));
        assert_eq!(binds, vec!["params.email"]);
        assert_eq!(out.matches("$1").count(), 1);
    }

    #[test]
    fn uuid_only_model_emits_string_coercion_helper() {
        let src = "model User {\n  id: UUID\n}\n";
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains("fn coerce_string("));
        assert!(out.contains("coerce_string("));
        assert!(out.contains("id: String"));
    }

    #[test]
    fn uuid_only_model_emits_string_coercion_helper_ts() {
        let src = "model User {\n  id: UUID\n}\n";
        let out = crate::axm::codegen::generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("function coerceString("));
    }

    #[test]
    fn array_fields_emit_collection_rules() {
        let src = "model User {\n  history: String[] .nonempty(\"need items\") .min_length(1) .max_length(3)\n}\n";
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains("if items.is_empty() { push_error(errors, path, \"need items\"); }"));
        assert!(out.contains("if items.len() < 1 {"));
        assert!(out.contains("if items.len() > 3 {"));
    }

    #[test]
    fn single_query_returns_bare_row_and_errors_on_empty() {
        let src = r#"
model User { id: UUID }
query GetUser($id: UUID) -> User {
  SELECT * FROM users WHERE id = $id;
}
"#;
        let out = generate_rust_models(&registry(src), &no_catalog());
        assert!(out.contains("-> Result<User, Box<dyn std::error::Error>>"));
        assert!(out.contains("GetUser returned no rows"));
        assert!(out.contains("Ok(row)"));
    }
}
