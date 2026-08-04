//! Rust code generation.

use std::fmt::Write;

use crate::catalog::{ColumnSchema, RuleKind, TableCatalog, TableSchema, ValidationRule};
use crate::codegen::util;
use crate::query::{QueryCatalog, QueryDefinition, QueryReturnType};

const REGEX_MATCHER: &str = r#"fn regex_is_match(pattern: &str, text: &str) -> bool {
    enum Atom {
        Char(char),
        Any,
        Class(Vec<(char, char)>, bool),
        Group(Vec<(usize, usize)>),
    }

    fn parse_class(re: &[char], i: usize) -> (Vec<(char, char)>, bool, usize) {
        let mut j = i + 1;
        let mut neg = false;
        if j < re.len() && re[j] == '^' {
            neg = true;
            j += 1;
        }
        let mut ranges = Vec::new();
        while j < re.len() && re[j] != ']' {
            let lo = re[j];
            if j + 2 < re.len() && re[j + 1] == '-' && re[j + 2] != ']' {
                ranges.push((lo, re[j + 2]));
                j += 3;
            } else {
                ranges.push((lo, lo));
                j += 1;
            }
        }
        if j >= re.len() {
            return (ranges, neg, j);
        }
        (ranges, neg, j + 1)
    }

    fn parse_group(re: &[char], i: usize) -> (Vec<(usize, usize)>, usize) {
        let mut depth = 0;
        let mut branches: Vec<(usize, usize)> = Vec::new();
        let mut start = i + 1;
        let mut j = i + 1;
        let mut in_class = false;
        while j < re.len() {
            if in_class {
                if re[j] == ']' {
                    in_class = false;
                }
                j += 1;
                continue;
            }
            match re[j] {
                '[' => in_class = true,
                '(' => depth += 1,
                ')' if depth == 0 => {
                    branches.push((start, j));
                    return (branches, j + 1);
                }
                ')' => depth -= 1,
                '|' if depth == 0 => {
                    branches.push((start, j));
                    start = j + 1;
                }
                _ => {}
            }
            j += 1;
        }
        branches.push((start, j));
        (branches, j)
    }

    fn parse_atom(re: &[char], i: usize) -> (Atom, usize) {
        match re[i] {
            '.' => (Atom::Any, i + 1),
            '[' => {
                let (ranges, neg, end) = parse_class(re, i);
                (Atom::Class(ranges, neg), end)
            }
            '\\' => {
                let e = re.get(i + 1).copied().unwrap_or('\\');
                let atom = match e {
                    'd' => Atom::Class(vec![('0', '9')], false),
                    'w' => Atom::Class(
                        vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        false,
                    ),
                    's' => Atom::Class(
                        vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                        false,
                    ),
                    'D' => Atom::Class(vec![('0', '9')], true),
                    'W' => Atom::Class(
                        vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        true,
                    ),
                    'S' => Atom::Class(
                        vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                        true,
                    ),
                    other => Atom::Char(other),
                };
                (atom, i + 2)
            }
            '(' => {
                let (branches, end) = parse_group(re, i);
                (Atom::Group(branches), end)
            }
            c => (Atom::Char(c), i + 1),
        }
    }

    fn parse_quant(re: &[char], i: usize) -> ((usize, Option<usize>), usize) {
        let Some(&c) = re.get(i) else {
            return ((1, Some(1)), i);
        };
        match c {
            '*' => ((0, None), i + 1),
            '+' => ((1, None), i + 1),
            '?' => ((0, Some(1)), i + 1),
            '{' => {
                let mut j = i + 1;
                let mut min = 0usize;
                let mut num = 0usize;
                let mut saw_num = false;
                while j < re.len() && re[j].is_ascii_digit() {
                    num = num.saturating_mul(10) + (re[j] as usize - '0' as usize);
                    if !saw_num {
                        min = num;
                        saw_num = true;
                    }
                    j += 1;
                }
                let mut max = None;
                if j < re.len() && re[j] == ',' {
                    j += 1;
                    let mut end = 0usize;
                    let mut has_end = false;
                    while j < re.len() && re[j].is_ascii_digit() {
                        end = end.saturating_mul(10) + (re[j] as usize - '0' as usize);
                        has_end = true;
                        j += 1;
                    }
                    if has_end {
                        max = Some(end);
                    }
                } else if saw_num {
                    max = Some(num);
                }
                if j < re.len() && re[j] == '}' {
                    ((min, max), j + 1)
                } else {
                    ((1, Some(1)), i)
                }
            }
            _ => ((1, Some(1)), i),
        }
    }

    fn atom_end_positions(atom: &Atom, re: &[char], text: &[char], j: usize) -> Vec<usize> {
        match atom {
            Atom::Char(c) => {
                if j < text.len() && text[j] == *c {
                    vec![j + 1]
                } else {
                    vec![]
                }
            }
            Atom::Any => {
                if j < text.len() {
                    vec![j + 1]
                } else {
                    vec![]
                }
            }
            Atom::Class(ranges, neg) => {
                if j < text.len() {
                    let c = text[j];
                    let hit = ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
                    if hit != *neg {
                        vec![j + 1]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            Atom::Group(branches) => {
                let mut ends = Vec::new();
                for &(s, e) in branches {
                    for k in match_ends(&re[s..e], 0, text, j) {
                        if !ends.contains(&k) {
                            ends.push(k);
                        }
                    }
                }
                ends
            }
        }
    }

    fn match_ends(re: &[char], i: usize, text: &[char], j: usize) -> Vec<usize> {
        if i >= re.len() {
            return vec![j];
        }
        match re[i] {
            '^' => {
                if j != 0 {
                    vec![]
                } else {
                    match_ends(re, i + 1, text, j)
                }
            }
            '$' => {
                if j == text.len() {
                    match_ends(re, i + 1, text, j)
                } else {
                    vec![]
                }
            }
            _ => {
                let (atom, next) = parse_atom(re, i);
                let ((min, max), qi) = parse_quant(re, next);
                let mut result = Vec::new();
                let max_steps = max.unwrap_or(text.len().saturating_sub(j) + 1);
                let mut states: Vec<usize> = vec![j];
                let mut step = 0usize;
                loop {
                    if step >= min && !states.is_empty() {
                        for &k in &states {
                            for e in match_ends(re, qi, text, k) {
                                if !result.contains(&e) {
                                    result.push(e);
                                }
                            }
                        }
                    }
                    if step >= max_steps || max.is_some_and(|m| step >= m) {
                        break;
                    }
                    let mut next_states = Vec::new();
                    let mut progressed = false;
                    for &k in &states {
                        for e in atom_end_positions(&atom, re, text, k) {
                            if e != k {
                                progressed = true;
                            }
                            if !next_states.contains(&e) {
                                next_states.push(e);
                            }
                        }
                    }
                    states = next_states;
                    step += 1;
                    if !progressed {
                        break;
                    }
                }
                result
            }
        }
    }

    let re: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let anchored = matches!(re.first(), Some('^'));
    let mut j = 0usize;
    loop {
        if !match_ends(&re, 0, &text, j).is_empty() {
            return true;
        }
        if anchored || j >= text.len() {
            return false;
        }
        j += 1;
    }
}
"#;

/// Generate a Rust module with serde structs, `validate` methods, and sqlx
/// query wrappers.
pub fn generate_rust(catalog: &TableCatalog, queries: &QueryCatalog) -> String {
    let mut out = String::new();
    out.push_str("// Generated by axiom. Do not edit.\n\n");
    out.push_str("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n");
    out.push_str("pub struct ValidationError {\n");
    out.push_str("    pub path: String,\n");
    out.push_str("    pub message: String,\n");
    out.push_str("}\n\n");

    for table in &catalog.tables {
        emit_table(&mut out, table);
    }

    for query in &queries.queries {
        emit_query(&mut out, query);
    }

    emit_preset_helpers(&mut out, catalog, queries);

    if regex_used(catalog, queries) {
        out.push('\n');
        out.push_str(REGEX_MATCHER);
        out.push('\n');
    }

    out
}

fn emit_table(out: &mut String, table: &TableSchema) {
    let type_name = util::type_name(table);

    let _ = writeln!(out, "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]");
    let _ = writeln!(out, "pub struct {type_name} {{");
    for column in &table.columns {
        let field = util::rust_field_name(&column.name);
        let ty = util::rust_type(&column.data_type);
        if column.nullable {
            let _ = writeln!(out, "    pub {field}: Option<{ty}>,");
        } else {
            let _ = writeln!(out, "    pub {field}: {ty},");
        }
    }
    out.push_str("}\n\n");

    let _ = writeln!(out, "impl {type_name} {{");
    let _ = writeln!(
        out,
        "    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {{"
    );
    out.push_str("        let mut errors: Vec<ValidationError> = Vec::new();\n");
    for column in &table.columns {
        emit_column_validation(out, column);
    }
    out.push_str("        if errors.is_empty() {\n");
    out.push_str("            Ok(())\n");
    out.push_str("        } else {\n");
    out.push_str("            Err(errors)\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
}

fn emit_column_validation(out: &mut String, column: &ColumnSchema) {
    let field = util::rust_field_name(&column.name);
    let transforms = util::rust_transform_chain(column);
    let validations: Vec<&ValidationRule> = column
        .rules
        .iter()
        .filter(|rule| !util::is_transform(&rule.kind))
        .collect();

    if transforms.is_none() && validations.is_empty() {
        return;
    }

    let base_indent = if column.nullable { "            " } else { "        " };
    let push_indent = format!("{base_indent}    ");

    let value_expr: String;
    if column.nullable {
        let _ = writeln!(out, "        if let Some(value) = &self.{field} {{");
        match &transforms {
            Some(chain) => {
                let _ = writeln!(out, "{base_indent}let {field} = value{chain};");
                value_expr = field.clone();
            }
            None => {
                value_expr = "value".to_string();
            }
        }
    } else if let Some(chain) = &transforms {
        let _ = writeln!(out, "{base_indent}let {field} = self.{field}{chain};");
        value_expr = field.clone();
    } else {
        value_expr = format!("self.{field}");
    }

    for rule in validations {
        let condition = rust_condition(&rule.kind, &value_expr, column.nullable);
        let message = util::escape_rust(&util::rule_message(rule));
        let _ = writeln!(out, "{base_indent}if {condition} {{");
        let _ = writeln!(out, "{push_indent}errors.push(ValidationError {{");
        let _ = writeln!(
            out,
            "{push_indent}    path: \"{field}\".to_string(),"
        );
        let _ = writeln!(out, "{push_indent}    message: \"{message}\".to_string(),");
        let _ = writeln!(out, "{push_indent}}});");
        let _ = writeln!(out, "{base_indent}}}");
    }

    if column.nullable {
        out.push_str("        }\n");
    }
}

fn rust_condition(kind: &RuleKind, value: &str, nullable: bool) -> String {
    match kind {
        RuleKind::Email => format!("!is_email(&{value})"),
        RuleKind::Url => format!("!is_url(&{value})"),
        RuleKind::Uuid => format!("!is_uuid(&{value})"),
        RuleKind::Ulid => format!("!is_ulid(&{value})"),
        RuleKind::Ipv4 => format!("!is_ipv4(&{value})"),
        RuleKind::Ipv6 => format!("!is_ipv6(&{value})"),
        RuleKind::IsoDate => format!("!is_iso_date(&{value})"),
        RuleKind::Alphanumeric => format!("!is_alphanumeric(&{value})"),
        RuleKind::MinLen(n) => format!("{value}.chars().count() < {n}"),
        RuleKind::MaxLen(n) => format!("{value}.chars().count() > {n}"),
        RuleKind::Min(n) => {
            let deref = if nullable { "*" } else { "" };
            format!("{deref}{value} < {n}")
        }
        RuleKind::Max(n) => {
            let deref = if nullable { "*" } else { "" };
            format!("{deref}{value} > {n}")
        }
        RuleKind::Regex(pattern) => {
            let escaped = util::escape_rust(pattern);
            format!("!regex_is_match(\"{escaped}\", &{value})")
        }
        RuleKind::Trim | RuleKind::LowerCase | RuleKind::UpperCase => String::new(),
    }
}

fn emit_preset_helpers(out: &mut String, catalog: &TableCatalog, queries: &QueryCatalog) {
    let used = used_presets(catalog, queries);

    if used.contains(&"email") {
        out.push_str(IS_EMAIL_HELPER);
        out.push('\n');
    }
    if used.contains(&"url") {
        out.push_str(IS_URL_HELPER);
        out.push('\n');
    }
    if used.contains(&"uuid") {
        out.push_str(IS_UUID_HELPER);
        out.push('\n');
    }
    if used.contains(&"ulid") {
        out.push_str(IS_ULID_HELPER);
        out.push('\n');
    }
    if used.contains(&"ipv4") {
        out.push_str(IS_IPV4_HELPER);
        out.push('\n');
    }
    if used.contains(&"ipv6") {
        out.push_str(IS_IPV6_HELPER);
        out.push('\n');
    }
    if used.contains(&"iso_date") {
        out.push_str(IS_ISO_DATE_HELPER);
        out.push('\n');
    }
    if used.contains(&"alphanumeric") {
        out.push_str(IS_ALPHANUMERIC_HELPER);
        out.push('\n');
    }
}

fn used_presets(catalog: &TableCatalog, queries: &QueryCatalog) -> Vec<&'static str> {
    let mut used = Vec::new();
    let mut push = |kind: &RuleKind| {
        let name = match kind {
            RuleKind::Email => "email",
            RuleKind::Url => "url",
            RuleKind::Uuid => "uuid",
            RuleKind::Ulid => "ulid",
            RuleKind::Ipv4 => "ipv4",
            RuleKind::Ipv6 => "ipv6",
            RuleKind::IsoDate => "iso_date",
            RuleKind::Alphanumeric => "alphanumeric",
            _ => return,
        };
        if !used.contains(&name) {
            used.push(name);
        }
    };
    for table in &catalog.tables {
        for column in &table.columns {
            for rule in &column.rules {
                push(&rule.kind);
            }
        }
    }
    for query in &queries.queries {
        for rules in query.validations.values() {
            for rule in rules {
                push(&rule.kind);
            }
        }
    }
    used
}

fn regex_used(catalog: &TableCatalog, queries: &QueryCatalog) -> bool {
    let in_table = catalog.tables.iter().any(|table| {
        table
            .columns
            .iter()
            .any(|column| column.rules.iter().any(|r| matches!(r.kind, RuleKind::Regex(_))))
    });
    if in_table {
        return true;
    }
    queries.queries.iter().any(|query| {
        query
            .validations
            .values()
            .flatten()
            .any(|r| matches!(r.kind, RuleKind::Regex(_)))
    })
}

/// Emit a sqlx-backed query function and its params struct.
fn emit_query(out: &mut String, query: &QueryDefinition) {
    let pascal = util::pascal_case(&query.name);
    let params_type = format!("{pascal}Params");
    let fn_name = util::rust_field_name(&query.name);

    let _ = writeln!(out, "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]");
    let _ = writeln!(out, "pub struct {params_type} {{");
    for param in &query.params {
        let field = util::rust_field_name(&param.name);
        let ty = util::rust_type(&param.param_type);
        let _ = writeln!(out, "    pub {field}: {ty},");
    }
    out.push_str("}\n\n");

    let _ = writeln!(out, "impl {params_type} {{");
    let _ = writeln!(
        out,
        "    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {{"
    );
    out.push_str("        let mut errors: Vec<ValidationError> = Vec::new();\n");
    for (param, rules) in &query.validations {
        emit_param_validation(out, param, rules);
    }
    out.push_str("        if errors.is_empty() {\n");
    out.push_str("            Ok(())\n");
    out.push_str("        } else {\n");
    out.push_str("            Err(errors)\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    let (ret_ty, fetch) = match &query.return_type {
        QueryReturnType::Many(row) => (format!("Vec<{row}>"), Some("fetch_all(pool)")),
        QueryReturnType::Single(row) => (format!("Option<{row}>"), Some("fetch_optional(pool)")),
        QueryReturnType::Exec => ("()".to_string(), None),
    };

    let _ = writeln!(out, "pub async fn {fn_name}(");
    let _ = writeln!(out, "    pool: &sqlx::PgPool,");
    let _ = writeln!(out, "    params: {params_type},");
    let _ = writeln!(out, ") -> Result<{ret_ty}, Box<dyn std::error::Error>> {{");
    out.push_str(
        "    params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;\n",
    );

    match &query.return_type {
        QueryReturnType::Many(row) | QueryReturnType::Single(row) => {
            let sql_lit = rust_raw_string(&query.sql);
            let _ = writeln!(out, "    let rows = sqlx::query_as!(");
            let _ = writeln!(out, "        {row},");
            let _ = writeln!(out, "        {sql_lit},");
            for field in bound_fields(query) {
                let _ = writeln!(out, "        params.{field},");
            }
            out.push_str("    )\n");
            let _ = writeln!(out, "    .{}", fetch.expect("fetch for row query"));
            out.push_str("    .await?;\n");
            out.push_str("    Ok(rows)\n");
        }
        QueryReturnType::Exec => {
            let sql_lit = rust_raw_string(&query.sql);
            let _ = writeln!(out, "    sqlx::query(");
            let _ = writeln!(out, "        {sql_lit},");
            out.push_str("    )\n");
            for field in bound_fields(query) {
                let _ = writeln!(out, "    .bind(params.{field})");
            }
            out.push_str("    .execute(pool)\n");
            out.push_str("    .await?;\n");
            out.push_str("    Ok(())\n");
        }
    }
    out.push_str("}\n\n");
}

fn emit_param_validation(out: &mut String, param: &str, rules: &[ValidationRule]) {
    let field = util::rust_field_name(param);
    let mut chain = String::new();
    for rule in rules {
        if !util::is_transform(&rule.kind) {
            continue;
        }
        chain.push_str(match &rule.kind {
            RuleKind::Trim => ".trim()",
            RuleKind::LowerCase => ".to_lowercase()",
            RuleKind::UpperCase => ".to_uppercase()",
            _ => unreachable!(),
        });
    }

    let validations: Vec<&ValidationRule> = rules
        .iter()
        .filter(|rule| !util::is_transform(&rule.kind))
        .collect();

    if chain.is_empty() && validations.is_empty() {
        return;
    }

    let value = if chain.is_empty() {
        format!("self.{field}")
    } else {
        let _ = writeln!(out, "        let {field} = self.{field}{chain};");
        field.clone()
    };

    for rule in validations {
        let condition = rust_condition(&rule.kind, &value, false);
        let message = util::escape_rust(&util::rule_message(rule));
        let _ = writeln!(out, "        if {condition} {{");
        let _ = writeln!(out, "            errors.push(ValidationError {{");
        let _ = writeln!(out, "                path: \"{field}\".to_string(),");
        let _ = writeln!(out, "                message: \"{message}\".to_string(),");
        let _ = writeln!(out, "            }});");
        let _ = writeln!(out, "        }}");
    }
}

/// Field names to bind, in `$1..$n` order, for the placeholders present in the
/// query SQL. At most the declared parameter count are bound.
fn bound_fields(query: &QueryDefinition) -> Vec<String> {
    let max = max_placeholder(&query.sql).min(query.params.len());
    (0..max)
        .map(|i| util::rust_field_name(&query.params[i].name))
        .collect()
}

fn max_placeholder(sql: &str) -> usize {
    let mut max = 0usize;
    let mut rest = sql;
    while let Some(pos) = rest.find('$') {
        let after = &rest[pos + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            max = max.max(n);
        }
        rest = if digits.is_empty() { after } else { &after[digits.len()..] };
    }
    max
}

/// Wrap SQL in a raw string literal, bumping the number of `#` delimiters if
/// the body contains a terminator.
fn rust_raw_string(sql: &str) -> String {
    let mut hashes = 1usize;
    loop {
        let close = format!("\"{}", "#".repeat(hashes));
        if !sql.contains(&close) {
            let hashes_str = "#".repeat(hashes);
            return format!("r{hashes_str}\"{sql}\"{hashes_str}");
        }
        hashes += 1;
    }
}

const IS_EMAIL_HELPER: &str = r#"fn is_email(value: &str) -> bool {
    if value.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = value.split('@');
    let Some(local) = parts.next() else {
        return false;
    };
    let Some(domain) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    !local.is_empty() && !domain.is_empty() && domain.contains('.')
}
"#;

const IS_URL_HELPER: &str = r#"fn is_url(value: &str) -> bool {
    if value.contains(char::is_whitespace) {
        return false;
    }
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"));
    match rest {
        Some(rest) => !rest.is_empty() && (rest.contains('.') || rest.starts_with("localhost")),
        None => false,
    }
}
"#;

const IS_UUID_HELPER: &str = r#"fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}
"#;

const IS_ULID_HELPER: &str = r#"fn is_ulid(value: &str) -> bool {
    value.len() == 26
        && value.chars().all(|c| {
            matches!(c, '0'..='9' | 'A'..='H' | 'J'..='K' | 'M'..='N' | 'P'..='T' | 'V'..='Z')
        })
}
"#;

const IS_IPV4_HELPER: &str = r#"fn is_ipv4(value: &str) -> bool {
    value.parse::<std::net::Ipv4Addr>().is_ok()
}
"#;

const IS_IPV6_HELPER: &str = r#"fn is_ipv6(value: &str) -> bool {
    value.parse::<std::net::Ipv6Addr>().is_ok()
}
"#;

const IS_ISO_DATE_HELPER: &str = r#"fn is_iso_date(value: &str) -> bool {
    if !value.is_ascii() {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let year: i32 = match value[0..4].parse() {
        Ok(y) => y,
        Err(_) => return false,
    };
    let month: u32 = match value[5..7].parse() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let day: u32 = match value[8..10].parse() {
        Ok(d) => d,
        Err(_) => return false,
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    day >= 1 && day <= max_day
}
"#;

const IS_ALPHANUMERIC_HELPER: &str = r#"fn is_alphanumeric(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_alphanumeric())
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ColumnSchema, TableSchema, ValidationRule};
    use std::borrow::Cow;

    fn no_queries() -> QueryCatalog<'static> {
        QueryCatalog::default()
    }

    fn col(
        name: &'static str,
        data_type: &'static str,
        nullable: bool,
        rules: Vec<ValidationRule<'static>>,
    ) -> ColumnSchema<'static> {
        ColumnSchema {
            name: Cow::Borrowed(name),
            data_type: Cow::Borrowed(data_type),
            nullable,
            primary_key: false,
            rules,
        }
    }

    fn rule(kind: RuleKind<'static>, msg: Option<&'static str>) -> ValidationRule<'static> {
        ValidationRule {
            kind,
            custom_message: msg.map(Cow::Borrowed),
        }
    }

    fn table(name: &'static str, columns: Vec<ColumnSchema<'static>>) -> TableSchema<'static> {
        TableSchema {
            name: Cow::Borrowed(name),
            columns,
        }
    }

    #[test]
    fn emits_struct_with_serde_derive() {
        let t = table(
            "users",
            vec![col("email", "VARCHAR(255)", false, vec![]), col("id", "BIGSERIAL", false, vec![])],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"));
        assert!(out.contains("pub struct Users {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub id: i64,"));
    }

    #[test]
    fn emits_validate_method_with_preset_and_message() {
        let t = table(
            "users",
            vec![col(
                "email",
                "VARCHAR(255)",
                false,
                vec![rule(RuleKind::Email, Some("Bad Email"))],
            )],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("pub fn validate(&self) -> Result<(), Vec<ValidationError>>"));
        assert!(out.contains("if !is_email(&self.email) {"));
        assert!(out.contains("message: \"Bad Email\".to_string()"));
        assert!(out.contains("fn is_email(value: &str) -> bool {"));
    }

    #[test]
    fn emits_transforms_before_validation() {
        let t = table(
            "users",
            vec![col(
                "username",
                "VARCHAR(32)",
                false,
                vec![
                    rule(RuleKind::Trim, None),
                    rule(RuleKind::LowerCase, None),
                    rule(RuleKind::Alphanumeric, None),
                ],
            )],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("let username = self.username.trim().to_lowercase();"));
        assert!(out.contains("if !is_alphanumeric(&username) {"));
    }

    #[test]
    fn emits_length_and_numeric_bounds() {
        let t = table(
            "accounts",
            vec![
                col("name", "VARCHAR", false, vec![rule(RuleKind::MinLen(3), None)]),
                col("age", "INT", false, vec![rule(RuleKind::Min(18), None)]),
                col("score", "INT", true, vec![rule(RuleKind::Max(100), None)]),
            ],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("self.name.chars().count() < 3"));
        assert!(out.contains("self.age < 18"));
        assert!(out.contains("*value > 100"));
    }

    #[test]
    fn nullable_fields_use_if_let() {
        let t = table(
            "sessions",
            vec![col(
                "external_id",
                "UUID",
                true,
                vec![rule(RuleKind::Uuid, None)],
            )],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("pub external_id: Option<String>,"));
        assert!(out.contains("if let Some(value) = &self.external_id {"));
        assert!(out.contains("if !is_uuid(&value) {"));
    }

    #[test]
    fn emits_regex_matcher_only_when_regex_rules_exist() {
        let plain = table("plain", vec![col("email", "VARCHAR", false, vec![rule(RuleKind::Email, None)])]);
        let out = generate_rust(&TableCatalog { tables: vec![plain] }, &no_queries());
        assert!(!out.contains("fn regex_is_match"));

        let regex = table(
            "slugged",
            vec![col(
                "slug",
                "VARCHAR",
                false,
                vec![rule(RuleKind::Regex(Cow::Borrowed("^[a-z0-9-]+$")), None)],
            )],
        );
        let out = generate_rust(&TableCatalog { tables: vec![regex] }, &no_queries());
        assert!(out.contains("fn regex_is_match"));
        assert!(out.contains("!regex_is_match(\"^[a-z0-9-]+$\", &self.slug)"));
    }

    #[test]
    fn only_emits_used_preset_helpers() {
        let t = table(
            "users",
            vec![
                col("email", "VARCHAR", false, vec![rule(RuleKind::Email, None)]),
                col("id", "BIGSERIAL", false, vec![]),
            ],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("fn is_email"));
        assert!(!out.contains("fn is_ipv6"));
        assert!(!out.contains("fn is_ulid"));
    }

    fn query_catalog() -> QueryCatalog<'static> {
        let q = QueryDefinition {
            name: Cow::Borrowed("get_user"),
            sql: "SELECT id, email FROM users WHERE email = $1 AND id < $2".to_string(),
            params: vec![
                crate::query::QueryParam {
                    name: Cow::Borrowed("email"),
                    param_type: Cow::Borrowed("String"),
                },
                crate::query::QueryParam {
                    name: Cow::Borrowed("max_id"),
                    param_type: Cow::Borrowed("BigInt"),
                },
            ],
            return_type: QueryReturnType::Single(Cow::Borrowed("Users")),
            validations: [(
                Cow::Borrowed("email"),
                vec![
                    rule(RuleKind::Email, Some("Bad Email")),
                    rule(RuleKind::Trim, None),
                    rule(RuleKind::LowerCase, None),
                ],
            )]
            .into_iter()
            .collect(),
        };
        QueryCatalog { queries: vec![q] }
    }

    #[test]
    fn emits_query_params_struct_and_sqlx_wrapper() {
        let out = generate_rust(&TableCatalog::default(), &query_catalog());
        assert!(out.contains("pub struct GetUserParams {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub max_id: i64,"));
        assert!(out.contains("impl GetUserParams {"));
        assert!(out.contains("pub async fn get_user("));
        assert!(out.contains("pool: &sqlx::PgPool,"));
        assert!(out.contains("params: GetUserParams,"));
        assert!(out.contains(") -> Result<Option<Users>, Box<dyn std::error::Error>> {"));
        assert!(out.contains("params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;"));
        assert!(out.contains("sqlx::query_as!("));
        assert!(out.contains("Users,"));
        assert!(out.contains("params.email,"));
        assert!(out.contains("params.max_id,"));
        assert!(out.contains(".fetch_optional(pool)"));
    }

    #[test]
    fn emits_exec_query_with_bind_chain() {
        let q = QueryDefinition {
            name: Cow::Borrowed("delete_user"),
            sql: "DELETE FROM users WHERE id = $1".to_string(),
            params: vec![crate::query::QueryParam {
                name: Cow::Borrowed("id"),
                param_type: Cow::Borrowed("Uuid"),
            }],
            return_type: QueryReturnType::Exec,
            validations: Default::default(),
        };
        let out = generate_rust(
            &TableCatalog::default(),
            &QueryCatalog { queries: vec![q] },
        );
        assert!(out.contains(") -> Result<(), Box<dyn std::error::Error>> {"));
        assert!(out.contains("sqlx::query("));
        assert!(out.contains(".bind(params.id)"));
        assert!(out.contains(".execute(pool)"));
        assert!(out.contains("DELETE FROM users WHERE id = $1"));
    }

    #[test]
    fn query_rules_drive_preset_helpers() {
        let out = generate_rust(&TableCatalog::default(), &query_catalog());
        assert!(out.contains("fn is_email(value: &str) -> bool {"));
        assert!(!out.contains("fn is_ipv6"));
    }

    #[test]
    fn rust_raw_string_escapes_terminator() {
        assert_eq!(rust_raw_string("a\"#b"), "r##\"a\"#b\"##");
        assert_eq!(rust_raw_string("plain"), "r#\"plain\"#");
    }
}
