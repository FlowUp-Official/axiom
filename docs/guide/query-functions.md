# Query Functions

Queries are declared inside `.axm` files as first-class contracts:

```axm
model User { id: UUID, email: String }

query GetUser($id: UUID) -> User? {
  SELECT id, email FROM users WHERE id = $id
}

query DeleteUser($id: UUID) {
  DELETE FROM users WHERE id = $id
}

query ListUsers($limit: Int) -> User[] {
  SELECT id, email FROM users ORDER BY id LIMIT $limit
}
```

Axiom turns each `query` declaration into a typed, async function in the
generated client based on the SQL body and the declared return contract.

## Query syntax

```text
query <Name>($<param>: <Type>, ...) [-> <Return>] {
  <SQL body>
}
```

- **`<Name>`** — `PascalCase`, unique across the workspace. It becomes a
  `camelCase` function in TypeScript and a `snake_case` function in Rust.
- **`$<param>: <Type>`** — typed parameters. The `$` is part of the *placeholder
  syntax*, not the name: the parameter is `id`, papers over the body as `$id`.
  Parameter names are `camelCase`.
- **`-> <Return>`** — the explicit result contract (see below). **Omitting it
  marks the query as an execution**: no rows are returned, so `-> Exec` never
  needs to be spelled out.
- **`<SQL body>`** — ordinary SQL, stored verbatim and passed to the driver.

### Return contracts

| Declaration | Contract | Generated client |
| ----------- | -------- | ---------------- |
| *(no `->`)* | Execution; no rows | `Promise<void>` / `Result<(), _>` |
| `-> T`      | Exactly one row | `T \| null` / `Option<T>` |
| `-> T?`     | Zero or one row  | `T \| null` / `Option<T>` |
| `-> T[]`    | Zero or more rows | `T[]` / `Vec<T>` |

The row type `T` is matched against the SQL catalog (case-insensitive) and the
linked `.axm` models and type aliases (case-sensitive). `-> users`, `-> Users`,
and `-> User` all resolve to the canonical type.

### Placeholders

Parameters can be referenced by name (`$email`) or positionally (`$1`, `$2`, ...)
and the two styles mix freely:

```axm
query ListUsers($limit: Int, $email: String) -> User[] {
  SELECT id, email FROM users
  WHERE email = $email AND id < $limit
  ORDER BY id
}
```

Before execution the placeholders are rewritten into the driver's parameter
syntax, so both styles call one typed function.

### Parameter types

Parameters map to the natural type in each generated language (`String`, `Int`,
`UUID`, ...), and can also reference a model or type alias. They become typed
struct fields / interfaces validated before the query runs.

## Verification

`axiom check` validates every declared query against the schema catalog:

- **SQL syntax** — the body parses with the configured dialect
  (`check.query-sql`).
- **Table / column references** — referenced tables and columns exist
  (`check.missing-table`, `check.missing-column`).
- **Placeholders** — every `$<name>` (or `$<n>`) matches a declared parameter
  (`check.query-placeholder`).
- **Return type** — the declared `->` type resolves to a table, model, or type
  alias (`check.query-return-type`).
- **Return contract** — a row-returning declaration needs a statement that
  produces rows (and vice versa), and the projected columns are fields of the
  declared row type (`check.query-contract`).

Queries are cross-checked against the rest of the `.axm` file (every
placeholder name unique, duplicate query names rejected, return types linkable)
as part of [`.axm` resolution](/guide/axm).