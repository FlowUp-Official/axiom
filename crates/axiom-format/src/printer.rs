//! Minimal line-buffer shared by the formatters.

/// A buffered list of source lines, joined deterministically on [`finish`].
#[derive(Debug, Default)]
pub struct Lines {
    lines: Vec<String>,
}

impl Lines {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub fn push_blank(&mut self) {
        self.lines.push(String::new());
    }

    pub fn last_mut(&mut self) -> Option<&mut String> {
        self.lines.last_mut()
    }

    /// The number of buffered lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Join the lines with `\n`, terminating with exactly one trailing newline.
    /// A trailing blank line (e.g. an empty trailing chunk) is trimmed first so
    /// the output is canonical.
    pub fn finish(mut self) -> String {
        while self.lines.last().is_some_and(|l| l.is_empty()) {
            self.lines.pop();
        }
        let mut out = self.lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}

/// Two-space indentation unit.
pub const INDENT: &str = "  ";

/// Indent `level` levels by two spaces each.
pub fn indent(level: usize) -> String {
    INDENT.repeat(level)
}

/// Whether a single-line field rendering fits the configured width.
pub fn fits_inline(field: &str, width: usize) -> bool {
    field.len() <= width
}

/// Canonical width used by the `.axm` formatter for choosing inline vs. broken
/// method chains.
pub const MAX_INLINE_WIDTH: usize = 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_joins_and_terminates_with_single_newline() {
        let mut lines = Lines::new();
        lines.push("a");
        lines.push_blank();
        lines.push("b");
        lines.push_blank();
        assert_eq!(lines.finish(), "a\n\nb\n");
    }

    #[test]
    fn finish_of_empty_buffer_is_empty() {
        assert_eq!(Lines::new().finish(), "");
    }
}
