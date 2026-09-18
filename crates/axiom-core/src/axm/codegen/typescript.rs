//! TypeScript code generation for `.axm` models, type aliases, and queries.
//!
//! Emits interfaces, reusable validation helpers, per-model `coerce` functions
//! that compose recursively, `coerce` functions for type aliases that carry
//! methods, and query wrappers. Every model exposes `safeParse()` and
//! `parse()`. The output has zero runtime dependencies: only the standard
//! TypeScript runtime is used, plus a `Sql` tag for query execution that the
//! surrounding project provides.
//!
//! Error paths are built as `Seg` arrays (`['f', name]` / `['i', index]`) and
//! only rendered to strings inside `fail()`, at the point of reporting.

use std::fmt::Write;
use std::path::Path;

use crate::axm::ast::{AnnotatedType, Literal, QueryReturn, Rule, SafeParseMode, Transform, TypeRef};
use crate::axm::codegen::{
    ModelEmission, NamedKind, Target, Uses, ValidationOptions, canonical_name, collect_uses,
    effective_fields, effective_safe_parse_mode, emit_plan, inline_annotated, model_name,
    named_kind, query_emitted,
};
use crate::axm::resolver::ModelRegistry;
use crate::catalog::TableCatalog;
use crate::codegen::util;

const EMAIL_RE: &str = r#"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$"#;
const URL_RE: &str = r#"^(https?://)?([\da-z.-]+)\.([a-z.]{2,6})([/\w .-]*)*/?$"#;
const UUID_RE: &str =
    r#"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"#;
const ULID_RE: &str = r#"^[0-9A-HJKMNP-TV-Z]{26}$"#;
const IPV4_RE: &str = r#"^(\d{1,3}\.){3}\d{1,3}$"#;
const IPV6_RE: &str = r#"^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$"#;
const DATE_RE: &str = r#"^\d{4}-\d{2}-\d{2}$"#;
const TIMESTAMP_RE: &str = r#"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?$"#;
const ALPHANUMERIC_RE: &str = r#"^[a-zA-Z0-9]+$"#;

/// Generate the TypeScript module body for a registry. Assumes the surrounding
/// output already defines `ValidationError` (which the SQL generator always
/// does), so it is reused rather than redefined.
pub fn generate_typescript_models(registry: &ModelRegistry, catalog: &TableCatalog) -> String {
    generate_typescript_models_with_options(registry, catalog, &ValidationOptions::default())
}

/// Generate TypeScript for a registry with explicit `codegen.validation`
/// options (which standalone parse APIs to emit and the default safeParse
/// error-aggregation mode).
pub fn generate_typescript_models_with_options(
    registry: &ModelRegistry,
    catalog: &TableCatalog,
    options: &ValidationOptions,
) -> String {
    if registry.is_empty() {
        return String::new();
    }

    let plan = emit_plan(registry, Target::TypeScript);
    let uses = collect_uses(registry, catalog, &plan, Target::TypeScript);
    let fail_fast = plan.iter().any(|(resolved, _)| {
        effective_safe_parse_mode(&resolved.model, options) == SafeParseMode::First
    });
    let mut out = String::new();
    out.push_str(
        "\n// ---------------------------------------------------------------------------\n",
    );
    out.push_str("// .axm models\n");
    out.push_str(
        "// ---------------------------------------------------------------------------\n\n",
    );

    let emitted_queries = registry
        .queries
        .iter()
        .filter(|resolved| query_emitted(&resolved.query, Target::TypeScript))
        .collect::<Vec<_>>();

    if !emitted_queries.is_empty() {
        out.push_str("import type { Sql } from 'postgres';\n\n");
    }

    emit_helpers(&mut out, &uses, fail_fast);

    for resolved in &registry.types {
        emit_type_alias_for(
            &mut out,
            registry,
            &resolved.path,
            &resolved.ty.name,
            &resolved.ty.ty,
        );
    }

    for (resolved, emission) in &plan {
        let fields = effective_fields(registry, catalog, &resolved.path, &resolved.model);
        emit_model(
            &mut out,
            registry,
            &resolved.path,
            &resolved.model,
            &fields,
            *emission,
            options,
        );
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

    for resolved in emitted_queries {
        emit_query(&mut out, registry, &resolved.path, &resolved.query);
    }

    out
}

fn emit_helpers(out: &mut String, uses: &Uses, fail_fast: bool) {
    out.push_str("type Seg = ['f', string] | ['i', number];\n\n");
    out.push_str("function renderPath(path: Seg[]): string {\n");
    out.push_str("  let out = '';\n");
    out.push_str("  for (const seg of path) {\n");
    out.push_str("    if (seg[0] === 'f') out += out === '' ? seg[1] : `.${seg[1]}`;\n");
    out.push_str("    else out += `[${seg[1]}]`;\n");
    out.push_str("  }\n");
    out.push_str("  return out;\n");
    out.push_str("}\n\n");

    if fail_fast {
        // `@safeParse("first")` short-circuits validation by throwing a
        // sentinel once the first error is recorded; `_axm_fail_fast` is only
        // true inside a first-mode `safeParse` call, so `fail` from any other
        // call site (nested "all" paths, query params) still collects normally.
        out.push_str("const AXM_STOP = Symbol('axm.stop');\n");
        out.push_str("let _axm_fail_fast = false;\n\n");
    }

    out.push_str(
        "function fail(errors: ValidationError[], path: Seg[], message: string): boolean {\n",
    );
    out.push_str("  errors.push({ path: renderPath(path), message });\n");
    if fail_fast {
        out.push_str("  if (_axm_fail_fast) throw AXM_STOP;\n");
    }
    out.push_str("  return false;\n");
    out.push_str("}\n\n");

    emit_coerce_helper(
        out,
        "coerceString",
        "string",
        "expected a string",
        "'string'",
        "",
    );
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
    if uses.bigint {
        out.push_str("function coerceBigInt(value: unknown, path: Seg[], errors: ValidationError[]): bigint {\n");
        out.push_str("  if (typeof value === 'bigint') return value;\n");
        out.push_str(
            "  if (typeof value === 'number' && Number.isInteger(value)) return BigInt(value);\n",
        );
        out.push_str("  fail(errors, path, 'expected a big integer');\n");
        out.push_str("  return 0n;\n");
        out.push_str("}\n\n");
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
        out.push_str(
            "  if (value === null || typeof value === 'string' || typeof value === 'number' ||\n",
        );
        out.push_str("      typeof value === 'boolean' || Array.isArray(value) || typeof value === 'object') return value;\n");
        out.push_str("  fail(errors, path, 'expected a JSON value');\n");
        out.push_str("  return null;\n");
        out.push_str("}\n\n");
    }
    if uses.date {
        emit_regex_coerce_helper(out, "coerceDate", DATE_RE, "", "expected an ISO 8601 date");
    }
    if uses.datetime {
        emit_regex_coerce_helper(
            out,
            "coerceDateTime",
            TIMESTAMP_RE,
            "",
            "expected an ISO 8601 timestamp",
        );
    }
    if uses.bytes {
        out.push_str("function coerceBytes(value: unknown, path: Seg[], errors: ValidationError[]): Uint8Array {\n");
        out.push_str("  if (value instanceof Uint8Array) return value;\n");
        out.push_str("  if (Array.isArray(value) && value.every((v) => typeof v === 'number')) return new Uint8Array(value);\n");
        out.push_str("  fail(errors, path, 'expected a byte sequence');\n");
        out.push_str("  return new Uint8Array();\n");
        out.push_str("}\n\n");
    }

    if uses.array {
        out.push_str("function coerceArray(value: unknown, path: Seg[], errors: ValidationError[]): unknown[] {\n");
        out.push_str("  if (!Array.isArray(value)) { fail(errors, path, 'expected an array'); return []; }\n");
        out.push_str("  return value;\n");
        out.push_str("}\n\n");
    }

    if uses.email {
        emit_regex_check_helper(
            out,
            "checkEmail",
            EMAIL_RE,
            "i",
            "must be a valid email address",
        );
    }
    if uses.url {
        emit_regex_check_helper(out, "checkUrl", URL_RE, "i", "must be a valid URL");
    }
    if uses.uuid {
        emit_regex_check_helper(out, "checkUuid", UUID_RE, "", "must be a valid UUID");
    }
    if uses.ulid {
        emit_regex_check_helper(out, "checkUlid", ULID_RE, "", "must be a valid ULID");
    }
    if uses.ipv4 {
        out.push_str("function checkIpv4(value: string, path: Seg[], errors: ValidationError[], message?: string): boolean {\n");
        out.push_str("  if (!IPV4.test(value) || value.split('.').some((oct) => Number(oct) > 255)) return fail(errors, path, message ?? 'must be a valid IPv4 address');\n");
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
        let _ = writeln!(out, "const IPV4 = {};", util::ts_regex_literal(IPV4_RE, ""));
    }
    if uses.ipv6 {
        emit_regex_check_helper(
            out,
            "checkIpv6",
            IPV6_RE,
            "",
            "must be a valid IPv6 address",
        );
    }
    if uses.isodate {
        emit_regex_check_helper(
            out,
            "checkIsoDate",
            DATE_RE,
            "",
            "must be a valid ISO 8601 date",
        );
    }
    if uses.alphanumeric {
        emit_regex_check_helper(
            out,
            "checkAlphanumeric",
            ALPHANUMERIC_RE,
            "",
            "must be alphanumeric",
        );
    }
    if uses.nonempty {
        out.push_str("function checkNonEmpty(value: string, path: Seg[], errors: ValidationError[], message?: string): boolean {\n");
        out.push_str("  if (value.length === 0) return fail(errors, path, message ?? 'must not be empty');\n");
        out.push_str("  return true;\n");
        out.push_str("}\n\n");
    }
    if uses.min_len {
        emit_bounded_helper(
            out,
            "checkMinLen",
            "value.length < bound",
            "must be at least ",
        );
    }
    if uses.max_len {
        emit_bounded_helper(
            out,
            "checkMaxLen",
            "value.length > bound",
            "must be at most ",
        );
    }
    if uses.min {
        emit_bounded_helper(out, "checkMin", "value < bound", "must be >= ");
    }
    if uses.max {
        emit_bounded_helper(out, "checkMax", "value > bound", "must be <= ");
    }
    if uses.regex {
        out.push_str("function checkRegex(value: string, pattern: RegExp, path: Seg[], errors: ValidationError[], message?: string): boolean {\n");
        out.push_str("  if (!pattern.test(value)) return fail(errors, path, message ?? 'must match the expected pattern');\n");
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

fn emit_regex_coerce_helper(
    out: &mut String,
    name: &str,
    pattern: &str,
    flags: &str,
    message: &str,
) {
    let _ = writeln!(
        out,
        "function {name}(value: unknown, path: Seg[], errors: ValidationError[]): string {{"
    );
    let _ = writeln!(
        out,
        "  if (typeof value === 'string' && {}.test(value)) return value;",
        util::ts_regex_literal(pattern, flags)
    );
    let _ = writeln!(out, "  fail(errors, path, '{message}');");
    let _ = writeln!(out, "  return '';");
    let _ = writeln!(out, "}}\n");
}

fn emit_regex_check_helper(
    out: &mut String,
    name: &str,
    pattern: &str,
    flags: &str,
    message: &str,
) {
    let _ = writeln!(
        out,
        "function {name}(value: string, path: Seg[], errors: ValidationError[], message?: string): boolean {{"
    );
    let _ = writeln!(
        out,
        "  if (!{}.test(value)) return fail(errors, path, message ?? '{message}');",
        util::ts_regex_literal(pattern, flags)
    );
    let _ = writeln!(out, "  return true;");
    let _ = writeln!(out, "}}\n");
}

fn emit_bounded_helper(out: &mut String, name: &str, condition: &str, prefix: &str) {
    let _ = writeln!(
        out,
        "function {name}(value: number, bound: number, path: Seg[], errors: ValidationError[], message?: string): boolean {{"
    );
    let _ = writeln!(
        out,
        "  if ({condition}) return fail(errors, path, message ?? `{prefix}${{bound}}`);"
    );
    let _ = writeln!(out, "  return true;");
    let _ = writeln!(out, "}}\n");
}

fn emit_type_alias_for(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    declared: &str,
    ann: &AnnotatedType,
) {
    let ty = ts_named_type(registry, path, &ann.base);
    let _ = writeln!(
        out,
        "export type {} = {};",
        canonical_name(registry, path, declared),
        ty
    );
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
    let name = canonical_name(registry, path, declared);
    let result_ty = ts_named_type(registry, path, &inlined.base);
    emit_annotated_fn(
        out, registry, path, &name, &result_ty, &inlined, "anchor", 0,
    );
}

/// Emit a standalone `coerce{Name}` function that validates `anchor` with the
/// given (already inlined) annotated type.
#[allow(clippy::too_many_arguments)]
fn emit_annotated_fn(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    name: &str,
    result_ty: &str,
    ann: &AnnotatedType,
    value_param: &str,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    let _ = writeln!(
        out,
        "function coerce{name}({value_param}: unknown, path: Seg[], errors: ValidationError[]): {result_ty} {{"
    );
    emit_annotated_value(out, registry, path, ann, value_param, indent + 2);
    let _ = writeln!(out, "{pad}  return value;");
    let _ = writeln!(out, "{pad}}}\n");
}

fn emit_model(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    model: &crate::axm::ast::ModelDecl,
    fields: &[crate::axm::codegen::EffectiveField],
    emission: ModelEmission,
    options: &ValidationOptions,
) {
    let type_name = model_name(model);
    let first = effective_safe_parse_mode(model, options) == SafeParseMode::First;

    let _ = writeln!(out, "export interface {type_name} {{");
    for field in fields {
        let optional = if field.optional { "?" } else { "" };
        let _ = writeln!(
            out,
            "  {}{}: {};",
            util::escape_ts(&field.emitted_name),
            optional,
            ts_named_type(registry, path, &field.annotated.base)
        );
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
    for field in fields {
        emit_field(out, registry, path, field);
    }
    let _ = writeln!(out, "  return out;");
    let _ = writeln!(out, "}}\n");

    if matches!(emission, ModelEmission::Full) {
        if options.emit_safe_parse {
            let _ = writeln!(
                out,
                "export type {type_name}Result = {{ ok: true; value: {type_name} }} | {{ ok: false; errors: ValidationError[] }};"
            );
            let _ = writeln!(
                out,
                "export function safeParse{type_name}(input: unknown): {type_name}Result {{"
            );
            let _ = writeln!(out, "  const errors: ValidationError[] = [];");
            if first {
                // `@safeParse("first")` (or `codegen.validation` `"errors": "first"`)
                // — stop coercing at the first error.
                let _ = writeln!(out, "  let value: {type_name} | undefined;");
                let _ = writeln!(out, "  _axm_fail_fast = true;");
                let _ = writeln!(out, "  try {{");
                let _ = writeln!(out, "    value = coerce{type_name}(input, [], errors);");
                let _ = writeln!(out, "  }} catch (e) {{");
                let _ = writeln!(out, "    if (e !== AXM_STOP) throw e;");
                let _ = writeln!(out, "  }} finally {{");
                let _ = writeln!(out, "    _axm_fail_fast = false;");
                let _ = writeln!(out, "  }}");
                let _ = writeln!(
                    out,
                    "  if (errors.length > 0) return {{ ok: false, errors }};"
                );
                let _ = writeln!(out, "  return {{ ok: true, value: value as {type_name} }};");
            } else {
                let _ = writeln!(out, "  const value = coerce{type_name}(input, [], errors);");
                let _ = writeln!(
                    out,
                    "  if (errors.length > 0) return {{ ok: false, errors }};"
                );
                let _ = writeln!(out, "  return {{ ok: true, value }};");
            }
            let _ = writeln!(out, "}}\n");
        }
        if options.emit_parse {
            let _ = writeln!(
                out,
                "export function parse{type_name}(input: unknown): {type_name} {{"
            );
            if options.emit_safe_parse {
                let _ = writeln!(out, "  const result = safeParse{type_name}(input);");
                let _ = writeln!(
                    out,
                    "  if (!result.ok) throw new Error('{type_name} validation failed: ' + JSON.stringify(result.errors));"
                );
                let _ = writeln!(out, "  return result.value;");
            } else if first {
                // Standalone `parse` (no `safeParse` requested): stop at the
                // first error and throw directly.
                let _ = writeln!(out, "  const errors: ValidationError[] = [];");
                let _ = writeln!(out, "  let value: {type_name} | undefined;");
                let _ = writeln!(out, "  _axm_fail_fast = true;");
                let _ = writeln!(out, "  try {{");
                let _ = writeln!(out, "    value = coerce{type_name}(input, [], errors);");
                let _ = writeln!(out, "  }} catch (e) {{");
                let _ = writeln!(out, "    if (e !== AXM_STOP) throw e;");
                let _ = writeln!(out, "  }} finally {{");
                let _ = writeln!(out, "    _axm_fail_fast = false;");
                let _ = writeln!(out, "  }}");
                let _ = writeln!(
                    out,
                    "  if (errors.length > 0) throw new Error('{type_name} validation failed: ' + JSON.stringify(errors));"
                );
                let _ = writeln!(out, "  return value as {type_name};");
            } else {
                let _ = writeln!(out, "  const errors: ValidationError[] = [];");
                let _ = writeln!(out, "  const value = coerce{type_name}(input, [], errors);");
                let _ = writeln!(
                    out,
                    "  if (errors.length > 0) throw new Error('{type_name} validation failed: ' + JSON.stringify(errors));"
                );
                let _ = writeln!(out, "  return value;");
            }
            let _ = writeln!(out, "}}\n");
        }
    }
}

fn emit_field(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    field: &crate::axm::codegen::EffectiveField,
) {
    let key = util::escape_ts(&field.emitted_name);
    let _ = writeln!(out, "  {{");
    let _ = writeln!(out, "    const key = '{key}';");
    let _ = writeln!(out, "    const fieldPath: Seg[] = [...path, ['f', key]];");
    let _ = writeln!(out, "    let raw = record[key];");

    match &field.default {
        Some(literal) => {
            let _ = writeln!(
                out,
                "    if (raw === undefined) raw = {};",
                ts_literal(literal)
            );
            emit_field_assign(out, registry, path, field, 4);
        }
        None if field.optional => {
            let _ = writeln!(out, "    if (raw !== undefined) {{");
            emit_field_assign(out, registry, path, field, 6);
            let _ = writeln!(out, "    }}");
        }
        None => {
            let _ = writeln!(out, "    if (raw === undefined) {{");
            let _ = writeln!(out, "      fail(errors, fieldPath, 'field is required');");
            let _ = writeln!(out, "    }} else {{");
            emit_field_assign(out, registry, path, field, 6);
            let _ = writeln!(out, "    }}");
        }
    }
    let _ = writeln!(out, "  }}");
}

/// Emit the statements that validate `raw` and write `out.{name} = value;`.
fn emit_field_assign(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    field: &crate::axm::codegen::EffectiveField,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    emit_annotated_value(out, registry, path, &field.annotated, "raw", indent);
    let _ = writeln!(
        out,
        "{pad}out.{} = value;",
        util::escape_ts(&field.emitted_name)
    );
}

/// Emit statements that bind `value` from the expression `raw`, applying the
/// annotated type's transforms and rules. The caller assigns `out.{key} = value`.
fn emit_annotated_value(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    ann: &AnnotatedType,
    raw: &str,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    match &ann.base {
        TypeRef::Nullable(inner) => {
            let _ = writeln!(out, "{pad}if ({raw} === null) {{");
            let _ = writeln!(out, "{}value = null;", " ".repeat(indent + 2));
            let _ = writeln!(out, "{pad}}} else {{");
            let nested = AnnotatedType {
                base: (**inner).clone(),
                transforms: ann.transforms.clone(),
                rules: ann.rules.clone(),
            };
            emit_annotated_value(out, registry, path, &nested, raw, indent + 2);
            let _ = writeln!(out, "{pad}}}");
        }
        _ => {
            let expr = coerce_value_expr(registry, path, &ann.base, raw, "fieldPath", "errors");
            let _ = writeln!(out, "{pad}const base = {expr};");
            if ann.transforms.is_empty() {
                let _ = writeln!(out, "{pad}let value = base;");
            } else {
                let chain: String = ann.transforms.iter().map(ts_transform_op).collect();
                let _ = writeln!(out, "{pad}let value = base{chain};");
            }
            for rule in &ann.rules {
                let call = ts_rule_call(rule, "value");
                let _ = writeln!(out, "{pad}{call};");
            }
        }
    }
}

/// An expression that coerces `value` to the annotated type base's runtime type.
fn coerce_value_expr(
    registry: &ModelRegistry,
    path: &Path,
    ty: &TypeRef,
    value: &str,
    path_expr: &str,
    errors: &str,
) -> String {
    match ty {
        TypeRef::String => format!("coerceString({value}, {path_expr}, {errors})"),
        TypeRef::Uuid => format!("coerceString({value}, {path_expr}, {errors})"),
        TypeRef::Int => format!("coerceInt({value}, {path_expr}, {errors})"),
        TypeRef::BigInt => format!("coerceBigInt({value}, {path_expr}, {errors})"),
        TypeRef::Float => format!("coerceFloat({value}, {path_expr}, {errors})"),
        TypeRef::Boolean => format!("coerceBoolean({value}, {path_expr}, {errors})"),
        TypeRef::Json => format!("coerceJson({value}, {path_expr}, {errors})"),
        TypeRef::Date => format!("coerceDate({value}, {path_expr}, {errors})"),
        TypeRef::DateTime => format!("coerceDateTime({value}, {path_expr}, {errors})"),
        TypeRef::Bytes => format!("coerceBytes({value}, {path_expr}, {errors})"),
        TypeRef::Named(name) => match named_kind(registry, path, name) {
            NamedKind::Model(name) | NamedKind::AliasFun(name) | NamedKind::Unknown(name) => {
                format!(
                    "coerce{}({value}, {path_expr}, {errors})",
                    util::pascal_case(&name)
                )
            }
            NamedKind::Pure(base) => {
                coerce_value_expr(registry, path, &base, value, path_expr, errors)
            }
        },
        TypeRef::Array(inner) => {
            let item_path = format!("[...{path_expr}, ['i', index]]");
            let item = coerce_value_expr(registry, path, inner, "entry", &item_path, errors);
            format!("coerceArray({value}, {path_expr}, {errors}).map((entry, index) => {item})")
        }
        TypeRef::Nullable(inner) => {
            let inner = coerce_value_expr(registry, path, inner, value, path_expr, errors);
            format!("{value} === null ? null : {inner}")
        }
    }
}

/// The emitted TypeScript type for a reference, with Named references
/// canonicalized and pure aliases folded into their base type.
fn ts_named_type(registry: &ModelRegistry, path: &Path, ty: &TypeRef) -> String {
    match ty {
        TypeRef::String => "string".to_string(),
        TypeRef::Uuid => "string".to_string(),
        TypeRef::Int | TypeRef::Float => "number".to_string(),
        TypeRef::BigInt => "bigint".to_string(),
        TypeRef::Boolean => "boolean".to_string(),
        TypeRef::Json => "unknown".to_string(),
        TypeRef::Date | TypeRef::DateTime => "string".to_string(),
        TypeRef::Bytes => "Uint8Array".to_string(),
        TypeRef::Named(name) => match named_kind(registry, path, name) {
            NamedKind::Model(name) | NamedKind::AliasFun(name) | NamedKind::Unknown(name) => {
                util::pascal_case(&name)
            }
            NamedKind::Pure(base) => ts_named_type(registry, path, &base),
        },
        TypeRef::Array(inner) => format!("{}[]", ts_named_type(registry, path, inner)),
        TypeRef::Nullable(inner) => format!("{} | null", ts_named_type(registry, path, inner)),
    }
}

fn ts_transform_op(transform: &Transform) -> &'static str {
    match transform {
        Transform::Trim => ".trim()",
        Transform::Lowercase => ".toLowerCase()",
        Transform::Uppercase => ".toUpperCase()",
    }
}

fn ts_rule_call(rule: &Rule, value: &str) -> String {
    let message = match rule.message() {
        Some(m) => format!(", \"{}\"", util::escape_ts(m)),
        None => String::new(),
    };
    match rule {
        Rule::Email(_) => format!("checkEmail({value}, fieldPath, errors{message})"),
        Rule::Url(_) => format!("checkUrl({value}, fieldPath, errors{message})"),
        Rule::Uuid(_) => format!("checkUuid({value}, fieldPath, errors{message})"),
        Rule::Ulid(_) => format!("checkUlid({value}, fieldPath, errors{message})"),
        Rule::Ipv4(_) => format!("checkIpv4({value}, fieldPath, errors{message})"),
        Rule::Ipv6(_) => format!("checkIpv6({value}, fieldPath, errors{message})"),
        Rule::IsoDate(_) => format!("checkIsoDate({value}, fieldPath, errors{message})"),
        Rule::Alphanumeric(_) => {
            format!("checkAlphanumeric({value}, fieldPath, errors{message})")
        }
        Rule::NonEmpty(_) => format!("checkNonEmpty({value}, fieldPath, errors{message})"),
        Rule::Min(n, _) => format!("checkMin({value}, {n}, fieldPath, errors{message})"),
        Rule::Max(n, _) => format!("checkMax({value}, {n}, fieldPath, errors{message})"),
        Rule::MinLength(n, _) => format!("checkMinLen({value}, {n}, fieldPath, errors{message})"),
        Rule::MaxLength(n, _) => format!("checkMaxLen({value}, {n}, fieldPath, errors{message})"),
        Rule::Regex(pattern, _) => format!(
            "checkRegex({value}, {}, fieldPath, errors{message})",
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

/// Rewrite `$1`, `$name`, and `$input.field` placeholders into template
/// interpolations of `params.`. Markers are only substituted outside string
/// literals, quoted identifiers, and comments (see
/// [`scan_dotted_placeholders`]); literals keep their `$`.
fn bind_sql(sql: &str, params: &[crate::axm::ast::ParamDecl]) -> String {
    let hits = crate::query::scan_dotted_placeholders(sql);
    let mut out = String::with_capacity(sql.len());
    let mut last = 0usize;
    let emit_escaped = |out: &mut String, s: &str| {
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '`' => out.push_str("\\`"),
                '\\' => out.push_str("\\\\"),
                '$' if it.peek() == Some(&'{') => out.push_str("\\$"),
                c => out.push(c),
            }
        }
    };

    for (start, len, token) in hits {
        emit_escaped(&mut out, &sql[last..start]);
        last = start + len;
        let fields: Vec<&str> = token.split('.').collect();

        if fields.len() > 1 && fields[0] == "input" {
            let bound = params.iter().any(|p| p.name == "input");
            if bound {
                let sub = fields[1..].join(".");
                let _ = write!(out, "${{params.input.{}}}", util::ts_field_name(&sub));
                continue;
            }
        } else if token.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = token.parse().unwrap_or(0);
            if n >= 1 && n <= params.len() {
                let _ = write!(
                    out,
                    "${{params.{}}}",
                    util::ts_field_name(&params[n - 1].name)
                );
                continue;
            }
        } else if fields.len() == 1
            && let Some(param) = params.iter().find(|p| p.name == token)
        {
            let _ = write!(out, "${{params.{}}}", util::ts_field_name(&param.name));
            continue;
        }

        emit_escaped(&mut out, &sql[start..start + len]);
    }
    emit_escaped(&mut out, &sql[last..]);
    out
}

fn emit_query(
    out: &mut String,
    registry: &ModelRegistry,
    path: &Path,
    query: &crate::axm::ast::QueryDecl,
) {
    let pascal = util::pascal_case(&query.name);
    let params_type = format!("{pascal}Params");
    let fn_name = util::ts_field_name(&query.name);

    let _ = writeln!(out, "export interface {params_type} {{");
    for param in &query.params {
        let field = util::ts_field_name(&param.name);
        let ty = ts_named_type(registry, path, &param.ty);
        let _ = writeln!(out, "  {field}: {ty};");
    }
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "export function validate{pascal}Params(params: {params_type}): ValidationError[] {{"
    );
    out.push_str("  const errors: ValidationError[] = [];\n");
    for param in &query.params {
        emit_param_validation(out, registry, path, param);
    }
    out.push_str("  return errors;\n");
    out.push_str("}\n\n");

    let bound_sql = bind_sql(&query.sql, &query.params);
    let (promise_ty, body) = match &query.return_type {
        QueryReturn::Exec => (
            "Promise<void>".to_string(),
            format!("await sql`\n{bound_sql}\n`;"),
        ),
        QueryReturn::Single(ty_ref) => {
            let ty = ts_named_type(registry, path, ty_ref);
            (
                format!("Promise<{ty}>"),
                format!(
                    "const rows = await sql<{ty}[]>`\n{bound_sql}\n`;\n  const row = rows[0];\n  if (row === undefined) throw new Error('{pascal} returned no rows');\n  return row;"
                ),
            )
        }
        QueryReturn::Optional(ty_ref) => {
            let ty = ts_named_type(registry, path, ty_ref);
            (
                format!("Promise<{ty} | null>"),
                format!(
                    "const rows = await sql<{ty}[]>`\n{bound_sql}\n`;\n  return rows[0] ?? null;"
                ),
            )
        }
        QueryReturn::Many(ty_ref) => {
            let ty = ts_named_type(registry, path, ty_ref);
            (
                format!("Promise<{ty}[]>"),
                format!("return await sql<{ty}[]>`\n{bound_sql}\n`;"),
            )
        }
    };

    let _ = writeln!(out, "export async function {fn_name}(");
    let _ = writeln!(out, "  sql: Sql,");
    let _ = writeln!(out, "  params: {params_type}");
    let _ = writeln!(out, "): {promise_ty} {{");
    let _ = writeln!(out, "  const errors = validate{pascal}Params(params);");
    out.push_str(
        "  if (errors.length > 0) throw new Error(`Validation failed: ${JSON.stringify(errors)}`);\n",
    );
    let _ = writeln!(out, "  {body}");
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
    let field = util::ts_field_name(&param.name);
    let _ = writeln!(out, "  {{");
    let _ = writeln!(out, "    const fieldPath: Seg[] = [['f', '{field}']];");
    emit_annotated_value(out, registry, path, &inlined, &format!("params.{field}"), 4);
    let _ = writeln!(out, "  }}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axm::resolver::{ModelRegistry, resolve_models};
    use crate::catalog::{ColumnSchema, TableCatalog, TableSchema};

    fn registry(src: &str) -> ModelRegistry {
        resolve_models(&[(std::path::PathBuf::from("models/test.axm"), src.to_string())])
            .expect("resolve")
    }

    fn no_catalog() -> TableCatalog<'static> {
        TableCatalog { tables: Vec::new() }
    }

    #[test]
    fn emits_interface_and_helpers() {
        let out = generate_typescript_models(
            &registry("model User {\n  email: String .email()\n  age: Int\n}"),
            &no_catalog(),
        );
        assert!(out.contains("export interface User {"));
        assert!(out.contains("  email: string;"));
        assert!(out.contains("  age: number;"));
        assert!(out.contains("function coerceString("));
        assert!(out.contains("function coerceInt("));
        assert!(out.contains("function checkEmail("));
        assert!(out.contains("function coerceUser("));
    }

    #[test]
    fn safe_parse_first_stops_at_first_error() {
        let src = "@safeParse(\"first\")\nmodel User {\n  email: String .email()\n  age: Int .min(0)\n}";
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("const AXM_STOP = Symbol('axm.stop');"), "{out}");
        assert!(out.contains("let _axm_fail_fast = false;"));
        assert!(out.contains("if (_axm_fail_fast) throw AXM_STOP;"));
        assert!(out.contains("_axm_fail_fast = true;"));
        assert!(out.contains("} catch (e) {"));
        assert!(out.contains("if (e !== AXM_STOP) throw e;"));
        assert!(out.contains("return { ok: true, value: value as User };"));
        assert!(out.contains("export function parseUser("));
    }

    #[test]
    fn safe_parse_all_is_default_and_parse_marker_is_noop() {
        let src = "@parse\n@safeParse(\"all\")\nmodel User {\n  email: String .email()\n}";
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(!out.contains("AXM_STOP"), "{out}");
        assert!(!out.contains("_axm_fail_fast"));
        assert!(out.contains("const value = coerceUser(input, [], errors);"));
        assert!(out.contains("export function parseUser("));
    }

    #[test]
    fn first_and_all_models_coexist_in_one_module() {
        let src = r#"
@safeParse("first")
model Admin {
  email: String .email()
}
@safeParse("all")
model User {
  email: String .email()
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("const AXM_STOP"));
        assert!(out.contains("export function safeParseUser(input: unknown): UserResult {"));
        let user_block = &out[out.find("safeParseUser").unwrap()..];
        assert!(!user_block.contains("_axm_fail_fast"));
    }

    #[test]
    fn emits_safe_parse_and_parse() {
        let out = generate_typescript_models(
            &registry("model User {\n  email: String\n}"),
            &no_catalog(),
        );
        assert!(out.contains("export type UserResult = { ok: true; value: User } | { ok: false; errors: ValidationError[] };"));
        assert!(out.contains("export function safeParseUser(input: unknown): UserResult {"));
        assert!(out.contains("export function parseUser(input: unknown): User {"));
        assert!(out.contains(
            "throw new Error('User validation failed: ' + JSON.stringify(result.errors));"
        ));
    }

    #[test]
    fn options_parse_only_emits_standalone_parse() {
        let opts = ValidationOptions {
            emit_safe_parse: false,
            emit_parse: true,
            default_errors: SafeParseMode::All,
        };
        let out = generate_typescript_models_with_options(
            &registry("model User {\n  email: String .email()\n}"),
            &no_catalog(),
            &opts,
        );
        assert!(!out.contains("function safeParse"), "{out}");
        assert!(!out.contains("UserResult"));
        assert!(out.contains("export function parseUser(input: unknown): User {"));
        assert!(!out.contains("const result = safeParseUser(input);"));
        assert!(out.contains(
            "if (errors.length > 0) throw new Error('User validation failed: ' + JSON.stringify(errors));"
        ));
    }

    #[test]
    fn options_safe_parse_only_emits_safe_parse() {
        let opts = ValidationOptions {
            emit_safe_parse: true,
            emit_parse: false,
            default_errors: SafeParseMode::All,
        };
        let out = generate_typescript_models_with_options(
            &registry("model User {\n  email: String\n}"),
            &no_catalog(),
            &opts,
        );
        assert!(out.contains("export function safeParseUser(input: unknown): UserResult {"));
        assert!(!out.contains("export function parseUser("), "{out}");
    }

    #[test]
    fn options_empty_emits_neither_api() {
        let opts = ValidationOptions {
            emit_safe_parse: false,
            emit_parse: false,
            default_errors: SafeParseMode::All,
        };
        let out = generate_typescript_models_with_options(
            &registry("model User {\n  email: String\n}"),
            &no_catalog(),
            &opts,
        );
        assert!(!out.contains("function safeParse"), "{out}");
        assert!(!out.contains("function parseUser"), "{out}");
        assert!(out.contains("export interface User {"));
        assert!(out.contains("function coerceUser("));
    }

    #[test]
    fn options_default_first_applies_without_decorator() {
        let opts = ValidationOptions {
            emit_safe_parse: true,
            emit_parse: true,
            default_errors: SafeParseMode::First,
        };
        let out = generate_typescript_models_with_options(
            &registry("model User {\n  email: String .email()\n}"),
            &no_catalog(),
            &opts,
        );
        assert!(out.contains("const AXM_STOP"), "{out}");
        assert!(out.contains("_axm_fail_fast = true;"));
        assert!(out.contains("export function safeParseUser(input: unknown): UserResult {"));
    }

    #[test]
    fn decorator_overrides_config_default() {
        let opts = ValidationOptions {
            emit_safe_parse: true,
            emit_parse: true,
            default_errors: SafeParseMode::First,
        };
        let out = generate_typescript_models_with_options(
            &registry("@safeParse(\"all\")\nmodel User {\n  email: String .email()\n}"),
            &no_catalog(),
            &opts,
        );
        assert!(!out.contains("AXM_STOP"), "{out}");
        assert!(out.contains("const value = coerceUser(input, [], errors);"));
    }

    #[test]
    fn emits_transforms_before_validation() {
        let out = generate_typescript_models(
            &registry("model User {\n  email: String .trim() .lowercase() .email()\n}"),
            &no_catalog(),
        );
        assert!(out.contains("let value = base.trim().toLowerCase();"));
        assert!(out.contains("checkEmail(value, fieldPath, errors);"));
    }

    #[test]
    fn defaults_apply_when_missing() {
        let out = generate_typescript_models(
            &registry("model User {\n  country: String = \"US\"\n  age?: Int\n}"),
            &no_catalog(),
        );
        assert!(out.contains("if (raw === undefined) raw = \"US\";"));
        assert!(out.contains("if (raw !== undefined) {"));
    }

    #[test]
    fn required_fields_fail_when_missing() {
        let out = generate_typescript_models(
            &registry("model User {\n  email: String\n}"),
            &no_catalog(),
        );
        assert!(out.contains("fail(errors, fieldPath, 'field is required');"));
    }

    #[test]
    fn recursive_array_validation() {
        let out = generate_typescript_models(
            &registry("type Address = String .nonempty()\nmodel User {\n  history: Address[]\n}"),
            &no_catalog(),
        );
        assert!(out.contains("history: Address[];"));
        assert!(out.contains(
            "coerceArray(raw, fieldPath, errors).map((entry, index) => coerceAddress(entry, [...fieldPath, ['i', index]], errors))"
        ));
    }

    #[test]
    fn unused_helpers_are_not_emitted() {
        let out = generate_typescript_models(
            &registry("model User {\n  name: String .nonempty()\n}"),
            &no_catalog(),
        );
        assert!(out.contains("function checkNonEmpty("));
        assert!(!out.contains("function checkUuid("));
        assert!(!out.contains("function coerceDateTime("));
        assert!(!out.contains("function coerceBigInt("));
    }

    #[test]
    fn empty_registry_generates_nothing() {
        assert_eq!(
            generate_typescript_models(&ModelRegistry::default(), &no_catalog()),
            ""
        );
    }

    #[test]
    fn target_override_filters_models() {
        let src = r#"
@target("typescript")
model Internal {
  id: UUID
}
@target("rust")
model RustOnly {
  id: UUID
}
@target('typescript', 'rust')
model Shared {
  id: UUID
}
model Open {
  id: UUID
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("export interface Internal {"));
        assert!(out.contains("export interface Shared {"));
        assert!(out.contains("export interface Open {"));
        assert!(!out.contains("RustOnly"));
    }

    #[test]
    fn no_codegen_models_emit_only_when_referenced() {
        let src = r#"
@no_codegen
model Secret {
  id: UUID
}
model User {
  account: Account
}
@no_codegen
model Account {
  id: UUID
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(!out.contains("Secret"));
        assert!(out.contains("export interface Account {"));
        assert!(out.contains("  account: Account;"));
        assert!(out.contains("function coerceAccount("));
        assert!(!out.contains("safeParseAccount"));
        assert!(!out.contains("parseAccount"));
        assert!(!out.contains("AccountResult"));
    }

    #[test]
    fn type_aliases_fold_and_emit() {
        let src = "type Email = String .email() .max_length(320)\ntype UserId = BigInt\nmodel User {\n  email: Email\n  id: UserId\n}\n";
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("export type Email = string;"));
        assert!(out.contains("export type UserId = bigint;"));
        assert!(out.contains("function coerceEmail("));
        assert!(out.contains("checkEmail(value, fieldPath, errors);"));
        assert!(out.contains("id: bigint;"));
        assert!(!out.contains("function coerceUserId("));
    }

    #[test]
    fn nullable_fields_emit_union_types() {
        let out =
            generate_typescript_models(&registry("model User {\n  bio: String?\n}"), &no_catalog());
        assert!(out.contains("  bio: string | null;"));
        assert!(out.contains("if (raw === null) {"));
        assert!(out.contains("value = null;"));
    }

    #[test]
    fn database_backed_models_merge_columns() {
        let catalog = TableCatalog {
            tables: vec![TableSchema {
                name: "users".into(),
                columns: vec![
                    ColumnSchema {
                        name: "id".into(),
                        data_type: "BIGSERIAL".into(),
                        nullable: false,
                        primary_key: true,
                    },
                    ColumnSchema {
                        name: "display_name".into(),
                        data_type: "VARCHAR(255)".into(),
                        nullable: true,
                        primary_key: false,
                    },
                ],
            }],
        };
        let src = "model User extends select<users> {\n  displayName: String .nonempty()\n}\n";
        let out = generate_typescript_models(&registry(src), &catalog);
        assert!(out.contains("  id: bigint;"));
        assert!(out.contains("  displayName?: string | null;"));
        assert!(out.contains("checkNonEmpty(value, fieldPath, errors);"));
        assert!(out.contains("export interface User {"));
    }

    #[test]
    fn queries_emit_params_binding_and_return_types() {
        let src = r#"
model User { id: UUID }
query GetUser($id: UUID) -> User? {
  SELECT * FROM users WHERE id = $id;
}
query GetActiveUsers() -> User[] {
  SELECT * FROM users WHERE active = true;
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("export interface GetUserParams {"));
        assert!(
            out.contains("export async function getUser(\n  sql: Sql,\n  params: GetUserParams")
        );
        assert!(out.contains("): Promise<User | null> {"));
        assert!(out.contains("${params.id}"));
        assert!(out.contains("const rows = await sql<User[]>`"));
        assert!(out.contains("return rows[0] ?? null;"));
        assert!(out.contains("Promise<User[]>"));
        assert!(out.contains("export async function getActiveUsers("));
    }

    #[test]
    fn target_override_filters_queries() {
        let src = r#"
@target("typescript")
query TsOnly($id: UUID) -> Int {
  SELECT 1;
}
@target("rust")
query RustOnly($id: UUID) -> Int {
  SELECT 1;
}
query Open($id: UUID) -> Int {
  SELECT 1;
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("export async function tsOnly("));
        assert!(out.contains("export async function open("));
        assert!(!out.contains("rustOnly"));
        assert!(!out.contains("RustOnlyParams"));
    }

    #[test]
    fn target_excluded_queries_do_not_pull_in_sql_import_or_helpers() {
        let src = r#"
@target("rust")
query OnlyRust($id: UUID) -> Int {
  SELECT 1;
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(!out.contains("Sql"), "no db import for a rust-only query");
        assert!(!out.contains("export async function"));
    }

    #[test]
    fn structured_input_params_bind_dotted_fields() {
        let src = r#"
model CreateUserInput { email: String .email() }
query CreateUser($input: CreateUserInput) {
  INSERT INTO users (email) VALUES ($input.email);
}
"#;
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("${params.input.email}"));
        assert!(out.contains("checkEmail(value, fieldPath, errors);"));
    }

    #[test]
    fn custom_rule_messages_flow_into_check_calls() {
        let src = "model User {\n  email: String .email(\"invalid email\")\n  age: Int .min(18, \"adults only\")\n}\n";
        let out = generate_typescript_models(&registry(src), &no_catalog());
        assert!(out.contains("checkEmail(value, fieldPath, errors, \"invalid email\");"));
        assert!(out.contains("checkMin(value, 18, fieldPath, errors, \"adults only\");"));
        assert!(out.contains("function checkEmail(value: string, path: Seg[], errors: ValidationError[], message?: string): boolean"));
        assert!(
            out.contains("return fail(errors, path, message ?? 'must be a valid email address');")
        );
    }

    #[test]
    fn bind_sql_skips_placeholders_inside_literals() {
        let params = vec![crate::axm::ast::ParamDecl {
            name: "email".into(),
            ty: TypeRef::String,
        }];
        let sql = "SELECT '<cost is $5>', email\n".to_string()
            + "FROM users -- $email is a comment\n"
            + "WHERE status = '$$draft$$' AND email = $email";
        let out = bind_sql(&sql, &params);
        assert!(out.contains("<cost is $5>"));
        assert!(out.contains("-- $email is a comment"));
        assert!(out.contains("$$draft$$"));
        assert!(out.contains("${params.email}"));
        assert_eq!(out.matches("${params.email}").count(), 1);
    }
}
