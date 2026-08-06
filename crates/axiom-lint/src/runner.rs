//! Rule registry, workspace view, and per-file lint driver.
//!
//! Each file is parsed once (`.axm` AST and SQL statement list when
//! applicable) and handed to every enabled rule. Lint results are cached in
//! the content-addressed [`ToolCache`] keyed by
//! `lint:<rules>:<blake3-of-source>`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axiom_core::axm::ast::AxmFile;
use axiom_core::axm::parser::parse_axm_file;
use axiom_core::cache::{compute_content_hash, ToolCache};
use axiom_diagnostics::{Diagnostic, Span};
use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::rules::axm::{DeadModel, RedundantValidator, UnusedImport};
use crate::rules::sql::{MissingWhereClause, SelectStar, UnindexedForeignKey};

/// Cross-file information that rules may need.
///
/// Populated by the CLI before running: every model name referenced anywhere
/// in the workspace (as a field type or an import), used by `dead-model`.
#[derive(Debug, Default)]
pub struct WorkspaceView {
    pub referenced_models: BTreeSet<String>,
}

impl WorkspaceView {
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Everything a rule needs to inspect one file.
pub struct LintContext<'a> {
    pub file: &'a Path,
    pub source: &'a str,
    /// The parsed `.axm` AST, when the file parses as one.
    pub axm: Option<AxmFile>,
    /// The parsed SQL statement list, when the file parses as SQL.
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
                Box::new(DeadModel),
                Box::new(RedundantValidator),
                Box::new(MissingWhereClause),
                Box::new(SelectStar),
                Box::new(UnindexedForeignKey),
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
        let key = format!("lint:{}:{}", rule_key, hex(compute_content_hash(src.as_bytes())));

        if let Some(cache) = cache.as_deref()
            && let Some(payload) = cache.get(&key)
            && let Ok(cached) = serde_json::from_slice::<Vec<Diagnostic>>(payload)
        {
            out.extend(cached);
            continue;
        }

        let ctx = build_context(path, src, workspace);
        let diags = runner.run_file(&ctx);

        if let Some(cache) = cache.as_mut()
            && let Ok(payload) = serde_json::to_vec(&diags)
        {
            cache.insert(key, payload);
        }
        out.extend(diags);
    }
    out
}

/// Parse a file into a [`LintContext`], tolerating both `.axm` and SQL inputs.
pub fn build_context<'a>(path: &'a Path, src: &'a str, workspace: &'a WorkspaceView) -> LintContext<'a> {
    let axm = parse_axm_file(src).ok();
    let statements = if path.extension().is_some_and(|e| e == "sql") {
        Parser::parse_sql(&GenericDialect {}, src).ok()
    } else {
        None
    };
    LintContext {
        file: path,
        source: src,
        axm,
        statements,
        workspace,
    }
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
