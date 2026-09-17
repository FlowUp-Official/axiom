//! Rust code generation.

use std::fmt::Write;

use crate::catalog::{TableCatalog, TableSchema};
use crate::codegen::util;
use crate::query::{QueryCatalog, QueryDefinition, QueryReturnType, RuleKind, ValidationRule};

pub(crate) const REGEX_MATCHER: &str = r#"fn regex_is_match(pattern: &str, text: &str) -> bool {
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

/// Generate a Rust module with serde structs, `validate` methods, and
/// tokio-postgres query wrappers.
pub fn generate_rust(catalog: &TableCatalog, queries: &QueryCatalog) -> String {
    let mut out = String::new();
    out.push_str("// Generated by axiom. Do not edit.\n");
    out.push_str("#![allow(dead_code)]\n");
    out.push_str("#![allow(clippy::ptr_arg)]\n\n");
    out.push_str("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n");
    out.push_str("pub struct ValidationError {\n");
    out.push_str("    pub path: String,\n");
    out.push_str("    pub message: String,\n");
    out.push_str("}\n\n");

    for table in &catalog.tables {
        emit_table(&mut out, table);
    }

    if !queries.queries.is_empty() {
        emit_db_helpers(&mut out);
    }

    for query in &queries.queries {
        emit_query(&mut out, catalog, query);
    }

    emit_preset_helpers(&mut out, queries);

    if regex_used(queries) {
        out.push('\n');
        out.push_str(REGEX_MATCHER);
        out.push('\n');
    }

    out
}

fn emit_table(out: &mut String, table: &TableSchema) {
    let type_name = util::type_name(table);

    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"
    );
    let _ = writeln!(out, "pub struct {type_name} {{");
    for column in &table.columns {
        let field = util::rust_field_ident(&column.name);
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
    out.push_str("        let _ = self;\n");
    out.push_str("        Ok(())\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
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

fn emit_preset_helpers(out: &mut String, queries: &QueryCatalog) {
    let used = used_presets(queries);

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

fn used_presets(queries: &QueryCatalog) -> Vec<&'static str> {
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
    for query in &queries.queries {
        for rules in query.validations.values() {
            for rule in rules {
                push(&rule.kind);
            }
        }
    }
    used
}

fn regex_used(queries: &QueryCatalog) -> bool {
    queries.queries.iter().any(|query| {
        query
            .validations
            .values()
            .flatten()
            .any(|r| matches!(r.kind, RuleKind::Regex(_)))
    })
}

/// Emit a tokio-postgres-backed query function and its params struct.
fn emit_query(out: &mut String, catalog: &TableCatalog, query: &QueryDefinition) {
    let pascal = util::pascal_case(&query.name);
    let params_type = format!("{pascal}Params");
    let fn_name = util::rust_field_name(&query.name);

    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"
    );
    let _ = writeln!(out, "pub struct {params_type} {{");
    for param in &query.params {
        let field = util::rust_field_ident(&param.name);
        let ty = util::rust_type(&param.param_type);
        let _ = writeln!(out, "    pub {field}: {ty},");
    }
    out.push_str("}\n\n");

    let _ = writeln!(out, "impl {params_type} {{");
    let _ = writeln!(
        out,
        "    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {{"
    );
    if query.validations.is_empty() {
        out.push_str("        let _ = self;\n");
        out.push_str("        Ok(())\n");
    } else {
        out.push_str("        let mut errors: Vec<ValidationError> = Vec::new();\n");
        for (param, rules) in &query.validations {
            emit_param_validation(out, param, rules);
        }
        out.push_str("        if errors.is_empty() {\n");
        out.push_str("            Ok(())\n");
        out.push_str("        } else {\n");
        out.push_str("            Err(errors)\n");
        out.push_str("        }\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    let row_sql = wrap_sql(&query.to_driver_sql());

    let ret_ty = match &query.return_type {
        QueryReturnType::Many(row) => format!("Vec<{}>", util::row_type(catalog, row)),
        QueryReturnType::Single(row) => format!("Option<{}>", util::row_type(catalog, row)),
        QueryReturnType::Exec => "()".to_string(),
    };

    let _ = writeln!(out, "pub async fn {fn_name}(");
    let _ = writeln!(out, "    client: &tokio_postgres::Client,");
    let _ = writeln!(out, "    params: {params_type},");
    let _ = writeln!(out, ") -> Result<{ret_ty}, Box<dyn std::error::Error>> {{");
    out.push_str(
        "    params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;\n",
    );

    // Emit the parameter bindings as owned text values that outlive the
    // borrowed `binds` slice.
    let bind_fields = bound_fields(query);
    for (index, field) in bind_fields.iter().enumerate() {
        let _ = writeln!(out, "    let bind{index} = params.{field}.to_axm_text();");
    }
    out.push_str("    let binds: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![");
    if !bind_fields.is_empty() {
        let refs: Vec<String> = (0..bind_fields.len())
            .map(|i| format!("&bind{i}"))
            .collect();
        out.push_str(&refs.join(", "));
    }
    out.push_str("];\n");

    match &query.return_type {
        QueryReturnType::Many(row) | QueryReturnType::Single(row) => {
            let row_ty = util::row_type(catalog, row);
            let sql_lit = rust_raw_string(&row_sql);
            if matches!(query.return_type, QueryReturnType::Many(_)) {
                let _ = writeln!(out, "    let rows = client.query({sql_lit}, &binds).await?;");
                let _ = writeln!(out, "    let mut out: Vec<{row_ty}> = Vec::with_capacity(rows.len());");
                out.push_str("    for row in rows {\n");
                out.push_str("        let js: String = row.try_get::<_, String>(0)?;\n");
                out.push_str("        let value: serde_json::Value = serde_json::from_str(&js)?;\n");
                let _ = writeln!(out, "        out.push(serde_json::from_value::<{row_ty}>(value)?);");
                out.push_str("    }\n");
                out.push_str("    Ok(out)\n");
            } else {
                let _ = writeln!(out, "    let row = client.query_opt({sql_lit}, &binds).await?;");
                out.push_str("    match row {\n");
                out.push_str("        Some(row) => {\n");
                out.push_str("            let js: String = row.try_get::<_, String>(0)?;\n");
                out.push_str("            let value: serde_json::Value = serde_json::from_str(&js)?;\n");
                let _ = writeln!(out, "            Ok(Some(serde_json::from_value::<{row_ty}>(value)?))");
                out.push_str("        }\n");
                out.push_str("        None => Ok(None),\n");
                out.push_str("    }\n");
            }
        }
        QueryReturnType::Exec => {
            let sql_lit = rust_raw_string(&query.to_driver_sql());
            let _ = writeln!(out, "    client.execute({sql_lit}, &binds).await?;");
            out.push_str("    Ok(())\n");
        }
    }
    out.push_str("}\n\n");
}

/// Wraps a row-shaped query in a `row_to_json` CTE so results can be decoded
/// from a single text column.
fn wrap_sql(sql: &str) -> String {
    let trimmed = sql.trim_end().trim_end_matches(';').trim_end();
    format!(
        "WITH axm_q AS ({trimmed}) SELECT row_to_json(axm_q)::text AS axm_row FROM axm_q"
    )
}

/// Emit the runtime helpers that tokio-postgres query wrappers rely on: a
/// text-format `ToSql` binder that accepts any target column type, and the
/// `AxmToText` coercion trait used to render parameters as Postgres text
/// literals. Kept byte-identical to the helpers emitted by the axm query
/// generator so the output compiles regardless of which query source is used.
fn emit_db_helpers(out: &mut String) {
    out.push_str(
        "// ---------------------------------------------------------------------------\n",
    );
    out.push_str("// tokio-postgres helpers\n");
    out.push_str(
        "// ---------------------------------------------------------------------------\n\n",
    );
    out.push_str("use tokio_postgres::types::{Format, IsNull, ToSql, Type};\n");
    out.push_str("use tokio_postgres::types::private::BytesMut;\n");
    out.push_str("use tokio_postgres::types::to_sql_checked;\n\n");

    out.push_str("#[derive(Debug, Clone)]\n");
    out.push_str("pub struct AxmTextValue {\n");
    out.push_str("    pub value: String,\n");
    out.push_str("    pub null: bool,\n");
    out.push_str("}\n\n");

    out.push_str("impl ToSql for AxmTextValue {\n");
    out.push_str("    fn to_sql(&self, _ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {\n");
    out.push_str("        if self.null {\n");
    out.push_str("            Ok(IsNull::Yes)\n");
    out.push_str("        } else {\n");
    out.push_str("            out.extend_from_slice(self.value.as_bytes());\n");
    out.push_str("            Ok(IsNull::No)\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    fn accepts(_ty: &Type) -> bool {\n");
    out.push_str("        true\n");
    out.push_str("    }\n");
    out.push_str("    fn encode_format(&self, _ty: &Type) -> Format {\n");
    out.push_str("        Format::Text\n");
    out.push_str("    }\n");
    out.push_str("    to_sql_checked!();\n");
    out.push_str("}\n\n");

    out.push_str("pub trait AxmToText {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue;\n");
    out.push_str("}\n\n");
    out.push_str("impl AxmToText for String {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        AxmTextValue { value: self.clone(), null: false }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("impl AxmToText for i64 {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        AxmTextValue { value: self.to_string(), null: false }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("impl AxmToText for f64 {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        AxmTextValue { value: self.to_string(), null: false }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("impl AxmToText for bool {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        AxmTextValue { value: self.to_string(), null: false }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("impl AxmToText for serde_json::Value {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        AxmTextValue { value: self.to_string(), null: false }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    out.push_str("impl<T: AxmToText> AxmToText for Option<T> {\n");
    out.push_str("    fn to_axm_text(&self) -> AxmTextValue {\n");
    out.push_str("        match self {\n");
    out.push_str("            Some(v) => v.to_axm_text(),\n");
    out.push_str("            None => AxmTextValue { value: String::new(), null: true },\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
}

fn emit_param_validation(out: &mut String, param: &str, rules: &[ValidationRule]) {
    let field = util::rust_field_ident(param);
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
/// query SQL. At most the declared parameter count are bound. Named
/// placeholders count as their parameter's declared position.
fn bound_fields(query: &QueryDefinition) -> Vec<String> {
    let max = query.max_placeholder_index().min(query.params.len());
    (0..max)
        .map(|i| util::rust_field_ident(&query.params[i].name))
        .collect()
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
    use crate::catalog::{ColumnSchema, TableSchema};
    use std::borrow::Cow;

    fn no_queries() -> QueryCatalog<'static> {
        QueryCatalog::default()
    }

    fn col(name: &'static str, data_type: &'static str, nullable: bool) -> ColumnSchema<'static> {
        ColumnSchema {
            name: Cow::Borrowed(name),
            data_type: Cow::Borrowed(data_type),
            nullable,
            primary_key: false,
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
            vec![
                col("email", "VARCHAR(255)", false),
                col("id", "BIGSERIAL", false),
            ],
        );
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]"));
        assert!(out.contains("pub struct Users {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub id: i64,"));
    }

    #[test]
    fn emits_trivial_validate() {
        let t = table("users", vec![col("email", "VARCHAR(255)", false)]);
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("pub fn validate(&self) -> Result<(), Vec<ValidationError>>"));
        assert!(out.contains("let _ = self;"));
        assert!(!out.contains("fn is_email"));
        assert!(!out.contains("errors.push"));
    }

    #[test]
    fn nullable_fields_use_option() {
        let t = table("sessions", vec![col("external_id", "UUID", true)]);
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(out.contains("pub external_id: Option<String>,"));
        assert!(!out.contains("if let Some(value)"));
        assert!(!out.contains("fn is_uuid"));
    }

    #[test]
    fn emits_regex_matcher_only_when_query_uses_regex() {
        let plain = table("plain", vec![col("email", "VARCHAR", false)]);
        let out = generate_rust(
            &TableCatalog {
                tables: vec![plain],
            },
            &no_queries(),
        );
        assert!(!out.contains("fn regex_is_match"));

        let q = QueryDefinition {
            name: Cow::Borrowed("find_slug"),
            sql: "SELECT id FROM accounts WHERE slug = $1".to_string(),
            params: vec![crate::query::QueryParam {
                name: Cow::Borrowed("slug"),
                param_type: Cow::Borrowed("String"),
            }],
            return_type: QueryReturnType::Exec,
            validations: [(
                Cow::Borrowed("slug"),
                vec![rule(RuleKind::Regex(Cow::Borrowed("^[a-z0-9-]+$")), None)],
            )]
            .into_iter()
            .collect(),
        };
        let out = generate_rust(&TableCatalog::default(), &QueryCatalog { queries: vec![q] });
        assert!(out.contains("fn regex_is_match"));
        assert!(out.contains("!regex_is_match(\"^[a-z0-9-]+$\", &self.slug)"));
    }

    #[test]
    fn only_emits_used_preset_helpers() {
        let t = table("users", vec![col("email", "VARCHAR", false)]);
        let out = generate_rust(&TableCatalog { tables: vec![t] }, &no_queries());
        assert!(!out.contains("fn is_email"));
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
    fn emits_query_params_struct_and_tokio_postgres_wrapper() {
        let out = generate_rust(&TableCatalog::default(), &query_catalog());
        assert!(out.contains("pub struct GetUserParams {"));
        assert!(out.contains("pub email: String,"));
        assert!(out.contains("pub max_id: i64,"));
        assert!(out.contains("impl GetUserParams {"));
        assert!(out.contains("pub async fn get_user("));
        assert!(out.contains("client: &tokio_postgres::Client,"));
        assert!(out.contains("params: GetUserParams,"));
        assert!(out.contains(") -> Result<Option<Users>, Box<dyn std::error::Error>> {"));
        assert!(out.contains(
            "params.validate().map_err(|errors| format!(\"validation failed: {errors:?}\"))?;"
        ));
        assert!(out.contains("let bind0 = params.email.to_axm_text();"));
        assert!(out.contains("let bind1 = params.max_id.to_axm_text();"));
        assert!(out.contains("let binds: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&bind0, &bind1];"));
        assert!(out.contains("WITH axm_q AS ("));
        assert!(out.contains("row_to_json(axm_q)::text AS axm_row FROM axm_q"));
        assert!(out.contains("query_opt("));
        assert!(out.contains("serde_json::from_value::<Users>(value)?"));
        assert!(!out.contains("sqlx"));
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
        let out = generate_rust(&TableCatalog::default(), &QueryCatalog { queries: vec![q] });
        assert!(out.contains(") -> Result<(), Box<dyn std::error::Error>> {"));
        assert!(out.contains("let bind0 = params.id.to_axm_text();"));
        assert!(out.contains("client.execute("));
        assert!(out.contains("DELETE FROM users WHERE id = $1"));
    }

    #[test]
    fn query_rules_drive_preset_helpers() {
        let out = generate_rust(&TableCatalog::default(), &query_catalog());
        assert!(out.contains("fn is_email(value: &str) -> bool {"));
        assert!(!out.contains("fn is_ipv6"));
    }

    #[test]
    fn named_placeholders_rewrite_to_positional_sql() {
        let q = QueryDefinition {
            name: Cow::Borrowed("delete_user"),
            sql: "DELETE FROM users WHERE id = $user_id".to_string(),
            params: vec![crate::query::QueryParam {
                name: Cow::Borrowed("user_id"),
                param_type: Cow::Borrowed("BigInt"),
            }],
            return_type: QueryReturnType::Exec,
            validations: Default::default(),
        };
        let out = generate_rust(&TableCatalog::default(), &QueryCatalog { queries: vec![q] });
        assert!(
            out.contains("DELETE FROM users WHERE id = $1"),
            "named placeholder must be rewritten to a positional marker:\n{out}"
        );
        assert!(out.contains("let bind0 = params.user_id.to_axm_text();"));
    }

    #[test]
    fn lowercase_return_type_resolves_to_table_type() {
        let t = table("users", vec![col("id", "BIGSERIAL", false)]);
        let q = QueryDefinition {
            name: Cow::Borrowed("get_user"),
            sql: "SELECT id FROM users WHERE id = $1".to_string(),
            params: vec![crate::query::QueryParam {
                name: Cow::Borrowed("id"),
                param_type: Cow::Borrowed("BigInt"),
            }],
            return_type: QueryReturnType::Single(Cow::Borrowed("users")),
            validations: Default::default(),
        };
        let out = generate_rust(
            &TableCatalog { tables: vec![t] },
            &QueryCatalog { queries: vec![q] },
        );
        assert!(
            out.contains("-> Result<Option<Users>, Box<dyn std::error::Error>>"),
            "return type must normalize to the table's PascalCase type:\n{out}"
        );
        assert!(
            out.contains("serde_json::from_value::<Users>(value)"),
            "row type must be canonical:\n{out}"
        );
    }

    #[test]
    fn rust_raw_string_escapes_terminator() {
        assert_eq!(rust_raw_string("a\"#b"), "r##\"a\"#b\"##");
        assert_eq!(rust_raw_string("plain"), "r#\"plain\"#");
    }
}
