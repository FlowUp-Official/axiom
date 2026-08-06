//! Byte-offset to line/column mapping for editor integration.
//!
//! The compiler layer works in raw byte offsets (`usize`), matching
//! [`axiom_diagnostics::Span`]. Editors address positions by line and column,
//! so this module owns the conversion. Columns are counted in UTF-16 code
//! units, matching the LSP spec.

/// A 0-based line and UTF-16 column position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPosition {
    pub line: u32,
    pub character: u32,
}

impl TextPosition {
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// Index of line-start byte offsets for a single source text.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the start of each line (0-based). Always starts with `0`.
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// 0-based line index for a byte offset, clamped to the last line.
    pub fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(insert) => insert - 1,
        }
    }

    /// Byte offset of the start of `line` (0-based).
    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts.get(line).copied().unwrap_or_else(|| {
            self.line_starts
                .last()
                .copied()
                .unwrap_or(0)
        })
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Convert a byte offset into a 0-based line + UTF-16 column position.
    pub fn position(&self, text: &str, offset: usize) -> TextPosition {
        let offset = offset.min(text.len());
        let line = self.line_of(offset);
        let line_start = self.line_start(line);
        let utf16_len: u32 = text[line_start..offset]
            .chars()
            .map(|ch| (ch as u32 >= 0x1_0000) as u32 + 1)
            .sum();
        TextPosition::new(line as u32, utf16_len)
    }

    /// Convert a 0-based line + UTF-16 column position into a byte offset.
    /// Returns `None` when the position is out of bounds for `text`.
    pub fn offset(&self, text: &str, pos: TextPosition) -> Option<usize> {
        let line = pos.line as usize;
        let line_start = self.line_start(line);
        if line >= self.line_count() {
            return None;
        }
        let line_end = if line + 1 < self.line_count() {
            self.line_start(line + 1).saturating_sub(1)
        } else {
            text.len()
        };
        let line_text = &text[line_start..line_end.min(text.len())];
        let mut utf16_seen: u32 = 0;
        for (i, ch) in line_text.char_indices() {
            let width = (ch as u32 >= 0x1_0000) as u32 + 1;
            if utf16_seen + width > pos.character {
                return Some(line_start + i);
            }
            utf16_seen += width;
        }
        if utf16_seen == pos.character {
            Some(line_start + line_text.len())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_byte_offsets_to_positions() {
        let text = "abc\ndef\nghi";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 0), TextPosition::new(0, 0));
        assert_eq!(index.position(text, 2), TextPosition::new(0, 2));
        assert_eq!(index.position(text, 4), TextPosition::new(1, 0));
        assert_eq!(index.position(text, 7), TextPosition::new(1, 3));
        assert_eq!(index.position(text, 10), TextPosition::new(2, 2));
        assert_eq!(index.position(text, 11), TextPosition::new(2, 3));
    }

    #[test]
    fn counts_utf16_units() {
        let text = "a😀b";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 1), TextPosition::new(0, 1));
        assert_eq!(index.position(text, 5), TextPosition::new(0, 3));
        assert_eq!(index.position(text, 6), TextPosition::new(0, 4));
        assert_eq!(index.offset(text, TextPosition::new(0, 3)), Some(5));
    }

    #[test]
    fn round_trips_positions() {
        let text = "line one\nline two\n";
        let index = LineIndex::new(text);
        for offset in 0..text.len() {
            let pos = index.position(text, offset);
            assert_eq!(index.offset(text, pos), Some(offset));
        }
    }

    #[test]
    fn clamps_out_of_bounds() {
        let text = "ab";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 99), TextPosition::new(0, 2));
        assert_eq!(index.offset(text, TextPosition::new(5, 0)), None);
    }
}
