//! Rule registry, workspace view, and per-file lint driver.
//!
//! Each file is parsed once (`.axm` AST and, for query bodies, a SQL
//! statement list) and handed to every enabled rule. SQL files contribute one
//! context; `.axm` files contribute one context per `query` body in addition
//! to the file-wide `.axm` context. Lint results are cached in the
//! content-addressed [`ToolCache`] keyed by `lint:<rules>:<blake3-of-source>`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axiom_core::axm::ast::AxmFile;
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::cache::{ToolCache, compute_content_hash};
use axiom_diagnostics::{Diagnostic, Span};
use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::rules::axm::{
    DeadModel, NamingConvention, RedundantValidator, UnusedImport, UnusedQueryParam,
    UnusedTypeAlias, UnsatisfiableValidator,
};
use crate::rules::sql::{
    MissingPrimaryKey, MissingWhereClause, SelectStar, UnindexedForeignKey,
};

/// Cross-file information that rules may need.
///
/// Populated by the CLI before running: every name referenced anywhere in the
/// workspace (imports, type-alias bases, model fields, and query parameter and
/// return types). `referenced_models` drives `dead-model`, and
/// `referenced_types` drives `unused-type-alias`.
#[derive(Debug, Default)]
pub struct WorkspaceView {
    pub referenced_models: BTreeSet<String>,
    pub referenced_types: BTreeSet<String>,
}

impl WorkspaceView {
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Everything a rule needs to inspect one file (or one SQL region within a
/// file — see [`build_contexts`]).
pub struct LintContext<'a> {
    pub file: &'a Path,
    /// The text being analyzed. For `.axm` files this is a query body, not the
    /// whole file.
    pub source: &'a str,
    /// Byte offset of `source` within its file (0 for whole-file contexts).
    /// Diagnostics report spans relative to this context, so callers shift
    /// them by `origin` before reporting.
    pub origin: usize,
    /// The parsed `.axm` AST, when the context covers a whole `.axm` file.
    pub axm: Option<AxmFile>,
    /// The parsed SQL statement list, when the context covers SQL text.
    pub statements: Option<Vec<Statement>>,
    pub workspace: &'a WorkspaceView,
}

/// A single lint rule. Implementations must be stateless; file state lives in
/// [`LintContext`].
pub trait LintRule {
    fn name(&self) -> &'static str;
    fn check(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic>;
}

/// An ordered set of [`LintRule`]s that run over a group of files.
pub struct LintRunner {
    rules: Vec<Box<dyn LintRule>>,
}

impl LintRunner {
    /// Every built-in rule, in stable order.
    pub fn all() -> Self {
        Self {
            rules: vec![
                Box::new(UnusedImport),
                Box::new(UnusedTypeAlias),
                Box::new(DeadModel),
                Box::new(RedundantValidator),
                Box::new(UnsatisfiableValidator),
                Box::new(NamingConvention),
                Box::new(UnusedQueryParam),
                Box::new(MissingWhereClause),
                Box::new(SelectStar),
                Box::new(UnindexedForeignKey),
                Box::new(MissingPrimaryKey),
            ],
        }
    }

    /// Only the named rules. Unknown names are silently dropped so callers can
    /// pre-validate names against [`Self::rule_names`].
    pub fn named(names: &[String]) -> Self {
        let wanted: BTreeSet<&str> = names.iter().map(|s| s.as_str()).collect();
        Self {
            rules: Self::all()
                .rules
                .into_iter()
                .filter(|r| wanted.contains(r.name()))
                .collect(),
        }
    }

    pub fn rule_names(&self) -> Vec<&'static str> {
        self.rules.iter().map(|r| r.name()).collect()
    }

    /// Stable key fragment for cache keys (sorted so rule order does not
    /// affect caching).
    pub fn rule_key(&self) -> String {
        let mut names: Vec<&str> = self.rule_names();
        names.sort_unstable();
        names.join("+")
    }

    pub fn run_file(&self, ctx: &LintContext<'_>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for rule in &self.rules {
            out.extend(rule.check(ctx));
        }
        out
    }
}

/// Selection of which rules to run.
#[derive(Debug, Default)]
pub struct LintOptions {
    /// Names of rules to run; empty means all.
    pub rules: Vec<String>,
}

/// Lint every file, reusing the content-addressed [`ToolCache`] when provided.
///
/// Cache failures degrade to recomputation; a cached payload is trusted only
/// when its key (which embeds the source hash) matches.
pub fn lint_sources(
    mut cache: Option<&mut ToolCache>,
    files: &[(PathBuf, String)],
    workspace: &WorkspaceView,
    options: &LintOptions,
) -> Vec<Diagnostic> {
    let runner = if options.rules.is_empty() {
        LintRunner::all()
    } else {
        LintRunner::named(&options.rules)
    };
    let rule_key = runner.rule_key();

    let mut out = Vec::new();
    for (path, src) in files {
        let key = format!(
            "lint:{}:{}",
            rule_key,
            hex(compute_content_hash(src.as_bytes()))
        );

        if let Some(cache) = cache.as_deref()
            && let Some(payload) = cache.get(&key)
            && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
        {
            out.extend(cached);
            continue;
        }

        let contexts = build_contexts(path, src, workspace);
        let mut diags = Vec::new();
        for ctx in &contexts {
            let mut rule_diags = runner.run_file(ctx);
            if ctx.origin > 0 {
                for diag in &mut rule_diags {
                    if let Some(span) = &mut diag.span {
                        span.start += ctx.origin;
                        span.end += ctx.origin;
                    }
                }
            }
            diags.extend(rule_diags);
        }

        if let Some(cache) = cache.as_mut()
            && let Ok(payload) = serde_json::to_vec(&diags)
        {
            cache.insert(key, payload);
        }
        out.extend(diags);
    }
    out
}

/// Build every lint context for a file:
///
/// - SQL files produce one context over the whole file.
/// - `.axm` files produce one context over the whole file (for `.axm` rules)
///   plus one context per `query` SQL body (for SQL rules), with `origin`
///   pointing at each body inside the file.
/// - Anything else produces a bare context with no parsed data.
pub fn build_contexts<'a>(
    path: &'a Path,
    src: &'a str,
    workspace: &'a WorkspaceView,
) -> Vec<LintContext<'a>> {
    if path.extension().is_some_and(|e| e == "sql") {
        return vec![LintContext {
            file: path,
            source: src,
            origin: 0,
            axm: None,
            statements: Parser::parse_sql(&GenericDialect {}, src).ok(),
            workspace,
        }];
    }

    let axm = parse_axm_file(src).ok();
    let Some(axm) = axm else {
        return vec![LintContext {
            file: path,
            source: src,
            origin: 0,
            axm: None,
            statements: None,
            workspace,
        }];
    };

    let mut contexts = Vec::with_capacity(axm.queries.len() + 1);
    let mut body_contexts = Vec::with_capacity(axm.queries.len());
    for query in &axm.queries {
        let Some(origin) = query_body_offset(src, &query.name, &query.sql) else {
            continue;
        };
        let body = &src[origin..origin + query.sql.len()];
        body_contexts.push(LintContext {
            file: path,
            source: body,
            origin,
            axm: None,
            statements: Parser::parse_sql(&GenericDialect {}, body).ok(),
            workspace,
        });
    }
    contexts.push(LintContext {
        file: path,
        source: src,
        origin: 0,
        axm: Some(axm),
        statements: None,
        workspace,
    });
    contexts.extend(body_contexts);
    contexts
}

/// Byte offset of a `query`'s trimmed SQL body within the full file source.
///
/// Anchored on the `query <name>` keyword pair (identifiers are unique per
/// file), then on the first `{` after it — parameter lists and `->` return
/// types never contain braces. Returns `None` when the body cannot be located.
fn query_body_offset(src: &str, name: &str, sql: &str) -> Option<usize> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for (start, _) in src.match_indices("query") {
        if start > 0 && src[..start].chars().next_back().is_some_and(is_word) {
            continue;
        }
        let after_kw = &src[start + "query".len()..];
        let Some(rel) = after_kw.find(name) else {
            continue;
        };
        let name_at = start + "query".len() + rel;
        let between = &src[start + "query".len()..name_at];
        if !between.trim().is_empty() {
            continue;
        }
        let before = src[..name_at].chars().next_back();
        let after = src[name_at + name.len()..].chars().next();
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            continue;
        }
        let rest = &src[name_at + name.len()..];
        let Some(k) = rest.find('{') else {
            continue;
        };
        let body = &rest[k + 1..];
        let lead = body.len() - body.trim_start().len();
        let origin = name_at + name.len() + k + 1 + lead;
        if src[origin..].starts_with(sql) {
            return Some(origin);
        }
    }
    None
}

/// Hex-encode a BLAKE3 digest for use in cache keys and messages.
pub fn hex(hash: [u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Find `name` as a whole word on the line beginning at `line_start`, returning
/// its byte span into `source`. Used for best-effort diagnostic placement.
pub fn word_span(source: &str, line_start: usize, name: &str) -> Option<Span> {
    let line_end = source[line_start..]
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let mut offset = 0;
    while offset <= line.len() {
        let Some(rel) = line[offset..].find(name) else {
            break;
        };
        let abs = offset + rel;
        let before = line[..abs].chars().last();
        let after = line[abs + name.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if boundary(before) && boundary(after) {
            return Some(Span::new(line_start + abs, line_start + abs + name.len()));
        }
        offset = abs + 1;
    }
    None
}

/// Byte offset of the start of line `n` (0-based) in `source`.
pub fn line_start_offset(source: &str, n: usize) -> usize {
    source
        .match_indices('\n')
        .nth(n)
        .map(|(i, _)| i + 1)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::rules::sql::{MissingWhereClause, SelectStar};

    #[test]
    fn axm_query_body_offset_located() {
        let src = "model User { id: UUID }\n\nquery DeleteAll() {\n  DELETE FROM users\n}\n\nquery List($n: Int) -> User[] {\n  SELECT id FROM users LIMIT $n\n}\n";
        let axm = parse_axm_file(src).unwrap();
        let first = query_body_offset(src, "DeleteAll", &axm.queries[0].sql);
        let second = query_body_offset(src, "List", &axm.queries[1].sql);
        assert_eq!(first, Some(src.find("DELETE").unwrap()));
        assert_eq!(second, Some(src.find("SELECT").unwrap()));
        assert_eq!(
            &src[first.unwrap()..first.unwrap() + axm.queries[0].sql.len()],
            "DELETE FROM users"
        );
    }

    #[test]
    fn query_body_offset_treats_leading_comment_as_body() {
        let src = "model User { id: UUID }\n\nquery List($n: Int) -> User[] {\n  -- SELECT counts silently, real body below\n  SELECT id FROM users LIMIT $n\n}\n";
        let axm = parse_axm_file(src).unwrap();
        let sql = &axm.queries[0].sql;
        let offset = query_body_offset(src, "List", sql).unwrap();
        assert_eq!(&src[offset..offset + sql.len()], sql);
        assert_eq!(&src[offset..offset + 2], "--");
    }

    #[test]
    fn sql_rules_run_over_axm_query_bodies() {
        let src = "model User { id: UUID }\n\nquery DeleteAll() {\n  DELETE FROM users\n}\n";
        let ws = WorkspaceView::empty();
        let contexts = build_contexts(Path::new("models/user.axm"), src, &ws);
        let body = contexts
            .iter()
            .find(|c| c.axm.is_none() && c.statements.is_some())
            .unwrap();
        let diags = MissingWhereClause.check(body);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.missing-where-clause");
        assert_eq!(diags[0].span, Some(Span::new(0, "delete".len())));
        assert!(SelectStar.check(body).is_empty());
    }

    fn select_star_diags(path: &str, src: &str) -> Vec<Diagnostic> {
        let ws = WorkspaceView::empty();
        let contexts = build_contexts(Path::new(path), src, &ws);
        contexts
            .iter()
            .flat_map(|ctx| SelectStar.check(ctx))
            .collect()
    }

    #[test]
    fn select_star_reported_once_per_axm_query_body() {
        let src = "model User { id: UUID }\n\nquery List() -> User[] {\n  SELECT * FROM users\n}\n";
        let diags = select_star_diags("models/user.axm", src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "lint.select-star");
    }

    #[test]
    fn select_star_reported_once_for_each_of_multiple_queries() {
        let src = "model User { id: UUID }\n\n\
                   query A() -> User[] {\n  SELECT * FROM users\n}\n\n\
                   query B() -> User[] {\n  SELECT * FROM users\n}\n";
        let diags = select_star_diags("models/user.axm", src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    #[test]
    fn select_star_reported_once_in_standalone_sql_file() {
        let src = "SELECT *\nFROM users;\n";
        let diags = select_star_diags("schema.sql", src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
