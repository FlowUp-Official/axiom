//! Lint rules over `.axm` domain-model files.

use axiom_core::axm::ast::{
    AnnotatedType, AxmFile, FieldDecl, ImportStmt, ModelDecl, Rule, TypeRef,
};
use axiom_diagnostics::{Diagnostic, Span};

use crate::runner::{LintContext, LintRule, word_span};

/// Flags `import { X } from "..."` statements whose names are never used as a
/// field type in the importing file. Aliased imports are matched by the written
/// (aliased) name.
#[derive(Debug)]
pub struct UnusedImport;

impl LintRule for UnusedImport {
    fn name(&self) -> &'static str {
        "unused-import"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };
        let used = collect_named_types(file);

        let mut out = Vec::new();
        for import in &file.imports {
            for name in &import.names {
                let written = name.alias.as_deref().unwrap_or(&name.name);
                if used.iter().any(|u| u == written) {
                    continue;
                }
                let span = import_name_span(ctx.source, import, written);
                let mut diag = Diagnostic::warning(
                    ctx.file,
                    "lint.unused-import",
                    format!("imported model `{written}` is never used"),
                )
                .with_help(format!(
                    "remove `{written}` from the import from \"{}\"",
                    import.source
                ));
                if let Some(span) = span {
                    diag = diag.with_span(span);
                }
                out.push(diag);
            }
        }
        out
    }
}

/// Flags models that no other model references or imports anywhere in the
/// workspace (including within their own file).
#[derive(Debug)]
pub struct DeadModel;

impl LintRule for DeadModel {
    fn name(&self) -> &'static str {
        "dead-model"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for model in &file.models {
            if ctx.workspace.referenced_models.contains(&model.name) {
                continue;
            }
            let span = model_name_span(ctx.source, &model.name);
            let mut diag = Diagnostic::warning(
                ctx.file,
                "lint.dead-model",
                format!("model `{}` is never referenced", model.name),
            )
            .with_help("reference it from another model to remove this warning");
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            out.push(diag);
        }
        out
    }
}

/// Flags validation rules that are redundant given earlier rules on the same
/// field: duplicate calls, or bounds strictly weaker than the effective bound
/// already established (e.g. `.min(10).min(5)`).
#[derive(Debug)]
pub struct RedundantValidator;

impl LintRule for RedundantValidator {
    fn name(&self) -> &'static str {
        "redundant-validator"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for model in &file.models {
            for field in &model.fields {
                self.check_field(ctx, model, field, &mut out);
            }
        }
        out
    }
}

impl RedundantValidator {
    fn check_field(
        &self,
        ctx: &LintContext<'_>,
        model: &ModelDecl,
        field: &FieldDecl,
        out: &mut Vec<Diagnostic>,
    ) {
        let mut min: Option<i64> = None;
        let mut max: Option<i64> = None;
        let mut min_len: Option<usize> = None;
        let mut max_len: Option<usize> = None;
        let mut seen: Vec<String> = Vec::new();

        for rule in &field.ty.rules {
            let canonical = rule_text(rule);
            let redundant = seen.contains(&canonical)
                || match rule {
                    Rule::Min(n, _) => min.is_some_and(|cur| *n <= cur),
                    Rule::Max(n, _) => max.is_some_and(|cur| *n >= cur),
                    Rule::MinLength(n, _) => min_len.is_some_and(|cur| *n <= cur),
                    Rule::MaxLength(n, _) => max_len.is_some_and(|cur| *n >= cur),
                    _ => false,
                };
            if redundant {
                let span = rule_span(ctx.source, &field.name, &canonical);
                let mut diag = Diagnostic::warning(
                    ctx.file,
                    "lint.redundant-validator",
                    format!(
                        "`{canonical}` is redundant on field `{}` of model `{}`",
                        field.name, model.name
                    ),
                );
                if let Some(span) = span {
                    diag = diag.with_span(span);
                }
                out.push(diag);
                continue;
            }
            seen.push(canonical);
            match rule {
                Rule::Min(n, _) => min = Some(min.map_or(*n, |cur| cur.max(*n))),
                Rule::Max(n, _) => max = Some(max.map_or(*n, |cur| cur.min(*n))),
                Rule::MinLength(n, _) => min_len = Some(min_len.map_or(*n, |cur| cur.max(*n))),
                Rule::MaxLength(n, _) => max_len = Some(max_len.map_or(*n, |cur| cur.min(*n))),
                _ => {}
            }
        }
    }
}

/// Flags declarations that break the naming conventions: models, type aliases,
/// and queries must be PascalCase; fields and query parameters must be
/// camelCase.
#[derive(Debug)]
pub struct NamingConvention;

impl LintRule for NamingConvention {
    fn name(&self) -> &'static str {
        "naming-convention"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for model in &file.models {
            if !is_pascal_case(&model.name) {
                push_naming(
                    ctx,
                    &mut out,
                    "model",
                    &model.name,
                    "PascalCase",
                    model_name_span(ctx.source, &model.name),
                );
            }
            for field in &model.fields {
                if !is_camel_case(&field.name) {
                    let span = field_line_start(ctx.source, &field.name)
                        .and_then(|ls| word_span(ctx.source, ls, &field.name));
                    push_naming(ctx, &mut out, "field", &field.name, "camelCase", span);
                }
            }
        }
        for ty in &file.types {
            if !is_pascal_case(&ty.name) {
                push_naming(
                    ctx,
                    &mut out,
                    "type",
                    &ty.name,
                    "PascalCase",
                    decl_name_span(ctx.source, "type", &ty.name),
                );
            }
        }
        for query in &file.queries {
            if !is_pascal_case(&query.name) {
                push_naming(
                    ctx,
                    &mut out,
                    "query",
                    &query.name,
                    "PascalCase",
                    decl_name_span(ctx.source, "query", &query.name),
                );
            }
            for param in &query.params {
                if !is_camel_case(&param.name) {
                    push_naming(
                        ctx,
                        &mut out,
                        "parameter",
                        &param.name,
                        "camelCase",
                        param_span(ctx.source, &param.name),
                    );
                }
            }
        }
        out
    }
}

fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn is_camel_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

fn push_naming(
    ctx: &LintContext<'_>,
    out: &mut Vec<Diagnostic>,
    kind: &str,
    name: &str,
    convention: &str,
    span: Option<Span>,
) {
    let mut diag = Diagnostic::warning(
        ctx.file,
        "lint.naming-convention",
        format!("{kind} `{name}` should be {convention}"),
    )
    .with_help(format!(
        "rename `{name}` to follow the {convention} convention"
    ));
    if let Some(span) = span {
        diag = diag.with_span(span);
    }
    out.push(diag);
}

/// Byte span of a `type`/`query`/`model` declaration name.
fn decl_name_span(source: &str, keyword: &str, name: &str) -> Option<Span> {
    let mut offset = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(keyword).map(str::trim_start) else {
            offset += line.len() + 1;
            continue;
        };
        if let Some(after) = rest.strip_prefix(name) {
            let next = after.chars().next();
            if next.is_none()
                || next.is_some_and(|c| c.is_ascii_whitespace() || matches!(c, '{' | '(' | '='))
            {
                let leading = line.len() - line.trim_start().len();
                let name_start = offset + leading + (line.trim_start().len() - rest.len());
                return Some(Span::new(name_start, name_start + name.len()));
            }
        }
        offset += line.len() + 1;
    }
    None
}

/// Byte span of a `$name:` parameter declaration.
fn param_span(source: &str, name: &str) -> Option<Span> {
    let needle = format!("${name}:");
    for (start, raw) in source.match_indices(&needle) {
        let end = start + raw.len() - 1;
        let before = source[..start].chars().next_back();
        let boundary = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if boundary {
            return Some(Span::new(start + 1, end));
        }
    }
    None
}

/// Every named type reference anywhere in the file: model/type fields, type
/// alias bases, and query parameters and return types.
fn collect_named_types(file: &AxmFile) -> Vec<String> {
    let mut names = Vec::new();
    for ty in &file.types {
        collect_annotated(&ty.ty, &mut names);
    }
    for model in &file.models {
        for field in &model.fields {
            collect_annotated(&field.ty, &mut names);
        }
    }
    for query in &file.queries {
        for param in &query.params {
            collect_type_refs(&param.ty, &mut names);
        }
        use axiom_core::axm::ast::QueryReturn;
        match &query.return_type {
            QueryReturn::Exec => {}
            QueryReturn::Single(ty) | QueryReturn::Optional(ty) | QueryReturn::Many(ty) => {
                collect_type_refs(ty, &mut names)
            }
        }
    }
    names
}

fn collect_annotated(ty: &AnnotatedType, out: &mut Vec<String>) {
    collect_type_refs(&ty.base, out);
}

fn collect_type_refs(ty: &TypeRef, out: &mut Vec<String>) {
    match ty {
        TypeRef::Named(name) => out.push(name.clone()),
        TypeRef::Array(inner) => collect_type_refs(inner, out),
        TypeRef::Nullable(inner) => collect_type_refs(inner, out),
        _ => {}
    }
}

/// The canonical spelling of a rule, e.g. `.min(5)` — without any custom
/// message, so duplicate rules with different messages are still detected.
fn rule_text(rule: &Rule) -> String {
    match rule {
        Rule::Min(n, _) => format!(".min({n})"),
        Rule::Max(n, _) => format!(".max({n})"),
        Rule::MinLength(n, _) => format!(".min_length({n})"),
        Rule::MaxLength(n, _) => format!(".max_length({n})"),
        Rule::Regex(_, _) => ".regex".to_string(),
        Rule::Email(_) => ".email()".to_string(),
        Rule::Url(_) => ".url()".to_string(),
        Rule::Uuid(_) => ".uuid()".to_string(),
        Rule::Ulid(_) => ".ulid()".to_string(),
        Rule::Ipv4(_) => ".ipv4()".to_string(),
        Rule::Ipv6(_) => ".ipv6()".to_string(),
        Rule::IsoDate(_) => ".isodate()".to_string(),
        Rule::Alphanumeric(_) => ".alphanumeric()".to_string(),
        Rule::NonEmpty(_) => ".nonempty()".to_string(),
    }
}

/// Byte span of the first line whose trimmed content begins with `name` and is
/// followed by `?` or `:` — i.e. a field declaration.
fn field_line_start(source: &str, name: &str) -> Option<usize> {
    let mut offset = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(name)
            && (rest.starts_with('?') || rest.starts_with(':'))
        {
            return Some(offset);
        }
        offset += line.len() + 1;
    }
    None
}

/// Byte span of the `import` statement that imports from `import.source`.
fn import_line_start(source: &str, import: &ImportStmt) -> Option<usize> {
    let marker = format!("\"{}\"", import.source);
    let mut offset = 0;
    for line in source.lines() {
        if line.trim_start().starts_with("import") && line.contains(&marker) {
            return Some(offset);
        }
        offset += line.len() + 1;
    }
    None
}

fn import_name_span(source: &str, import: &ImportStmt, name: &str) -> Option<Span> {
    let line_start = import_line_start(source, import)?;
    word_span(source, line_start, name)
}

fn model_name_span(source: &str, name: &str) -> Option<Span> {
    let span = decl_name_span(source, "model", name)?;
    Some(span)
}

/// Byte span of the first occurrence of `text` (e.g. `.min(5)`) on the field's
/// declaration line.
fn rule_span(source: &str, field: &str, text: &str) -> Option<Span> {
    let line_start = field_line_start(source, field)?;
    let line_end = source[line_start..]
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let rel = line.find(text)?;
    Some(Span::new(line_start + rel, line_start + rel + text.len()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::runner::WorkspaceView;

    fn ctx<'a>(source: &'a str, workspace: &'a WorkspaceView) -> LintContext<'a> {
        let file = Path::new("models/test.axm");
        let axm = axiom_core::axm::parser::parse_axm_file(source).ok();
        LintContext {
            file,
            source,
            origin: 0,
            axm,
            statements: None,
            workspace,
        }
    }

    #[test]
    fn unused_import_is_reported() {
        let source = "import { Address, ZipCode } from \"geo\"\nmodel User {\n  name: String\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnusedImport.check(&c);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.code == "lint.unused-import"));
        assert!(diags.iter().all(|d| d.span.is_some()));
    }

    #[test]
    fn used_import_is_not_reported() {
        let source = "import { Address } from \"geo\"\nmodel User {\n  billing: Address\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(UnusedImport.check(&c).is_empty());
    }

    #[test]
    fn aliased_import_matches_written_name() {
        let source = "import { Address as Home } from \"geo\"\nmodel User {\n  billing: Home\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(UnusedImport.check(&c).is_empty());
    }

    #[test]
    fn dead_model_is_reported() {
        let source = "model Internal {\n  x: String\n}\nmodel Api {\n  y: String\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_models.insert("Api".to_string());
        let c = ctx(source, &ws);
        let diags = DeadModel.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.dead-model");
    }

    #[test]
    fn referenced_models_are_not_dead() {
        let source = "model Address {\n  street: String\n}\nmodel User {\n  billing: Address\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_models.insert("User".to_string());
        ws.referenced_models.insert("Address".to_string());
        let c = ctx(source, &ws);
        assert!(DeadModel.check(&c).is_empty());
    }

    #[test]
    fn redundant_min_is_reported() {
        let source = "model T {\n  x: Int .min(10) .min(5)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = RedundantValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.redundant-validator");
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn stricter_bounds_are_not_redundant() {
        let source =
            "model T {\n  x: Int .min(5) .min(10)\n  y: String .max_length(20) .max_length(10)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(RedundantValidator.check(&c).is_empty());
    }

    #[test]
    fn duplicate_validator_is_reported() {
        let source = "model T {\n  email: String .email() .email()\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = RedundantValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn naming_convention_flags_bad_identifiers() {
        let source = "type email_address = String\ntype ValidEmail = String\nmodel user {\n  Name: String\n  email: String\n}\nquery listUsers($User: Int) -> user[] {\n  SELECT id FROM users\n}\n";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = NamingConvention.check(&c);
        let kinds: Vec<&str> = diags
            .iter()
            .map(|d| d.message.split(' ').next().unwrap())
            .collect();
        assert!(diags.iter().all(|d| d.code == "lint.naming-convention"));
        assert!(kinds.contains(&"type"), "{kinds:?}");
        assert!(kinds.contains(&"model"), "{kinds:?}");
        assert!(kinds.contains(&"query"), "{kinds:?}");
        assert!(kinds.contains(&"field"), "{kinds:?}");
        assert!(kinds.contains(&"parameter"), "{kinds:?}");
        assert!(diags.iter().all(|d| d.span.is_some()));
    }

    #[test]
    fn naming_convention_passes_canonical_names() {
        let source = "type EmailAddress = String\nmodel User {\n  displayName: String\n}\nquery ListUsers($limit: Int) -> User[] {\n  SELECT id FROM users\n}\n";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(NamingConvention.check(&c).is_empty());
    }
}
