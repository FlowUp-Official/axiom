# Query Functions

Query files are plain SQL files where each statement is preceded by a function
signature comment (`-- @fn`) and optional per-parameter validation comments
(`-- @validate <param>(rules)`). Axiom turns each annotated statement into a
typed, async function in the generated client.

## Function signatures

```sql
-- @fn get_user(email: String) : Users
SELECT id, email FROM users WHERE email = $1

-- @validate email(email, trim, lower)
-- @fn get_users(limit: Int) : Users[]
SELECT id, email FROM users ORDER BY id LIMIT $1

-- @fn delete_user(id: BigInt) : Exec
DELETE FROM users WHERE id = $1
```

- **`-- @fn <name>(<param>: <Type>, ...) : <Return>`** — the function signature.
  Parameter names are snake_case and become `camelCase` arguments in TypeScript
  and `snake_case` fields in Rust.
- **`-- @validate <param>(<rules>)`** — validation rules applied to a single
  parameter before the query runs. The rule list uses the same syntax as column
  annotations.

### Parameter types

Parameters map to native types in each generated language (e.g. `String`, `Int`,
`BigInt`, `Uuid`). They become typed struct fields / interfaces with matching
argument validation.

### Return types

| Signature      | Generated client                           |
| -------------- | ------------------------------------------ |
| `: Users`      | A single, optional row (`Users \| null`)    |
| `: Users[]`    | Zero or more rows (`Users[]`)               |
| `: Exec`       | No rows; the function performs an execution |

The row type refers to a table in the catalog, whose columns define the shape of
the returned struct or interface.

## Parameter validation

Rules reuse the [column rule reference](/guide/sql-annotations#rule-reference):

```sql
-- @validate email(email, trim, lower)
-- @validate limit(min=1, max=100)
-- @fn get_users(limit: Int, email: String) : Users[]
SELECT id, email FROM users
WHERE email = $2 AND id < $1
ORDER BY id
```

Each `@validate` line targets the named parameter. Parameters without rules are
still typed and bound, but skip the extra checks.

## Generated behavior

- Positional `$1`, `$2`, ... placeholders are rewritten into the driver's
  parameter syntax in the generated query.
- Validation runs **before** the query, so invalid input never reaches the
  database.
- Failures collect all rule violations with their messages, not just the first.
