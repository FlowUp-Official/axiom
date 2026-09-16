# The `.axm` Language

`.axm` files layer a typed application contract on top of your SQL schema.
A file contains `import`s, `type` aliases, `model` declarations, and `query`
declarations. Everything is compiled into the generated TypeScript and Rust
clients as real, runnable code — no runtime config, no duplicated logic.

Identifiers are **strictly case-sensitive**: `User` and `user` are distinct
names. Primitive types are `PascalCase` keywords (`String`, `UUID`, `Int`, `...`),
model and type names are `PascalCase`, and fields and query parameters are
`camelCase`.

## Imports

Share models and types across files without polluting namespaces:

```axm
import { User, Email as ContactEmail } from "./users"
```

The `source` is a relative `.axm` file reference. Imported names must exist in
the target file, and a bare import never collides with a locally declared name.
Import cycles are reported by the resolver.

## Types

Reusable, named refinements of a primitive or existing type:

```axm
type Email = String.email().max_length(320)
type NonEmptyString = String.nonempty().trim()
```

## Models

A model is a typed shape. The canonical, database-backed form spells out its
source relation:

```axm
model User extends select<users> {
  id: UUID
  email: String.email().max_length(320)
  username: String.nonempty().trim()
  age: Int.min(0).max(150)
}
```

- `extends select<users>` binds the model to the `users` table in the SQL
  catalog. The relation is a database identifier, resolved case-sensitively: a
  fully qualified name (`select<public.users>`) matches that table exactly,
  while an unqualified name (`select<users>`) also matches the last segment of
  a qualified table. An unmatched relation is reported by `axiom check` as
  `error[check.model-source]`. Bare models with no source remain valid as pure
  application models.
- Fields are `name: <type>` with optional `?` (`age?`) for absent values and an
  optional default (`country = "US"`), applied only when the field is missing.

## Field rules and transformations

Rules are chained onto a field or type with dot calls. Each rule can carry a
custom failure message as a second argument, e.g. `Int.min(0, "must be ≥ 0")`.

| Rule | Example | Description |
| ---- | ------- | ----------- |
| `email` | `String.email()` | Must be a valid email address |
| `url` | `String.url()` | Must be a valid URL |
| `uuid` | `String.uuid()` | Must be a valid UUID |
| `ulid` | `String.ulid()` | Must be a valid ULID |
| `ipv4` | `String.ipv4()` | Must be a valid IPv4 address |
| `ipv6` | `String.ipv6()` | Must be a valid IPv6 address |
| `isodate` | `String.isodate()` | Must be an ISO 8601 date |
| `alphanumeric` | `String.alphanumeric()` | Letters and digits only |
| `nonempty` | `String.nonempty()` | Must not be empty |
| `min_length` | `String.min_length(3)` | Minimum string length |
| `max_length` | `String.max_length(255)` | Maximum string length |
| `min` | `Int.min(0)` | Minimum numeric value |
| `max` | `Int.max(100)` | Maximum numeric value |
| `regex` | `String.regex("^[a-z]+$")` | Must match the custom regular expression |

The compiler enforces a rule/base compatibility table: `min`/`max` apply to
`Int`/`BigInt`/`Float`; `email`, `url`, `uuid`, `ulid`, `ipv4`, `ipv6`,
`isodate`, `alphanumeric`, and `regex` apply to `String` (and string-based type
aliases); `nonempty`/`min_length`/`max_length` apply to `String` or to a
collection (`T[]`, on the array itself). `trim`/`lowercase`/`uppercase` apply
only to `String`. Violations are reported by `axiom check` as
`error[check.rule-base]`.

Transformations run before validation and are also chained:

| Transform | Example | Description |
| --------- | ------- | ----------- |
| `trim` | `String.trim()` | Trim surrounding whitespace |
| `lowercase` | `String.lowercase()` | Normalize to lowercase |
| `uppercase` | `String.uppercase()` | Normalize to uppercase |

Rules are compiled into the generated `validate()` methods, so invalid input
never reaches the application or the database.

## Queries

Database interactions are declared as typed contracts:

```axm
query GetUser($id: UUID) -> User? {
  SELECT id, email FROM users WHERE id = $id
}
```

See [Query Functions](/guide/query-functions) for the full contract syntax,
placeholder rules, and verification.

## Resolution

On every command, `.axm` files are parsed and *resolved*: imports are linked,
duplicate model/type/query names are rejected, and every referenced type is
checked. Query return types are resolved against the full catalog — a table
declared in `schema.sql` is a valid return type even though it is invisible to
`.axm` import resolution. Errors surface as `check.model-*` diagnostics from
`axiom check` and inline in the editor via `axiom-lsp`.