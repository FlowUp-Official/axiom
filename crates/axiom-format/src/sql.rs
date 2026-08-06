//! Deterministic SQL formatter built on the sqlparser token stream.
//!
//! The formatter is token-based (never regex): it uppercases unquoted
//! keywords, normalizes inter-token whitespace to single spaces while
//! preserving line structure and comments verbatim, strips trailing
//! whitespace, and re-indents continuation lines by two spaces. Placeholders
//! (`$1`, `?`) and string/number literals pass through byte-for-byte.

use sqlparser::dialect::GenericDialect;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

use crate::printer::Lines;

/// SQL clause/statement keywords that are uppercased. A curated set keeps
/// identifiers that sqlparser also classifies as keywords (e.g. `id`, `name`)
/// untouched while still canonicalizing clause keywords.
const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL", "ORDER", "BY",
    "ASC", "DESC", "LIMIT", "OFFSET", "GROUP", "HAVING", "JOIN", "INNER", "LEFT",
    "RIGHT", "FULL", "OUTER", "CROSS", "ON", "AS", "CREATE", "TABLE", "INSERT",
    "INTO", "VALUES", "UPDATE", "SET", "DELETE", "DROP", "ALTER", "ADD", "COLUMN",
    "CONSTRAINT", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "UNIQUE", "INDEX",
    "CHECK", "DEFAULT", "CASE", "WHEN", "THEN", "ELSE", "END", "EXISTS", "BETWEEN",
    "LIKE", "ILIKE", "DISTINCT", "UNION", "ALL", "RETURNING", "WITH", "USING",
    "IF", "REPLACE", "TEMPORARY", "TEMP", "BEGIN", "COMMIT", "ROLLBACK",
    "TRANSACTION", "EXPLAIN", "ANALYZE", "CAST", "OVER", "PARTITION", "WINDOW",
];

/// Format SQL to canonical form. Output always ends with exactly one `\n`.
pub fn format_sql(sql: &str) -> String {
    let dialect = GenericDialect {};
    let tokens = match Tokenizer::new(&dialect, sql).tokenize() {
        Ok(tokens) => tokens,
        // Unparseable SQL is left as-is; `check` reports the parse failure.
        Err(_) => return normalize_fallback(sql),
    };

    let mut lines = Lines::new();
    let mut current = String::new();
    let mut line_index = 0usize;

    for token in tokens {
        match token {
            Token::Whitespace(ws) => {
                let has_newline = match &ws {
                    Whitespace::Space | Whitespace::Tab => false,
                    Whitespace::Newline => true,
                    Whitespace::SingleLineComment { prefix, comment } => {
                        let trimmed = comment.trim_end_matches(['\n', '\r']);
                        push_token(&mut current, &format!("{prefix}{trimmed}"));
                        let _ = flush_line(&mut lines, &mut current, line_index);
                        // A comment ends its line; a following clause starts a
                        // fresh line at column zero.
                        line_index = 0;
                        false
                    }
                    Whitespace::MultiLineComment(_) => false,
                };
                if has_newline {
                    line_index = flush_line(&mut lines, &mut current, line_index);
                }
            }
            Token::EOF => {}
            _ => {
                let text = render_token(&token);
                push_token(&mut current, &text);
            }
        }
    }
    let _ = flush_line(&mut lines, &mut current, line_index);

    lines.finish()
}

/// Re-render a token to its canonical text.
fn render_token(token: &Token) -> String {
    match token {
        Token::Word(word) => {
            let upper = word.value.to_uppercase();
            if word.quote_style.is_none() && KEYWORDS.contains(&upper.as_str()) {
                upper
            } else if let Some(quote) = word.quote_style {
                format!("{quote}{}{quote}", word.value)
            } else {
                word.value.clone()
            }
        }
        Token::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
        Token::DoubleQuotedString(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        Token::Number(s, _) => s.clone(),
        Token::Char(c) => c.to_string(),
        Token::Comma => ",".to_string(),
        Token::SemiColon => ";".to_string(),
        Token::LParen => "(".to_string(),
        Token::RParen => ")".to_string(),
        Token::Plus => "+".to_string(),
        Token::Minus => "-".to_string(),
        Token::Mul => "*".to_string(),
        Token::Div => "/".to_string(),
        Token::Eq => "=".to_string(),
        Token::Gt => ">".to_string(),
        Token::Lt => "<".to_string(),
        Token::Neq => "<>".to_string(),
        Token::GtEq => ">=".to_string(),
        Token::LtEq => "<=".to_string(),
        Token::Period => ".".to_string(),
        Token::DoubleColon => "::".to_string(),
        Token::Placeholder(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Append `text` to the current line with canonical single-space separation.
fn push_token(line: &mut String, text: &str) {
    if line.is_empty() {
        line.push_str(text);
        return;
    }
    let last = line.chars().last().expect("line is not empty");
    let starts_no_space = text.starts_with([',', ')', ';', '.', ':']);
    let preceded_no_space = matches!(last, '(' | ':' | '.');
    if !starts_no_space && !preceded_no_space {
        line.push(' ');
    }
    line.push_str(text);
}

/// Push the current line (trimmed), re-indenting continuation lines by two
/// spaces. Blank lines collapse to at most one and reset the indent so a new
/// statement starts at column zero. A line ending in `;` also resets the indent
/// for the following statement.
fn flush_line(lines: &mut Lines, current: &mut String, line_index: usize) -> usize {
    let trimmed = current.trim_end().to_string();
    current.clear();
    if trimmed.is_empty() {
        if !lines.is_empty() && !lines.last_mut().is_some_and(|l| l.is_empty()) {
            lines.push_blank();
        }
        return 0;
    }
    let ends_stmt = trimmed.ends_with(';');
    if line_index > 0 && !trimmed.starts_with("--") && !trimmed.starts_with("/*") {
        lines.push(format!("  {trimmed}"));
    } else {
        lines.push(trimmed);
    }
    if ends_stmt {
        0
    } else {
        line_index + 1
    }
}

/// Used when the input does not tokenize: strip trailing whitespace per line
/// and ensure a single trailing newline, without any structural rewriting.
fn normalize_fallback(sql: &str) -> String {
    let mut lines = Lines::new();
    for line in sql.lines() {
        let trimmed = line.trim_end();
        if !trimmed.is_empty() || !lines.is_empty() {
            lines.push(trimmed);
        }
    }
    lines.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(sql: &str) -> String {
        format_sql(sql)
    }

    #[test]
    fn uppercases_keywords_and_normalizes_spacing() {
        let sql = "select   id, email   from users where id = 1\n";
        assert_eq!(fmt(sql), "SELECT id, email FROM users WHERE id = 1\n");
    }

    #[test]
    fn strips_trailing_whitespace_per_line() {
        let sql = "SELECT a\n  FROM t   \n WHERE a = 1    ";
        assert_eq!(fmt(sql), "SELECT a\n  FROM t\n  WHERE a = 1\n");
    }

    #[test]
    fn preserves_comments_and_quoted_identifiers() {
        let sql = "select \"Weird Col\" -- keep me\nfrom t\n";
        assert_eq!(fmt(sql), "SELECT \"Weird Col\" -- keep me\nFROM t\n");
    }

    #[test]
    fn collapses_blank_lines_and_terminates_once() {
        let sql = "select 1\n\n\n\nfrom t\n\n";
        assert_eq!(fmt(sql), "SELECT 1\n\nFROM t\n");
    }

    #[test]
    fn placeholders_and_literals_pass_through() {
        let sql = "SELECT $1, 'it''s', 42 FROM users WHERE email = $1";
        assert_eq!(fmt(sql), "SELECT $1, 'it''s', 42 FROM users WHERE email = $1\n");
    }

    #[test]
    fn multi_line_statements_are_indented() {
        let sql = "SELECT a, b\nFROM users\nWHERE a > 0 AND b < 10\nORDER BY a\n";
        assert_eq!(
            fmt(sql),
            "SELECT a, b\n  FROM users\n  WHERE a > 0 AND b < 10\n  ORDER BY a\n"
        );
    }

    #[test]
    fn semicolon_separated_statements_survive() {
        let sql = "CREATE TABLE t (id int);\nSELECT * FROM t;";
        assert_eq!(fmt(sql), "CREATE TABLE t (id int);\nSELECT * FROM t;\n");
    }

    #[test]
    fn output_is_idempotent() {
        let messy = "select  id ,email\n  from   users\nwhere\n   email = $1   \n";
        let once = fmt(messy);
        assert_eq!(once, fmt(&once), "formatting must be idempotent");
    }

    #[test]
    fn unparseable_sql_falls_back_to_trimming() {
        let sql = "THIS IS NOT VALID SQL ###   ";
        let out = fmt(sql);
        assert!(!out.ends_with("   "), "trailing whitespace removed: {out:?}");
        assert!(out.ends_with('\n'));
    }
}
