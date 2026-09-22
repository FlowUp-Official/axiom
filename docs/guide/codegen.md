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
- **Transaction functions** — one `export async function` per `transaction`
  declaration. Each body runs inside `sql.begin(async (tx) => { ... })`; the
  function returns the result of the last statement and rolls back automatically
  on any error.

Declarations flagged `@target("rust")` (models **or** queries) are skipped by
the TypeScript generator — a filtered-out query emits neither its function nor
a `Sql` import. Models flagged `@no_codegen` (and its variants) keep only the parts of their public interface that are not suppressed; a `@no_codegen` model is omitted
entirely when no emitted declaration references it. `@safeParse("first")`
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
- **Transaction functions** — one `pub async fn` per `transaction` declaration.
  Each body acquires a `tokio_postgres::Transaction` via
  `client.transaction().await?`, executes every statement against it, commits on
  success with `txn.commit().await?`, and rolls back with `txn.rollback()` on
  any error.

Declarations flagged `@target("typescript")` (models **or** queries) are
skipped by the Rust generator; models flagged `@no_codegen` (and its variants) keep only the parts of their public interface that are not suppressed (`pub struct` or the public `safe_parse` entry points). `@safeParse("first")` models emit a fail-fast
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
  per target in both generators; `@no_codegen` (and its variants, for models only) suppress the
  standalone parse API and/or public types; `@parse` is a no-op marker; `@safeParse("first")` /
  `@safeParse("all")` (models only) select fail-fast vs. collect-all error
  handling for the standalone parse API. Transactions accept the same
  `@target(...)` decorator as queries; `@no_codegen` and its variants, and `@safeParse(...)` are
  only valid on models. Models referenced only by emitted
  declarations are pulled in with just their type and coercion logic.
- **Only what you use** — validation helpers (e.g. regex presets for email/UUID)
  are emitted lazily, so unused rules do not bloat the output.
- **Deterministic output** — generation is a pure function of the inputs, so
  repeated runs produce byte-identical files (when inputs are unchanged,
  generation is skipped entirely via the cache).

## `.axm` model types

Every `model` declaration in a `.axm` file is generated as a real type in
both TypeScript and Rust. These types are **publicly accessible** from the
generated API — consumers import them directly from the generated output file.

### TypeScript model types

Each model declaration produces an `export interface`:

```ts
// From: model User { name: String; age: Int }
export interface User {
  name: string;
  age: number;
}
```

Import from the generated API file (e.g. `gen/api.ts`):

```ts
import { User } from "./gen/api";
```

### Rust model types

Each model declaration produces a `pub struct`:

```rust
// From: model User { name: String; age: Int }
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct User {
    pub name: String,
    pub age: i64,
}
```

Import from the generated API file (e.g. `gen/core.rs`):

```rust
use my_project::core::User;
```

### Field type mappings

| Axiom type | TypeScript | Rust |
| ---------- | ---------- | ---- |
| `String` | `string` | `String` |
| `Int` | `number` | `i64` |
| `BigInt` | `bigint` | `i64` |
| `Float` | `number` | `f64` |
| `Boolean` | `boolean` | `bool` |
| `UUID` | `string` | `String` |
| `Date` | `string` | `String` |
| `DateTime` | `string` | `String` |
| `Json` | `unknown` | `serde_json::Value` |
| `Bytes` | `Uint8Array` | `Vec<u8>` |
| `ModelName` | `ModelName` | `ModelName` |

### Nullable and array fields

| Axiom type | TypeScript | Rust |
| ---------- | ---------- | ---- |
| `Field?` (optional) | `Type \| undefined` | `Option<Type>` |
| `Type?` (nullable) | `Type \| null` | `Option<Type>` |
| `Type[]` (array) | `Type[]` | `Vec<Type>` |

### Model-to-model references

A model field can reference another model by name. The generated type uses
the referenced model's type directly:

```axm
model Address { street: String; city: String }
model User { name: String; address: Address; addresses: Address[] }
```

```ts
// TypeScript
export interface Address { street: string; city: string; }
export interface User {
  name: string;
  address: Address;
  addresses: Address[];
}
```

```rust
// Rust
#[derive(...)]
pub struct Address { pub street: String, pub city: String, }
#[derive(...)]
pub struct User {
    pub name: String,
    pub address: Address,
    pub addresses: Vec<Address>,
}
```

Cyclic references (e.g. `A → B → A`) are handled automatically: the
involved models box their cyclic fields (`Box<A>` in Rust) to keep struct
sizes finite.
