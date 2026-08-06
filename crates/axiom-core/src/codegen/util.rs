//! Shared helpers for the code generators.

use crate::catalog::{ColumnSchema, RuleKind, TableSchema};

/// Convert a table name such as `public.users` into a PascalCase type name,
/// e.g. `Users`.
pub fn type_name(table: &TableSchema) -> String {
    pascal_case(&table.name)
}

/// Convert a dotted name such as `public.users` (or any snake/kebab/dotted
/// string) into a PascalCase identifier, e.g. `Users`.
pub fn pascal_case(name: &str) -> String {
    let last = name.rsplit('.').next().unwrap_or(name);
    let mut out = String::new();
    let mut cap = true;
    for c in last.chars() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            cap = true;
        } else if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Convert a column name to a camelCase TypeScript field name.
pub fn ts_field_name(name: &str) -> String {
    let pascal = to_pascal_case(name);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Convert a column name to a snake_case Rust field name.
pub fn rust_field_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_was_lower_or_digit = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_was_lower_or_digit {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_was_lower_or_digit = false;
        } else if c == '-' || c == ' ' {
            out.push('_');
            prev_was_lower_or_digit = false;
        } else {
            out.push(c);
            prev_was_lower_or_digit = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn to_pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            cap = true;
        } else if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Map a SQL data type to a TypeScript type name.
pub fn ts_type(data_type: &str) -> &'static str {
    match core_type(data_type).as_str() {
        "BIGINT" | "BIGSERIAL" | "INT" | "INT2" | "INT4" | "INT8" | "INTEGER" | "SMALLINT"
        | "SERIAL" | "SMALLSERIAL" | "FLOAT" | "FLOAT4" | "FLOAT8" | "REAL" | "DOUBLE"
        | "DOUBLE PRECISION" | "DECIMAL" | "DEC" | "NUMERIC" => "number",
        "BOOL" | "BOOLEAN" => "boolean",
        _ => "string",
    }
}

/// Map a SQL data type to a Rust type name.
pub fn rust_type(data_type: &str) -> &'static str {
    match core_type(data_type).as_str() {
        "BIGINT" | "BIGSERIAL" | "INT8" => "i64",
        "INT" | "INT2" | "INT4" | "INTEGER" | "SMALLINT" | "SERIAL" | "SMALLSERIAL" => "i32",
        "FLOAT" | "FLOAT4" | "REAL" => "f32",
        "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" | "DECIMAL" | "DEC" | "NUMERIC" => "f64",
        "BOOL" | "BOOLEAN" => "bool",
        _ => "String",
    }
}

fn core_type(data_type: &str) -> String {
    data_type
        .split('(')
        .next()
        .unwrap_or(data_type)
        .trim()
        .to_ascii_uppercase()
}

/// Escape a string for embedding in a double-quoted JS/TS string literal.
pub fn escape_ts(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a string for embedding in a double-quoted Rust string literal.
pub fn escape_rust(s: &str) -> String {
    escape_ts(s)
}

/// Escape a string for embedding in a JavaScript/TypeScript template literal
/// body. Backticks, backslashes, and literal `${` sequences are escaped so that
/// only the placeholders the generator injects interpolate. Positional `$1`
/// style markers are left untouched for placeholder substitution.
pub fn escape_ts_template(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            '$' if it.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out
}

/// Build a JavaScript regex literal from a pattern string, escaping `/`.
pub fn ts_regex_literal(pattern: &str, flags: &str) -> String {
    format!("/{}/{}", pattern.replace('/', "\\/"), flags)
}

/// Is this rule a transform rather than a validation?
pub fn is_transform(kind: &RuleKind<'_>) -> bool {
    matches!(kind, RuleKind::Trim | RuleKind::LowerCase | RuleKind::UpperCase)
}

/// Return the TypeScript transform chain (e.g. `.trim().toLowerCase()`) for a
/// column, preserving annotation order, or `None` if there are no transforms.
pub fn ts_transform_chain(column: &ColumnSchema<'_>) -> Option<String> {
    let mut chain = String::new();
    for rule in &column.rules {
        let op = match &rule.kind {
            RuleKind::Trim => ".trim()",
            RuleKind::LowerCase => ".toLowerCase()",
            RuleKind::UpperCase => ".toUpperCase()",
            _ => continue,
        };
        chain.push_str(op);
    }
    if chain.is_empty() {
        None
    } else {
        Some(chain)
    }
}

/// Return the Rust transform chain (e.g. `.trim().to_lowercase()`) for a
/// column, preserving annotation order, or `None` if there are no transforms.
pub fn rust_transform_chain(column: &ColumnSchema<'_>) -> Option<String> {
    let mut chain = String::new();
    for rule in &column.rules {
        let op = match &rule.kind {
            RuleKind::Trim => ".trim()",
            RuleKind::LowerCase => ".to_lowercase()",
            RuleKind::UpperCase => ".to_uppercase()",
            _ => continue,
        };
        chain.push_str(op);
    }
    if chain.is_empty() {
        None
    } else {
        Some(chain)
    }
}

/// Default human-readable message for a rule kind.
pub fn default_message(kind: &RuleKind<'_>) -> String {
    match kind {
        RuleKind::Email => "must be a valid email address".to_string(),
        RuleKind::Url => "must be a valid URL".to_string(),
        RuleKind::Uuid => "must be a valid UUID".to_string(),
        RuleKind::Ulid => "must be a valid ULID".to_string(),
        RuleKind::Ipv4 => "must be a valid IPv4 address".to_string(),
        RuleKind::Ipv6 => "must be a valid IPv6 address".to_string(),
        RuleKind::IsoDate => "must be a valid ISO 8601 date".to_string(),
        RuleKind::Alphanumeric => "must be alphanumeric".to_string(),
        RuleKind::MinLen(n) => format!("must be at least {n} characters long"),
        RuleKind::MaxLen(n) => format!("must be at most {n} characters long"),
        RuleKind::Min(n) => format!("must be greater than or equal to {n}"),
        RuleKind::Max(n) => format!("must be less than or equal to {n}"),
        RuleKind::Regex(p) => format!("must match {p}"),
        RuleKind::Trim | RuleKind::LowerCase | RuleKind::UpperCase => String::new(),
    }
}

/// Resolve the message for a rule: its custom message or the default.
pub fn rule_message(rule: &crate::catalog::ValidationRule<'_>) -> String {
    match &rule.custom_message {
        Some(m) => m.to_string(),
        None => default_message(&rule.kind),
    }
}
