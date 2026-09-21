//! AST-based canonical formatter for `.axm` model files.
//!
//! The formatter parses the file with the real `.axm` parser and re-prints the
//! AST, so it cannot mangle syntax it does not understand. Invalid input is
//! returned as an error and left untouched by callers.

use axiom_core::axm::ast::{
    AnnotatedType, FieldDecl, ImportStmt, ImportedName, Literal, ModelDecl, ModelOverride,
    ParamDecl, QueryDecl, QueryReturn, Rule, Transform, TransactionDecl, TypeRef,
};
use axiom_core::axm::parser::parse_axm_file;

use crate::printer::{Lines, MAX_INLINE_WIDTH, fits_inline, indent};

/// Format a `.axm` source file to canonical form.
///
/// Returns an `Err(message)` when the source does not parse.
pub fn format_axm(src: &str) -> Result<String, String> {
    let file = parse_axm_file(src).map_err(|e| e.to_string())?;
    let mut lines = Lines::new();

    let mut first_item = true;
    for item in file
        .imports
        .iter()
        .map(|i| vec![format_import(i)])
        .chain(file.models.iter().map(format_model))
        .chain(file.queries.iter().map(format_query))
        .chain(file.transactions.iter().map(format_transaction))
    {
        if !first_item {
            lines.push_blank();
        }
        for line in item {
            lines.push(line);
        }
        first_item = false;
    }

    Ok(lines.finish())
}

fn format_import(import: &ImportStmt) -> String {
    format!(
        "import {{ {} }} from \"{}\"",
        import
            .names
            .iter()
            .map(format_imported_name)
            .collect::<Vec<_>>()
            .join(", "),
        import.source
    )
}

fn format_imported_name(name: &ImportedName) -> String {
    match &name.alias {
        Some(alias) => format!("{} as {alias}", name.name),
        None => name.name.clone(),
    }
}

/// Render a full top-level model including any `@...` decorators (e.g.
/// `@target(...)`, `@no_codegen`, `@parse`, `@safeParse(...)`) and the
/// `extends select<...>` source.
fn format_model(model: &ModelDecl) -> Vec<String> {
    let mut out: Vec<String> = model.overrides.iter().map(format_override).collect();
    if let Some(ann) = &model.alias {
        out.push(format!("model {} = {};", model.name, format_annotated(ann)));
        return out;
    }
    let source = model
        .source
        .as_ref()
        .map(|s| format!(" extends select<{}>", s.relation))
        .unwrap_or_default();
    out.push(format!("model {}{source} {{", model.name));
    for field in &model.fields {
        out.push(format!("{}{}", indent(1), format_field(field)));
    }
    out.push("}".to_string());
    out
}

fn format_override(override_: &ModelOverride) -> String {
    match override_ {
        ModelOverride::Target(targets) => format!(
            "@target({})",
            targets
                .iter()
                .map(|t| format!("\"{}\"", t.name()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ModelOverride::NoCodegen => "@no_codegen".to_string(),
        ModelOverride::Parse => "@parse".to_string(),
        ModelOverride::SafeParse(mode) => format!("@safeParse(\"{}\")", mode.name()),
    }
}

/// Render a full query declaration.
fn format_query(query: &QueryDecl) -> Vec<String> {
    let params = query
        .params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match &query.return_type {
        QueryReturn::Exec => String::new(),
        QueryReturn::Single(ty) => format!(" -> {}", format_type(ty)),
        QueryReturn::Optional(ty) => format!(" -> {}?", format_type(ty)),
        QueryReturn::Many(ty) => format!(" -> {}[]", format_type(ty)),
    };
    let mut out: Vec<String> = query.overrides.iter().map(format_override).collect();
    out.push(format!("query {}({params}){ret} {{", query.name));
    // SQL bodies are stored verbatim by the parser (only the outer edges are
    // trimmed), so each line may carry indentation from the source. Dedent the
    // common continuation prefix first, then indent uniformly — otherwise every
    // format pass re-indents an already-indented body and the output drifts.
    for body_line in dedent_lines(&query.sql) {
        out.push(format!("{}{body_line}", indent(1)));
    }
    out.push("}".to_string());
    out
}

/// Remove the common leading whitespace shared by the continuation lines of a
/// body, preserving relative indentation (e.g. inside `$$...$$` literals).
///
/// The parser stores the SQL body verbatim, trimming only the outer edges, so
/// the first line is already flush but continuation lines keep their source
/// indent. Canonical output prefixes every line with one indent level, so the
/// minimum continuation indent in a formatted body is always non-zero; measuring
/// the min over continuation lines (skipping the first) makes formatting a
/// fixed point instead of shifting every line by +1 level on each pass.
fn dedent_lines(body: &str) -> Vec<String> {
    let lines: Vec<&str> = body.lines().collect();
    let min_indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);
    lines
        .into_iter()
        .map(|l| {
            // Only strip whitespace, never content. The first line is
            // pre-trimmed by the parser, so slicing off `min_indent` characters
            // there would eat part of the statement.
            let leading = l.chars().take_while(|c| c.is_whitespace()).count();
            l.chars().skip(leading.min(min_indent)).collect()
        })
        .collect()
}

/// Render a full transaction declaration. Mirrors [`format_query`] but emits
/// the `transaction` keyword; the SQL body, params, return type, and `@`
/// decorators are formatted identically.
fn format_transaction(transaction: &TransactionDecl) -> Vec<String> {
    let params = transaction
        .params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match &transaction.return_type {
        QueryReturn::Exec => String::new(),
        QueryReturn::Single(ty) => format!(" -> {}", format_type(ty)),
        QueryReturn::Optional(ty) => format!(" -> {}?", format_type(ty)),
        QueryReturn::Many(ty) => format!(" -> {}[]", format_type(ty)),
    };
    let mut out: Vec<String> = transaction.overrides.iter().map(format_override).collect();
    out.push(format!("transaction {}({params}){ret} {{", transaction.name));
    for body_line in dedent_lines(&transaction.sql) {
        out.push(format!("{}{body_line}", indent(1)));
    }
    out.push("}".to_string());
    out
}

fn format_param(param: &ParamDecl) -> String {
    format!("${}: {}", param.name, format_type(&param.ty))
}

/// Render a field, breaking long rule chains onto continuation lines.
fn format_field(field: &FieldDecl) -> String {
    let base = format!("{}: {}", field_name(field), format_type(&field.ty.base));
    let calls = format_calls(&field.ty);
    let inline_full = format!("{}{}", base, calls.join(""));
    let inline = match &field.default {
        Some(lit) => format!("{inline_full} = {}", format_literal(lit)),
        None => inline_full.clone(),
    };

    if calls.len() <= 3 && fits_inline(&inline, MAX_INLINE_WIDTH) {
        return inline;
    }

    // Break the chain: one rule per continuation line, indented one level past
    // the field. The default (if any) rides on the final continuation line.
    let mut out = base;
    for (i, call) in calls.iter().enumerate() {
        out.push('\n');
        let last = i == calls.len() - 1;
        match &field.default {
            Some(lit) if last => {
                out.push_str(&format!("{}{call} = {}", indent(2), format_literal(lit)));
            }
            _ => {
                out.push_str(&format!("{}{call}", indent(2)));
            }
        }
    }
    out
}

fn field_name(field: &FieldDecl) -> String {
    let base = if is_ident(&field.name) {
        field.name.clone()
    } else {
        format!("\"{}\"", escape_string(&field.name))
    };
    if field.optional {
        format!("{base}?")
    } else {
        base
    }
}

/// Whether `s` is a valid bare `.axm` identifier (matching the parser's
/// `ident` rule). Field names that are not — e.g. `"first-name"` — are
/// formatted with quotes so the output still parses.
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `.transform().rule()` chain, transformations first (matching codegen's
/// transform-before-validation order), each call fully rendered.
fn format_calls(ty: &AnnotatedType) -> Vec<String> {
    let mut calls = Vec::new();
    for t in &ty.transforms {
        calls.push(format_transform(t));
    }
    for r in &ty.rules {
        calls.push(format_rule(r));
    }
    calls
}

/// The base type with its method chain, e.g. `String.email().min(3)`.
fn format_annotated(ty: &AnnotatedType) -> String {
    let calls = format_calls(ty);
    format!("{}{}", format_type(&ty.base), calls.join(""))
}

fn format_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::String => "String".to_string(),
        TypeRef::Int => "Int".to_string(),
        TypeRef::BigInt => "BigInt".to_string(),
        TypeRef::Float => "Float".to_string(),
        TypeRef::Boolean => "Boolean".to_string(),
        TypeRef::Uuid => "UUID".to_string(),
        TypeRef::Date => "Date".to_string(),
        TypeRef::DateTime => "DateTime".to_string(),
        TypeRef::Json => "Json".to_string(),
        TypeRef::Bytes => "Bytes".to_string(),
        TypeRef::Named(name) => name.clone(),
        TypeRef::Array(inner) => format!("{}[]", format_type(inner)),
        TypeRef::Nullable(inner) => format!("{}?", format_type(inner)),
    }
}

fn format_transform(transform: &Transform) -> String {
    match transform {
        Transform::Trim => ".trim()".to_string(),
        Transform::Lowercase => ".lowercase()".to_string(),
        Transform::Uppercase => ".uppercase()".to_string(),
    }
}

fn format_rule(rule: &Rule) -> String {
    match rule {
        Rule::Min(n, m) => format_message_call("min", &[n.to_string()], m),
        Rule::Max(n, m) => format_message_call("max", &[n.to_string()], m),
        Rule::MinLength(n, m) => format_message_call("min_length", &[n.to_string()], m),
        Rule::MaxLength(n, m) => format_message_call("max_length", &[n.to_string()], m),
        Rule::Regex(pattern, m) => format_message_call(
            "regex",
            &[format_literal(&Literal::String(pattern.clone()))],
            m,
        ),
        Rule::Email(m) => format_message_call("email", &[], m),
        Rule::Url(m) => format_message_call("url", &[], m),
        Rule::Uuid(m) => format_message_call("uuid", &[], m),
        Rule::Ulid(m) => format_message_call("ulid", &[], m),
        Rule::Ipv4(m) => format_message_call("ipv4", &[], m),
        Rule::Ipv6(m) => format_message_call("ipv6", &[], m),
        Rule::IsoDate(m) => format_message_call("isodate", &[], m),
        Rule::Alphanumeric(m) => format_message_call("alphanumeric", &[], m),
        Rule::NonEmpty(m) => format_message_call("nonempty", &[], m),
    }
}

/// Render `.name(args, "msg")` from a rule's argument list and optional message.
fn format_message_call(name: &str, args: &[String], message: &Option<String>) -> String {
    let head = args.join(", ");
    match message {
        Some(m) => format!(
            ".{name}({}{}{})",
            head,
            if head.is_empty() { "" } else { ", " },
            format_literal(&Literal::String(m.clone()))
        ),
        None if head.is_empty() => format!(".{name}()"),
        None => format!(".{name}({head})"),
    }
}

fn format_literal(literal: &Literal) -> String {
    match literal {
        Literal::String(s) => format!("\"{}\"", escape_string(s)),
        Literal::Int(n) => n.to_string(),
        Literal::Float(f) => format_float(*f),
        Literal::Bool(b) => b.to_string(),
    }
}

/// Render a float keeping a fractional marker so it round-trips as `float`.
fn format_float(f: f64) -> String {
    let mut text = f.to_string();
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        text.push_str(".0");
    }
    text
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_axm(src).expect("formats cleanly")
    }

    #[test]
    fn formats_simple_model_with_import() {
        let src = r#"
import { Address } from   "address";
model User {
      name : String
      age: Int .min(18)
}
"#;
        assert_eq!(
            fmt(src),
            "import { Address } from \"address\"\n\
             \nmodel User {\n\
             \x20 name: String\n\
             \x20 age: Int.min(18)\n\
             }\n"
        );
    }

    #[test]
    fn formats_extends_select_and_aliased_imports() {
        let src = r#"import { User as DbUser } from "users"
model UserView extends select<public.users> {
  id: UUID
  email: Email
}"#;
        assert_eq!(
            fmt(src),
            "import { User as DbUser } from \"users\"\n\
             \nmodel UserView extends select<public.users> {\n\
             \x20 id: UUID\n\
             \x20 email: Email\n\
             }\n"
        );
    }

    #[test]
    fn breaks_long_rule_chains() {
        let src =
            "model User {\n  username: String .alphanumeric() .min(3) .max(20) .nonempty()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  username: String\n    .alphanumeric()\n    .min(3)\n    .max(20)\n    .nonempty()\n}\n"
        );
    }

    #[test]
    fn keeps_short_chains_inline() {
        let src = "model User {\n  email: String .trim() .lowercase() .email()\n}";
        assert_eq!(
            fmt(src),
            "model User {\n  email: String.trim().lowercase().email()\n}\n"
        );
    }

    #[test]
    fn formats_defaults_and_optional_fields() {
        let src = "model User { country : String = \"US\"\n  age?: Int.min(18) }";
        assert_eq!(
            fmt(src),
            "model User {\n  country: String = \"US\"\n  age?: Int.min(18)\n}\n"
        );
    }

    #[test]
    fn formats_type_aliases() {
        let src = "model Email = String.email();\nmodel UserId = BigInt\nmodel User {\n  email: Email\n  id: UserId\n}";
        let out = fmt(src);
        assert_eq!(
            out,
            "model Email = String.email();\n\
             \nmodel UserId = BigInt;\n\
             \nmodel User {\n\
             \x20 email: Email\n\
             \x20 id: UserId\n\
             }\n"
        );
    }

    #[test]
    fn formats_model_overrides() {
        let src =
            "@target(\"typescript\", \"rust\")\nmodel User {\n  id: UUID\n}\n@no_codegen\nmodel Secret {\n  name: String\n}";
        let out = fmt(src);
        assert!(
            out.contains("@target(\"typescript\", \"rust\")\nmodel User {"),
            "{out}"
        );
        assert!(out.contains("@no_codegen\nmodel Secret {"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn single_quote_targets_normalize_to_double_quotes() {
        let src = "@target('rust', 'typescript')\nmodel M { a: String }";
        let out = fmt(src);
        assert!(
            out.contains("@target(\"rust\", \"typescript\")\nmodel M {"),
            "{out}"
        );
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn formats_parse_and_safe_parse_overrides() {
        let src = "@parse\n@safeParse('first')\nmodel User {\n  id: UUID\n}";
        let out = fmt(src);
        assert!(out.contains("@parse\n@safeParse(\"first\")\nmodel User {"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");

        let src = "@safeParse(all)\nmodel M { a: String }";
        let out = fmt(src);
        assert!(out.contains("@safeParse(\"all\")\nmodel M {"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn transforms_precede_validations() {
        let src = "model User {\n  email: String .email() .trim() .lowercase()\n}";
        let out = fmt(src);
        assert!(
            out.contains("email: String.trim().lowercase().email()"),
            "{out}"
        );
    }

    #[test]
    fn formats_query_declarations() {
        let src = r#"query GetUser($id: UUID) -> User? {
  SELECT * FROM users WHERE id = $id;
}
query CreateUser($input: NewUser) {
  INSERT INTO users (email) VALUES ($input.email);
}"#;
        let out = fmt(src);
        assert!(out.contains("query GetUser($id: UUID) -> User? {"), "{out}");
        assert!(out.contains("query CreateUser($input: NewUser) {"), "{out}");
    }

    #[test]
    fn escaping_round_trips_string_literals() {
        let src = r#"model User { slug: String .regex("^[a-z0-9-\"\\n]+$") }"#;
        let out = fmt(src);
        assert!(out.contains(".regex(\"^[a-z0-9-\\\"\\\\n]+$\")"), "{out}");
    }

    #[test]
    fn floats_keep_fractional_marker() {
        let src = "model M { ratio: Float = 0.5\n  whole: Float = 2.0 }";
        let out = fmt(src);
        assert!(out.contains("whole: Float = 2.0"), "{out}");
    }

    #[test]
    fn rules_with_messages_round_trip() {
        let src = "model User {\n  username: String .nonempty(\"required\") .min_length(3, \"too short\")\n}";
        let out = fmt(src);
        assert!(out.contains(".nonempty(\"required\")"), "{out}");
        assert!(out.contains(".min_length(3, \"too short\")"), "{out}");
    }

     #[test]
    fn formats_transaction_declarations() {
        let src = r#"transaction Transfer($from: UUID, $to: UUID, $amount: Int) -> User {
  UPDATE accounts SET balance = balance - $amount WHERE id = $from;
  UPDATE accounts SET balance = balance + $amount WHERE id = $to;
  SELECT * FROM accounts WHERE id = $from;
}"#;
        let out = fmt(src);
        assert!(out.contains("transaction Transfer($from: UUID, $to: UUID, $amount: Int) -> User {"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn formats_transaction_decorators_and_return_shapes() {
        let src = r#"@target("typescript")
transaction CreatePost($userId: UUID) -> Post {
  INSERT INTO posts (user_id) VALUES ($userId);
  SELECT * FROM posts WHERE user_id = $userId;
}"#;
        let out = fmt(src);
        assert!(out.contains("@target(\"typescript\")\ntransaction CreatePost("), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");

        let src = r#"transaction T($id: UUID) {
  UPDATE a SET x = 1 WHERE id = $id;
  UPDATE b SET y = 2 WHERE id = $id;
}"#;
        let out = fmt(src);
        assert!(out.contains("transaction T($id: UUID) {"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");

        let src = r#"transaction Many($id: UUID) -> Post[] {
  INSERT INTO posts (user_id) VALUES ($id);
  SELECT * FROM posts WHERE user_id = $id;
}"#;
        let out = fmt(src);
        assert!(out.contains("-> Post[]"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn formats_transaction_multiline_body_without_drift() {
        let src = r#"transaction Transfer($from: UUID, $to: UUID) -> User {
  UPDATE accounts
    SET balance = balance - 1
    WHERE id = $from;
  UPDATE accounts
    SET balance = balance + 1
    WHERE id = $to;
  SELECT * FROM accounts WHERE id = $from;
}"#;
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice, "formatting must be idempotent");
        assert!(once.contains("  UPDATE accounts\n    SET balance = balance - 1\n    WHERE id = $from;"), "{once}");
    }

    #[test]
    fn output_is_idempotent() {
        let messy = r#"
import {A,B} from "geo";
model Email = String .email() .max_length(320)
model User {
  email : String .trim() .email()
  address: Address[]
  tags: String[] .nonempty()
  username: String .alphanumeric() .min(3) .max(20) .lowercase()
  created: DateTime = "2024-01-01T00:00:00Z"
  private: Boolean
  bio: String?
}
query ListUsers($limit: Int) -> User[] {
  SELECT * FROM users ORDER BY id LIMIT $limit;
}
"#;
        let once = fmt(messy);
        assert_eq!(once, fmt(&once), "formatting must be idempotent");
    }

    #[test]
    fn empty_models_format_to_brace_pair() {
        assert_eq!(fmt("model Empty { }"), "model Empty {\n}\n");
    }

    #[test]
    fn quoted_field_names_round_trip() {
        // Non-identifier names keep their quotes; bare identifiers drop them.
        let src = r#"model T {
  "first-name": String
  "x:y": String
  id: String
}"#;
        let out = fmt(src);
        assert!(out.contains("\"first-name\": String"), "{out}");
        assert!(out.contains("\"x:y\": String"), "{out}");
        assert!(out.contains("id: String"), "{out}");
        assert_eq!(out, fmt(&out), "formatting must be idempotent");
    }

    #[test]
    fn invalid_axm_is_reported() {
        let err = format_axm("model {").expect_err("should fail to parse");
        assert!(!err.is_empty());
    }

    #[test]
    fn multiline_sql_bodies_do_not_drift() {
        let src = r#"query Stats($limit: Int) {
  SELECT id, name
    FROM users
    WHERE active = true
    ORDER BY id
    LIMIT $limit;
}
query InsertAdmin($email: String) {
  INSERT INTO users (email, body)
  VALUES ($email, $$
    <div>
      <p>admin</p>
    </div>
  $$);
}"#;
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice, "formatting must be idempotent");
        assert_eq!(twice, fmt(&twice), "must not drift on repeated passes");
        assert!(once.contains("  SELECT id, name\n  FROM users\n"), "{once}");
        assert!(once.contains("      <p>admin</p>\n"), "{once}");
    }
}
