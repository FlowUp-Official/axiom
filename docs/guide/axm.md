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

A trailing `;` is optional on import statements:

```axm
import { User } from "./users";
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

As with imports, a trailing `;` is optional on type declarations:

```axm
type Email = String.email().max_length(320);
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
- Field names may be double-quoted. Quoted and bare names mix freely and are
  equivalent — `"email"` and `email` name the same field. Quoting is also the
  only way to spell a field name that is not a valid bare identifier, e.g.
  `"first-name": String`.

## Model decorators

A model or query may be preceded by `@` decorators that override how **that one
declaration** is generated. They apply to `model` and `query` declarations
only, never to `type` aliases or imports. `@target(...)` works for both models
and queries; `@no_codegen`, `@parse`, and `@safeParse(...)` apply to models
only (using any of them on a query is a parse error).

### `@target(...)` — restrict code generation

By default every model and every query is generated for all configured targets.
`@target` limits a single declaration to the named targets:

```axm
@target("rust")
model AuditLog extends select<audit_logs> {
  id: UUID
  payload: String
}

@target("typescript", "rust")
model Shared {
  id: UUID
}

@target("rust")
query DailyPurging() {
  DELETE FROM audit_logs WHERE created_at < now() - interval '30 days';
}
```

- Target names are lowercase and case-sensitive; the recognized values are
  `typescript` and `rust`. A model or query lists any subset, in any order.
- Arguments may be double-quoted, single-quoted, or bare:
  `@target("rust")`, `@target('typescript')`, and `@target(rust)` are all
  equivalent.
- The declaration is **omitted entirely** from every generator whose target is
  not listed — no interface/struct, no validation, no parse API, and (for
  queries) no `fn`/`export` wrapper. Using a `@target`-excluded model or query
  from an emitted declaration is the author's responsibility; the generated
  output will not compile.
- A declaration with no `@target` restriction is emitted for every configured
  target.

### `@no_codegen` — suppress the standalone validation API

```axm
@no_codegen
model InternalThing {
  id: UUID
}
```

A `@no_codegen` model never receives its own standalone parse entry point
(`Result` / `safeParse` / `parse` in TypeScript, `SomeModel::parse` /
`SomeModel::safe_parse` in Rust). Its type/interface/struct is still emitted —
and still exported — so it can appear as a field type, type alias, or query
return, and `coerce`/validation helpers it depends on are included as needed.

- A `@no_codegen` model that nothing references is dropped from the output
  entirely (no dead code).
- A `@no_codegen` model pulled in by an emitted model, type alias, or query is
  emitted with its type and coercion logic but without the standalone parse API.
- `@no_codegen` only applies to `model` declarations; `@target("...")` on a
  `query` is allowed, but `@no_codegen` on a query is a parse error.
- `@target` and `@no_codegen` are **mutually exclusive** on the same model;
  each decorator may appear **at most once**. An unknown decorator name or an
  unknown target is a parse error.

### `@parse` — explicit no-op marker

```axm
@parse
model FanoutUser {
  id: UUID
  email: String .email()
}
```

`@parse` is a no-op: a full model already receives `safeParse`/`parse`
(TypeScript) and `{Model}::safe_parse`/`{Model}::parse` (Rust), so `@parse`
simply makes the default behavior explicit. It takes no arguments and does not
change the generated output. It may combine with `@safeParse(...)` or
`@target(...)`, but not with `@no_codegen`.

### `@safeParse("first")` / `@safeParse("all")` — control error collection

```axm
@safeParse("first")
model StrictUser {
  id: UUID
  email: String .email()
  age: Int .min(0)
}

@safeParse("all")
model LenientUser {
  id: UUID
  email: String .email()
  age: Int .min(0)
}
```

By default every model collects **all** validation errors during a parse run —
the equivalent of `@safeParse("all")` (or no `@safeParse` at all).
`@safeParse("first")` switches a single model to fail-fast mode.

- **`@safeParse("all")` (default):** `safeParse`/`safe_parse` coerces every
  field, returns every error, and `parse` panics with the full error list.
- **`@safeParse("first")`:** validation stops at the first error; the returned
  error list (or panic) contains only that first error. This is cheaper for
  deep or wide structures since untouched fields are never coerced.
- Arguments may be double-quoted, single-quoted, or bare:
  `@safeParse("first")`, `@safeParse('first')`, and `@safeParse(first)` are all
  equivalent; the recognized modes are `first` and `all` (lowercase,
  case-sensitive).
- `@safeParse(...)` only applies to `model` declarations; using it on a
  `query` is a parse error. It may combine with `@parse`, `@target(...)`, and
  a second use is a parse error.
- `@safeParse(...)` and `@parse` are **mutually exclusive** with
  `@no_codegen` on the same model; each decorator may appear **at most once**.
  An unknown decorator name, unknown target, or unknown safeParse mode is a
  parse error.
- In a module containing at least one `@safeParse("first")` model, the Rust
  generator emits `thread_local!`/`axm_stopped` scaffolding and the TypeScript
  generator emits an `AXM_STOP` sentinel and `_axm_fail_fast` flag. `"all"`
  models in the same module still collect all errors, and from within a
  `"first"` parse a nested `"all"` model stops at the same first error.

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

A `query` block may contain multiple SQL statements separated by `;`:

```axm
query GetUser($id: UUID) -> User? {
  SELECT id, email
  FROM users
  WHERE id = $id;
  DELETE FROM users WHERE id = $id
}
```

The trailing `;` on the last statement is optional. When there are multiple
statements, the return contract applies to the last one (the result set a
caller receives). Semicolons inside the SQL body (in string literals, comments,
etc.) are preserved verbatim.

A `;` is **not** allowed after the closing `}` of a `query` (or `model`)
block.

See [Query Functions](/guide/query-functions) for the full contract syntax,
placeholder rules, and verification.

## Resolution

On every command, `.axm` files are parsed and *resolved*: imports are linked,
duplicate model/type/query names are rejected, and every referenced type is
checked. Query return types are resolved against the full catalog — a table
declared in `schema.sql` is a valid return type even though it is invisible to
`.axm` import resolution. Errors surface as `check.model-*` diagnostics from
`axiom check` and inline in the editor via `axiom-lsp`.