//! Query contracts consumed by the code generators.
//!
//! Query definitions are parsed from `.axm` files (``query`` declarations)
//! and assembled into a [`QueryCatalog`] by [`crate::axm::query_catalog`].
//! This module holds the structural types shared between the parser,
//! generators, and `axiom check`: parameters, return shape, and SQL body.
//!
//! The SQL body may reference parameters by name (`$email`) or positionally
//! (`$1`). [`QueryDefinition::to_driver_sql`] rewrites named placeholders to
//! positional markers so a body is valid for drivers that only understand
//! positional placeholders.

use std::borrow::Cow;
use std::collections::BTreeMap;

/// A single validation rule attached to a query parameter.
///
/// Query-parameter rules currently have no `.axm` syntax; the type is kept for
/// the code generators, which degrade gracefully when a parameter carries no
/// rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationRule<'a> {
    /// The kind of rule to apply.
    pub kind: RuleKind<'a>,
    /// Optional user-supplied message. If absent the rule inherits the
    /// parameter-level fallback message.
    pub custom_message: Option<Cow<'a, str>>,
}

/// The possible validation rule kinds understood by Axiom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKind<'a> {
    // Numeric / length bounds
    MinLen(usize),
    MaxLen(usize),
    Min(i64),
    Max(i64),
    // Custom rule (regular expression)
    Regex(Cow<'a, str>),
    // Built-in presets
    Email,
    Url,
    Uuid,
    Ulid,
    Ipv4,
    Ipv6,
    IsoDate,
    Alphanumeric,
    // Transform flags
    Trim,
    LowerCase,
    UpperCase,
}

/// A single bound parameter of a query function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParam<'a> {
    pub name: Cow<'a, str>,
    pub param_type: Cow<'a, str>,
}

/// The row shape a query produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryReturnType<'a> {
    /// A single optional row of the given type.
    Single(Cow<'a, str>),
    /// Zero or more rows of the given type.
    Many(Cow<'a, str>),
    /// No rows are returned.
    Exec,
}

/// A compiled query contract: name, raw SQL body, parameters, and shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDefinition<'a> {
    pub name: Cow<'a, str>,
    pub sql: String,
    pub params: Vec<QueryParam<'a>>,
    pub return_type: QueryReturnType<'a>,
    pub validations: BTreeMap<Cow<'a, str>, Vec<ValidationRule<'a>>>,
}

/// All queries parsed from one or more `.axm` files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryCatalog<'a> {
    pub queries: Vec<QueryDefinition<'a>>,
}

impl<'a> QueryCatalog<'a> {
    /// Return the query with the given name, if present.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn query_by_name(&self, name: &str) -> Option<&QueryDefinition<'a>> {
        self.queries.iter().find(|q| q.name == name)
    }
}

/// The form of a `$` placeholder found in query SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder<'a> {
    /// A positional marker `$1`, `$2`, ... (1-based).
    Positional(usize),
    /// A named marker `$email`, bound to the declared parameter of that name.
    Named(&'a str),
}

/// Scan a query body for `$` placeholders. Returns the byte offset, length,
/// and kind of each marker so callers can rewrite the original text.
///
/// Placeholders are only recognized outside string literals (single-quoted,
/// PostgreSQL dollar-quoted, and `E'...'`), quoted identifiers, and comments.
pub fn scan_placeholders(sql: &str) -> Vec<(usize, usize, Placeholder<'_>)> {
    fn skip_until_char(bytes: &[u8], quote: u8, mut i: usize) -> usize {
        i += 1;
        while i < bytes.len() {
            if bytes[i] == quote {
                if i + 1 < bytes.len() && bytes[i + 1] == quote {
                    i += 2;
                    continue;
                }
                return i + 1;
            }
            i += 1;
        }
        bytes.len()
    }

    /// For a `$` at `open`, return the offset just past the matching closing
    /// delimiter `$tag$ ... $tag$` when this starts a dollar-quoted string.
    fn dollar_quote_end(sql: &str, open: usize) -> Option<usize> {
        let rest = &sql[open + 1..];
        let close_ix = rest.find('$')?;
        let tag = &rest[..close_ix];
        if !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let delimiter_end = open + 1 + close_ix + 1;
        let closer = format!("${tag}$");
        let pos = sql[delimiter_end..].find(&closer)?;
        Some(delimiter_end + pos + closer.len())
    }

    let mut out = Vec::new();
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = sql[i..].chars().next().unwrap_or('\0');
        match c {
            '\'' => i = skip_until_char(bytes, b'\'', i),
            '"' => i = skip_until_char(bytes, b'"', i),
            '$' => {
                if let Some(end) = dollar_quote_end(sql, i) {
                    i = end;
                    continue;
                }
                let start = i;
                let after = &sql[i + 1..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                let token = &after[..end];
                if !token.is_empty() {
                    let kind = if token.bytes().all(|b| b.is_ascii_digit()) {
                        Placeholder::Positional(token.parse().unwrap_or(0))
                    } else {
                        Placeholder::Named(token)
                    };
                    out.push((start, 1 + end, kind));
                    i = start + 1 + end;
                } else {
                    i += 1;
                }
            }
            '-' if sql[i..].starts_with("--") => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if sql[i..].starts_with("/*") => {
                i += 2;
                while i < bytes.len() && !sql[i..].starts_with("*/") {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => i += c.len_utf8(),
        }
    }
    out
}

impl<'a> QueryDefinition<'a> {
    /// The 1-based declared index of the parameter with the given name.
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.params
            .iter()
            .position(|p| p.name == name)
            .map(|i| i + 1)
    }

    /// The SQL body with every named placeholder rewritten to a positional
    /// `$N` marker, where `N` is the parameter's declared index, so the body
    /// is valid for drivers that only understand positional placeholders.
    /// Positional markers are kept as-is and unknown names are left untouched.
    pub fn to_driver_sql(&self) -> String {
        let mut out = String::with_capacity(self.sql.len());
        let mut last = 0usize;
        for (start, len, kind) in scan_placeholders(&self.sql) {
            out.push_str(&self.sql[last..start]);
            match kind {
                Placeholder::Named(name) => {
                    if let Some(idx) = self.param_index(name) {
                        out.push('$');
                        out.push_str(&idx.to_string());
                    } else {
                        out.push_str(&self.sql[start..start + len]);
                    }
                }
                Placeholder::Positional(_) => {
                    out.push_str(&self.sql[start..start + len]);
                }
            }
            last = start + len;
        }
        out.push_str(&self.sql[last..]);
        out
    }

    /// The highest placeholder index in the body. Named placeholders count as
    /// their parameter's declared position; unknown names are ignored.
    pub fn max_placeholder_index(&self) -> usize {
        scan_placeholders(&self.sql)
            .into_iter()
            .map(|(_, _, kind)| match kind {
                Placeholder::Positional(n) => n,
                Placeholder::Named(name) => self.param_index(name).unwrap_or(0),
            })
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query<'a>(sql: &str, params: &[(&'a str, &'a str)]) -> QueryDefinition<'a> {
        QueryDefinition {
            name: Cow::Borrowed("q"),
            sql: sql.to_string(),
            params: params
                .iter()
                .map(|(name, ty)| QueryParam {
                    name: Cow::Borrowed(name),
                    param_type: Cow::Borrowed(ty),
                })
                .collect(),
            return_type: QueryReturnType::Exec,
            validations: BTreeMap::new(),
        }
    }

    #[test]
    fn scan_placeholders_mixes_positional_and_named() {
        let sql = "SELECT * FROM users WHERE email = $email AND id < $limit AND rank = $2";
        let hits = scan_placeholders(sql);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].2, Placeholder::Named("email"));
        assert_eq!(hits[1].2, Placeholder::Named("limit"));
        assert_eq!(hits[2].2, Placeholder::Positional(2));
    }

    #[test]
    fn dollar_quoted_strings_are_not_placeholders() {
        let sql = r#"SELECT body FROM posts WHERE body = $$some$tag$$ OR title = $title"#;
        let hits = scan_placeholders(sql);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].2, Placeholder::Named("title"));
    }

    #[test]
    fn to_driver_sql_keeps_positional_and_unknown_names() {
        let q = query(
            "SELECT id, email FROM users\nWHERE email = $email AND id < $limit AND id > $unknown",
            &[("limit", "Int"), ("email", "String")],
        );
        assert_eq!(q.max_placeholder_index(), 2);
        assert_eq!(
            q.to_driver_sql(),
            "SELECT id, email FROM users\nWHERE email = $2 AND id < $1 AND id > $unknown"
        );
    }

    #[test]
    fn named_placeholders_rewrite_to_positions() {
        let q = query(
            "SELECT id FROM users WHERE email = $email",
            &[("email", "String")],
        );
        assert_eq!(q.param_index("email"), Some(1));
        assert_eq!(q.to_driver_sql(), "SELECT id FROM users WHERE email = $1");
    }
}
