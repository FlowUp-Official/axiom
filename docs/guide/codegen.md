# Code Generation

Each entry in the `outputs` map of your `axiom.json` selects a generator. All
generators derive their output from the same catalog, so table and query changes
propagate consistently across languages.

## TypeScript

A TypeScript module that pairs with the `postgres` driver:

- **Row interfaces** — one `export interface` per table, with `camelCase`
  properties derived from the SQL column names.
- **Validation** — each interface exposes a `validate()`-style check that runs
  the compiled column rules (email, UUID, regex, normalization, ...) and
  collects `{ path, message }` errors.
- **Query functions** — one `export async function` per `query` declaration in
  a `.axm` file, taking the `Sql` client and a typed params object. Positional
  (`$1`) and named (`$email`) placeholders are rewritten to postgres.js
  parameter syntax, and params are validated before the query executes.

Declarations flagged `@target("rust")` (models **or** queries) are skipped by
the TypeScript generator — a filtered-out query emits neither its function nor
a `Sql` import. Models flagged `@no_codegen` always get the interface and
`coerce` function but never a `safeParse`/`parse` entry point, and are omitted
entirely when no emitted declaration references them. `@safeParse("first")`
models emit fail-fast `safeParse` functions that stop at the first validation
error (plus a module-level `AXM_STOP` sentinel when any model asks for it).
See [Model decorators](/guide/axm#model-decorators).

```ts
import type { Sql } from 'postgres';

export interface Users {
  id: number;
  email: string;
}

export interface GetUserParams {
  id: string;
}

export async function getUser(
  sql: Sql,
  params: GetUserParams,
): Promise<Users | null> {
  // ...params validation + SELECT
}
```

## Rust

A Rust module that pairs with `tokio-postgres`:

- **Serde structs** — one `#[derive(Debug, Clone, Serialize, Deserialize)]`
  struct per table.
- **Validation** — each struct implements a `validate()` method running the
  compiled column rules.
- **Query functions** — one `pub async fn` per `query` declaration in a `.axm`
  file, taking `&tokio_postgres::Client` and a typed params struct. Parameter
  validation runs before the query executes; params are bound as text so
  Postgres coerces them to the target column types at runtime.

Declarations flagged `@target("typescript")` (models **or** queries) are
skipped by the Rust generator; models flagged `@no_codegen` get the `pub
struct` and the free `coerce_*` function but no `impl` block with
`parse`/`safe_parse`. `@safeParse("first")` models emit a fail-fast
`safe_parse` that records only the first validation error (using
`thread_local!` state emitted when any model asks for it). See
[Model decorators](/guide/axm#model-decorators).

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Users {
    pub id: i64,
    pub email: String,
}

pub struct GetUserParams {
    pub email: String,
}

pub async fn get_user(
    client: &tokio_postgres::Client,
    params: GetUserParams,
) -> Result<Option<Users>, Box<dyn std::error::Error>> {
    // ...params validation + row_to_json decode
}
```

## Shared behavior

- **Naming conversions** — SQL `snake_case` names are converted per target:
  `camelCase` for TypeScript, `snake_case` for Rust.
- **Per-model decorators** — `@target(...)` filters both model and query output
  per target in both generators; `@no_codegen` (models only) suppresses the
  standalone parse API; `@parse` is a no-op marker; `@safeParse("first")` /
  `@safeParse("all")` (models only) select fail-fast vs. collect-all error
  handling for the standalone parse API. Models referenced only by emitted
  declarations are pulled in with just their type and coercion logic.
- **Only what you use** — validation helpers (e.g. regex presets for email/UUID)
  are emitted lazily, so unused rules do not bloat the output.
- **Deterministic output** — generation is a pure function of the inputs, so
  repeated runs produce byte-identical files (when inputs are unchanged,
  generation is skipped entirely via the cache).
