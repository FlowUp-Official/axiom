//! `.axm` language parser built on `winnow`.
//!
//! The grammar is deliberately small and keyword-scoped so it can grow without
//! redesigning the AST:
//!
//! ```text
//! file           := item*
//! item           := override* (model | query | transaction) | import | type_decl
//! import         := "import" "{" name ("," name)* "}" "from" string [";"]
//! name           := ident ("as" ident)?
//! type_decl      := "type" ident "=" annotated_type [";"]
//! model          := "model" ident ("extends" "select<" db_ident ">")? "{" field* "}"
//! override       := "@" ident ("(" word ("," word)* ")")?
//! word           := string | ident
//! field          := (ident | string) "?"? ":" annotated_type ("=" literal)?
//! annotated_type := type_ref call*
//! call           := "." ident "(" args? ")"
//! query          := "query" ident "(" param* ")" ("->" type_ref)? "{" sql_body "}"
//! transaction    := "transaction" ident "(" param* ")" ("->" type_ref)? "{" sql_body "}"
//! param          := "$"? ident ":" type_ref
//! type_ref       := base ("?" | "[]")*
//! base           := "String" | "Int" | "BigInt" | "Float" | "Boolean"
//!                  | "UUID" | "Date" | "DateTime" | "Json" | "Bytes" | ident
//! ```
//!
//! Semicolons are **optional** on `import` and `type` declarations. A `;`
//! after the closing `}` of a `model`, `query`, or `transaction` block is never
//! accepted. Inside `query {}` and `transaction {}` blocks, multiple SQL
//! statements are separated by `;` (the trailing `;` on the last statement is
//! optional). The statement-count rules are enforced by `axiom check`: a
//! `query` body holds exactly one statement, a `transaction` body two or more.
//!
//! Rule and transformation calls are classified into strongly typed AST
//! variants (`Rule` / `Transform`) at parse time rather than being stored as
//! raw strings. Rule names are lowercase and canonical: `min_length` /
//! `max_length` (the `min_len` / `max_len` aliases are rejected). Axiom
//! identifiers are never case-canonicalized here.
//!
//! Model, query, and transaction decorators: a decoration-eligible declaration
//! may be preceded by `@target(...)` (restrict codegen to the listed targets,
//! quoted or bare words), which applies to all three. `@no_codegen` (never emit
//! a standalone validation API) applies to `model` declarations only and is
//! rejected on `query`, `transaction`, and `type` declarations. The same is
//! true of the model-only `@parse` and `@safeParse(...)` decorators. `@target`
//! and `@no_codegen` are mutually exclusive on a model, each decorator may
//! appear at most once per declaration, and an unknown decorator name or target
//! is rejected. `@target` accepts either quote style: `@target("typescript",
//! "rust")` and `@target('rust', 'typescript')`.

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
    AnnotatedType, AxmFile, FieldDecl, ImportStmt, ImportedName, Literal, ModelDecl, ModelOverride,
    ModelSource, ParamDecl, QueryDecl, QueryReturn, Rule, SafeParseMode, Target, TransactionDecl,
    Transform, TypeDecl, TypeRef,
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
        .verify(|s: &str| crate::axm::is_identifier(s))
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
        "min_length" => {
            let (v, m) = int_arg(true)?;
            Ok(Call::Rule(Rule::MinLength(
                v.unwrap_or(0).max(0) as usize,
                m,
            )))
        }
        "max_length" => {
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
    let name = alt((ident, string_literal)).parse_next(input)?;
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
        overrides: Vec::new(),
    })
}

/// A `@...` decorator that precedes a `model` or `query` declaration. Fails
/// (backtracks) when the input does not begin with `@`, so
/// `repeat(0.., model_override)` can collect any number of decorators and stop
/// cleanly at the first item that is not one.
fn model_override(input: &mut &str) -> PResult<ModelOverride> {
    (ws, '@').parse_next(input)?;
    let name = ident.parse_next(input)?;
    match name.as_str() {
        "target" => {
            let Some(words): Option<Vec<String>> = opt(delimited(
                ('(', ws).map(|(c, _): (char, ())| c),
                separated(1.., target_word, (ws, ',')),
                (ws, ')').map(|(_, c): ((), char)| c),
            ))
            .parse_next(input)?
            else {
                return Err(ErrMode::Cut(ContextError::from_external_error(
                    input,
                    RuleError(
                        "`@target` requires a target list, e.g. `@target(\"typescript\", \"rust\")`"
                            .into(),
                    ),
                )));
            };
            let mut targets = Vec::with_capacity(words.len());
            for word in words {
                let Some(target) = Target::parse(&word) else {
                    return Err(ErrMode::Cut(ContextError::from_external_error(
                        input,
                        RuleError(format!(
                            "unknown codegen target `{word}` in `@target` (expected `typescript` or `rust`)"
                        )),
                    )));
                };
                targets.push(target);
            }
            Ok(ModelOverride::Target(targets))
        }
        "no_codegen" => {
            if opt('(').parse_next(input)?.is_some() {
                return Err(ErrMode::Cut(ContextError::from_external_error(
                    input,
                    RuleError("`@no_codegen` takes no arguments".into()),
                )));
            }
            Ok(ModelOverride::NoCodegen)
        }
        "parse" => {
            if opt('(').parse_next(input)?.is_some() {
                return Err(ErrMode::Cut(ContextError::from_external_error(
                    input,
                    RuleError("`@parse` takes no arguments".into()),
                )));
            }
            Ok(ModelOverride::Parse)
        }
        "safeParse" => {
            let Some((_ws, word)): Option<((), String)> = opt(delimited(
                ('(', ws).map(|(c, _): (char, ())| c),
                (ws, alt((quoted_word, ident))),
                (ws, ')').map(|(_, c): ((), char)| c),
            ))
            .parse_next(input)?
            else {
                return Err(ErrMode::Cut(ContextError::from_external_error(
                    input,
                    RuleError(
                        "`@safeParse` requires one argument: `@safeParse(\"first\")` or `@safeParse(\"all\")`"
                            .into(),
                    ),
                )));
            };
            let Some(mode) = SafeParseMode::parse(&word) else {
                return Err(ErrMode::Cut(ContextError::from_external_error(
                    input,
                    RuleError(format!(
                        "unknown safeParse mode `{word}` (expected `first` or `all`)"
                    )),
                )));
            };
            Ok(ModelOverride::SafeParse(mode))
        }
        other => Err(ErrMode::Cut(ContextError::from_external_error(
            input,
            RuleError(format!("unknown model override `@{other}`")),
        ))),
    }
}

/// A target name inside `@target(...)`: a quoted string (either quote style)
/// or a bare identifier. `separated` consumes whitespace up to the comma only,
/// so skip any leading whitespace here.
fn target_word(input: &mut &str) -> PResult<String> {
    let (_, word) = (ws, alt((quoted_word, ident))).parse_next(input)?;
    Ok(word)
}

/// A string literal delimited by either `'` or `"` (the rest of the grammar
/// only accepts double quotes; `@target` accepts both).
fn quoted_word(input: &mut &str) -> PResult<String> {
    let mut quote = alt(('\'', '"')).parse_next(input)?;
    let mut out = String::new();
    loop {
        let chunk = take_while(0.., |c: char| c != quote && c != '\\').parse_next(input)?;
        out.push_str(chunk);
        if opt('\\').parse_next(input)?.is_some() {
            let escaped = any.parse_next(input)?;
            out.push(match escaped {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\'' => '\'',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
        } else {
            break;
        }
    }
    quote.parse_next(input)?;
    Ok(out)
}

/// Reject decorator combinations that cannot be honored: `@target` with
/// `@no_codegen`, a parse-API toggle (`@no_codegen`) with a parse-API shape
/// decorator (`@parse`/`@safeParse`), or a repeated decorator of the same kind.
fn validate_override_combination(input: &mut &str, overrides: &[ModelOverride]) -> PResult<()> {
    let mut has_target = false;
    let mut has_no_codegen = false;
    let mut has_parse = false;
    let mut has_safe_parse = false;
    for override_ in overrides {
        match override_ {
            ModelOverride::Target(_) => {
                if has_target {
                    return Err(ErrMode::Cut(ContextError::from_external_error(
                        input,
                        RuleError("model override `@target` is specified more than once".into()),
                    )));
                }
                has_target = true;
            }
            ModelOverride::NoCodegen => {
                if has_no_codegen {
                    return Err(ErrMode::Cut(ContextError::from_external_error(
                        input,
                        RuleError("model override `@no_codegen` is specified more than once".into()),
                    )));
                }
                has_no_codegen = true;
            }
            ModelOverride::Parse => {
                if has_parse {
                    return Err(ErrMode::Cut(ContextError::from_external_error(
                        input,
                        RuleError("model override `@parse` is specified more than once".into()),
                    )));
                }
                has_parse = true;
            }
            ModelOverride::SafeParse(_) => {
                if has_safe_parse {
                    return Err(ErrMode::Cut(ContextError::from_external_error(
                        input,
                        RuleError("model override `@safeParse` is specified more than once".into()),
                    )));
                }
                has_safe_parse = true;
            }
        }
    }
    if has_target && has_no_codegen {
        return Err(ErrMode::Cut(ContextError::from_external_error(
            input,
            RuleError("`@target` cannot be combined with `@no_codegen` on the same model".into()),
        )));
    }
    if has_no_codegen && (has_parse || has_safe_parse) {
        return Err(ErrMode::Cut(ContextError::from_external_error(
            input,
            RuleError(
                "`@no_codegen` cannot be combined with `@parse` or `@safeParse` on the same model"
                    .into(),
            ),
        )));
    }
    Ok(())
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
    let (params, return_type) = decl_header(input)?;
    ws(input)?;
    let sql = scan_sql_body.parse_next(input)?;

    Ok(QueryDecl {
        name,
        params,
        return_type: return_contract(return_type),
        sql,
        overrides: Vec::new(),
    })
}

fn transaction_decl(input: &mut &str) -> PResult<TransactionDecl> {
    kw("transaction").parse_next(input)?;
    ws(input)?;
    let name = ident.parse_next(input)?;
    ws(input)?;
    let (params, return_type) = decl_header(input)?;
    ws(input)?;
    let sql = scan_sql_body.parse_next(input)?;

    Ok(TransactionDecl {
        name,
        params,
        return_type: return_contract(return_type),
        sql,
        overrides: Vec::new(),
    })
}

/// Parse the shared `($param: Type, ...) (-> Type)?` header of a `query` or
/// `transaction` declaration.
fn decl_header(input: &mut &str) -> PResult<(Vec<ParamDecl>, Option<TypeRef>)> {
    let params: Vec<ParamDecl> = delimited(
        ('(', ws).map(|(c, _): (char, ())| c),
        separated(0.., param_decl, (ws, ',')),
        (ws, ')').map(|(_, c): ((), char)| c),
    )
    .parse_next(input)?;
    let return_type = opt((ws, "->", ws, type_ref))
        .parse_next(input)?
        .map(|(_, _, _, ty)| ty);
    Ok((params, return_type))
}

/// Normalize the raw `-> Type` header into a [`QueryReturn`]: `-> T` is one
/// value, `-> T?` zero or one, and `-> T[]` zero or more.
fn return_contract(ty: Option<TypeRef>) -> QueryReturn {
    match ty {
        None => QueryReturn::Exec,
        Some(TypeRef::Array(inner)) => QueryReturn::Many(*inner),
        Some(TypeRef::Nullable(inner)) => QueryReturn::Optional(*inner),
        Some(other) => QueryReturn::Single(other),
    }
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
    Transaction(TransactionDecl),
}

/// A single top-level declaration, optionally preceded by decorators. A
/// decorator (or decorators) may precede a `model`, `query`, or `transaction`
/// declaration; `@no_codegen` is rejected on `query` and `transaction` (it only
/// applies to `model`), and decorators cannot decorate a `type` or `import`.
fn item(input: &mut &str) -> PResult<Item> {
    let overrides: Vec<ModelOverride> = repeat(0.., model_override).parse_next(input)?;
    if overrides.is_empty() {
        alt((
            import_stmt.map(Item::Import),
            type_decl.map(Item::Type),
            model_decl.map(Item::Model),
            query_decl.map(Item::Query),
            transaction_decl.map(Item::Transaction),
        ))
        .parse_next(input)
    } else {
        ws(input)?;
        // Decorators may introduce a `model`, `query`, or `transaction`; peek
        // at the keyword so each is recognized and validates its own decorator
        // rules.
        if peek(kw("model")).parse_next(input).is_ok() {
            let mut model = model_decl.parse_next(input)?;
            validate_override_combination(input, &overrides)?;
            model.overrides = overrides;
            Ok(Item::Model(model))
        } else if peek(kw("query")).parse_next(input).is_ok() {
            validate_query_overrides(input, &overrides)?;
            let mut query = query_decl.parse_next(input)?;
            query.overrides = overrides;
            Ok(Item::Query(query))
        } else if peek(kw("transaction")).parse_next(input).is_ok() {
            validate_query_overrides(input, &overrides)?;
            let mut transaction = transaction_decl.parse_next(input)?;
            transaction.overrides = overrides;
            Ok(Item::Transaction(transaction))
        } else {
            Err(ErrMode::Cut(ContextError::from_external_error(
                input,
                RuleError(
                    "`@...` decorators must precede a `model`, `query`, or `transaction` declaration"
                        .into(),
                ),
            )))
        }
    }
}

/// `@no_codegen`, `@parse`, and `@safeParse` apply to `model` declarations
/// only; a decorated `query` or `transaction` still honors
/// duplicate-`@target` rejection.
fn validate_query_overrides(input: &mut &str, overrides: &[ModelOverride]) -> PResult<()> {
    for override_ in overrides {
        let message = match override_ {
            ModelOverride::NoCodegen => "`@no_codegen` only applies to `model` declarations",
            ModelOverride::Parse => "`@parse` only applies to `model` declarations",
            ModelOverride::SafeParse(_) => "`@safeParse` only applies to `model` declarations",
            ModelOverride::Target(_) => continue,
        };
        return Err(ErrMode::Cut(ContextError::from_external_error(
            input,
            RuleError(message.into()),
        )));
    }
    validate_override_combination(input, overrides)
}

fn axm_file(input: &mut &str) -> PResult<AxmFile> {
    ws(input)?;
    let items: Vec<Item> = repeat(0.., terminated(item, ws)).parse_next(input)?;

    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut models = Vec::new();
    let mut queries = Vec::new();
    let mut transactions = Vec::new();
    for item in items {
        match item {
            Item::Import(i) => imports.push(i),
            Item::Type(t) => types.push(t),
            Item::Model(m) => models.push(m),
            Item::Query(q) => queries.push(q),
            Item::Transaction(t) => transactions.push(t),
        }
    }
    Ok(AxmFile {
        imports,
        types,
        models,
        queries,
        transactions,
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
    fn parses_quoted_field_names_in_models() {
        // A quoted field name is ordinary syntax: the quotes are stripped and
        // the content is the field name. All three spellings mix freely.
        let file = parse(
            r#"
model User extends select<users> {
  id: UUID,
  "email": String.email().max_length(320),
  "username": String.nonempty().trim(),
  age: Int.min(0).max(150),
}
"#,
        );
        let fields = &file.models[0].fields;
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["id", "email", "username", "age"]);
        assert_eq!(fields[1].ty.rules.len(), 2);
        assert_eq!(fields[3].ty.rules.len(), 2);
    }

    #[test]
    fn quoted_field_names_support_optional_and_default() {
        let file = parse(
            r#"model User {
  "age"?: Int
  "country": String = "US"
}"#,
        );
        let fields = &file.models[0].fields;
        assert_eq!(fields[0].name, "age");
        assert!(fields[0].optional);
        assert_eq!(fields[1].name, "country");
        assert_eq!(fields[1].default, Some(Literal::String("US".into())));
    }

    #[test]
    fn quoted_field_names_may_hold_non_identifiers() {
        // Quoted names can carry characters that a bare identifier cannot.
        let file = parse(r#"model T { "first-name": String "x:y": Int }"#);
        assert_eq!(file.models[0].fields[0].name, "first-name");
        assert_eq!(file.models[0].fields[1].name, "x:y");
    }

    #[test]
    fn parses_bare_model_without_source() {
        let file = parse("model User { name: String }");
        assert_eq!(file.models[0].source, None);
    }

    #[test]
    fn parses_target_override_with_both_quote_styles() {
        let file = parse("@target(\"typescript\", \"rust\")\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![ModelOverride::Target(vec![
                Target::TypeScript,
                Target::Rust
            ])]
        );

        let file = parse("@target('rust', 'typescript')\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![ModelOverride::Target(vec![
                Target::Rust,
                Target::TypeScript
            ])]
        );
        assert_eq!(
            file.models[0].target_restriction(),
            Some(&[Target::Rust, Target::TypeScript][..])
        );
    }

    #[test]
    fn parses_single_target_as_bare_word() {
        let file = parse("@target(typescript)\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![ModelOverride::Target(vec![Target::TypeScript])]
        );
    }

    #[test]
    fn parses_no_codegen_override() {
        let file = parse("@no_codegen\nmodel User { id: UUID }");
        assert_eq!(file.models[0].overrides, vec![ModelOverride::NoCodegen]);
        assert!(file.models[0].is_no_codegen());
        assert!(file.models[0].target_restriction().is_none());
    }

    #[test]
    fn target_override_applies_to_queries() {
        let file = parse(
            "@target(\"rust\")\nquery Log($msg: String) { INSERT INTO logs (msg) VALUES ($msg) }",
        );
        assert_eq!(
            file.queries[0].overrides,
            vec![ModelOverride::Target(vec![Target::Rust])]
        );
        assert_eq!(
            file.queries[0].target_restriction(),
            Some(&[Target::Rust][..])
        );
    }

    #[test]
    fn no_codegen_rejected_on_queries_and_decorators_rejected_on_others() {
        let err = parse_err(
            "@no_codegen\nquery Log($msg: String) { INSERT INTO logs (msg) VALUES ($msg) }",
        );
        assert!(err.contains("only applies to `model`"), "{err}");
        let err = parse_err("@parse\nquery Log($msg: String) { SELECT $msg }");
        assert!(err.contains("`@parse` only applies to `model`"), "{err}");
        let err = parse_err("@safeParse(\"first\")\nquery Log($msg: String) { SELECT $msg }");
        assert!(err.contains("`@safeParse` only applies to `model`"), "{err}");
        parse_err("@no_codegen\ntype Email = String;");
        parse_err("@target(\"rust\")\nimport { User } from \"./users\";");
        let err = parse_err("@target(\"rust\")\ntype Email = String;");
        assert!(err.contains("must precede a `model`"), "{err}");
    }

    #[test]
    fn decorators_can_stack_on_a_query() {
        let file = parse(
            "query A($x: Int) { SELECT $x }\n@target(\"typescript\", \"rust\")\nquery B($y: Int) { SELECT $y }",
        );
        assert_eq!(file.queries[0].target_restriction(), None);
        assert_eq!(
            file.queries[1].target_restriction(),
            Some(&[Target::TypeScript, Target::Rust][..])
        );
    }

    #[test]
    fn rejects_target_combined_with_no_codegen() {
        let err = parse_err("@target(\"typescript\")\n@no_codegen\nmodel User { id: UUID }");
        assert!(err.contains("cannot be combined"), "{err}");
    }

    #[test]
    fn rejects_repeated_overrides() {
        let err =
            parse_err("@target(\"typescript\")\n@target(\"rust\")\nmodel User { a: String }");
        assert!(err.contains("more than once"), "{err}");
        let err = parse_err("@no_codegen\n@no_codegen\nmodel User { a: String }");
        assert!(err.contains("more than once"), "{err}");
        let err = parse_err("@parse\n@parse\nmodel User { a: String }");
        assert!(err.contains("more than once"), "{err}");
        let err = parse_err("@safeParse(\"first\")\n@safeParse(\"all\")\nmodel User { a: String }");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn rejects_unknown_target_and_override() {
        let err = parse_err("@target(\"golang\")\nmodel User { a: String }");
        assert!(err.contains("unknown codegen target `golang`"), "{err}");
        let err = parse_err("@truncate\nmodel User { a: String }");
        assert!(err.contains("unknown model override `@truncate`"), "{err}");
    }

    #[test]
    fn rejects_missing_or_unexpected_arguments() {
        let err = parse_err("@target\nmodel User { a: String }");
        assert!(err.contains("requires a target list"), "{err}");
        let err = parse_err("@no_codegen()\nmodel User { a: String }");
        assert!(err.contains("takes no arguments"), "{err}");
        let err = parse_err("@parse(\"x\")\nmodel User { a: String }");
        assert!(err.contains("takes no arguments"), "{err}");
        let err = parse_err("@safeParse\nmodel User { a: String }");
        assert!(err.contains("requires one argument"), "{err}");
        let err = parse_err("@safeParse(\"first\", \"all\")\nmodel User { a: String }");
        assert!(err.contains("requires one argument"), "{err}");
    }

    #[test]
    fn parses_parse_and_safe_parse_overrides() {
        let file = parse("@parse\n@safeParse(\"first\")\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![
                ModelOverride::Parse,
                ModelOverride::SafeParse(SafeParseMode::First)
            ]
        );
        assert_eq!(file.models[0].safe_parse_mode(), SafeParseMode::First);

        let file = parse("@safeParse('all')\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![ModelOverride::SafeParse(SafeParseMode::All)]
        );
        assert_eq!(file.models[0].safe_parse_mode(), SafeParseMode::All);

        let file = parse("@safeParse(all)\nmodel User { id: UUID }");
        assert_eq!(
            file.models[0].overrides,
            vec![ModelOverride::SafeParse(SafeParseMode::All)]
        );

        let file = parse("@target(\"typescript\")\n@parse\n@safeParse(\"all\")\nmodel User { a: String }");
        assert_eq!(
            file.models[0].target_restriction(),
            Some(&[Target::TypeScript][..])
        );
        assert_eq!(file.models[0].safe_parse_mode(), SafeParseMode::All);
    }

    #[test]
    fn safe_parse_defaults_to_all() {
        let file = parse("@target(\"rust\")\nmodel User { id: UUID }");
        assert_eq!(file.models[0].safe_parse_mode(), SafeParseMode::All);
    }

    #[test]
    fn rejects_unknown_safe_parse_mode() {
        let err = parse_err("@safeParse(\"both\")\nmodel User { id: UUID }");
        assert!(err.contains("unknown safeParse mode `both`"), "{err}");
    }

    #[test]
    fn rejects_parse_api_overrides_combined_with_no_codegen() {
        let err = parse_err("@no_codegen\n@parse\nmodel User { a: String }");
        assert!(err.contains("cannot be combined"), "{err}");
        let err = parse_err("@safeParse(\"first\")\n@no_codegen\nmodel User { a: String }");
        assert!(err.contains("cannot be combined"), "{err}");
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
    fn rules_accept_messages() {
        let file = parse(
            r#"model T {
  a: String.email("Invalid email address")
  b: String.min_length(3, "Too short")
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
    fn length_rule_aliases_are_rejected() {
        let err = parse_err("model T {\n  b: String.min_len(3)\n}");
        assert!(err.contains("unknown rule `.min_len()`"), "{err}");
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
    fn semicolons_on_imports_and_types_are_optional() {
        let file = parse(
            r#"
import { User, Email as ContactEmail } from "./users"
type Email = String.email().max_length(320)
type NonEmptyString = String.nonempty().trim()
import { Address } from "./geo";
type UserId = BigInt;
"#,
        );
        assert_eq!(file.imports.len(), 2);
        assert_eq!(file.imports[0].source, "./users");
        assert_eq!(file.imports[1].source, "./geo");
        assert_eq!(file.types.len(), 3);
        assert_eq!(file.types[0].name, "Email");
        assert_eq!(file.types[1].name, "NonEmptyString");
        assert_eq!(file.types[2].name, "UserId");
    }

    #[test]
    fn semicolons_after_closing_braces_are_rejected() {
        let err = parse_err(
            r#"model User extends select<users> {
    id: UUID,
    email: String.email().max_length(320),
    username: String.nonempty().trim(),
    age: Int.min(0).max(150),
};"#,
        );
        assert!(err.contains(';'), "{err}");

        let err = parse_err(
            r#"query ListUsers($limit: Int) -> User[] {
    SELECT id, email FROM users ORDER BY id LIMIT $limit
};"#,
        );
        assert!(err.contains(';'), "{err}");
    }

    #[test]
    fn multiple_sql_statements_are_supported_in_query_body() {
        let file = parse(
            r#"
query GetUser($id: UUID) -> User? {
    SELECT id, email
    FROM users
    WHERE id = $id;
    DELETE FROM users WHERE id = $id;
}
"#,
        );
        let q = &file.queries[0];
        assert_eq!(q.params.len(), 1);
        assert_eq!(
            q.sql,
            "SELECT id, email\n    FROM users\n    WHERE id = $id;\n    DELETE FROM users WHERE id = $id;"
        );
    }

    #[test]
    fn parses_transaction_with_params_and_return_type() {
        let file = parse(
            r#"transaction CreatePost($userId: UUID, $content: String) -> Post {
    INSERT INTO posts (user_id, content) VALUES ($userId, $content);
    UPDATE users SET post_count = post_count + 1 WHERE id = $userId;
    SELECT id, user_id, content FROM posts WHERE user_id = $userId ORDER BY id DESC LIMIT 1;
}"#,
        );
        assert!(file.queries.is_empty());
        let t = &file.transactions[0];
        assert_eq!(t.name, "CreatePost");
        assert_eq!(t.params.len(), 2);
        assert_eq!(t.params[0].name, "userId");
        assert_eq!(t.params[1].name, "content");
        assert_eq!(t.return_type, QueryReturn::Single(TypeRef::Named("Post".into())));
        assert_eq!(t.sql.matches(';').count(), 3);
    }

    #[test]
    fn parses_transaction_return_shape_variants() {
        let file = parse("transaction T1() -> User { SELECT * FROM users; SELECT * FROM users; }");
        assert_eq!(file.transactions[0].return_type, QueryReturn::Single(TypeRef::Named("User".into())));

        let file = parse("transaction T2() -> User? { SELECT 1; SELECT * FROM users; }");
        assert_eq!(file.transactions[0].return_type, QueryReturn::Optional(TypeRef::Named("User".into())));

        let file = parse("transaction T3() -> User[] { SELECT 1; SELECT * FROM users; }");
        assert_eq!(file.transactions[0].return_type, QueryReturn::Many(TypeRef::Named("User".into())));

        let file = parse("transaction T4() { UPDATE a SET x = 1; UPDATE b SET y = 2; }");
        assert_eq!(file.transactions[0].return_type, QueryReturn::Exec);
    }

    #[test]
    fn transaction_target_decorator_is_valid() {
        let file = parse(
            r#"@target("typescript")
transaction CreatePost($userId: UUID) {
    INSERT INTO posts (user_id) VALUES ($userId);
    UPDATE users SET post_count = post_count + 1 WHERE id = $userId;
}"#,
        );
        let t = &file.transactions[0];
        assert_eq!(
            t.target_restriction(),
            Some([Target::TypeScript].as_slice())
        );
    }

    #[test]
    fn model_only_decorators_are_rejected_on_transaction() {
        for decorator in ["@no_codegen", "@parse", "@safeParse(\"all\")"] {
            let err = parse_err(&format!(
                "{decorator}\ntransaction T($id: UUID) {{ UPDATE a SET x = 1; UPDATE b SET y = 2; }}"
            ));
            assert!(err.contains("only applies to `model`"), "{err}");
        }
    }

    #[test]
    fn transaction_keyword_requires_word_boundary_for_query() {
        // `transaction` must not shadow `query` or `model` matching.
        let file = parse(
            r#"
model User { id: UUID }
transaction T($id: UUID) {
    UPDATE users SET x = 1 WHERE id = $id;
    UPDATE users SET y = 2 WHERE id = $id;
}
query GetUser($id: UUID) -> User? { SELECT * FROM users WHERE id = $id; }
"#,
        );
        assert_eq!(file.models.len(), 1);
        assert_eq!(file.queries.len(), 1);
        assert_eq!(file.transactions.len(), 1);
    }

    #[test]
    fn single_query_statement_makes_trailing_semicolon_optional() {
        let file = parse(
            r#"query ListUsers($limit: Int) -> User[] {
    SELECT id, email FROM users ORDER BY id LIMIT $limit
}"#,
        );
        assert!(file.queries[0].sql.contains("LIMIT $limit"));

        let file = parse("query Q() { SELECT 1; }");
        assert_eq!(file.queries[0].sql, "SELECT 1;");
    }

    #[test]
    fn parses_full_user_spec_example() {
        let file = parse(
            r#"
import { User, Email as ContactEmail } from "./users"
type Email = String.email().max_length(320)
type NonEmptyString = String.nonempty().trim()
model User extends select<users> {
    id: UUID,
    email: String.email().max_length(320),
    username: String.nonempty().trim(),
    age: Int.min(0).max(150),
}
query GetUser($id: UUID) -> User? {
    SELECT id, email
    FROM users
    WHERE id = $id;
    DELETE FROM users WHERE id = $id;
}
query ListUsers($limit: Int) -> User[] {
    SELECT id, email FROM users ORDER BY id LIMIT $limit
}
"#,
        );
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.types.len(), 2);
        assert_eq!(file.models.len(), 1);
        assert_eq!(file.queries.len(), 2);
        // Multi-statement query has two semicolons.
        assert_eq!(file.queries[0].sql.matches(';').count(), 2);
        // Single-statement query has none.
        assert_eq!(file.queries[1].sql.matches(';').count(), 0);
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
