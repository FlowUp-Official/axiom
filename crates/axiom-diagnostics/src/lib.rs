//! Shared diagnostic model for Axiom tooling.
//!
//! `check`, `format`, `lint`, the `.axm` parser, the SQL parser, and the
//! resolver all report problems through a single [`Diagnostic`] shape so the
//! CLI renders them consistently and caches them uniformly. Diagnostics are
//! [`serde`]-serializable so lint/check caches can persist analysis results.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How severe a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    /// Does not block a command, but should be surfaced.
    Warning,
    /// Blocks the command (fails `check` / `lint`; a parse failure).
    Error,
}

impl Severity {
    /// Stable identifier used in plain (non-TTY) output and cache keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// A byte range into a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A single problem found during checking, formatting, or linting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub file: PathBuf,
    /// Byte span into `file`'s source, when known.
    pub span: Option<Span>,
    /// Stable rule/check identifier, e.g. `lint.redundant-validator` or
    /// `check.missing-table`. Used for filtering and cache keys.
    pub code: String,
    pub message: String,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(file: impl Into<PathBuf>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            file: file.into(),
            span: None,
            code: code.into(),
            message: message.into(),
            help: None,
        }
    }

    pub fn warning(file: impl Into<PathBuf>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            file: file.into(),
            span: None,
            code: code.into(),
            message: message.into(),
            help: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// Machine-friendly single-line form: `file: severity[code]: message`.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.file.display())?;
        write!(
            f,
            "{}[{}]: {}",
            self.severity.as_str(),
            self.code,
            self.message
        )?;
        if let Some(help) = &self.help {
            write!(f, " ({help})")?;
        }
        Ok(())
    }
}

/// Renders diagnostics for a terminal or for machine consumption.
///
/// TTY output is colored, includes `--> file:line:col` locations and source
/// snippets with carets when the source is available. Non-TTY output is a
/// stable, greppable single-line format (`file: severity[code]: message`).
pub struct Renderer {
    pub tty: bool,
}

impl Renderer {
    pub fn new(tty: bool) -> Self {
        Self { tty }
    }

    /// Render a single diagnostic. `source` is the file's text, used only for
    /// TTY snippets and line/column computation; it may be `None`.
    pub fn render(&self, diag: &Diagnostic, source: Option<&str>) -> String {
        if !self.tty {
            return format!("{}\n", diag);
        }
        self.render_tty(diag, source)
    }

    fn render_tty(&self, diag: &Diagnostic, source: Option<&str>) -> String {
        use owo_colors::{OwoColorize, Stream};

        let mut out = String::new();
        let header = match diag.severity {
            Severity::Error => format!("{}[{}]: {}", "error", diag.code, diag.message)
                .red()
                .bold()
                .to_string(),
            Severity::Warning => format!("{}[{}]: {}", "warning", diag.code, diag.message)
                .yellow()
                .bold()
                .to_string(),
        };
        out.push_str(&header);
        out.push('\n');

        if let Some(span) = diag.span
            && let Some(src) = source
            && let Some((line, col)) = line_col(src, span.start)
        {
            let location = format!(" --> {}:{line}:{col}", diag.file.display())
                .if_supports_color(Stream::Stderr, |s| s.blue().to_string())
                .to_string();
            out.push_str(&location);
            out.push('\n');

            let line_text = src.lines().nth(line - 1).unwrap_or("");
            let line_no = line.to_string();
            let gutter = " ".repeat(line_no.len());
            out.push_str(&format!(" {gutter} |\n"));
            out.push_str(&format!(" {line_no} | {line_text}\n"));
            let marker = caret_line(line_text, span.start, span.end, line, src);
            out.push_str(&format!(" {gutter} | {marker}\n"));
        }

        if let Some(help) = &diag.help {
            let dimmed = format!(" = help: {help}")
                .if_supports_color(Stream::Stderr, |s| s.dimmed().to_string())
                .to_string();
            out.push_str(&dimmed);
            out.push('\n');
        }
        out
    }
}

/// Compute the 1-based line and column of a byte offset in `source`.
fn line_col(source: &str, offset: usize) -> Option<(usize, usize)> {
    let offset = offset.min(source.len());
    let mut line = 1;
    let mut line_start = 0;
    for (i, b) in source.bytes().enumerate() {
        if i == offset {
            return Some((line, offset - line_start + 1));
        }
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    if offset == source.len() {
        return Some((line, offset - line_start + 1));
    }
    None
}

/// Build a caret underline for the span on its source line. The span may cross
/// lines; the caret is clamped to the span's starting line.
fn caret_line(line_text: &str, start: usize, end: usize, line: usize, source: &str) -> String {
    let line_start = source
        .char_indices()
        .filter(|(_, c)| *c == '\n')
        .map(|(i, _)| i + 1)
        .nth(line.saturating_sub(2))
        .unwrap_or(0);
    let col = start.saturating_sub(line_start).min(line_text.len());
    let span_end = end.saturating_sub(line_start).min(line_text.len());
    let width = span_end.saturating_sub(col).max(1);

    let mut caret = " ".repeat(col);
    caret.push('^');
    if width > 1 {
        caret.push_str(&"^".repeat(width - 1));
    }
    caret
}

/// Render a slice of diagnostics, joined by newlines. Returns the string with
/// a trailing newline so callers can print it directly.
pub fn render_all(
    renderer: &Renderer,
    diagnostics: &[Diagnostic],
    source_by_path: impl Fn(&std::path::Path) -> Option<String>,
) -> String {
    let mut out = String::new();
    for diag in diagnostics {
        let source = diag.span.as_ref().and_then(|_| source_by_path(&diag.file));
        out.push_str(&renderer.render(diag, source.as_deref()));
    }
    out
}

/// Count diagnostics by severity.
pub fn summary(diagnostics: &[Diagnostic]) -> (usize, usize) {
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    (errors, diagnostics.len() - errors)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_render_is_greppable() {
        let diag = Diagnostic::error("models/user.axm", "check.parse", "boom")
            .with_span(Span::new(5, 9));
        let renderer = Renderer::new(false);
        let out = renderer.render(&diag, None);
        assert_eq!(out, "models/user.axm: error[check.parse]: boom\n");
    }

    #[test]
    fn tty_render_includes_location_and_caret() {
        let source = "model User {\n  name: string\n}";
        // byte offset of `name` (line 2, col 3)
        let start = source.find("name").expect("name present");
        let diag = Diagnostic::error("user.axm", "lint.foo", "bad")
            .with_span(Span::new(start, start + 4));
        let renderer = Renderer::new(true);
        let out = renderer.render(&diag, Some(source));
        assert!(out.contains("--> user.axm:2:3"), "got: {out}");
        assert!(out.contains("name"), "snippet should show the line: {out}");
        assert!(out.contains("^^^^"), "caret should underline: {out}");
    }

    #[test]
    fn line_col_handles_end_of_file() {
        assert_eq!(line_col("abc", 3), Some((1, 4)));
        assert_eq!(line_col("a\nb", 2), Some((2, 1)));
        assert_eq!(line_col("a\nb", 999), Some((2, 2)));
    }

    #[test]
    fn spans_are_clamped_to_start() {
        let span = Span::new(10, 5);
        assert_eq!(span.len(), 0);
        assert_eq!(span.start, 10);
        assert_eq!(span.end, 10);
    }

    #[test]
    fn summary_counts_severities() {
        let diags = vec![
            Diagnostic::error("a", "x", "e"),
            Diagnostic::warning("b", "y", "w"),
            Diagnostic::warning("c", "z", "w"),
        ];
        assert_eq!(summary(&diags), (1, 2));
    }
}
