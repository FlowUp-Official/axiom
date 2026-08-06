# Query Functions

Query files are plain SQL files where each statement is preceded by a function
signature comment (`-- @fn`) and optional per-parameter validation comments
(`-- @validate <param>(rules)`). Axiom turns each annotated statement into a
typed, async function in the generated client.

## Function signatures

```sql
-- @fn get_user($email: String) : users
SELECT id, email FROM users WHERE email = $email

-- @fn delete_user(id: BigInt)
DELETE FROM users WHERE id = $id

-- @fn get_users($limit: Int, $email: String) : users[]
-- @validate email(email, trim, lower)
-- @validate limit(min=1, max=100)
SELECT id, email FROM users
WHERE email = $email AND id < $limit
ORDER BY id
```

- **`-- @fn <name>(<param>: <Type>, ...) [: <Return>]`** — the function
  signature. A leading `$` on a parameter name marks it as a named placeholder,
  letting the body reference it directly (`$email`); it is not part of the
  parameter's identifier. Parameter names are snake_case and become `camelCase`
  arguments in TypeScript and `snake_case` fields in Rust.
- **Return type is optional.** Omitting it (or writing `: Exec`) means the query
  performs an execution and returns no rows, so `: Exec` never needs to be
  spelled out.
- **`-- @validate <param>(<rules>)`** — validation rules applied to a single
  parameter before the query runs. The rule list uses the same syntax as column
  annotations.

### Placeholders

Parameters can be referenced positionally (`$1`, `$2`, ...) or by name
(`$email`) in the query body. Named placeholders are rewritten to positional
markers for the driver, so both styles mix freely:

```sql
-- @fn get_users($limit: Int, $email: String) : Users[]
SELECT id, email FROM users
WHERE email = $email AND id < $limit
```

A placeholder that does not match a declared parameter is a check error
(`check.query-placeholder`), as is a positional index beyond the declared
parameter count.

### Parameter types

Parameters map to native types in each generated language (e.g. `String`, `Int`,
`BigInt`, `Uuid`). They become typed struct fields / interfaces with matching
argument validation.

### Return types

The return type names a table (or model). Matching is case-insensitive and the
generated client uses the canonical type name, so `: users` and `: Users` are
equivalent for `CREATE TABLE users`.

| Signature      | Generated client                           |
| -------------- | ------------------------------------------ |
| `: Users`      | A single, optional row (`Users \| null`)    |
| `: Users[]`    | Zero or more rows (`Users[]`)               |
| *(omitted)*    | No rows; the function performs an execution |

The row type refers to a table in the catalog, whose columns define the shape of
the returned struct or interface.

## Parameter validation

Rules reuse the [column rule reference](/guide/sql-annotations#rule-reference):

```sql
-- @validate email(email, trim, lower)
-- @validate limit(min=1, max=100)
-- @fn get_users(limit: Int, email: String) : Users[]
SELECT id, email FROM users
WHERE email = $1 AND id < $2
ORDER BY id
```

Each `@validate` line targets the named parameter. Parameters without rules are
still typed and bound, but skip the extra checks.

## Generated behavior

- Positional `$1`, `$2`, ... and named `$name` placeholders are rewritten into
  the driver's parameter syntax in the generated query.
- Validation runs **before** the query, so invalid input never reaches the
  database.
- Failures collect all rule violations with their messages, not just the first.
