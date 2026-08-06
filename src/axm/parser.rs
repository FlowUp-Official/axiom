//! `.axm` DSL parser built on `winnow`.
//!
//! The grammar is deliberately small and keyword-scoped so it can grow
//! (unions, nullable, literals, etc.) without redesigning the AST:
//!
//! ```text
//! file      := (import | model)*
//! import    := "import" "{" ident ("," ident)* "}" "from" string ";"
//! model     := "export"? "model" ident "{" field* "}"
//! field     := ident "?"? ":" type (call)* ("=" literal)?
//! call      := "." ident "(" literal? ")"
//! type      := base ("[" "]")*
//! base      := "string" | "int" | "float" | "boolean" | "json"
//!            | "timestamp" | ident
//! ```
//!
//! Rule and transformation calls are classified into strongly typed AST
//! variants (`Rule` / `Transform`) at parse time rather than being stored as
//! raw strings.

use std::fmt;

use winnow::ascii::multispace0;
use winnow::combinator::{alt, delimited, not, opt, peek, repeat, separated, terminated};
use winnow::error::{ContextError, ErrMode, FromExternalError};
use winnow::token::{any, one_of, take_while};
use winnow::Parser;

/// Parser result type for this module. Modal (`ErrMode`) so that semantic
/// errors (unknown rules, bad arguments) can be raised as [`ErrMode::Cut`] and
/// propagate through `repeat` combinators instead of being treated as "no more
/// items".
type PResult<T> = winnow::ModalResult<T, ContextError>;

use crate::axm::ast::{
    AxmFile, FieldDecl, ImportStmt, Literal, ModelDecl, Rule, Transform, TypeRef,
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
    let file = axm_file
        .parse_next(&mut rest)
        .map_err(|e| AxmParseError {
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

fn ws(input: &mut &str) -> PResult<()> {
    multispace0.map(|_| ()).parse_next(input)
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
/// so `export` does not match the prefix of `exported`.
fn kw<'a>(word: &'static str) -> impl Parser<&'a str, (), ErrMode<ContextError>> {
    (word, peek(not(one_of(is_word_char)))).value(())
}

/// A primitive type keyword, e.g. `string`, `int`.
fn primitive<'a>(
    word: &'static str,
    ty: TypeRef,
) -> impl Parser<&'a str, TypeRef, ErrMode<ContextError>> {
    (word, peek(not(one_of(is_word_char)))).value(ty)
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
    let integer = take_while(1.., |c: char| c.is_ascii_digit()).parse_next(input)?;
    let frac = opt(('.', take_while(1.., |c: char| c.is_ascii_digit())))
        .parse_next(input)?;
    match frac {
        Some((_, frac)) => {
            let text = format!("{integer}.{frac}");
            Ok(Literal::Float(text.parse().unwrap_or(0.0)))
        }
        None => Ok(Literal::Int(integer.parse().unwrap_or(0))),
    }
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
        primitive("string", TypeRef::String),
        primitive("int", TypeRef::Int),
        primitive("float", TypeRef::Float),
        primitive("boolean", TypeRef::Boolean),
        primitive("json", TypeRef::Json),
        primitive("timestamp", TypeRef::Timestamp),
        ident.map(TypeRef::Named),
    ))
    .parse_next(input)?;

    let mut ty = base;
    while opt("[]").parse_next(input)?.is_some() {
        ty = TypeRef::Array(Box::new(ty));
    }
    Ok(ty)
}

fn classify_call(name: &str, arg: Option<Literal>) -> Result<Call, RuleError> {
    let no_arg = |expected: &str| match &arg {
        Some(_) => Err(RuleError(format!(
            "`.{name}()` takes no arguments (it is a `{expected}`)"
        ))),
        None => Ok(()),
    };
    let int_arg = |name: &str| match &arg {
        Some(Literal::Int(n)) => Ok(*n),
        Some(_) => Err(RuleError(format!("`.{name}()` expects an integer argument"))),
        None => Err(RuleError(format!("`.{name}()` requires an integer argument"))),
    };
    let usize_arg = |name: &str| int_arg(name).map(|n| n.max(0) as usize);
    let string_arg = |name: &str| match &arg {
        Some(Literal::String(s)) => Ok(s.clone()),
        Some(_) => Err(RuleError(format!("`.{name}()` expects a string argument"))),
        None => Err(RuleError(format!("`.{name}()` requires a string argument"))),
    };

    match name {
        "trim" => {
            no_arg("transformation")?;
            Ok(Call::Transform(Transform::Trim))
        }
        "lowercase" => {
            no_arg("transformation")?;
            Ok(Call::Transform(Transform::Lowercase))
        }
        "uppercase" => {
            no_arg("transformation")?;
            Ok(Call::Transform(Transform::Uppercase))
        }
        "email" => {
            no_arg("validation")?;
            Ok(Call::Rule(Rule::Email))
        }
        "url" => {
            no_arg("validation")?;
            Ok(Call::Rule(Rule::Url))
        }
        "uuid" => {
            no_arg("validation")?;
            Ok(Call::Rule(Rule::Uuid))
        }
        "alphanumeric" => {
            no_arg("validation")?;
            Ok(Call::Rule(Rule::Alphanumeric))
        }
        "nonempty" => {
            no_arg("validation")?;
            Ok(Call::Rule(Rule::NonEmpty))
        }
        "min" => Ok(Call::Rule(Rule::Min(int_arg("min")?))),
        "max" => Ok(Call::Rule(Rule::Max(int_arg("max")?))),
        "min_len" => Ok(Call::Rule(Rule::MinLen(usize_arg("min_len")?))),
        "max_len" => Ok(Call::Rule(Rule::MaxLen(usize_arg("max_len")?))),
        "regex" => Ok(Call::Rule(Rule::Regex(string_arg("regex")?))),
        _ => Err(RuleError(format!("unknown rule `.{name}()`"))),
    }
}

fn field_decl(input: &mut &str) -> PResult<FieldDecl> {
    let name = ident.parse_next(input)?;
    let optional = opt('?').parse_next(input)?.is_some();
    ws(input)?;
    ':'.parse_next(input)?;
    ws(input)?;
    let ty = type_ref.parse_next(input)?;

    let mut transformations = Vec::new();
    let mut validations = Vec::new();
    loop {
        ws(input)?;
        if opt('.').parse_next(input)?.is_none() {
            break;
        }
        let name = ident.parse_next(input)?;
        let arg = delimited('(', opt(literal), ')').parse_next(input)?;
        let call = classify_call(&name, arg).map_err(|e| {
            ErrMode::Cut(ContextError::from_external_error(input, e))
        })?;
        match call {
            Call::Transform(t) => transformations.push(t),
            Call::Rule(r) => validations.push(r),
        }
    }

    let default = opt((ws, '=', ws, literal))
        .parse_next(input)?
        .map(|(_, _, _, lit)| lit);

    Ok(FieldDecl {
        name,
        ty,
        optional,
        default,
        transformations,
        validations,
    })
}

fn import_stmt(input: &mut &str) -> PResult<ImportStmt> {
    kw("import").parse_next(input)?;
    ws(input)?;
    let names = delimited(
        ('{', ws).map(|(c, _): (char, ())| c),
        separated(0.., ident, (ws, ',', ws).map(|(_, c, _): ((), char, ())| c)),
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

fn model_decl(input: &mut &str) -> PResult<ModelDecl> {
    let exported = opt(kw("export")).parse_next(input)?.is_some();
    ws(input)?;
    kw("model").parse_next(input)?;
    ws(input)?;
    let name = ident.parse_next(input)?;
    ws(input)?;
    let fields = delimited(
        ('{', ws).map(|(c, _): (char, ())| c),
        repeat(0.., terminated(field_decl, ws)),
        '}',
    )
    .parse_next(input)?;
    Ok(ModelDecl {
        exported,
        name,
        fields,
    })
}

enum Item {
    Import(ImportStmt),
    Model(ModelDecl),
}

fn axm_file(input: &mut &str) -> PResult<AxmFile> {
    ws(input)?;
    let items: Vec<Item> = repeat(
        0..,
        terminated(
            alt((import_stmt.map(Item::Import), model_decl.map(Item::Model))),
            ws,
        ),
    )
    .parse_next(input)?;

    let mut imports = Vec::new();
    let mut models = Vec::new();
    for item in items {
        match item {
            Item::Import(i) => imports.push(i),
            Item::Model(m) => models.push(m),
        }
    }
    Ok(AxmFile { imports, models })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> AxmFile {
        parse_axm_file(src).expect("parse succeeds")
    }

    #[test]
    fn parses_exported_and_private_models() {
        let file = parse(
            r#"
export model Address { }

model User { }
"#,
        );
        assert_eq!(file.models.len(), 2);
        assert_eq!(file.models[0].name, "Address");
        assert!(file.models[0].exported);
        assert_eq!(file.models[1].name, "User");
        assert!(!file.models[1].exported);
    }

    #[test]
    fn parses_import_statement() {
        let file = parse(r#"import { Address } from "address"
model User { }"#);
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].names, vec!["Address"]);
        assert_eq!(file.imports[0].source, "address");
    }

    #[test]
    fn parses_multi_name_import() {
        let file = parse(r#"import { Address, ZipCode } from "geo";
model User { }"#);
        assert_eq!(file.imports[0].names, vec!["Address", "ZipCode"]);
    }

    #[test]
    fn parses_field_types_and_arrays_recursively() {
        let file = parse(
            r#"model User {
  name: string
  age: int
  score: float
  active: boolean
  metadata: json
  created_at: timestamp
  address: Address
  history: Address[]
  tags: string[]
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0].ty, TypeRef::String);
        assert_eq!(fields[1].ty, TypeRef::Int);
        assert_eq!(fields[2].ty, TypeRef::Float);
        assert_eq!(fields[3].ty, TypeRef::Boolean);
        assert_eq!(fields[4].ty, TypeRef::Json);
        assert_eq!(fields[5].ty, TypeRef::Timestamp);
        assert_eq!(fields[6].ty, TypeRef::Named("Address".to_string()));
        assert_eq!(
            fields[7].ty,
            TypeRef::Array(Box::new(TypeRef::Named("Address".to_string())))
        );
        assert_eq!(
            fields[8].ty,
            TypeRef::Array(Box::new(TypeRef::String))
        );
    }

    #[test]
    fn parses_typed_rules_and_transforms() {
        let file = parse(
            r#"model User {
  email: string .trim() .lowercase() .email()
  age: int .min(18) .max(120)
  slug: string .regex("^[a-z0-9-]+$") .min_len(3)
  code: string .uuid() .max_len(36)
  username: string .nonempty() .alphanumeric()
}"#,
        );
        let email = &file.models[0].fields[0];
        assert_eq!(
            email.transformations,
            vec![Transform::Trim, Transform::Lowercase]
        );
        assert_eq!(email.validations, vec![Rule::Email]);

        let age = &file.models[0].fields[1];
        assert!(age.transformations.is_empty());
        assert_eq!(age.validations, vec![Rule::Min(18), Rule::Max(120)]);

        let slug = &file.models[0].fields[2];
        assert_eq!(
            slug.validations,
            vec![
                Rule::Regex("^[a-z0-9-]+$".to_string()),
                Rule::MinLen(3)
            ]
        );

        let code = &file.models[0].fields[3];
        assert_eq!(code.validations, vec![Rule::Uuid, Rule::MaxLen(36)]);

        let username = &file.models[0].fields[4];
        assert_eq!(
            username.validations,
            vec![Rule::NonEmpty, Rule::Alphanumeric]
        );
    }

    #[test]
    fn parses_optional_fields_and_defaults() {
        let file = parse(
            r#"model User {
  age?: int .min(18)
  country: string = "US"
  region?: string = "na"
  enabled: boolean = true
  quota: int = 42
  ratio: float = 0.5
}"#,
        );
        let fields = &file.models[0].fields;
        assert!(fields[0].optional);
        assert!(fields[1].default.is_some());
        assert_eq!(fields[1].default, Some(Literal::String("US".to_string())));
        assert!(fields[2].optional);
        assert_eq!(fields[2].default, Some(Literal::String("na".to_string())));
        assert_eq!(fields[3].default, Some(Literal::Bool(true)));
        assert_eq!(fields[4].default, Some(Literal::Int(42)));
        assert_eq!(fields[5].default, Some(Literal::Float(0.5)));
    }

    #[test]
    fn default_does_not_replace_null_semantics_at_parse_time() {
        let file = parse(
            r#"model User {
  country: string = "US"
}"#,
        );
        let field = &file.models[0].fields[0];
        assert_eq!(field.default, Some(Literal::String("US".to_string())));
        assert!(!field.optional);
    }

    #[test]
    fn rejects_unknown_rules() {
        let err = parse_axm_file("model User {\n  x: string .banana()\n}").expect_err("unknown rule");
        assert!(err.message.contains("banana"), "{}", err.message);
    }

    #[test]
    fn rejects_missing_arguments() {
        let err = parse_axm_file("model User {\n  x: int .min()\n}").expect_err("missing arg");
        assert!(err.message.contains("requires an integer"), "{}", err.message);

        let err = parse_axm_file("model User {\n  x: string .email(1)\n}").expect_err("bad arg");
        assert!(err.message.contains("takes no arguments"), "{}", err.message);
    }

    #[test]
    fn rejects_empty_model_name_and_trailing_junk() {
        assert!(parse_axm_file("model { }").is_err());
        assert!(parse_axm_file("model User { } $$$").is_err());
    }

    #[test]
    fn keywords_require_word_boundaries() {
        let file = parse("model Exported { }");
        assert_eq!(file.models[0].name, "Exported");
        // `string` used as a model name must not be parsed as the primitive.
        let file = parse("model string { x: int }");
        assert_eq!(file.models[0].name, "string");
    }
}
