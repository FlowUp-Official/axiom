//! TypeScript code generation for `.axm` models.
//!
//! Emits interfaces, reusable validation helpers, and per-model `coerce`
//! functions that compose recursively. Every model exposes `safeParse()` and
//! `parse()`. The output has zero runtime dependencies: only the standard
//! TypeScript runtime is used.
//!
//! Error paths are built as `Seg` arrays (`['f', name]` / `['i', index]`) and
//! only rendered to strings inside `fail()`, at the point of reporting.

use std::fmt::Write;

use crate::axm::ast::{FieldDecl, Literal, ModelDecl, Rule, Transform, TypeRef};
use crate::axm::codegen::{collect_uses, model_name, Uses};
use crate::axm::resolver::ModelRegistry;
use crate::codegen::util;

const EMAIL_RE: &str = r#"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$"#;
const URL_RE: &str = r#"^(https?://)?([\da-z.-]+)\.([a-z.]{2,6})([/\w .-]*)*/?$"#;
const UUID_RE: &str = r#"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"#;
const TIMESTAMP_RE: &str = r#"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?$"#;

/// Generate the TypeScript module body for a set of models. Assumes the
/// surrounding output already defines `ValidationError` (which the SQL
/// generator always does), so it is reused rather than redefined.
pub fn generate_typescript_models(registry: &ModelRegistry) -> String {
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
        emit_model(&mut out, &resolved.model);
    }

    out
}

fn emit_helpers(out: &mut String, uses: &Uses) {
    out.push_str("type Seg = ['f', string] | ['i', number];\n\n");
    out.push_str("function renderPath(path: Seg[]): string {\n");
    out.push_str("  let out = '';\n");
    out.push_str("  for (const seg of path) {\n");
    out.push_str("    if (seg[0] === 'f') out += out === '' ? seg[1] : `.${seg[1]}`;\n");
    out.push_str("    else out += `[${seg[1]}]`;\n");
    out.push_str("  }\n");
    out.push_str("  return out;\n");
    out.push_str("}\n\n");

    out.push_str("function fail(errors: ValidationError[], path: Seg[], message: string): boolean {\n");
    out.push_str("  errors.push({ path: renderPath(path), message });\n");
    out.push_str("  return false;\n");
    out.push_str("}\n\n");

    emit_coerce_helper(out, "coerceString", "string", "expected a string", "'string'", "");
    if uses.int {
        emit_coerce_helper(
            out,
            "coerceInt",
            "number",
            "expected an integer",
            "'number'",
            " || !Number.isInteger(value)",
        );
    }
    if uses.float {
        emit_coerce_helper(
            out,
            "coerceFloat",
            "number",
            "expected a number",
            "'number'",
            " || !Number.isFinite(value)",
        );
    }
    if uses.boolean {
        emit_coerce_helper(
            out,
            "coerceBoolean",
            "boolean",
            "expected a boolean",
            "'boolean'",
            "",
        );
    }
    if uses.json {
        out.push_str("function coerceJson(value: unknown, path: Seg[], errors: ValidationError[]): unknown {\n");
        out.push_str("  if (value === null || typeof value === 'string' || typeof value === 'number' ||\n");
        out.push_str("      typeof value === 'boolean' || Array.isArray(value) || typeof value === 'object') return value;\n");
        out.push_str("  fail(errors, path, 'expected a JSON value');\n");
        out.push_str("  return null;\n");
        out.push_str("}\n\n");
    }

    if uses.timestamp {
        out.push_str("function coerceTimestamp(value: unknown, path: Seg[], errors: ValidationError[]): string {\n");
        out.push_str("  if (typeof value === 'string' && ");
        out.push_str(&format!("{}.test(value)) return value;\n", util::ts_regex_literal(TIMESTAMP_RE, "")));
        out.push_str("  fail(errors, path, 'expected an ISO 8601 timestamp');\n");
        out.push_str("  return '';\n");
        out.push_str("}\n\n");
    }

    if uses.array {
        out.push_str("function coerceArray(value: unknown, path: Seg[], errors: ValidationError[]): unknown[] {\n");
        out.push_str("  if (!Array.isArray(value)) { fail(errors, path, 'expected an array'); return []; }\n");
        out.push_str("  return value;\n");
        out.push_str("}\n\n");
    }

    if uses.email {
        out.push_str("function checkEmail(value: string, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str(&format!(
            "  if (!{}.test(value)) return fail(errors, path, 'must be a valid email address');\n",
            util::ts_regex_literal(EMAIL_RE, "i")
        ));
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.url {
        out.push_str("function checkUrl(value: string, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str(&format!(
            "  if (!{}.test(value)) return fail(errors, path, 'must be a valid URL');\n",
            util::ts_regex_literal(URL_RE, "i")
        ));
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.uuid {
        out.push_str("function checkUuid(value: string, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str(&format!(
            "  if (!{}.test(value)) return fail(errors, path, 'must be a valid UUID');\n",
            util::ts_regex_literal(UUID_RE, "")
        ));
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.alphanumeric {
        out.push_str("function checkAlphanumeric(value: string, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str("  if (!/^[a-zA-Z0-9]+$/.test(value)) return fail(errors, path, 'must be alphanumeric');\n");
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.nonempty {
        out.push_str("function checkNonEmpty(value: string, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str("  if (value.length === 0) return fail(errors, path, 'must not be empty');\n");
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.min_len {
        emit_bounded_helper(out, "checkMinLen", "value.length < bound", "must be at least ");
    }
    if uses.max_len {
        emit_bounded_helper(out, "checkMaxLen", "value.length > bound", "must be at most ");
    }
    if uses.min {
        emit_bounded_helper(out, "checkMin", "value < bound", "must be >= ");
    }
    if uses.max {
        emit_bounded_helper(out, "checkMax", "value > bound", "must be <= ");
    }
    if uses.regex {
        out.push_str("function checkRegex(value: string, pattern: RegExp, path: Seg[], errors: ValidationError[]): boolean {\n");
        out.push_str("  if (!pattern.test(value)) return fail(errors, path, 'must match the expected pattern');\n");
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
}

fn emit_coerce_helper(
    out: &mut String,
    name: &str,
    ty: &str,
    message: &str,
    type_guard: &str,
    extra: &str,
) {
    let _ = writeln!(
        out,
        "function {name}(value: unknown, path: Seg[], errors: ValidationError[]): {ty} {{"
    );
    let _ = writeln!(
        out,
        "  if (typeof value !== {type_guard}{extra}) {{ fail(errors, path, '{message}'); return {default_value}; }}",
        default_value = match ty {
            "string" => "''",
            "number" => "0",
            "boolean" => "false",
            _ => "null",
        },
    );
    let _ = writeln!(out, "  return value;");
    let _ = writeln!(out, "}}\n");
}

fn emit_bounded_helper(out: &mut String, name: &str, condition: &str, prefix: &str) {
    let _ = writeln!(out, "function {name}(value: number, bound: number, path: Seg[], errors: ValidationError[]): boolean {{");
    let _ = writeln!(out, "  if ({condition}) return fail(errors, path, `{prefix}${{bound}}`);");
    let _ = writeln!(out, "  return true;");
    let _ = writeln!(out, "}}\n");
}

fn emit_model(out: &mut String, model: &ModelDecl) {
    let type_name = model_name(model);

    let _ = writeln!(out, "export interface {type_name} {{");
    for field in &model.fields {
        let _ = writeln!(out, "  {}: {};", field.name, ts_type(&field.ty));
    }
    let _ = writeln!(out, "}}\n");

    let _ = writeln!(
        out,
        "function coerce{type_name}(value: unknown, path: Seg[], errors: ValidationError[]): {type_name} {{"
    );
    let _ = writeln!(out, "  const out = {{}} as {type_name};");
    let _ = writeln!(
        out,
        "  if (value === null || typeof value !== 'object' || Array.isArray(value)) {{"
    );
    let _ = writeln!(out, "    fail(errors, path, 'expected an object');");
    let _ = writeln!(out, "    return out;");
    let _ = writeln!(out, "  }}");
    let _ = writeln!(out, "  const record = value as Record<string, unknown>;");
    for field in &model.fields {
        emit_field(out, field);
    }
    let _ = writeln!(out, "  return out;");
    let _ = writeln!(out, "}}\n");

    let _ = writeln!(
        out,
        "export type {type_name}Result = {{ ok: true; value: {type_name} }} | {{ ok: false; errors: ValidationError[] }};"
    );
    let _ = writeln!(out, "export function safeParse{type_name}(input: unknown): {type_name}Result {{");
    let _ = writeln!(out, "  const errors: ValidationError[] = [];");
    let _ = writeln!(out, "  const value = coerce{type_name}(input, [], errors);");
    let _ = writeln!(out, "  if (errors.length > 0) return {{ ok: false, errors }};");
    let _ = writeln!(out, "  return {{ ok: true, value }};");
    let _ = writeln!(out, "}}\n");
    let _ = writeln!(out, "export function parse{type_name}(input: unknown): {type_name} {{");
    let _ = writeln!(out, "  const result = safeParse{type_name}(input);");
    let _ = writeln!(
        out,
        "  if (!result.ok) throw new Error('{type_name} validation failed: ' + JSON.stringify(result.errors));"
    );
    let _ = writeln!(out, "  return result.value;");
    let _ = writeln!(out, "}}\n");
}

fn emit_field(out: &mut String, field: &FieldDecl) {
    let _ = writeln!(out, "  {{");
    let _ = writeln!(out, "    const key = '{}';", util::escape_ts(&field.name));
    let _ = writeln!(out, "    const fieldPath: Seg[] = [...path, ['f', key]];");
    let _ = writeln!(out, "    let raw = record[key];");

    match &field.default {
        Some(literal) => {
            let _ = writeln!(out, "    if (raw === undefined) raw = {};", ts_literal(literal));
            emit_field_body(out, field, 4);
        }
        None if field.optional => {
            let _ = writeln!(out, "    if (raw !== undefined) {{");
            emit_field_body(out, field, 6);
            let _ = writeln!(out, "    }}");
        }
        None => {
            let _ = writeln!(out, "    if (raw === undefined) {{");
            let _ = writeln!(out, "      fail(errors, fieldPath, 'field is required');");
            let _ = writeln!(out, "    }} else {{");
            emit_field_body(out, field, 6);
            let _ = writeln!(out, "    }}");
        }
    }
    let _ = writeln!(out, "  }}");
}

fn emit_field_body(out: &mut String, field: &FieldDecl, indent: usize) {
    let pad = " ".repeat(indent);
    let body = " ".repeat(indent + 2);
    let coerced = ts_coerce_value_expr(&field.ty, "raw", "fieldPath", "errors");
    let _ = writeln!(out, "{pad}const base = {coerced};");
    if field.transformations.is_empty() {
        let _ = writeln!(out, "{pad}let value = base;");
    } else {
        let chain: String = field
            .transformations
            .iter()
            .map(ts_transform_op)
            .collect();
        let _ = writeln!(out, "{pad}let value = base{chain};");
    }
    for rule in &field.validations {
        let call = ts_rule_call(rule, "value");
        let _ = writeln!(out, "{pad}{call};");
    }
    let _ = writeln!(out, "{body}out.{} = value;", util::escape_ts(&field.name));
}

fn ts_transform_op(transform: &Transform) -> &'static str {
    match transform {
        Transform::Trim => ".trim()",
        Transform::Lowercase => ".toLowerCase()",
        Transform::Uppercase => ".toUpperCase()",
    }
}

fn ts_rule_call(rule: &Rule, value: &str) -> String {
    match rule {
        Rule::Email => format!("checkEmail({value}, fieldPath, errors)"),
        Rule::Url => format!("checkUrl({value}, fieldPath, errors)"),
        Rule::Uuid => format!("checkUuid({value}, fieldPath, errors)"),
        Rule::Alphanumeric => format!("checkAlphanumeric({value}, fieldPath, errors)"),
        Rule::NonEmpty => format!("checkNonEmpty({value}, fieldPath, errors)"),
        Rule::Min(n) => format!("checkMin({value}, {n}, fieldPath, errors)"),
        Rule::Max(n) => format!("checkMax({value}, {n}, fieldPath, errors)"),
        Rule::MinLen(n) => format!("checkMinLen({value}, {n}, fieldPath, errors)"),
        Rule::MaxLen(n) => format!("checkMaxLen({value}, {n}, fieldPath, errors)"),
        Rule::Regex(pattern) => format!(
            "checkRegex({value}, {}, fieldPath, errors)",
            util::ts_regex_literal(pattern, "")
        ),
    }
}

fn ts_literal(literal: &Literal) -> String {
    match literal {
        Literal::String(s) => format!("\"{}\"", util::escape_ts(s)),
        Literal::Int(n) => n.to_string(),
        Literal::Float(f) => f.to_string(),
        Literal::Bool(b) => b.to_string(),
    }
}

fn ts_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::String | TypeRef::Timestamp => "string".to_string(),
        TypeRef::Int | TypeRef::Float => "number".to_string(),
        TypeRef::Boolean => "boolean".to_string(),
        TypeRef::Json => "unknown".to_string(),
        TypeRef::Named(name) => util::pascal_case(name),
        TypeRef::Array(inner) => format!("{}[]", ts_type(inner)),
    }
}

fn ts_coerce_value_expr(ty: &TypeRef, value: &str, path: &str, errors: &str) -> String {
    match ty {
        TypeRef::String => format!("coerceString({value}, {path}, {errors})"),
        TypeRef::Int => format!("coerceInt({value}, {path}, {errors})"),
        TypeRef::Float => format!("coerceFloat({value}, {path}, {errors})"),
        TypeRef::Boolean => format!("coerceBoolean({value}, {path}, {errors})"),
        TypeRef::Json => format!("coerceJson({value}, {path}, {errors})"),
        TypeRef::Timestamp => format!("coerceTimestamp({value}, {path}, {errors})"),
        TypeRef::Named(name) => {
            format!("coerce{}({value}, {path}, {errors})", util::pascal_case(name))
        }
        TypeRef::Array(inner) => {
            let item_path = format!("[...{path}, ['i', index]]");
            let item = ts_coerce_value_expr(inner, "entry", &item_path, errors);
            format!("coerceArray({value}, {path}, {errors}).map((entry, index) => {item})")
        }
    }
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
            index: [(
                file.models[0].name.clone(),
                0usize,
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn emits_interface_and_helpers() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  email: string .email()\n  age: int\n}",
        ));
        assert!(out.contains("export interface User {"));
        assert!(out.contains("  email: string;"));
        assert!(out.contains("  age: number;"));
        assert!(out.contains("function coerceString("));
        assert!(out.contains("function coerceInt("));
        assert!(out.contains("function checkEmail("));
        assert!(out.contains("function coerceUser("));
    }

    #[test]
    fn emits_safe_parse_and_parse() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  email: string\n}",
        ));
        assert!(out.contains("export type UserResult = { ok: true; value: User } | { ok: false; errors: ValidationError[] };"));
        assert!(out.contains("export function safeParseUser(input: unknown): UserResult {"));
        assert!(out.contains("export function parseUser(input: unknown): User {"));
        assert!(out.contains("throw new Error('User validation failed: ' + JSON.stringify(result.errors));"));
    }

    #[test]
    fn emits_transforms_before_validation() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  email: string .trim() .lowercase() .email()\n}",
        ));
        assert!(out.contains("let value = base.trim().toLowerCase();"));
        assert!(out.contains("checkEmail(value, fieldPath, errors);"));
    }

    #[test]
    fn defaults_apply_when_missing() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  country: string = \"US\"\n  age?: int\n}",
        ));
        assert!(out.contains("if (raw === undefined) raw = \"US\";"));
        assert!(out.contains("if (raw !== undefined) {"));
    }

    #[test]
    fn required_fields_fail_when_missing() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  email: string\n}",
        ));
        assert!(out.contains("fail(errors, fieldPath, 'field is required');"));
    }

    #[test]
    fn recursive_array_validation_compiles() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  history: Address[]\n}",
        ));
        assert!(out.contains("history: Address[];"));
        assert!(out.contains(
            "coerceArray(raw, fieldPath, errors).map((entry, index) => coerceAddress(entry, [...fieldPath, ['i', index]], errors))"
        ));
    }

    #[test]
    fn unused_helpers_are_not_emitted() {
        let out = generate_typescript_models(&registry(
            "export model User {\n  name: string .nonempty()\n}",
        ));
        assert!(out.contains("function checkNonEmpty("));
        assert!(!out.contains("function checkUuid("));
        assert!(!out.contains("function coerceTimestamp("));
    }

    #[test]
    fn empty_registry_generates_nothing() {
        assert_eq!(generate_typescript_models(&ModelRegistry::default()), "");
    }

    #[test]
    fn parse_helpers_exist_for_every_model() {
        let src = r#"
export model Address { street: string }
model User { billing: Address }
"#;
        let file = parse_axm_file(src).unwrap();
        let models: Vec<_> = file.models.into_iter().collect();
        let registry = ModelRegistry {
            models: models
                .iter()
                .map(|m| crate::axm::resolver::ResolvedModel {
                    path: std::path::PathBuf::from("models/test.axm"),
                    model: m.clone(),
                })
                .collect(),
            index: models
                .iter()
                .enumerate()
                .map(|(i, m)| (m.name.clone(), i))
                .collect(),
        };
        let out = generate_typescript_models(&registry);
        assert!(out.contains("export function parseAddress("));
        assert!(out.contains("export function parseUser("));
    }
}
