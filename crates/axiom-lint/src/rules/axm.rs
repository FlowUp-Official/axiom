//! Lint rules over `.axm` domain-model files.

use axiom_core::axm::ast::{AxmFile, FieldDecl, ImportStmt, ModelDecl, Rule, TypeRef};
use axiom_diagnostics::{Diagnostic, Span};

use crate::runner::{word_span, LintContext, LintRule};

/// Flags `import { X } from "..."` statements whose names are never used as a
/// field type in the importing file.
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
                if used.contains(name) {
                    continue;
                }
                let span = import_name_span(ctx.source, import, name);
                let mut diag = Diagnostic::warning(
                    ctx.file,
                    "lint.unused-import",
                    format!("imported model `{name}` is never used"),
                )
                .with_help(format!(
                    "remove `{name}` from the import from \"{}\"",
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

/// Flags non-exported models that no other model references or imports.
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
            if model.exported || ctx.workspace.referenced_models.contains(&model.name) {
                continue;
            }
            let span = model_name_span(ctx.source, &model.name);
            let mut diag = Diagnostic::warning(
                ctx.file,
                "lint.dead-model",
                format!("model `{}` is never referenced", model.name),
            )
            .with_help("export it or reference it from another model to remove this warning");
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

        for rule in &field.validations {
            let text = rule_text(rule);
            let redundant = seen.contains(&text)
                || match rule {
                    Rule::Min(n) => min.is_some_and(|cur| *n <= cur),
                    Rule::Max(n) => max.is_some_and(|cur| *n >= cur),
                    Rule::MinLen(n) => min_len.is_some_and(|cur| *n <= cur),
                    Rule::MaxLen(n) => max_len.is_some_and(|cur| *n >= cur),
                    _ => false,
                };
            if redundant {
                let span = rule_span(ctx.source, &field.name, &text);
                let mut diag = Diagnostic::warning(
                    ctx.file,
                    "lint.redundant-validator",
                    format!(
                        "`{text}` is redundant on field `{}` of model `{}`",
                        field.name, model.name
                    ),
                );
                if let Some(span) = span {
                    diag = diag.with_span(span);
                }
                out.push(diag);
                continue;
            }
            seen.push(text);
            match rule {
                Rule::Min(n) => min = Some(min.map_or(*n, |cur| cur.max(*n))),
                Rule::Max(n) => max = Some(max.map_or(*n, |cur| cur.min(*n))),
                Rule::MinLen(n) => min_len = Some(min_len.map_or(*n, |cur| cur.max(*n))),
                Rule::MaxLen(n) => max_len = Some(max_len.map_or(*n, |cur| cur.min(*n))),
                _ => {}
            }
        }
    }
}

/// Every model name referenced as a field type anywhere in the file.
fn collect_named_types(file: &AxmFile) -> Vec<String> {
    let mut names = Vec::new();
    for model in &file.models {
        for field in &model.fields {
            collect_type_refs(&field.ty, &mut names);
        }
    }
    names
}

fn collect_type_refs(ty: &TypeRef, out: &mut Vec<String>) {
    match ty {
        TypeRef::Named(name) => out.push(name.clone()),
        TypeRef::Array(inner) => collect_type_refs(inner, out),
        _ => {}
    }
}

fn rule_text(rule: &Rule) -> String {
    match rule {
        Rule::Min(n) => format!(".min({n})"),
        Rule::Max(n) => format!(".max({n})"),
        Rule::MinLen(n) => format!(".min_len({n})"),
        Rule::MaxLen(n) => format!(".max_len({n})"),
        Rule::Regex(p) => format!(".regex(\"{p}\")"),
        Rule::Email => ".email()".to_string(),
        Rule::Url => ".url()".to_string(),
        Rule::Uuid => ".uuid()".to_string(),
        Rule::Alphanumeric => ".alphanumeric()".to_string(),
        Rule::NonEmpty => ".nonempty()".to_string(),
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
    let mut offset = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("export model")
            .or_else(|| trimmed.strip_prefix("model"))
            .map(str::trim_start)
        else {
            offset += line.len() + 1;
            continue;
        };
        if let Some(after) = rest.strip_prefix(name) {
            let next = after.chars().next();
            if next.is_none() || next.is_some_and(|c| c.is_ascii_whitespace() || c == '{') {
                let leading = line.len() - line.trim_start().len();
                let name_start = offset + leading + (line.trim_start().len() - rest.len());
                return Some(Span::new(name_start, name_start + name.len()));
            }
        }
        offset += line.len() + 1;
    }
    None
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
            axm,
            statements: None,
            workspace,
        }
    }

    #[test]
    fn unused_import_is_reported() {
        let source = "import { Address, ZipCode } from \"geo\"\nexport model User {\n  name: string\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = UnusedImport.check(&c);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.code == "lint.unused-import"));
        assert!(diags.iter().all(|d| d.span.is_some()));
    }

    #[test]
    fn used_import_is_not_reported() {
        let source = "import { Address } from \"geo\"\nexport model User {\n  billing: Address\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(UnusedImport.check(&c).is_empty());
    }

    #[test]
    fn dead_model_is_reported() {
        let source = "model Internal {\n  x: string\n}\nexport model Api {\n  y: string\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = DeadModel.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.dead-model");
    }

    #[test]
    fn exported_and_referenced_models_are_not_dead() {
        let source = "export model Address {\n  street: string\n}\nmodel User {\n  billing: Address\n}";
        let mut ws = WorkspaceView::empty();
        ws.referenced_models.insert("User".to_string());
        let c = ctx(source, &ws);
        assert!(DeadModel.check(&c).is_empty());
    }

    #[test]
    fn redundant_min_is_reported() {
        let source = "model T {\n  x: int .min(10) .min(5)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = RedundantValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.redundant-validator");
        assert!(diags[0].span.is_some());
    }

    #[test]
    fn stricter_bounds_are_not_redundant() {
        let source = "model T {\n  x: int .min(5) .min(10)\n  y: string .max_len(20) .max_len(10)\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        assert!(RedundantValidator.check(&c).is_empty(), "{:?}", RedundantValidator.check(&c));
    }

    #[test]
    fn duplicate_validator_is_reported() {
        let source = "model T {\n  email: string .email() .email()\n}";
        let ws = WorkspaceView::empty();
        let c = ctx(source, &ws);
        let diags = RedundantValidator.check(&c);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
