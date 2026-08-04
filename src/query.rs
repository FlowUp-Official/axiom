//! Named query parsing for `-- @fn` / `-- @validate` header annotations.
//!
//! Query files are plain SQL files where each statement is preceded by a
//! function signature comment and zero or more per-parameter validation
//! comments:
//!
//! ```sql
//! -- @fn get_user(email: String) : Users[]
//! -- @validate email(email, trim, lower)
//! SELECT id, email FROM users WHERE email = $1
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;

use miette::SourceSpan;

use crate::catalog::{parse_rules_content, split_top_level, ValidationRule};
use crate::errors::AxiomError;

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
    use crate::catalog::RuleKind;

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
}
