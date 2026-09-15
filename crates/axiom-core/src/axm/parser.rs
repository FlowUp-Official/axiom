//! `.axm` language parser built on `winnow`.
//!
//! The grammar is deliberately small and keyword-scoped so it can grow without
//! redesigning the AST:
//!
//! ```text
//! file           := item*
//! item           := import | type_decl | model_decl | query_decl
//! import         := "import" "{" name ("," name)* "}" "from" string ";"
//! name           := ident ("as" ident)?
//! type_decl      := "type" ident "=" annotated_type ";"
//! model          := "model" ident ("extends" "select<" db_ident ">")? "{" field* "}"
//! field          := ident "?"? ":" annotated_type ("=" literal)?
//! annotated_type := type_ref call*
//! call           := "." ident "(" args? ")"
//! query          := "query" ident "(" param* ")" ("->" type_ref)? "{" sql_body "}"
//! param          := "$"? ident ":" type_ref
//! type_ref       := base ("?" | "[]")*
//! base           := "String" | "Int" | "BigInt" | "Float" | "Boolean"
//!                  | "UUID" | "Date" | "DateTime" | "Json" | "Bytes" | ident
//! ```
//!
//! Rule and transformation calls are classified into strongly typed AST
//! variants (`Rule` / `Transform`) at parse time rather than being stored as
//! raw strings. Rule names are lowercase; the aliases `min_len` / `max_len`
//! are accepted for `min_length` / `max_length`. Axiom identifiers are never
//! case-canonicalized here.

use std::fmt;

use winnow::Parser;
use winnow::combinator::{alt, delimited, not, opt, peek, repeat, separated, terminated};
use winnow::error::{ContextError, ErrMode, FromExternalError};
use winnow::token::{any, one_of, take_while};

/// Parser result type for this module. Modal (`ErrMode`) so that semantic
/// errors (unknown rules, bad arguments) can be raised as [`ErrMode::Cut`] and
/// propagate through `repeat` combinators instead of being treated as "no more
/// items".
type PResult<T> = winnow::ModalResult<T, ContextError>;

use crate::axm::ast::{
    AnnotatedType, AxmFile, FieldDecl, ImportStmt, ImportedName, Literal, ModelDecl, ModelSource,
    ParamDecl, QueryDecl, QueryReturn, Rule, Transform, TypeDecl, TypeRef,
};

/// A failed `.axm` parse with a human-readable message.
#[derive(Debug)]
pub struct AxmParseError {
    pub message: String,
}

impl fmt::Display for AxmParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AxmParseError {}

/// Error produced when a rule/transform call is unknown or given a bad
/// argument. Carried through `winnow`'s `try_map`.
#[derive(Debug)]
pub struct RuleError(pub String);

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RuleError {}

/// A classified rule/transform call.
enum Call {
    Transform(Transform),
    Rule(Rule),
}

/// Parse a full `.axm` source file.
pub fn parse_axm_file(input: &str) -> Result<AxmFile, AxmParseError> {
    let mut rest = input;
    let file = axm_file.parse_next(&mut rest).map_err(|e| AxmParseError {
        message: render_parse_error(&e),
    })?;
    if !rest.trim().is_empty() {
        return Err(AxmParseError {
            message: format!("unexpected trailing input: `{}`", rest.trim()),
        });
    }
    Ok(file)
}

/// Render a modal parse error as a single, human-readable line. Semantic
/// failures (unknown rules, bad arguments) surface their message directly;
/// everything else falls back to the standard context error text.
fn render_parse_error(e: &ErrMode<ContextError>) -> String {
    match e {
        ErrMode::Cut(c) | ErrMode::Backtrack(c) => c
            .cause()
            .map(|cause| cause.to_string())
            .unwrap_or_else(|| c.to_string()),
        ErrMode::Incomplete(_) => e.to_string(),
    }
}

/// Skip whitespace and `// ...` line comments.
fn ws(input: &mut &str) -> PResult<()> {
    loop {
        let rest = input.trim_start();
        *input = rest;
        if let Some(after) = rest.strip_prefix("//") {
            let end = after.find('\n').unwrap_or(after.len());
            *input = &rest[2 + end..];
            continue;
        }
        return Ok(());
    }
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn ident(input: &mut &str) -> PResult<String> {
    take_while(1.., is_word_char)
        .verify(|s: &str| {
            s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        })
        .map(|s: &str| s.to_string())
        .parse_next(input)
}

/// Match `word` only when it is not immediately followed by a word character,
/// so `model` does not match the prefix of `modelX`.
fn kw<'a>(word: &'static str) -> impl Parser<&'a str, (), ErrMode<ContextError>> {
    (word, peek(not(one_of(is_word_char)))).value(())
}

/// A primitive type keyword, e.g. `String`, `UUID`. Capitalization is
/// significant: `string`, `uuid` are ordinary identifiers, not primitives,
/// and are rejected at the semantic layer.
fn primitive<'a>(
    word: &'static str,
    ty: TypeRef,
) -> impl Parser<&'a str, TypeRef, ErrMode<ContextError>> {
    (word, peek(not(one_of(is_word_char)))).value(ty)
}

/// A database identifier inside `select<...>`, which may be schema-qualified
/// (`public.users`) and follows the SQL dialect's identifier charset.
fn db_ident(input: &mut &str) -> PResult<String> {
    take_while(1.., |c: char| {
        c.is_ascii_alphanumeric() || c == '_' || c == '.'
    })
    .map(|s: &str| s.to_string())
    .parse_next(input)
}

fn string_literal(input: &mut &str) -> PResult<String> {
    delimited('"', escaped_string, '"').parse_next(input)
}

fn escaped_string(input: &mut &str) -> PResult<String> {
    let mut out = String::new();
    loop {
        let chunk = take_while(0.., |c: char| c != '"' && c != '\\').parse_next(input)?;
        out.push_str(chunk);
        if opt('\\').parse_next(input)?.is_some() {
            let escaped = any.parse_next(input)?;
            out.push(match escaped {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
        } else {
            break;
        }
    }
    Ok(out)
}

fn number_literal(input: &mut &str) -> PResult<Literal> {
    take_while(1.., |c: char| c.is_ascii_digit())
        .parse_next(input)
        .and_then(|integer: &str| {
            let frac =
                opt(('.', take_while(1.., |c: char| c.is_ascii_digit()))).parse_next(input)?;
            match frac {
                Some((_, frac)) => {
                    let text = format!("{integer}.{frac}");
                    Ok(Literal::Float(text.parse().unwrap_or(0.0)))
                }
                None => Ok(Literal::Int(integer.parse().unwrap_or(0))),
            }
        })
}

fn literal(input: &mut &str) -> PResult<Literal> {
    alt((
        string_literal.map(Literal::String),
        kw("true").value(Literal::Bool(true)),
        kw("false").value(Literal::Bool(false)),
        number_literal,
    ))
    .parse_next(input)
}

fn type_ref(input: &mut &str) -> PResult<TypeRef> {
    let base = alt((
        primitive("String", TypeRef::String),
        primitive("Int", TypeRef::Int),
        primitive("BigInt", TypeRef::BigInt),
        primitive("Float", TypeRef::Float),
        primitive("Boolean", TypeRef::Boolean),
        primitive("UUID", TypeRef::Uuid),
        primitive("Date", TypeRef::Date),
        primitive("DateTime", TypeRef::DateTime),
        primitive("Json", TypeRef::Json),
        primitive("Bytes", TypeRef::Bytes),
        ident.map(TypeRef::Named),
    ))
    .parse_next(input)?;

    let mut ty = base;
    loop {
        if opt('?').parse_next(input)?.is_some() {
            ty = TypeRef::Nullable(Box::new(ty));
        } else if opt("[]").parse_next(input)?.is_some() {
            ty = TypeRef::Array(Box::new(ty));
        } else {
            break;
        }
    }
    Ok(ty)
}

/// Classify a rule/transform call with its argument list (0, 1, or 2 literals;
/// the final literal is a custom message where a rule accepts one). Returns an
/// error for unknown calls or calls with the wrong argument shape.
fn classify_call(name: &str, args: &[Literal]) -> Result<Call, RuleError> {
    let no_arg = |rule_name: &str| -> Result<Option<String>, RuleError> {
        match args.len() {
            0 => Ok(None),
            1 => match &args[0] {
                Literal::String(msg) => Ok(Some(msg.clone())),
                other => rule_err(format!(
                    "`.{name}()` on `{rule_name}` takes no argument other than an optional message (got `{other:?}`)"
                )),
            },
            _ => rule_err(format!("`.{name}()` takes at most one (message) argument")),
        }
    };

    let int_arg = |is_len: bool| -> Result<(Option<i64>, Option<String>), RuleError> {
        match args.len() {
            0 => rule_err(format!(
                "`.{name}()` requires an integer argument{}",
                if is_len {
                    " (e.g. `.min_length(3)`) or a string literal"
                } else {
                    ""
                }
            )),
            1 => match (&args[0], is_len) {
                (Literal::Int(n), _) => Ok((Some(*n), None)),
                (Literal::String(_), true) => rule_err(format!(
                    "`.{name}()` requires an integer argument; a message alone must come after the value: `.min_length(3, \"...\")`"
                )),
                (other, _) => rule_err(format!(
                    "`.{name}()` expects an integer argument (got `{other:?}`)"
                )),
            },
            2 => match (&args[0], &args[1]) {
                (Literal::Int(n), Literal::String(msg)) => Ok((Some(*n), Some(msg.clone()))),
                (Literal::Int(n), other) => rule_err(format!(
                    "`.{name}()` second (message) argument must be a string (got `{other:?}`) for value `{n}`"
                )),
                (other, _) => rule_err(format!(
                    "`.{name}()` first argument must be an integer (got `{other:?}`)"
                )),
            },
            _ => rule_err(format!("`.{name}()` takes at most two arguments")),
        }
    };

    let string_arg = || -> Result<(Option<String>, Option<String>), RuleError> {
        match args.len() {
            0 => rule_err(format!("`.{name}()` requires a string argument")),
            1 => match &args[0] {
                Literal::String(s) => Ok((Some(s.clone()), None)),
                other => rule_err(format!(
                    "`.{name}()` expects a string argument (got `{other:?}`)"
                )),
            },
            2 => match (&args[0], &args[1]) {
                (Literal::String(s), Literal::String(msg)) => {
                    Ok((Some(s.clone()), Some(msg.clone())))
                }
                (Literal::String(s), other) => rule_err(format!(
                    "`.{name}()` second (message) argument must be a string (got `{other:?}`) for pattern `{s}`"
                )),
                (other, _) => rule_err(format!(
                    "`.{name}()` first argument must be a string (got `{other:?}`)"
                )),
            },
            _ => rule_err(format!("`.{name}()` takes at most two arguments")),
        }
    };

    match name {
        "trim" => {
            no_arg("trim")?;
            Ok(Call::Transform(Transform::Trim))
        }
        "lowercase" => {
            no_arg("lowercase")?;
            Ok(Call::Transform(Transform::Lowercase))
        }
        "uppercase" => {
            no_arg("uppercase")?;
            Ok(Call::Transform(Transform::Uppercase))
        }
        "email" => {
            let m = no_arg("email")?;
            Ok(Call::Rule(Rule::Email(m)))
        }
        "url" => {
            let m = no_arg("url")?;
            Ok(Call::Rule(Rule::Url(m)))
        }
        "uuid" => {
            let m = no_arg("uuid")?;
            Ok(Call::Rule(Rule::Uuid(m)))
        }
        "ulid" => {
            let m = no_arg("ulid")?;
            Ok(Call::Rule(Rule::Ulid(m)))
        }
        "ipv4" => {
            let m = no_arg("ipv4")?;
            Ok(Call::Rule(Rule::Ipv4(m)))
        }
        "ipv6" => {
            let m = no_arg("ipv6")?;
            Ok(Call::Rule(Rule::Ipv6(m)))
        }
        "isodate" => {
            let m = no_arg("isodate")?;
            Ok(Call::Rule(Rule::IsoDate(m)))
        }
        "alphanumeric" => {
            let m = no_arg("alphanumeric")?;
            Ok(Call::Rule(Rule::Alphanumeric(m)))
        }
        "nonempty" => {
            let m = no_arg("nonempty")?;
            Ok(Call::Rule(Rule::NonEmpty(m)))
        }
        "min" => {
            let (v, m) = int_arg(false)?;
            Ok(Call::Rule(Rule::Min(v.unwrap_or(0), m)))
        }
        "max" => {
            let (v, m) = int_arg(false)?;
            Ok(Call::Rule(Rule::Max(v.unwrap_or(0), m)))
        }
        "min_length" | "min_len" => {
            let (v, m) = int_arg(true)?;
            Ok(Call::Rule(Rule::MinLength(
                v.unwrap_or(0).max(0) as usize,
                m,
            )))
        }
        "max_length" | "max_len" => {
            let (v, m) = int_arg(true)?;
            Ok(Call::Rule(Rule::MaxLength(
                v.unwrap_or(0).max(0) as usize,
                m,
            )))
        }
        "regex" => {
            let (v, m) = string_arg()?;
            Ok(Call::Rule(Rule::Regex(v.unwrap_or_default(), m)))
        }
        _ => Err(RuleError(format!("unknown rule `.{}()`", name))),
    }
}

/// A generic `Err(RuleError(..))` helper so calls inside the (non-generic)
/// closures above can return any `Result` payload.
fn rule_err<O>(msg: String) -> Result<O, RuleError> {
    Err(RuleError(msg))
}

/// Parse the chained calls of an annotated type, e.g. `.trim().email()`.
fn annotated_type(input: &mut &str) -> PResult<AnnotatedType> {
    let base = type_ref.parse_next(input)?;
    let mut transforms = Vec::new();
    let mut rules = Vec::new();
    loop {
        ws(input)?;
        if opt('.').parse_next(input)?.is_none() {
            break;
        }
        let name = ident.parse_next(input)?;
        let args: Vec<Literal> = opt(delimited(
            ('(', ws).map(|(c, _): (char, ())| c),
            separated(0..=2, literal, (ws, ',', ws)),
            (ws, ')').map(|(_, c): ((), char)| c),
        ))
        .parse_next(input)?
        .unwrap_or_default();
        let call = classify_call(&name, &args)
            .map_err(|e| ErrMode::Cut(ContextError::from_external_error(input, e)))?;
        match call {
            Call::Transform(t) => transforms.push(t),
            Call::Rule(r) => rules.push(r),
        }
    }
    Ok(AnnotatedType {
        base,
        transforms,
        rules,
    })
}

fn field_decl(input: &mut &str) -> PResult<FieldDecl> {
    ws(input)?;
    let name = ident.parse_next(input)?;
    let optional = opt('?').parse_next(input)?.is_some();
    ws(input)?;
    ':'.parse_next(input)?;
    ws(input)?;
    let ty = annotated_type.parse_next(input)?;

    let default = opt((ws, '=', ws, literal))
        .parse_next(input)?
        .map(|(_, _, _, lit)| lit);

    Ok(FieldDecl {
        name,
        optional,
        ty,
        default,
    })
}

fn import_stmt(input: &mut &str) -> PResult<ImportStmt> {
    kw("import").parse_next(input)?;
    ws(input)?;
    let names: Vec<ImportedName> = delimited(
        ('{', ws).map(|(c, _): (char, ())| c),
        separated(0.., imported_name, (ws, ',')),
        (ws, '}').map(|(_, c): ((), char)| c),
    )
    .parse_next(input)?;
    ws(input)?;
    kw("from").parse_next(input)?;
    ws(input)?;
    let source = string_literal.parse_next(input)?;
    opt(';').parse_next(input)?;
    Ok(ImportStmt { names, source })
}

fn imported_name(input: &mut &str) -> PResult<ImportedName> {
    ws(input)?;
    let name = ident.parse_next(input)?;
    let alias = opt((ws, kw("as"), ws, ident))
        .parse_next(input)?
        .map(|(_, _, _, a)| a);
    Ok(ImportedName { name, alias })
}

fn type_decl(input: &mut &str) -> PResult<TypeDecl> {
    kw("type").parse_next(input)?;
    ws(input)?;
    let name = ident.parse_next(input)?;
    ws(input)?;
    '='.parse_next(input)?;
    ws(input)?;
    let ty = annotated_type.parse_next(input)?;
    opt(';').parse_next(input)?;
    Ok(TypeDecl { name, ty })
}

fn model_decl(input: &mut &str) -> PResult<ModelDecl> {
    kw("model").parse_next(input)?;
    ws(input)?;
    let name = ident.parse_next(input)?;

    let source = opt((ws, kw("extends"), ws, kw("select"), '<', db_ident, '>'))
        .parse_next(input)?
        .map(|(_, _, _, _, _, relation, _)| ModelSource { relation });

    ws(input)?;
    let fields = delimited(
        ('{', ws).map(|(c, _): (char, ())| c),
        repeat(
            0..,
            terminated(
                field_decl,
                (ws, opt(',')).map(|(_, c): ((), Option<char>)| c),
            ),
        ),
        (ws, '}').map(|(_, c): ((), char)| c),
    )
    .parse_next(input)?;

    Ok(ModelDecl {
        name,
        source,
        fields,
    })
}

fn param_decl(input: &mut &str) -> PResult<ParamDecl> {
    ws(input)?;
    let _dollar = opt('$').parse_next(input)?;
    let name = ident.parse_next(input)?;
    ws(input)?;
    ':'.parse_next(input)?;
    ws(input)?;
    let ty = type_ref.parse_next(input)?;
    Ok(ParamDecl { name, ty })
}

fn query_decl(input: &mut &str) -> PResult<QueryDecl> {
    kw("query").parse_next(input)?;
    ws(input)?;
    let name = ident.parse_next(input)?;
    ws(input)?;
    let params: Vec<ParamDecl> = delimited(
        ('(', ws).map(|(c, _): (char, ())| c),
        separated(0.., param_decl, (ws, ',')),
        (ws, ')').map(|(_, c): ((), char)| c),
    )
    .parse_next(input)?;

    let return_type = opt((ws, "->", ws, type_ref))
        .parse_next(input)?
        .map(|(_, _, _, ty)| ty);

    ws(input)?;
    let sql = scan_sql_body.parse_next(input)?;

    let return_type = match return_type {
        None => QueryReturn::Exec,
        Some(ty) => match ty {
            TypeRef::Array(inner) => QueryReturn::Many(*inner),
            TypeRef::Nullable(inner) => QueryReturn::Optional(*inner),
            other => QueryReturn::Single(other),
        },
    };

    Ok(QueryDecl {
        name,
        params,
        return_type,
        sql,
    })
}

/// Scan a raw SQL body between braces. Handles single/double-quoted strings,
/// dollar-quoted strings, `--` and `/* */` comments, and nested braces. The
/// opening `{` is consumed; the body is returned with trailing whitespace
/// trimmed.
fn scan_sql_body(input: &mut &str) -> PResult<String> {
    let src = *input;
    let bytes = src.as_bytes();
    let n = bytes.len();

    let mut i = 0usize;
    while i < n && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    if i >= n || bytes[i] != b'{' {
        return Err(ErrMode::Backtrack(ContextError::from_external_error(
            input,
            RuleError("expected `{` to open the SQL body".into()),
        )));
    }
    i += 1;
    let body_start = i;
    let mut depth = 1i32;

    while i < n {
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        if i + 1 < n && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        if i + 1 < n && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'$' => {
                // Dollar-quoted string: `$[tag]$ ... $[tag]$`. A placeholder
                // (`$id`, `$1`) is not followed by `$`, so it is plain text.
                let mut j = i + 1;
                while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j < n && bytes[j] == b'$' {
                    let tag = &src[i..=j];
                    match src[j + 1..].find(tag) {
                        Some(off) => i = j + 1 + off + tag.len(),
                        None => i = n,
                    }
                } else {
                    i += 1;
                }
            }
            b'-' if i + 1 < n && bytes[i + 1] == b'-' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => match src[i + 2..].find("*/") {
                Some(off) => i = i + 2 + off + 2,
                None => i = n,
            },
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    let body = src[body_start..i - 1].trim().to_string();
                    *input = &src[i..];
                    return Ok(body);
                }
            }
            _ => i += 1,
        }
    }

    Err(ErrMode::Cut(ContextError::from_external_error(
        input,
        RuleError("unterminated SQL body: missing closing `}`".into()),
    )))
}

enum Item {
    Import(ImportStmt),
    Type(TypeDecl),
    Model(ModelDecl),
    Query(QueryDecl),
}

fn axm_file(input: &mut &str) -> PResult<AxmFile> {
    ws(input)?;
    let items: Vec<Item> = repeat(
        0..,
        terminated(
            alt((
                import_stmt.map(Item::Import),
                type_decl.map(Item::Type),
                model_decl.map(Item::Model),
                query_decl.map(Item::Query),
            )),
            ws,
        ),
    )
    .parse_next(input)?;

    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut models = Vec::new();
    let mut queries = Vec::new();
    for item in items {
        match item {
            Item::Import(i) => imports.push(i),
            Item::Type(t) => types.push(t),
            Item::Model(m) => models.push(m),
            Item::Query(q) => queries.push(q),
        }
    }
    Ok(AxmFile {
        imports,
        types,
        models,
        queries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> AxmFile {
        parse_axm_file(src).expect("parse succeeds")
    }

    fn parse_err(src: &str) -> String {
        parse_axm_file(src).expect_err("parse fails").message
    }

    #[test]
    fn parses_model_with_extends_select() {
        let file = parse(
            r#"
model User extends select<users> {
    email: String.email(),
    username: Username,
    displayName: String.min_length(1).max_length(100)
}
"#,
        );
        let model = &file.models[0];
        assert_eq!(model.name, "User");
        assert_eq!(
            model.source,
            Some(ModelSource {
                relation: "users".into()
            })
        );
        assert_eq!(model.fields.len(), 3);
        assert_eq!(model.fields[0].name, "email");
        assert_eq!(model.fields[0].ty.base, TypeRef::String);
        assert_eq!(model.fields[0].ty.rules.len(), 1);
        assert_eq!(model.fields[2].ty.rules.len(), 2);
    }

    #[test]
    fn parses_schema_qualified_select_source() {
        let file = parse("model User extends select<public.users> { id: UUID }");
        assert_eq!(
            file.models[0].source,
            Some(ModelSource {
                relation: "public.users".into()
            })
        );
    }

    #[test]
    fn parses_bare_model_without_source() {
        let file = parse("model User { name: String }");
        assert_eq!(file.models[0].source, None);
    }

    #[test]
    fn parses_capitalized_primitives() {
        let file = parse(
            r#"model T {
  a: String
  b: Int
  c: BigInt
  d: Float
  e: Boolean
  f: UUID
  g: Date
  h: DateTime
  i: Json
  j: Bytes
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(fields[0].ty.base, TypeRef::String);
        assert_eq!(fields[1].ty.base, TypeRef::Int);
        assert_eq!(fields[2].ty.base, TypeRef::BigInt);
        assert_eq!(fields[3].ty.base, TypeRef::Float);
        assert_eq!(fields[4].ty.base, TypeRef::Boolean);
        assert_eq!(fields[5].ty.base, TypeRef::Uuid);
        assert_eq!(fields[6].ty.base, TypeRef::Date);
        assert_eq!(fields[7].ty.base, TypeRef::DateTime);
        assert_eq!(fields[8].ty.base, TypeRef::Json);
        assert_eq!(fields[9].ty.base, TypeRef::Bytes);
    }

    #[test]
    fn lowercase_primitives_are_named_not_primitive() {
        let file = parse("model T {\n  a: string\n  b: uuid\n  c: int\n  d: boolean\n}");
        let lower = ["string", "uuid", "int", "boolean"];
        for (field, expected) in file.models[0].fields.iter().zip(lower) {
            assert_eq!(
                field.ty.base,
                TypeRef::Named(expected.into()),
                "lowercase primitive must parse as a Named identifier"
            );
        }
    }

    #[test]
    fn parses_nullable_and_array_combinations() {
        let file = parse(
            r#"model T {
  a: String?
  b: User[]
  c: User?[]
  d: User[]?
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(
            fields[0].ty.base,
            TypeRef::Nullable(Box::new(TypeRef::String))
        );
        assert_eq!(
            fields[1].ty.base,
            TypeRef::Array(Box::new(TypeRef::Named("User".into())))
        );
        assert_eq!(
            fields[2].ty.base,
            TypeRef::Array(Box::new(TypeRef::Nullable(Box::new(TypeRef::Named(
                "User".into()
            )))))
        );
        assert_eq!(
            fields[3].ty.base,
            TypeRef::Nullable(Box::new(TypeRef::Array(Box::new(TypeRef::Named(
                "User".into()
            )))))
        );
    }

    #[test]
    fn parses_type_alias_declaration() {
        let file = parse(
            r#"
type Email = String.email().max_length(320);
type Username = String
    .min_length(3)
    .max_length(32);
"#,
        );
        assert_eq!(file.types.len(), 2);
        assert_eq!(file.types[0].name, "Email");
        assert_eq!(file.types[0].ty.base, TypeRef::String);
        assert_eq!(file.types[0].ty.rules.len(), 2);
        assert_eq!(file.types[1].name, "Username");
        assert_eq!(file.types[1].ty.rules.len(), 2);
        assert_eq!(file.types[1].ty.rules[0], Rule::MinLength(3, None));
    }

    #[test]
    fn rules_accept_aliases_and_messages() {
        let file = parse(
            r#"model T {
  a: String.email("Invalid email address")
  b: String.min_len(3, "Too short")
  c: String.regex("^[a-z]+$", "lowercase only")
  d: Int.min(0)
  e: String.min_length(3)
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(
            fields[0].ty.rules,
            vec![Rule::Email(Some("Invalid email address".into()))]
        );
        assert_eq!(
            fields[1].ty.rules,
            vec![Rule::MinLength(3, Some("Too short".into()))]
        );
        assert_eq!(
            fields[2].ty.rules,
            vec![Rule::Regex(
                "^[a-z]+$".into(),
                Some("lowercase only".into())
            )]
        );
        assert_eq!(fields[3].ty.rules, vec![Rule::Min(0, None)]);
        assert_eq!(fields[4].ty.rules, vec![Rule::MinLength(3, None)]);
    }

    #[test]
    fn new_validation_rules_are_recognized() {
        let file = parse(
            r#"model T {
  a: String.ulid()
  b: String.ipv4()
  c: String.ipv6()
  d: String.isodate()
  e: String.alphanumeric()
  f: String.nonempty()
  g: String.url()
  h: String.uuid()
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(fields[0].ty.rules, vec![Rule::Ulid(None)]);
        assert_eq!(fields[1].ty.rules, vec![Rule::Ipv4(None)]);
        assert_eq!(fields[2].ty.rules, vec![Rule::Ipv6(None)]);
        assert_eq!(fields[3].ty.rules, vec![Rule::IsoDate(None)]);
        assert_eq!(fields[4].ty.rules, vec![Rule::Alphanumeric(None)]);
        assert_eq!(fields[5].ty.rules, vec![Rule::NonEmpty(None)]);
        assert_eq!(fields[6].ty.rules, vec![Rule::Url(None)]);
        assert_eq!(fields[7].ty.rules, vec![Rule::Uuid(None)]);
    }

    #[test]
    fn parses_aliased_import() {
        let file = parse(
            r#"import { User as DbUser, Email } from "./users.axm";
model T { owner: DbUser }"#,
        );
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].names[0].name, "User");
        assert_eq!(file.imports[0].names[0].alias, Some("DbUser".into()));
        assert_eq!(file.imports[0].names[1].alias, None);
        assert_eq!(file.imports[0].source, "./users.axm");
    }

    #[test]
    fn parses_query_declarations() {
        let file = parse(
            r#"
query GetUser($id: UUID) -> User? {
    SELECT id, email
    FROM users
    WHERE id = $id;
}

query DeleteUser($id: UUID) {
    DELETE FROM users
    WHERE id = $id;
}

query ListUsers($limit: Int) -> User[] {
    SELECT *
    FROM users
    ORDER BY id
    LIMIT $limit;
}

query CreateUser($id: BigInt, $tags: String[]) -> User {
    INSERT INTO users (id) VALUES ($id);
}
"#,
        );
        assert_eq!(file.queries.len(), 4);
        let get = &file.queries[0];
        assert_eq!(get.name, "GetUser");
        assert_eq!(get.params.len(), 1);
        assert_eq!(get.params[0].name, "id");
        assert_eq!(get.params[0].ty, TypeRef::Uuid);
        assert!(
            matches!(get.return_type, QueryReturn::Optional(TypeRef::Named(ref n)) if n == "User")
        );
        assert!(get.sql.contains("SELECT id, email"));

        let del = &file.queries[1];
        assert_eq!(del.return_type, QueryReturn::Exec);

        let list = &file.queries[2];
        assert!(
            matches!(list.return_type, QueryReturn::Many(TypeRef::Named(ref n)) if n == "User")
        );
        assert_eq!(
            list.sql,
            "SELECT *\n    FROM users\n    ORDER BY id\n    LIMIT $limit;"
        );

        let create = &file.queries[3];
        assert_eq!(
            create.params[1].ty,
            TypeRef::Array(Box::new(TypeRef::String))
        );
        assert!(matches!(create.return_type, QueryReturn::Single(_)));
    }

    #[test]
    fn query_body_respects_strings_and_comments() {
        let file = parse(
            r#"query Q() {
    SELECT '{' AS brace, "}col{" AS col
    -- an } inside a comment
    FROM t
    WHERE x = $$ { not a brace $$
    AND y = '}';
}"#,
        );
        let body = &file.queries[0].sql;
        assert!(body.contains("'{'"));
        assert!(body.contains("\"}col{\""));
        assert!(body.contains("an } inside a comment"));
        assert!(body.contains("$$ { not a brace $$"));
    }

    #[test]
    fn query_body_supports_structured_params() {
        let file = parse(
            r#"query CreateUser($input: CreateUserInput) -> User {
    INSERT INTO users (email, username, display_name)
    VALUES ($input.email, $input.username, $input.displayName)
    RETURNING id, email, username, display_name, created_at;
}"#,
        );
        assert_eq!(file.queries[0].params[0].name, "input");
        assert_eq!(
            file.queries[0].params[0].ty,
            TypeRef::Named("CreateUserInput".into())
        );
        assert!(file.queries[0].sql.contains("$input.email"));
    }

    #[test]
    fn missing_query_body_braces_is_an_error() {
        let err = parse_err("query Q() -> User { SELECT 1");
        assert!(err.contains("closing `}`"), "{err}");
    }

    #[test]
    fn rejects_unknown_rules() {
        let err = parse_err("model User {\n  x: String.banana()\n}");
        assert!(err.contains("banana"), "{}", err);
    }

    #[test]
    fn rejects_missing_arguments() {
        let err = parse_err("model User {\n  x: String.min()\n}");
        assert!(err.contains("requires an integer"), "{}", err);

        let err = parse_err("model User {\n  x: String.email(1)\n}");
        assert!(err.contains("takes no argument"), "{}", err);
    }

    #[test]
    fn rejects_bad_optional_marker() {
        // `?` in the middle is not valid: it must follow the name or the type.
        assert!(parse_axm_file("model User { a: ? String }").is_err());
    }

    #[test]
    fn parses_optional_and_nullable_fields() {
        // `name?` marks the field optional (may be absent); a `?` on the type
        // makes it nullable (must be present, but may be null).
        let file = parse("model User {\n  a?: Int\n  b: String?\n}");
        let fields = &file.models[0].fields;
        assert!(fields[0].optional);
        assert_eq!(fields[0].ty.base, TypeRef::Int);
        assert!(!fields[1].optional);
        assert_eq!(
            fields[1].ty.base,
            TypeRef::Nullable(Box::new(TypeRef::String))
        );
    }

    #[test]
    fn parses_field_defaults() {
        let file = parse(
            r#"model User {
  country: String = "US"
  quota: BigInt = 42
  ratio: Float = 0.5
  enabled: Boolean = true
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(fields[0].default, Some(Literal::String("US".into())));
        assert_eq!(fields[1].default, Some(Literal::Int(42)));
        assert_eq!(fields[2].default, Some(Literal::Float(0.5)));
        assert_eq!(fields[3].default, Some(Literal::Bool(true)));
    }

    #[test]
    fn comments_are_skipped() {
        let file = parse(
            r#"
// This is an Axiom comment
type Email = String.email(); // trailing comment
// another
model User extends select<users> {
    email: Email,
}
"#,
        );
        assert_eq!(file.types.len(), 1);
        assert!(file.models[0].source.is_some());
    }

    #[test]
    fn keywords_require_word_boundaries() {
        let file = parse("model Modeled { x: Int }");
        assert_eq!(file.models[0].name, "Modeled");
        let file = parse("model Select { x: Int }");
        assert_eq!(file.models[0].name, "Select");
    }

    #[test]
    fn combined_file_parses() {
        let file = parse(
            r#"
type Email = String.email().max_length(320);

model User extends select<users> {
    email: Email,
    username: String.min_length(3).max_length(32)
}

query GetUser($id: UUID) -> User? {
    SELECT id, email, username, created_at FROM users WHERE id = $id;
}

query ListUsers($limit: Int) -> User[] {
    SELECT id, email, username FROM users ORDER BY created_at DESC LIMIT $limit;
}
"#,
        );
        assert_eq!(file.types.len(), 1);
        assert_eq!(file.models.len(), 1);
        assert_eq!(file.queries.len(), 2);
        assert_eq!(
            file.declarations().collect::<Vec<_>>(),
            vec!["Email", "User", "GetUser", "ListUsers"]
        );
    }

    #[test]
    fn rejects_empty_model_name_and_trailing_junk() {
        assert!(parse_axm_file("model { }").is_err());
        assert!(parse_axm_file("model User { } $$$").is_err());
    }
}
