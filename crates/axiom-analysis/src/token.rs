//! Tolerant, byte-precise scanners for SQL and `.axm` sources.
//!
//! These scanners produce the *position* layer only — they never attempt to
//! parse or validate. All syntax/type/resolution logic lives in `axiom-core`
//! and `axiom-check`; this module just maps tokens back to byte offsets so the
//! editor can underline, complete, hover, and rename accurately.
//!
//! The scanners never fail: on any malformed input they skip forward and keep
//! producing whatever tokens they can.

use crate::position::LineIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Unquoted identifier or keyword.
    Word,
    /// `"quoted identifier"` — `text` holds the raw slice including quotes.
    QuotedIdent,
    /// `'string literal'` (or `E'...'`) — `text` holds the raw slice.
    String,
    /// Numeric literal.
    Number,
    /// `$1` or `:name` parameter placeholder.
    Placeholder,
    /// Operators and punctuation.
    Punct,
    /// Line or block comment.
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    /// Raw source slice.
    pub text: String,
}

impl Token {
    /// The identifier value of a word or quoted identifier, without quotes.
    pub fn ident_value(&self) -> &str {
        match self.kind {
            TokenKind::QuotedIdent if self.text.len() >= 2 => {
                &self.text[1..self.text.len() - 1]
            }
            _ => &self.text,
        }
    }

    pub fn is_word(&self) -> bool {
        matches!(self.kind, TokenKind::Word | TokenKind::QuotedIdent)
    }

    pub fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }
}

fn is_word_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The scanner source text along with a byte-positioned cursor.
struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.text[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }
}

/// Scan an identifier-ish run (unquoted word, digits) starting at `start`.
fn scan_word(s: &Scanner, start: usize) -> String {
    s.text[start..].chars().take_while(|&c| is_word_char(c)).collect()
}

/// Scan an identifier-ish run (unquoted word, digits) starting at `start`.
fn scan_number(sc: &Scanner, start: usize) -> String {
    let mut end = start;
    let bytes = sc.text.as_bytes();
    while end < bytes.len() {
        let c = bytes[end] as char;
        if c.is_ascii_digit() {
            end += 1;
        } else {
            break;
        }
    }
    // Optional decimal fraction: `.` must be followed by a digit.
    if end + 1 < bytes.len() && bytes[end] == b'.' && (bytes[end + 1] as char).is_ascii_digit() {
        end += 1;
        while end < bytes.len() && (bytes[end] as char).is_ascii_digit() {
            end += 1;
        }
    }
    sc.text[start..end].to_string()
}

pub fn tokenize_sql(src: &str) -> Vec<Token> {
    let mut scanner = Scanner::new(src);
    let mut tokens = Vec::new();
    while let Some(c) = scanner.peek() {
        match c {
            c if c.is_whitespace() => {
                scanner.skip_whitespace();
            }
            '-' if scanner.peek_second() == Some('-') => {
                let start = scanner.pos;
                scanner.pos += 2;
                while let Some(c) = scanner.peek() {
                    if c == '\n' {
                        break;
                    }
                    scanner.pos += c.len_utf8();
                }
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            '/' if scanner.peek_second() == Some('*') => {
                let start = scanner.pos;
                scanner.pos += 2;
                while scanner.pos + 1 < src.len() {
                    if src.as_bytes()[scanner.pos] == b'*'
                        && src.as_bytes()[scanner.pos + 1] == b'/'
                    {
                        scanner.pos += 2;
                        break;
                    }
                    scanner.pos += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            '\'' => {
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    scanner.pos += c.len_utf8();
                    if c == '\'' {
                        if scanner.peek() == Some('\'') {
                            scanner.pos += 1;
                            continue;
                        }
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::String,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            '"' => {
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    scanner.pos += c.len_utf8();
                    if c == '"' {
                        if scanner.peek() == Some('"') {
                            scanner.pos += 1;
                            continue;
                        }
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::QuotedIdent,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            '$' => {
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    if c.is_ascii_digit() || is_word_char(c) {
                        scanner.pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::Placeholder,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            ':' if scanner
                .text[scanner.pos + 1..]
                .chars()
                .next()
                .is_some_and(is_word_start) =>
            {
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    if is_word_char(c) {
                        scanner.pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::Placeholder,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            c if c.is_ascii_digit() => {
                let start = scanner.pos;
                let text = scan_number(&scanner, start);
                scanner.pos += text.len();
                tokens.push(Token {
                    kind: TokenKind::Number,
                    start,
                    end: scanner.pos,
                    text,
                });
            }
            // `E'...'` escaped strings.
            c if (c == 'e' || c == 'E') && scanner.peek_second() == Some('\'') => {
                scanner.pos += 1;
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    scanner.pos += c.len_utf8();
                    if c == '\'' {
                        if scanner.peek() == Some('\'') {
                            scanner.pos += 1;
                            continue;
                        }
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::String,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            c if is_word_start(c) => {
                let start = scanner.pos;
                let text = scan_word(&scanner, start);
                scanner.pos += text.len();
                tokens.push(Token {
                    kind: TokenKind::Word,
                    start,
                    end: scanner.pos,
                    text,
                });
            }
            _ => {
                let start = scanner.pos;
                scanner.pos += c.len_utf8();
                tokens.push(Token {
                    kind: TokenKind::Punct,
                    start,
                    end: scanner.pos,
                    text: c.to_string(),
                });
            }
        }
    }
    tokens
}

/// Scan an `.axm` source. Comments are `#` and `//` to end of line; strings
/// are double-quoted with `\"` escapes.
pub fn tokenize_axm(src: &str) -> Vec<Token> {
    let mut scanner = Scanner::new(src);
    let mut tokens = Vec::new();
    while let Some(c) = scanner.peek() {
        match c {
            c if c.is_whitespace() => {
                scanner.skip_whitespace();
            }
            '#' | '/' if (c == '#') || (scanner.peek_second() == Some('/')) => {
                let start = scanner.pos;
                if c == '/' {
                    scanner.pos += 2;
                } else {
                    scanner.pos += 1;
                }
                while let Some(c) = scanner.peek() {
                    if c == '\n' {
                        break;
                    }
                    scanner.pos += c.len_utf8();
                }
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            '"' => {
                let start = scanner.pos;
                scanner.pos += 1;
                while let Some(c) = scanner.peek() {
                    scanner.pos += c.len_utf8();
                    if c == '\\' {
                        if let Some(c) = scanner.peek() {
                            scanner.pos += c.len_utf8();
                        }
                        continue;
                    }
                    if c == '"' {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::String,
                    start,
                    end: scanner.pos,
                    text: src[start..scanner.pos].to_string(),
                });
            }
            c if c.is_ascii_digit() => {
                let start = scanner.pos;
                let text = scan_number(&scanner, start);
                scanner.pos += text.len();
                tokens.push(Token {
                    kind: TokenKind::Number,
                    start,
                    end: scanner.pos,
                    text,
                });
            }
            c if is_word_start(c) => {
                let start = scanner.pos;
                let text = scan_word(&scanner, start);
                scanner.pos += text.len();
                tokens.push(Token {
                    kind: TokenKind::Word,
                    start,
                    end: scanner.pos,
                    text,
                });
            }
            _ => {
                let start = scanner.pos;
                scanner.pos += c.len_utf8();
                tokens.push(Token {
                    kind: TokenKind::Punct,
                    start,
                    end: scanner.pos,
                    text: c.to_string(),
                });
            }
        }
    }
    tokens
}

/// The token containing `offset`, searching among `kind` candidates.
pub fn token_at(tokens: &[Token], offset: usize) -> Option<&Token> {
    tokens
        .iter()
        .find(|t| !matches!(t.kind, TokenKind::Comment) && t.start <= offset && offset <= t.end)
}

/// The last significant (non-comment) token strictly before `offset`.
pub fn prev_token(tokens: &[Token], offset: usize) -> Option<&Token> {
    tokens
        .iter()
        .rfind(|t| !matches!(t.kind, TokenKind::Comment) && t.end <= offset)
}

/// Line index plus tokens, cached per file.
#[derive(Debug, Clone)]
pub struct PositionIndex {
    pub lines: LineIndex,
    pub tokens: Vec<Token>,
}

impl PositionIndex {
    pub fn new_sql(src: &str) -> Self {
        Self {
            lines: LineIndex::new(src),
            tokens: tokenize_sql(src),
        }
    }

    pub fn new_axm(src: &str) -> Self {
        Self {
            lines: LineIndex::new(src),
            tokens: tokenize_axm(src),
        }
    }

    pub fn token_at(&self, offset: usize) -> Option<&Token> {
        token_at(&self.tokens, offset)
    }

    pub fn prev_token(&self, offset: usize) -> Option<&Token> {
        prev_token(&self.tokens, offset)
    }

    /// Find a word token whose value equals `name` (case-insensitive), at or
    /// after `from`.
    pub fn find_word(&self, name: &str, from: usize) -> Option<&Token> {
        let lower = name.to_lowercase();
        self.tokens.iter().find(|t| {
            t.is_word()
                && t.start >= from
                && t.ident_value().to_lowercase() == lower
        })
    }

    /// Find a word token matching `name` (case-insensitive) anywhere.
    pub fn find_word_any(&self, name: &str) -> Option<&Token> {
        self.find_word(name, 0)
    }

    pub fn to_text_position(&self, text: &str, offset: usize) -> crate::TextPosition {
        self.lines.position(text, offset)
    }

    /// Resolve a line/column position back to a byte offset. Returns `None`
    /// when the position falls outside `text`.
    pub fn from_text_position(&self, text: &str, pos: crate::TextPosition) -> Option<usize> {
        self.lines.offset(text, pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_sql_basics() {
        let src = "SELECT u.email, 'it''s' FROM users u WHERE id = $1;";
        let tokens = tokenize_sql(src);
        let words: Vec<&str> = tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Word))
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(
            words,
            vec!["SELECT", "u", "email", "FROM", "users", "u", "WHERE", "id"]
        );
        let placeholders: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Placeholder)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(placeholders, vec!["$1"]);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
    }

    #[test]
    fn scans_comments() {
        let src = "-- hello\nSELECT 1 /* block */";
        let tokens = tokenize_sql(src);
        assert_eq!(
            tokens.iter().filter(|t| t.kind == TokenKind::Comment).count(),
            2
        );
    }

    #[test]
    fn scanner_never_fails() {
        let src = "SELECT 'unterminated FROM";
        let tokens = tokenize_sql(src);
        assert!(!tokens.is_empty());
    }

    #[test]
    fn scans_axm() {
        let src = "import User from \"user\"\n\nmodel Account {\n  email: string .email() .trim()\n}";
        let tokens = tokenize_axm(src);
        let words: Vec<&str> = tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Word))
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(
            words,
            vec!["import", "User", "from", "model", "Account", "email", "string", "email", "trim"]
        );
    }

    #[test]
    fn finds_word_after_offset() {
        let src = "SELECT id FROM users";
        let idx = PositionIndex::new_sql(src);
        let tok = idx.find_word("users", 0).unwrap();
        assert_eq!(&src[tok.start..tok.end], "users");
        assert!(idx.find_word("users", tok.end).is_none());
    }

    #[test]
    fn token_at_handles_boundaries() {
        let src = "SELECT id FROM users";
        let idx = PositionIndex::new_sql(src);
        // Cursor at the boundary right after "SELECT" belongs to SELECT.
        let tok = idx.token_at("SELECT".len()).unwrap();
        assert_eq!(tok.ident_value(), "SELECT");
    }
}
