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
- **Query functions** — one `export async function` per `-- @fn` annotation,
  taking the `Sql` client and a typed params object. Positional (`$1`) and named
  (`$email`) placeholders are rewritten to postgres.js parameter syntax, and
  parameter rules run before the query executes.

```ts
import type { Sql } from 'postgres';

export interface Users {
  id: number;
  email: string;
}

export interface GetUserParams {
  email: string;
}

export async function getUser(
  sql: Sql,
  params: GetUserParams,
): Promise<Users | null> {
  const email = params.email.trim().toLowerCase();
  // ...validation + SELECT
}
```

## Rust

A Rust module that pairs with `sqlx`:

- **Serde structs** — one `#[derive(Debug, Clone, Serialize, Deserialize)]`
  struct per table.
- **Validation** — each struct implements a `validate()` method running the
  compiled column rules.
- **Query functions** — one `pub async fn` per `-- @fn` annotation, taking
  `&sqlx::PgPool` and a typed params struct. Parameter validation runs before
  `sqlx::query_as!` / `sqlx::query` execution.

```rust
use sqlx::PgPool;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Users {
    pub id: i64,
    pub email: String,
}

pub struct GetUserParams {
    pub email: String,
}

pub async fn get_user(
    pool: &PgPool,
    params: GetUserParams,
) -> Result<Option<Users>, Box<dyn std::error::Error>> {
    let email = params.email.trim().to_lowercase();
    // ...validation + sqlx::query_as!
}
```

## Shared behavior

- **Naming conversions** — SQL `snake_case` names are converted per target:
  `camelCase` for TypeScript, `snake_case` for Rust.
- **Only what you use** — validation helpers (e.g. regex presets for email/UUID)
  are emitted lazily, so unused rules do not bloat the output.
- **Deterministic output** — generation is a pure function of the inputs, so
  repeated runs produce byte-identical files (when inputs are unchanged,
  generation is skipped entirely via the cache).
