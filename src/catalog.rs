//! Zero-copy schema catalog and SQL comment-annotation parser.
//!
//! Axiom reads validation metadata out of the `-- @validate` and
//! `-- @override` comments that live directly above column definitions in SQL
//! DDL. Everything that can borrow from the input is kept as a `Cow<'a, str>`
//! so parsing allocates only when a value must be synthesized (e.g. the
//! normalized `DataType` display string).

use std::borrow::Cow;

use sqlparser::ast::{ColumnDef, ColumnOption, ObjectName, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span, Token, TokenWithSpan, Tokenizer};

/// A single validation rule attached to a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationRule<'a> {
    /// The kind of rule to apply.
    pub kind: RuleKind<'a>,
    /// Optional user-supplied message. If absent the rule inherits the
    /// column-level fallback message.
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

/// A single column extracted from a `CREATE TABLE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSchema<'a> {
    /// Column name, borrowed from the SQL source when possible.
    pub name: Cow<'a, str>,
    /// Canonicalized column data type, e.g. `VARCHAR(255)`.
    pub data_type: Cow<'a, str>,
    /// Whether the column may contain `NULL`.
    pub nullable: bool,
    /// Whether the column is (part of) the table's primary key.
    pub primary_key: bool,
    /// Validation rules gathered from the immediately preceding annotation.
    pub rules: Vec<ValidationRule<'a>>,
}

/// A single table and its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema<'a> {
    /// Fully qualified table name, e.g. `public.users`.
    pub name: Cow<'a, str>,
    /// The columns of the table, in source order.
    pub columns: Vec<ColumnSchema<'a>>,
}

/// A parsed catalog of one or more `CREATE TABLE` statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableCatalog<'a> {
    /// Tables in the order they were declared.
    pub tables: Vec<TableSchema<'a>>,
}

impl<'a> TableCatalog<'a> {
    /// Return the table with the given (suffix) name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn table_by_name(&self, name: &str) -> Option<&TableSchema<'a>> {
        self.tables
            .iter()
            .find(|t| t.name == name || t.name.ends_with(&format!(".{name}")))
    }
}

/// Parse a single annotation line into validation rules.
///
/// The line may or may not include the leading `--` comment marker and either
/// the `@validate` or `@override` keyword, e.g.:
///
/// ```sql
/// -- @validate email[msg="Invalid email"], min_len=5, msg="Default error"
/// -- @override trim, lower
/// ```
///
/// A trailing `msg="..."` segment acts as the column-level fallback message:
/// rules without their own inline `[msg="..."]` inherit it.
pub fn parse_annotation_line<'a>(line: &'a str) -> Vec<ValidationRule<'a>> {
    let content = strip_annotation_prefix(line);

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

/// Strip the `--` marker and `@validate` / `@override` keyword from a line.
fn strip_annotation_prefix(line: &str) -> &str {
    let mut rest = line.trim();
    if let Some(after) = rest.strip_prefix("--") {
        rest = after;
    }
    rest = rest.trim_start();

    for keyword in ["@validate", "@override"] {
        if rest
            .get(..keyword.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(keyword))
        {
            return &rest[keyword.len()..];
        }
    }

    rest
}

struct ParsedSegment<'a> {
    name: &'a str,
    value: Option<&'a str>,
    msg: Option<&'a str>,
}

/// Split a string on `delimiter`, ignoring delimiters inside double quotes.
fn split_top_level(s: &str, delimiter: char) -> Vec<&str> {
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

/// Parse SQL source into a zero-copy [`TableCatalog`].
///
/// Every `CREATE TABLE` statement is parsed; each column's definition is
/// paired with the rules from the `-- @validate` / `-- @override` comment that
/// immediately precedes it.
pub fn parse_sql_catalog<'a>(sql: &'a str) -> Result<TableCatalog<'a>, Box<dyn std::error::Error>> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql)?;
    let tokens = Tokenizer::new(&dialect, sql).tokenize_with_location()?;
    let line_starts = compute_line_starts(sql);
    let annotations = collect_annotations(sql);

    let mut catalog = TableCatalog::default();

    for stmt in &statements {
        let Statement::CreateTable(create) = stmt else {
            continue;
        };

        let located = locate_column_lines(&create.columns, &tokens, sql, &line_starts);

        let mut columns = Vec::with_capacity(create.columns.len());
        let mut prev_line = 0u64;

        for (col, loc) in create.columns.iter().zip(located.iter()) {
            let Some((line, name)) = loc else {
                continue;
            };

            // The immediately preceding annotation: the last one on a line
            // after the previous column and before this one.
            let mut rules: Vec<ValidationRule<'a>> = Vec::new();
            for (annotation_line, annotation_rules) in &annotations {
                if *annotation_line > prev_line && *annotation_line < *line {
                    rules = annotation_rules.clone();
                }
            }

            columns.push(ColumnSchema {
                name: name.clone(),
                data_type: Cow::Owned(col.data_type.to_string()),
                nullable: column_nullable(col),
                primary_key: column_primary_key(col),
                rules,
            });

            prev_line = *line;
        }

        catalog.tables.push(TableSchema {
            name: Cow::Owned(object_name_to_string(&create.name)),
            columns,
        });
    }

    Ok(catalog)
}

fn column_nullable(col: &ColumnDef) -> bool {
    col.options.iter().all(|o| {
        !matches!(o.option, ColumnOption::NotNull | ColumnOption::PrimaryKey(_))
    })
}

/// Format an object name (e.g. `"public"."users"`) as `public.users`,
/// dropping any quote characters.
fn object_name_to_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
        .collect::<Vec<_>>()
        .join(".")
}

fn column_primary_key(col: &ColumnDef) -> bool {
    col.options
        .iter()
        .any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)))
}

/// Scan the source for `-- @validate` / `-- @override` comment lines and parse
/// their rules, returning `(1-based line, rules)` pairs.
fn collect_annotations<'a>(sql: &'a str) -> Vec<(u64, Vec<ValidationRule<'a>>)> {
    let mut out = Vec::new();

    for (idx, raw) in sql.lines().enumerate() {
        let trimmed = raw.trim();
        let Some(rest) = trimmed.strip_prefix("--") else {
            continue;
        };
        let rest = rest.trim_start();

        let is_annotation = ["@validate", "@override"]
            .iter()
            .any(|kw| rest.get(..kw.len()).is_some_and(|p| p.eq_ignore_ascii_case(kw)));
        if is_annotation {
            out.push(((idx + 1) as u64, parse_annotation_line(rest)));
        }
    }

    out
}

fn compute_line_starts(sql: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in sql.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn offset_at(line_starts: &[usize], loc: Location) -> usize {
    let line = loc.line as usize;
    let col = loc.column as usize;
    line_starts.get(line - 1).copied().unwrap_or(0) + (col - 1)
}

/// Borrow the exact source text covered by a token span.
fn slice_span<'a>(sql: &'a str, line_starts: &[usize], span: &Span) -> &'a str {
    let start = offset_at(line_starts, span.start);
    let end = offset_at(line_starts, span.end);
    &sql[start..end]
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'`' && last == b'`') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Words that begin a table-level constraint rather than a column definition.
fn is_constraint_start(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "primary"
            | "key"
            | "unique"
            | "constraint"
            | "foreign"
            | "check"
            | "references"
            | "exclude"
            | "index"
    )
}

/// Locate the source line and borrowed name of each column definition.
///
/// Returns one entry per [`ColumnDef`], in order. An entry is `None` when the
/// column could not be located in the token stream (should not happen for
/// well-formed DDL).
fn locate_column_lines<'a>(
    columns: &[ColumnDef],
    tokens: &[TokenWithSpan],
    sql: &'a str,
    line_starts: &[usize],
) -> Vec<Option<(u64, Cow<'a, str>)>> {
    let significant: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| !matches!(&t.token, Token::Whitespace(_)))
        .collect();

    // Candidate column starts: identifier tokens at paren-depth 1 that directly
    // follow `(` or `,`.
    let mut candidates: Vec<(u64, String, Cow<'a, str>)> = Vec::new();
    let mut depth: i64 = 0;

    for (idx, tws) in significant.iter().enumerate() {
        match &tws.token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }

        if depth != 1 {
            continue;
        }
        let Token::Word(word) = &tws.token else {
            continue;
        };
        let prev = idx
            .checked_sub(1)
            .and_then(|i| significant.get(i))
            .map(|t| &t.token);
        if !matches!(prev, Some(Token::LParen) | Some(Token::Comma)) {
            continue;
        }
        if is_constraint_start(&word.value) {
            continue;
        }

        let borrowed = Cow::Borrowed(strip_quotes(slice_span(sql, line_starts, &tws.span)));
        candidates.push((tws.span.start.line, word.value.clone(), borrowed));
    }

    // Match candidates to the parsed columns by name (case-insensitive).
    let mut used = vec![false; candidates.len()];
    let mut out = Vec::with_capacity(columns.len());

    for col in columns {
        let needle = col.name.value.to_ascii_lowercase();
        let mut found = None;
        for (i, (line, value, name)) in candidates.iter().enumerate() {
            if !used[i] && value.to_ascii_lowercase() == needle {
                used[i] = true;
                found = Some((*line, name.clone()));
                break;
            }
        }
        out.push(found);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg<'a>(rule: &'a ValidationRule<'a>) -> Option<&'a str> {
        rule.custom_message.as_deref()
    }

    #[test]
    fn parses_standalone_presets() {
        let rules = parse_annotation_line("-- @validate email, uuid, url");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].kind, RuleKind::Email);
        assert_eq!(rules[1].kind, RuleKind::Uuid);
        assert_eq!(rules[2].kind, RuleKind::Url);
        for rule in &rules {
            assert_eq!(msg(rule), None);
        }
    }

    #[test]
    fn parses_bounds_and_regex() {
        let rules = parse_annotation_line(
            "-- @validate min_len=5, max_len=255, min=0, max=100, regex=\"^[a-z]+$\"",
        );
        assert_eq!(rules[0].kind, RuleKind::MinLen(5));
        assert_eq!(rules[1].kind, RuleKind::MaxLen(255));
        assert_eq!(rules[2].kind, RuleKind::Min(0));
        assert_eq!(rules[3].kind, RuleKind::Max(100));
        assert_eq!(rules[4].kind, RuleKind::Regex(Cow::Borrowed("^[a-z]+$")));
    }

    #[test]
    fn inline_messages_override_fallback() {
        let rules = parse_annotation_line(
            "-- @validate email[msg=\"Invalid email\"], uuid, msg=\"Default error\"",
        );
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].kind, RuleKind::Email);
        assert_eq!(msg(&rules[0]), Some("Invalid email"));
        assert_eq!(rules[1].kind, RuleKind::Uuid);
        assert_eq!(msg(&rules[1]), Some("Default error"));
    }

    #[test]
    fn bounds_with_inline_messages() {
        let rules = parse_annotation_line(
            "-- @validate min_len=5[msg=\"Must be 5+ chars\"], max_len=100, msg=\"Bad length\"",
        );
        assert_eq!(rules[0].kind, RuleKind::MinLen(5));
        assert_eq!(msg(&rules[0]), Some("Must be 5+ chars"));
        assert_eq!(rules[1].kind, RuleKind::MaxLen(100));
        assert_eq!(msg(&rules[1]), Some("Bad length"));
    }

    #[test]
    fn multi_rule_with_transforms() {
        let rules = parse_annotation_line("-- @validate trim, lower, email[msg=\"Invalid Email\"]");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].kind, RuleKind::Trim);
        assert_eq!(rules[1].kind, RuleKind::LowerCase);
        assert_eq!(rules[2].kind, RuleKind::Email);
        assert_eq!(msg(&rules[2]), Some("Invalid Email"));
    }

    #[test]
    fn override_annotation_is_parsed() {
        let rules = parse_annotation_line("-- @override upper, ulid");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].kind, RuleKind::UpperCase);
        assert_eq!(rules[1].kind, RuleKind::Ulid);
    }

    #[test]
    fn message_with_commas_and_brackets() {
        let rules = parse_annotation_line("-- @validate regex=\"[0-9]+\"[msg=\"Must match [0-9]+\"]");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind, RuleKind::Regex(Cow::Borrowed("[0-9]+")));
        assert_eq!(msg(&rules[0]), Some("Must match [0-9]+"));
    }

    #[test]
    fn ignores_unknown_rules() {
        let rules = parse_annotation_line("-- @validate email, banana, uuid");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].kind, RuleKind::Email);
        assert_eq!(rules[1].kind, RuleKind::Uuid);
    }

    const SAMPLE_SQL: &str = r#"
CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    -- @validate email[msg="Invalid email"], min_len=5, msg="Bad value"
    email VARCHAR(255) NOT NULL,
    -- @validate trim, lower, uuid
    external_id UUID,
    name TEXT
);

-- @validate ipv4, max_len=15
CREATE TABLE sessions (
    ip INET,
    created_at TIMESTAMP NOT NULL
);
"#;

    #[test]
    fn parses_ddl_into_catalog() {
        let catalog = parse_sql_catalog(SAMPLE_SQL).expect("parse sql");
        assert_eq!(catalog.tables.len(), 2);

        let users = catalog.table_by_name("users").expect("users table");
        assert_eq!(users.columns.len(), 4);

        let id = &users.columns[0];
        assert_eq!(id.name.as_ref(), "id");
        assert!(id.primary_key);
        assert!(!id.nullable);
        assert!(id.rules.is_empty());

        let email = &users.columns[1];
        assert_eq!(email.name.as_ref(), "email");
        assert_eq!(email.data_type.as_ref(), "VARCHAR(255)");
        assert!(!email.nullable);
        assert_eq!(email.rules.len(), 2);
        assert_eq!(email.rules[0].kind, RuleKind::Email);
        assert_eq!(msg(&email.rules[0]), Some("Invalid email"));
        assert_eq!(email.rules[1].kind, RuleKind::MinLen(5));
        assert_eq!(msg(&email.rules[1]), Some("Bad value"));

        let external_id = &users.columns[2];
        assert_eq!(external_id.name.as_ref(), "external_id");
        assert_eq!(external_id.rules.len(), 3);
        assert_eq!(external_id.rules[0].kind, RuleKind::Trim);
        assert_eq!(external_id.rules[1].kind, RuleKind::LowerCase);
        assert_eq!(external_id.rules[2].kind, RuleKind::Uuid);

        let name = &users.columns[3];
        assert_eq!(name.name.as_ref(), "name");
        assert!(name.rules.is_empty());

        let sessions = catalog.table_by_name("sessions").expect("sessions table");
        let ip = &sessions.columns[0];
        assert_eq!(ip.name.as_ref(), "ip");
        assert_eq!(ip.rules.len(), 2);
        assert_eq!(ip.rules[0].kind, RuleKind::Ipv4);
        assert_eq!(ip.rules[1].kind, RuleKind::MaxLen(15));
    }

    #[test]
    fn dangling_annotation_does_not_leak() {
        let sql = r#"
CREATE TABLE t (
    -- @validate email
    a TEXT,
    b TEXT
);
"#;
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        let table = catalog.table_by_name("t").expect("table t");
        assert_eq!(table.columns[0].rules.len(), 1);
        assert_eq!(table.columns[0].rules[0].kind, RuleKind::Email);
        assert!(table.columns[1].rules.is_empty());
    }

    #[test]
    fn zero_copy_names_borrow_from_input() {
        let catalog = parse_sql_catalog(SAMPLE_SQL).expect("parse sql");
        let users = catalog.table_by_name("users").expect("users table");
        let email = &users.columns[1];
        assert!(matches!(email.name, Cow::Borrowed(_)));
        assert!(matches!(email.rules[0].custom_message, Some(Cow::Borrowed(_))));
    }

    #[test]
    fn quoted_identifiers_are_supported() {
        let sql = r#"
CREATE TABLE "mixed" (
    -- @validate url
    "weird col" TEXT
);
"#;
        let catalog = parse_sql_catalog(sql).expect("parse sql");
        let table = catalog.table_by_name("mixed").expect("table");
        assert_eq!(table.columns[0].name.as_ref(), "weird col");
        assert_eq!(table.columns[0].rules[0].kind, RuleKind::Url);
    }
}
