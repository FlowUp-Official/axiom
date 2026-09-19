//! Lint rules over `.axm` domain-model files.

use std::collections::BTreeSet;

use axiom_core::axm::ast::{
    AnnotatedType, AxmFile, FieldDecl, ImportStmt, ModelDecl, QueryDecl, Rule, TypeRef,
};
use axiom_core::axm::is_identifier;
use axiom_core::query::{Placeholder, scan_placeholders};
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
            .with_help(
                "reference it from a model field, query, type alias, or import, or remove it",
            );
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
            if violates_pascal_case(&model.name) {
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
                if violates_camel_case(&field.name) {
                    let span = field_line_start(ctx.source, &field.name)
                        .and_then(|ls| word_span(ctx.source, ls, &field.name));
                    push_naming(ctx, &mut out, "field", &field.name, "camelCase", span);
                }
            }
        }
        for ty in &file.types {
            if violates_pascal_case(&ty.name) {
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
            if violates_pascal_case(&query.name) {
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
                if violates_camel_case(&param.name) {
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

/// Whether `name` is a bare identifier that violates Axiom's PascalCase
/// convention for models, type aliases, and queries.
///
/// Quoted names are not identifiers, so they are exempt: the grammar lets an
/// author deliberately escape identifier rules (e.g. `"first-name": String`),
/// and such names are not expected to follow a case convention.
fn violates_pascal_case(name: &str) -> bool {
    is_identifier(name) && !is_pascal_case(name)
}

/// Whether `name` is a bare identifier that violates Axiom's camelCase
/// convention for fields and query parameters. Quoted names are exempt, as for
/// [`violates_pascal_case`].
fn violates_camel_case(name: &str) -> bool {
    is_identifier(name) && !is_camel_case(name)
}

/// PascalCase over Axiom's identifier grammar: no `_`, and an uppercase ASCII
/// first letter. Capitalization marks word boundaries, so acronyms and runs of
/// capitals are allowed (`User`, `UserProfile`, `UUID`, `User2`) — matching the
/// primitive type names (`UUID`, `DateTime`) and the code generator.
fn is_pascal_case(name: &str) -> bool {
    !name.contains('_') && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// camelCase over Axiom's identifier grammar: no `_`, and a lowercase ASCII
/// first letter (`user`, `userName`, `user2`). Capital runs after the first
/// character are allowed, so `userID` remains valid.
fn is_camel_case(name: &str) -> bool {
    !name.contains('_') && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
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

/// Flags `type` aliases that no model, field, query, or other alias references
/// anywhere in the workspace.
#[derive(Debug)]
pub struct UnusedTypeAlias;

impl LintRule for UnusedTypeAlias {
    fn name(&self) -> &'static str {
        "unused-type-alias"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for ty in &file.types {
            if ctx.workspace.referenced_types.contains(&ty.name) {
                continue;
            }
            let span = decl_name_span(ctx.source, "type", &ty.name);
            let mut diag = Diagnostic::warning(
                ctx.file,
                "lint.unused-type-alias",
                format!("type alias `{}` is never referenced", ty.name),
            )
            .with_help("reference it from a model, query, or another type alias, or remove it");
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            out.push(diag);
        }
        out
    }
}

/// Flags validator combinations that no value can satisfy: contradictory
/// numeric bounds (`.min(10) .max(5)`), contradictory length bounds
/// (`.min_length(10) .max_length(5)`), or `.nonempty()` combined with
/// `.max_length(0)`.
#[derive(Debug)]
pub struct UnsatisfiableValidator;

impl LintRule for UnsatisfiableValidator {
    fn name(&self) -> &'static str {
        "unsatisfiable-validator"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for model in &file.models {
            for field in &model.fields {
                let span = field_line_start(ctx.source, &field.name)
                    .and_then(|ls| word_span(ctx.source, ls, &field.name));
                self.check_type(
                    ctx,
                    &field.ty,
                    span,
                    &format!("field `{}` of model `{}`", field.name, model.name),
                    &mut out,
                );
            }
        }
        for ty in &file.types {
            let span = decl_name_span(ctx.source, "type", &ty.name);
            self.check_type(ctx, &ty.ty, span, &format!("type `{}`", ty.name), &mut out);
        }
        out
    }
}

impl UnsatisfiableValidator {
    fn check_type(
        &self,
        ctx: &LintContext<'_>,
        ty: &AnnotatedType,
        span: Option<Span>,
        subject: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let mut min: Option<i64> = None;
        let mut max: Option<i64> = None;
        let mut min_len: Option<usize> = None;
        let mut max_len: Option<usize> = None;
        let mut nonempty = false;
        for rule in &ty.rules {
            match rule {
                Rule::Min(n, _) => min = Some(min.map_or(*n, |cur| cur.max(*n))),
                Rule::Max(n, _) => max = Some(max.map_or(*n, |cur| cur.min(*n))),
                Rule::MinLength(n, _) => min_len = Some(min_len.map_or(*n, |cur| cur.max(*n))),
                Rule::MaxLength(n, _) => max_len = Some(max_len.map_or(*n, |cur| cur.min(*n))),
                Rule::NonEmpty(_) => nonempty = true,
                _ => {}
            }
        }

        let mut push = |message: String, help: &str| {
            let mut diag =
                Diagnostic::warning(ctx.file, "lint.unsatisfiable-validator", message)
                    .with_help(help);
            if let Some(span) = span {
                diag = diag.with_span(span);
            }
            out.push(diag);
        };

        if let (Some(lo), Some(hi)) = (min, max)
            && lo > hi
        {
            push(
                format!(
                    "{subject} has contradictory bounds: `.min({lo})` with `.max({hi})` can never be satisfied"
                ),
                "widen the bounds so a value can satisfy both",
            );
        }
        if let (Some(lo), Some(hi)) = (min_len, max_len)
            && lo > hi
        {
            push(
                format!(
                    "{subject} has contradictory length bounds: `.min_length({lo})` with `.max_length({hi})` can never be satisfied"
                ),
                "widen the length bounds so a value can satisfy both",
            );
        }
        if nonempty && max_len == Some(0) {
            push(
                format!(
                    "{subject} combines `.nonempty()` with `.max_length(0)`, so no value can satisfy it"
                ),
                "raise the maximum length or drop `.nonempty()`",
            );
        }
    }
}

/// Flags query parameters declared in a signature but never referenced by the
/// query's SQL body (`$name`, `$1`, or `$name.field`).
#[derive(Debug)]
pub struct UnusedQueryParam;

impl LintRule for UnusedQueryParam {
    fn name(&self) -> &'static str {
        "unused-query-param"
    }

    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some(file) = &ctx.axm else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for query in &file.queries {
            let used = used_param_indices(query);
            for (index, param) in query.params.iter().enumerate() {
                if used.contains(&index) {
                    continue;
                }
                let span = param_span(ctx.source, &param.name);
                let mut diag = Diagnostic::warning(
                    ctx.file,
                    "lint.unused-query-param",
                    format!(
                        "parameter `${}` of query `{}` is never used in its SQL body",
                        param.name, query.name
                    ),
                )
                .with_help(format!(
                    "remove the parameter or reference `${}` in the query body",
                    param.name
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

/// The 0-based indices of parameters referenced by `query.sql`, whether by name
/// (`$email`), by position (`$1`), or as the base of a structured path
/// (`$input.email`).
fn used_param_indices(query: &QueryDecl) -> BTreeSet<usize> {
    let mut used = BTreeSet::new();
    for (_, _, kind) in scan_placeholders(&query.sql) {
        match kind {
            Placeholder::Positional(n) if n >= 1 && n <= query.params.len() => {
                used.insert(n - 1);
            }
            Placeholder::Named(name) => {
                if let Some(index) = query.params.iter().position(|p| p.name == name) {
                    used.insert(index);
                }
            }
            _ => {}
        }
    }
    used
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
    fn dead_model_reports_only_unreferenced_models() {
        let source = "model Live {\n  x: String\n}\nmodel AlsoLive {\n  y: String\n}\n\
                      model Dead {\n  z: String\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_models.insert("Live".to_string());
        ws.referenced_models.insert("AlsoLive".to_string());
        let c = ctx(source, &ws);
        let diags = DeadModel.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.dead-model");
        assert!(diags[0].message.contains("Dead"), "{}", diags[0].message);
        assert!(!diags[0].message.contains("AlsoLive"));
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn model_referenced_as_query_return_is_not_dead() {
        let source = "model User {\n  id: UUID\n}\n\
                      query GetUser($id: UUID) -> User? {\n  SELECT id FROM users\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_models.insert("User".to_string());
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

    fn flagged_names(diags: &[Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter_map(|d| d.message.split('`').nth(1).map(str::to_string))
            .collect()
    }

    #[test]
    fn naming_convention_rejects_underscores_and_wrong_initial_case() {
        let source = "type user_name = String\n\
                      type User_name = String\n\
                      type UserProfile_name = String\n\
                      model user {\n  ok: String\n}\n\
                      model User {\n  user_Name: String\n}\n\
                      query userName() {\n  SELECT id FROM users\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = NamingConvention.check(&c);
        assert!(
            diags.iter().all(|d| d.code == "lint.naming-convention"),
            "{diags:?}"
        );
        let names = flagged_names(&diags);
        for expected in [
            "user_name",
            "User_name",
            "UserProfile_name",
            "user",
            "user_Name",
            "userName",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "expected `{expected}` to be flagged, got {names:?}"
            );
        }
        let spanless: Vec<_> = diags.iter().filter(|d| d.span.is_none()).collect();
        assert!(spanless.is_empty(), "diagnostics without spans: {spanless:?}");
    }

    #[test]
    fn naming_convention_accepts_acronyms_digits_and_camel_case() {
        let source = "type EmailAddress = String\n\
                      type UUID = String\n\
                      model UserProfile {\n  userName: String\n  user: String\n  user2: String\n}\n\
                      query ListUsers($userId: UUID, $id: String) -> UserProfile[] {\n  SELECT userName FROM users\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = NamingConvention.check(&c);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn naming_convention_skips_quoted_field_names() {
        let source = "model User {\n  \"first-name\": String\n  \"x:y\": Int\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = NamingConvention.check(&c);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn unused_type_alias_is_reported() {
        let source = "type Email = String\nmodel User {\n  name: String\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnusedTypeAlias.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.unused-type-alias");
        assert!(diags[0].message.contains("Email"));
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn referenced_type_alias_is_not_reported() {
        let source = "type Email = String\nmodel User {\n  email: Email\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_types.insert("Email".to_string());
        let c = ctx(source, &ws);
        assert!(UnusedTypeAlias.check(&c).is_empty());
    }

    #[test]
    fn contradictory_numeric_bounds_are_reported() {
        let source = "model T {\n  x: Int .min(10) .max(5)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnsatisfiableValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.unsatisfiable-validator");
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn contradictory_length_bounds_are_reported() {
        let source = "model T {\n  x: String .min_length(10) .max_length(5)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnsatisfiableValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.unsatisfiable-validator");
    }

    #[test]
    fn nonempty_with_zero_max_length_is_reported() {
        let source = "model T {\n  x: String .nonempty() .max_length(0)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert_eq!(UnsatisfiableValidator.check(&c).len(), 1);
    }

    #[test]
    fn satisfiable_bounds_are_not_reported() {
        let source = "model T {\n  x: Int .min(5) .max(10)\n  y: String .min_length(5) .max_length(10)\n  z: String .nonempty() .max_length(10)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(
            UnsatisfiableValidator.check(&c).is_empty(),
            "{:?}",
            UnsatisfiableValidator.check(&c)
        );
    }

    #[test]
    fn out_of_order_bounds_use_effective_values() {
        let source = "model T {\n  x: Int .max(5) .min(10)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert_eq!(UnsatisfiableValidator.check(&c).len(), 1);
    }

    #[test]
    fn unused_query_param_is_reported() {
        let source = "model User { id: UUID }\nquery GetUser($id: UUID, $ghost: String) -> User? {\n  SELECT id FROM users WHERE id = $id\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnusedQueryParam.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.unused-query-param");
        assert!(diags[0].message.contains("ghost"));
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn positional_and_dotted_params_count_as_used() {
        let source = "model User { id: UUID }\nquery A($id: UUID) -> User? {\n  SELECT id FROM users WHERE id = $1\n}\nquery B($input: User) -> User? {\n  INSERT INTO users (id) VALUES ($input.id) RETURNING id\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(
            UnusedQueryParam.check(&c).is_empty(),
            "{:?}",
            UnusedQueryParam.check(&c)
        );
    }
}
