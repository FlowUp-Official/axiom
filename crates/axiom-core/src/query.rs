//! Named query parsing for `-- @fn` / `-- @validate` header annotations.
//!
//! Query files are plain SQL files where each statement is preceded by a
//! function signature comment and zero or more per-parameter validation
//! comments. The body may reference parameters by name (`$email`) or
//! positionally (`$1`), and omitting a return type marks the query as an
//! execution (no rows):
//!
//! ```sql
//! -- @fn get_user($email: String) : users
//! SELECT id, email FROM users WHERE email = $email
//!
//! -- @fn delete_user(id: BigInt)
//! DELETE FROM users WHERE id = $id
//!
//! -- @fn get_users($limit: Int, $email: String) : users[]
//! -- @validate email(email, trim, lower)
//! -- @validate limit(min=1, max=100)
//! SELECT id, email FROM users
//! WHERE email = $email AND id < $limit
//! ORDER BY id
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;

use miette::SourceSpan;

use crate::errors::AxiomError;

/// A single validation rule attached to a query parameter.
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

/// Parse a bare rule list (no `--`/`@validate` prefix) such as
/// `email[msg="Bad"], min_len=5` into validation rules.
pub(crate) fn parse_rules_content<'a>(content: &'a str) -> Vec<ValidationRule<'a>> {
    let mut fallback: Option<Cow<'a, str>> = None;
    let mut parsed_segments = Vec::new();

    for segment in split_top_level(content, ',') {
        let Some(parsed) = parse_rule_segment(segment) else {
            continue;
        };

        if parsed.name.eq_ignore_ascii_case("msg") {
            fallback = parsed.value.map(Cow::Borrowed);
        } else {
            parsed_segments.push(parsed);
        }
    }

    let mut rules = Vec::new();
    for parsed in parsed_segments {
        let Some(kind) = parse_rule_kind(parsed.name, parsed.value) else {
            continue;
        };

        let custom_message = match parsed.msg {
            Some(inline) => Some(Cow::Borrowed(inline)),
            None => fallback.clone(),
        };

        rules.push(ValidationRule {
            kind,
            custom_message,
        });
    }

    rules
}

struct ParsedSegment<'a> {
    name: &'a str,
    value: Option<&'a str>,
    msg: Option<&'a str>,
}

/// Split a string on `delimiter`, ignoring delimiters inside double quotes.
pub(crate) fn split_top_level(s: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;

    for (i, b) in s.bytes().enumerate() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_quotes = false;
            }
        } else if b == b'"' {
            in_quotes = true;
        } else if b == delimiter as u8 {
            parts.push(&s[start..i]);
            start = i + 1;
        }
    }

    parts.push(&s[start..]);
    parts
}

/// Parse a single comma-separated segment into `name[=value][[msg="..."]]`.
fn parse_rule_segment<'a>(segment: &'a str) -> Option<ParsedSegment<'a>> {
    let bytes = segment.as_bytes();
    let len = bytes.len();

    let mut i = skip_ws(bytes, 0);
    let name_start = i;
    while i < len && !matches!(bytes[i], b'[' | b'=' | b' ' | b'\t') {
        i += 1;
    }
    let name = segment[name_start..i].trim();
    if name.is_empty() {
        return None;
    }

    i = skip_ws(bytes, i);
    let mut value = None;
    if i < len && bytes[i] == b'=' {
        let (val, next) = read_value(segment, i + 1);
        value = val;
        i = next;
    }

    i = skip_ws(bytes, i);
    let mut msg = None;
    if i < len && bytes[i] == b'[' {
        let (m, _) = read_bracket_msg(segment, i + 1);
        msg = m;
    }

    Some(ParsedSegment { name, value, msg })
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Read a rule value starting after `=`; either a quoted string or bare text
/// that runs until whitespace or `[`.
fn read_value(segment: &str, start: usize) -> (Option<&str>, usize) {
    let bytes = segment.as_bytes();
    let mut i = skip_ws(bytes, start);

    if i < bytes.len() && bytes[i] == b'"' {
        let (content, after) = read_quoted(segment, i);
        return (Some(content), after);
    }

    let value_start = i;
    while i < bytes.len() && bytes[i] != b'[' && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i == value_start {
        (None, i)
    } else {
        (Some(&segment[value_start..i]), i)
    }
}

/// Read the `msg="..."` payload inside a rule's `[...]` bracket.
fn read_bracket_msg(segment: &str, start: usize) -> (Option<&str>, usize) {
    let bytes = segment.as_bytes();
    let len = bytes.len();
    let mut i = skip_ws(bytes, start);

    if segment[i..].get(..3).is_some_and(|p| p.eq_ignore_ascii_case("msg")) {
        i += 3;
        i = skip_ws(bytes, i);
        if i < len && bytes[i] == b'=' {
            i += 1;
            i = skip_ws(bytes, i);
            if i < len && bytes[i] == b'"' {
                let (content, after) = read_quoted(segment, i);
                let j = skip_ws(bytes, after);
                if j < len && bytes[j] == b']' {
                    return (Some(content), j + 1);
                }
                return (Some(content), j);
            }
        }
    }

    let mut j = i;
    while j < len && bytes[j] != b']' {
        j += 1;
    }
    (None, j.min(len))
}

/// Read a double-quoted string starting at `start` (which must point at `"`).
/// Returns the unescaped inner content (without the quotes) and the index just
/// past the closing quote.
fn read_quoted(segment: &str, start: usize) -> (&str, usize) {
    let bytes = segment.as_bytes();
    debug_assert!(bytes[start] == b'"');

    let mut i = start + 1;
    let mut escaped = false;
    while i < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[i] == b'\\' {
            escaped = true;
        } else if bytes[i] == b'"' {
            break;
        }
        i += 1;
    }

    let content = &segment[start + 1..i];
    let after = if i < bytes.len() { i + 1 } else { i };
    (content, after)
}

/// Map a parsed rule name (and optional value) to a [`RuleKind`].
fn parse_rule_kind<'a>(name: &str, value: Option<&'a str>) -> Option<RuleKind<'a>> {
    match name.to_ascii_lowercase().as_str() {
        "email" => Some(RuleKind::Email),
        "url" => Some(RuleKind::Url),
        "uuid" => Some(RuleKind::Uuid),
        "ulid" => Some(RuleKind::Ulid),
        "ipv4" => Some(RuleKind::Ipv4),
        "ipv6" => Some(RuleKind::Ipv6),
        "isodate" | "iso_date" => Some(RuleKind::IsoDate),
        "alphanumeric" | "alnum" => Some(RuleKind::Alphanumeric),
        "trim" => Some(RuleKind::Trim),
        "lower" | "lowercase" => Some(RuleKind::LowerCase),
        "upper" | "uppercase" => Some(RuleKind::UpperCase),
        "min_len" | "minlen" => Some(RuleKind::MinLen(value?.parse().ok()?)),
        "max_len" | "maxlen" => Some(RuleKind::MaxLen(value?.parse().ok()?)),
        "min" => Some(RuleKind::Min(value?.parse().ok()?)),
        "max" => Some(RuleKind::Max(value?.parse().ok()?)),
        "regex" => Some(RuleKind::Regex(Cow::Borrowed(value?))),
        _ => None,
    }
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

/// A parsed named query: signature, raw SQL body, and per-parameter rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDefinition<'a> {
    pub name: Cow<'a, str>,
    pub sql: String,
    pub params: Vec<QueryParam<'a>>,
    pub return_type: QueryReturnType<'a>,
    pub validations: BTreeMap<Cow<'a, str>, Vec<ValidationRule<'a>>>,
}

/// All queries parsed from one or more query files.
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
pub fn scan_placeholders(sql: &str) -> Vec<(usize, usize, Placeholder<'_>)> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(pos) = sql[search_from..].find('$') {
        let start = search_from + pos;
        let after = &sql[start + 1..];
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
        }
        search_from = start + 1 + end;
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

struct QueryBuilder<'a> {
    name: Cow<'a, str>,
    params: Vec<QueryParam<'a>>,
    return_type: QueryReturnType<'a>,
    validations: BTreeMap<Cow<'a, str>, Vec<ValidationRule<'a>>>,
    sql: String,
}

impl<'a> QueryBuilder<'a> {
    fn finish(mut self) -> QueryDefinition<'a> {
        while self.sql.ends_with('\n') {
            self.sql.pop();
        }
        QueryDefinition {
            name: self.name,
            sql: self.sql,
            params: self.params,
            return_type: self.return_type,
            validations: self.validations,
        }
    }
}

/// Parse a query file into a [`QueryCatalog`].
///
/// Sections are delimited by `-- @fn` lines. Within a section, `-- @validate`
/// lines contribute rules for the named parameter and all other non-annotation
/// lines form the raw SQL body. A `-- @fn` line that does not follow the
/// expected signature is reported as a [`AxiomError::QueryAnnotationError`]
/// with a source span pointing at the offending line.
pub fn parse_query_file<'a>(src: &'a str) -> Result<QueryCatalog<'a>, AxiomError> {
    let mut catalog = QueryCatalog::default();
    let mut current: Option<QueryBuilder<'a>> = None;
    let mut line_start = 0usize;

    for line in src.split('\n') {
        let line_content = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line_content.trim_start();
        let Some(rest) = trimmed.strip_prefix("--") else {
            if let Some(builder) = current.as_mut() {
                builder.sql.push_str(line_content);
                builder.sql.push('\n');
            }
            line_start += line.len() + 1;
            continue;
        };
        let rest = rest.trim_start();

        if rest.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("@fn")) {
            if let Some(builder) = current.take() {
                catalog.queries.push(builder.finish());
            }
            match parse_fn_signature(line_content) {
                Some((name, params, return_type)) => {
                    current = Some(QueryBuilder {
                        name,
                        params,
                        return_type,
                        validations: BTreeMap::new(),
                        sql: String::new(),
                    });
                }
                None => {
                    return Err(AxiomError::QueryAnnotationError {
                        message: format!(
                            "malformed `-- @fn` annotation: `{}`",
                            line_content.trim()
                        ),
                        src: src.to_string(),
                        span: SourceSpan::new(line_start.into(), line_content.len()),
                    });
                }
            }
            line_start += line.len() + 1;
            continue;
        }

        if rest.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("@validate")) {
            if let Some(builder) = current.as_mut()
                && let Some((param, rules)) = parse_param_validation(line_content)
                && !rules.is_empty()
            {
                builder
                    .validations
                    .entry(param)
                    .or_default()
                    .extend(rules);
            }
            line_start += line.len() + 1;
            continue;
        }

        if let Some(builder) = current.as_mut() {
            builder.sql.push_str(line_content);
            builder.sql.push('\n');
        }
        line_start += line.len() + 1;
    }

    if let Some(builder) = current.take() {
        catalog.queries.push(builder.finish());
    }

    Ok(catalog)
}

/// Parse `-- @fn <name>(<param>:<type>, ...) : <return_type>`.
fn parse_fn_signature<'a>(
    line: &'a str,
) -> Option<(Cow<'a, str>, Vec<QueryParam<'a>>, QueryReturnType<'a>)> {
    let rest = line.trim();
    let rest = rest.strip_prefix("--")?.trim_start();
    let (_, rest) = if rest.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("@fn")) {
        (&rest[..3], &rest[3..])
    } else {
        return None;
    };
    let rest = rest.trim_start();

    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    if close < open {
        return None;
    }

    let name = rest[..open].trim();
    if name.is_empty() {
        return None;
    }

    let mut params = Vec::new();
    for part in split_top_level(&rest[open + 1..close], ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (pname, ptype) = part.split_once(':')?;
        let (pname, ptype) = (pname.trim(), ptype.trim());
        if pname.is_empty() || ptype.is_empty() {
            return None;
        }
        // A leading `$` marks the parameter as a named placeholder in the SQL
        // body (`$email`); it is not part of the parameter's identifier.
        let pname = pname.strip_prefix('$').unwrap_or(pname);
        params.push(QueryParam {
            name: Cow::Borrowed(pname),
            param_type: Cow::Borrowed(ptype),
        });
    }

    let return_type = parse_return_type(&rest[close + 1..]);

    Some((Cow::Borrowed(name), params, return_type))
}

fn parse_return_type<'a>(s: &'a str) -> QueryReturnType<'a> {
    let s = s.trim().trim_start_matches(':').trim();
    if s.is_empty() || s.eq_ignore_ascii_case("exec") {
        return QueryReturnType::Exec;
    }
    if let Some(inner) = s.strip_suffix("[]")
        && !inner.trim().is_empty()
    {
        return QueryReturnType::Many(Cow::Borrowed(inner.trim()));
    }
    QueryReturnType::Single(Cow::Borrowed(s))
}

/// Parse `-- @validate <param>(<rules...>)`.
fn parse_param_validation<'a>(line: &'a str) -> Option<(Cow<'a, str>, Vec<ValidationRule<'a>>)> {
    let rest = line.trim();
    let rest = rest.strip_prefix("--")?.trim_start();
    let (_, rest) = if rest.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("@validate")) {
        (&rest[..9], &rest[9..])
    } else {
        return None;
    };
    let rest = rest.trim_start();

    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    if close <= open {
        return None;
    }

    let param = rest[..open].trim();
    if param.is_empty() {
        return None;
    }

    let rules = parse_rules_content(&rest[open + 1..close]);

    Some((Cow::Borrowed(param), rules))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fn_signature_with_many_return() {
        let src = "-- @fn get_users(email: String, limit: Int) : Users[]";
        let catalog = parse_query_file(src).expect("parse query");
        let q = &catalog.queries[0];
        assert_eq!(q.name.as_ref(), "get_users");
        assert_eq!(q.params.len(), 2);
        assert_eq!(q.params[0].name.as_ref(), "email");
        assert_eq!(q.params[0].param_type.as_ref(), "String");
        assert_eq!(q.params[1].name.as_ref(), "limit");
        assert_eq!(q.params[1].param_type.as_ref(), "Int");
        assert_eq!(q.return_type, QueryReturnType::Many(Cow::Borrowed("Users")));
    }

    #[test]
    fn parses_fn_signature_single_and_exec() {
        let single = parse_query_file("-- @fn get_user(email: String) : User").expect("parse");
        assert_eq!(
            single.queries[0].return_type,
            QueryReturnType::Single(Cow::Borrowed("User"))
        );

        let exec = parse_query_file("-- @fn delete_user(id: Uuid) : Exec").expect("parse");
        assert_eq!(exec.queries[0].return_type, QueryReturnType::Exec);
    }

    #[test]
    fn parses_fn_signature_without_spaces() {
        let catalog =
            parse_query_file("-- @fn get_user(email:String):Users[]").expect("parse");
        let q = &catalog.queries[0];
        assert_eq!(q.name.as_ref(), "get_user");
        assert_eq!(q.params[0].name.as_ref(), "email");
        assert_eq!(q.params[0].param_type.as_ref(), "String");
        assert_eq!(q.return_type, QueryReturnType::Many(Cow::Borrowed("Users")));
    }

    #[test]
    fn extracts_param_validation_rules() {
        let src = "-- @fn get_user(email: String) : User\n-- @validate email(email, trim, lower)\nSELECT * FROM users WHERE email = $1";
        let catalog = parse_query_file(src).expect("parse");
        let q = &catalog.queries[0];
        assert_eq!(q.sql, "SELECT * FROM users WHERE email = $1");
        let rules = q.validations.get("email").expect("email rules");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].kind, RuleKind::Email);
        assert_eq!(rules[1].kind, RuleKind::Trim);
        assert_eq!(rules[2].kind, RuleKind::LowerCase);
    }

    #[test]
    fn parses_multiple_queries_in_one_file() {
        let src = r#"
-- @fn get_user(email: String) : User
SELECT * FROM users WHERE email = $1

-- @fn delete_user(id: Uuid) : Exec
DELETE FROM users WHERE id = $1
"#;
        let catalog = parse_query_file(src).expect("parse");
        assert_eq!(catalog.queries.len(), 2);
        assert_eq!(catalog.query_by_name("get_user").expect("q1").sql, "SELECT * FROM users WHERE email = $1");
        assert_eq!(
            catalog.query_by_name("delete_user").expect("q2").sql,
            "DELETE FROM users WHERE id = $1"
        );
    }

    #[test]
    fn validation_before_fn_applies_to_previous_section() {
        let src = "-- @fn get_user(email: String) : User\nSELECT 1\n-- @validate email(email)\n-- @fn delete_user(id: Uuid) : Exec\nDELETE FROM users WHERE id = $1";
        let catalog = parse_query_file(src).expect("parse");
        let get_user = catalog.query_by_name("get_user").expect("get_user");
        assert!(get_user.validations.contains_key("email"));
        let delete_user = catalog.query_by_name("delete_user").expect("delete_user");
        assert!(delete_user.validations.is_empty());
    }

    #[test]
    fn comments_inside_sql_body_are_preserved() {
        let src = "-- @fn get_user(email: String) : User\n-- where clause\nSELECT * FROM users WHERE email = $1 -- trailing\n";
        let catalog = parse_query_file(src).expect("parse");
        assert_eq!(
            catalog.queries[0].sql,
            "-- where clause\nSELECT * FROM users WHERE email = $1 -- trailing"
        );
    }

    #[test]
    fn malformed_fn_annotation_reports_source_span() {
        let src = "-- @fn get_user(email String) : User\nSELECT 1";
        let err = parse_query_file(src).expect_err("malformed annotation must error");
        let line = src.split('\n').next().expect("first line");

        match &err {
            AxiomError::QueryAnnotationError {
                message,
                src: diagnostic_src,
                span,
            } => {
                assert!(message.contains("get_user"), "message: {message}");
                assert_eq!(*diagnostic_src, src, "source should carry the full file");
                assert_eq!(span.offset(), 0, "span should start at the annotation line");
                assert_eq!(span.len(), line.len(), "span should cover the whole line");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }

        let report = format!("{:?}", miette::Report::new(err));
        assert!(
            report.contains("Invalid annotation syntax"),
            "report should use the diagnostic display, got: {report}"
        );
        assert!(
            report.contains("Syntax error near this line"),
            "report should include the source label, got: {report}"
        );
    }

    #[test]
    fn dollar_prefixed_params_and_named_placeholders() {
        let src = "-- @fn get_user($email: String) : users\nSELECT id, email FROM users WHERE email = $email";
        let catalog = parse_query_file(src).expect("parse");
        assert_eq!(catalog.queries.len(), 1);
        let q = &catalog.queries[0];
        assert_eq!(q.name.as_ref(), "get_user");
        assert_eq!(q.params[0].name.as_ref(), "email", "`$` prefix must be stripped");
        assert_eq!(q.sql, "SELECT id, email FROM users WHERE email = $email");
        assert_eq!(q.param_index("email"), Some(1));
        assert_eq!(
            q.to_driver_sql(),
            "SELECT id, email FROM users WHERE email = $1",
            "named placeholders rewrite to positional markers"
        );
    }

    #[test]
    fn missing_return_type_is_an_execution() {
        let catalog = parse_query_file("-- @fn delete_user(id: BigInt)\nDELETE FROM users WHERE id = $id")
            .expect("parse");
        assert_eq!(catalog.queries[0].return_type, QueryReturnType::Exec);
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
    fn to_driver_sql_keeps_positional_and_unknown_names() {
        let src = "-- @fn get_users($limit: Int, $email: String) : Users[]\nSELECT id, email FROM users\nWHERE email = $email AND id < $limit AND id > $unknown";
        let catalog = parse_query_file(src).expect("parse");
        let q = &catalog.queries[0];
        assert_eq!(q.max_placeholder_index(), 2);
        assert_eq!(
            q.to_driver_sql(),
            "SELECT id, email FROM users\nWHERE email = $2 AND id < $1 AND id > $unknown"
        );
    }
}
