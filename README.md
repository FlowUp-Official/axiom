# Axiom

> A high-performance code generator for SQL schemas, `.axm` models, and queries, built for large monorepos.

Axiom turns your SQL schema and `.axm` model and query definitions into type-safe, validated client code — for both **TypeScript** and **Rust** — keeps the databases that mirror them in sync, and brings the whole compiler into your editor via a Language Server. It is written entirely in Rust.

## Why Axiom?

Hand-written database layers rot. SQL drifts from the application code that talks to it, validation logic gets reimplemented per service, and every new API endpoint means copy-pasting the same fragile types.

Axiom makes your database the **single source of truth**:

- **Declarative, validated configuration.** `axiom.json` is checked against a generated JSON schema at load time, so misconfiguration fails fast with a readable diagnostic — not a runtime surprise.
- **One schema, many languages.** Generate consistent, type-safe TypeScript and Rust client code from the same SQL inputs, so a table change propagates everywhere at once.
- **Embedded input validation.** Field and parameter rules (email, UUID, range, length, regex, normalization, and more) are declared in `.axm` files and compiled straight into the generated code.
- **Synchronized databases.** Push your schema to Postgres targets so the live database, the generated clients, and your source of truth never diverge.
- **IDE-grade authoring.** A native Language Server (`axiom-lsp`) plugs the compiler directly into your editor — diagnostics as you type, go-to-definition, hover, completion, and cross-file rename — plus a [Zed extension](extensions/axiom) that wires it up in one click. Generated `axiom.json` files reference a versioned `$schema` URL for autocompletion and inline errors in any JSON-schema-aware editor.
- **A small, focused command surface.** Seven focused commands: bootstrap (`init`), generate, push, check, format, and lint — each one doing exactly one thing well.

## Fast by design

Axiom is built around the philosophy that **code generation should never slow you down**.

- **Rust, end to end.** The entire pipeline — parsing, analysis, and codegen — is native code with no interpreter overhead.
- **BLAKE3 content hashing.** Configuration and every resolved input are hashed with BLAKE3, so unchanged work is detected instantly and reliably.
- **Zero-copy caching.** The build cache is serialized with `rkyv` and memory-mapped back with `memmap2`, so a cache hit is a handful of pointer reads — no parsing, no deserialization.
- **Atomic cache writes.** Cache updates are written to a temporary file and renamed into place, so interrupted runs never corrupt state.
- **Sub-millisecond no-ops.** When nothing changed, Axiom reports "Everything up to date" in well under a millisecond and exits.

The net effect: repeated runs cost microseconds, letting generation be invoked freely in watch modes, pre-commit hooks, and CI without noticeable overhead.

## Made for monorepos

Monorepos multiply the pain of database-driven development — many services, many schemas, many languages, all sharing one repository. Axiom is designed around that reality:

- **Per-directory configuration.** Each package or service owns an `axiom.json` in its own directory, auto-detected without global state.
- **Glob-driven inputs.** Schema and `.axm` model files are resolved with flexible glob patterns, so your inputs stay aligned with your directory layout.
- **Independent, hashed caching.** Every project caches against its own config and inputs; changes in one directory never invalidate another, keeping incremental builds fast at monorepo scale.
- **Polyglot from one source.** The same SQL drives both TypeScript and Rust targets, so services written in different languages stay consistent without duplicated effort.
- **Predictable schema URLs.** Versioned `$schema` links mean every release's config format is pinned and verifiable across the whole repository.

## In your editor

The same compiler that powers `axiom check` runs inside your editor, so problems surface the moment you type — not in CI. `axiom-lsp` is a language server built on the identical parsing, resolution, and validation engine as the CLI.

- **Diagnostics as you type** — parse errors, unresolved tables and columns, missing models, bad placeholders, and every rule the checker reports.
- **Go to definition** — jump from a table or column in a query to its `CREATE TABLE`, or from a model type to its declaration.
- **Hover** — column types, nullability, primary keys, and model fields.
- **Completion** — tables after `FROM`/`JOIN`/`UPDATE`, columns after `alias.`, model types, and validator chains.
- **Rename** — rename a table, column, or model across every referencing file.
- **Formatting** — format a buffer with the same engine as `axiom format`.

A [Zed extension](extensions/axiom) is bundled for one-click setup; any editor that speaks LSP (VS Code, Neovim, Helix, and more) can point at the same server.

## Features

- Single-binary CLI with seven focused commands: initialize a project, generate typed clients, push a schema to a database, verify correctness, format, lint, and print the config schema.
- TypeScript and Rust code generation from SQL schemas and `.axm` models and queries.
- Typed, async query functions generated from `query` declarations in `.axm` files, with **named parameters** (`$email`) or positional placeholders (`$1`), row-return contracts (`-> T`, `-> T?`, `-> T[]`), and return types resolved against tables and models.
- Compiler-grade `check` with diagnostics for unresolved tables/columns, missing models, bad placeholders, return-contract mismatches, and invalid return types — the exact same engine the editor runs.
- A **Language Server** (`axiom-lsp`) with diagnostics, completion, hover, go-to-definition, rename, and formatting, plus a **Zed extension**.
- Deterministic `format` for `.axm` models and SQL inputs, and static-analysis `lint` rules.
- Field and parameter validation rules compiled into the output: `email`, `url`, `uuid`, `ulid`, `ipv4`, `ipv6`, `isodate`, `alphanumeric`, `nonempty`, `trim`, `lowercase`, `uppercase`, `min_length`, `max_length`, `min`, `max`, and custom `regex`.
- JSON Schema validation of `axiom.json` at load time, with colorized `miette` diagnostics on failure.
- Postgres schema synchronization with flexible URL resolution from CLI flags, `.env` files, and environment variables.
- Release artifacts signed with SHA-256 checksums for both Linux and Windows.

## Linting

`axiom lint` runs static-analysis rules over your `.axm` models and SQL schema inputs — including the SQL body of every `query` declaration. The following table reflects the **actual implementation** in `crates/axiom-lint`, as verified against the source and the test suite; it is not inferred from documentation.

Every rule is enabled by default and can be selected individually with `axiom lint --rules <name>`. Unknown rule names are silently dropped. There is no per-rule `lint` section in `axiom.json` yet; rules only have a severity baked in at compile time (`missing-where-clause` is an error, everything else is a warning). Lint results are cached in the content-addressed `ToolCache`. Note that the in-editor diagnostics come from `axiom check` — **lint rules are not surfaced by `axiom-lsp`**.

| Lint rule / feature | Description | Status | Configuration | Tests |
| ------------------- | ----------- | ------ | ------------- | ----- |
| `missing-where-clause` | Errors on `DELETE`/`UPDATE` without a `WHERE` clause, including inside `.axm` query bodies | ✅ Implemented | Enabled by default; `--rules missing-where-clause` | Unit (`crates/axiom-lint/src/rules/sql.rs`) + CLI integration (`cli_integration.rs`) |
| `unused-import` | Warns on `import { … } from "…"` names never used as a field/parameter/return type in the importing file; aliased imports matched by written name | ✅ Implemented | Enabled by default; `--rules unused-import` | Unit (`rules/axm.rs`, 3 tests) |
| `redundant-validator` | Warns on duplicate validators or bounds strictly weaker than one already on the same field (e.g. `.min(10) .min(5)`), with custom messages ignored for matching | ✅ Implemented | Enabled by default; `--rules redundant-validator` | Unit (`rules/axm.rs`, 3 tests) |
| `unindexed-foreign-key` | Warns on foreign-key columns not covered by any `CREATE INDEX` (or a `PRIMARY KEY`/`UNIQUE` on the column); runs over CREATE TABLE/CREATE INDEX statements | ✅ Implemented | Enabled by default; `--rules unindexed-foreign-key` | Unit (`rules/sql.rs`, 7 tests) |
| `dead-model` | Warns on a `model` (block or alias) never referenced by any model field, import, query or transaction parameter or return type, or a model-alias base anywhere in the workspace | ✅ Implemented | Enabled by default; `--rules dead-model` | Unit (`rules/axm.rs`, 3 tests) + integration (`check_integration.rs`) |
| `unused-query-param` | Warns on a declared `$param` never referenced by the query's SQL body, whether by name (`$email`), position (`$1`), or as a structured-path base (`$input.field`) | ✅ Implemented | Enabled by default; `--rules unused-query-param` | Unit (`rules/axm.rs`, 2 tests) + CLI integration (`cli_integration.rs`) |
| `unsatisfiable-validator` | Warns on validator combinations no value can satisfy: contradictory numeric bounds (`.min(10) .max(5)`), contradictory length bounds (`.min_length(10) .max_length(5)`), or `.nonempty()` with `.max_length(0)` | ✅ Implemented | Enabled by default; `--rules unsatisfiable-validator` | Unit (`rules/axm.rs`, 5 tests) |
| `missing-primary-key` | Warns on a `CREATE TABLE` that declares neither a `PRIMARY KEY` nor a `UNIQUE` constraint, leaving rows with no stable identity | ✅ Implemented | Enabled by default; `--rules missing-primary-key` | Unit (`rules/sql.rs`, 5 tests) |
| `dead-model` | Warns on models never referenced anywhere in the workspace: as a model field type, an import, a query return or parameter type, or (transitively) a type-alias base | ✅ Implemented | Enabled by default; `--rules dead-model` | Unit (`rules/axm.rs`, 4 tests) + integration (`check_integration.rs`, 3 tests) + CLI (`cli_integration.rs`) |
| `select-star` | Warns on unqualified `*` projections (`SELECT *`, `SELECT id, *`, `SELECT DISTINCT *`); each parsed SQL body is analyzed exactly once, so an `.axm` query body is never double-reported, `users.*` is not flagged, and comments/string literals containing `SELECT *` are ignored | ✅ Implemented | Enabled by default; `--rules select-star` | Unit (`rules/sql.rs`, 9 tests) + driver integration (`runner.rs`, 3 tests) + CLI (`cli_integration.rs`) |
| `naming-convention` | Warns when models/type aliases/queries are not PascalCase or fields/parameters are not camelCase, validating the whole identifier (no underscores, correct initial case); quoted field names that deliberately escape the identifier grammar are exempt | ✅ Implemented | Enabled by default; `--rules naming-convention` | Unit (`rules/axm.rs`, 5 tests) + CLI (`cli_integration.rs`) |

No additional lint rule names are referenced anywhere in the codebase or documentation, and no rule name is documented-but-unimplemented. Every rule in this table is also listed in [`docs/guide/lint.md`](docs/guide/lint.md).

### Correctness checks (`axiom-check`)

The lint rules above are line- or file-local. The two capabilities below are **workspace correctness errors** — a violation means the generated output will not compile — so they run under `axiom check`, not `axiom lint`. Both were surfaced by the architecture audit and are now implemented.

| Check | Description | Subsystem | Tests |
| ----- | ----------- | --------- | ----- |
| `target-excluded-reference` | A declaration emitted for a target references a model whose `@target(...)` omits that target, so the generated output names a type/`coerce` the target never emitted. Uses `emit_plan`'s per-target model set; type aliases (emitted for every target) and queries are checked too | `axiom-check` (error) | Integration (`crates/axiom-check/tests/check_integration.rs`: direct, transitive, alias, query, and same-target cases) |
| `duplicate-field` | A model declares the same field name twice; the resolver rejects duplicate declarations but never inspected a single model's field list | `axiom-check` (error) | Integration (`check_integration.rs`: duplicate and triple-duplicate cases) |

All six capabilities surfaced by the architecture audit — the two above plus `unused-query-param`, `dead-model`, `unsatisfiable-validator`, and `missing-primary-key` — are now implemented; none remain in the "missing" state. The remaining ideas below are heuristic or feature-level rather than clear-cut deterministic analysis.

### Potential future ideas

The following are plausible enhancements rather than clear-cut missing static analysis, because they are either configuration/transport features, heuristic, or depend on behavior outside the current parsed inputs:

- **Per-rule config and configurable severities** — there is no `lint` section in `axiom.json`; rule selection is `--rules` only and unknown names are dropped silently. A `lint: { rules: { "…": "error" | "warn" | "off" } }` block would make severities and defaults author-controlled.
- **Surface lint rules in `axiom-lsp`** — editor diagnostics currently come only from `axiom check`; lint findings never reach the editor.
- **`optional-field-with-default`** — `field?: T = value` is contradictory (a missing field is filled from the default, so it is never absent); depends on confirming the emitted coercion order in both codegen targets.
- **`redundant-transform`** — repeated idempotent transforms such as `.trim() .trim()`.
- **`destructive-schema-change`** — `DROP TABLE`/`DROP COLUMN` in schema inputs, which `axiom push` applies to the database with no migration tracking or confirmation.
- **`case-collision`** — identifiers differing only by case across files (`User` vs `user`), which Axiom's deliberately case-sensitive namespaces accept but humans frequently confuse.
- **`unbounded-select`** — a `-> T[]` contract whose body has no `LIMIT`/`ORDER BY`, a pagination and memory footgun.
- **`reserved-word-identifier` and `duplicate-index`** — schema-level hygiene that needs a reserved-word table / index-signature comparison.
- **Schema-evolution diffing** — `axiom push` re-applies schema files with no migration history or drift detection; a `diff`/`migrate` workflow is a larger feature, not a rule.

## Install

Prebuilt binaries are published for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc` on every release.

- **GitHub Releases** — download the latest archive (`axiom-v<version>-<target>.tar.gz` or `.zip`) for your platform, verify its `.sha256` checksum, and place the `axiom` binary on your `PATH`.
- **proto** — if you use the [proto](https://moonrepo.dev/docs/proto) version manager, Axiom is available as a proto plugin through the bundled `axiom-plugin.toml`, which wires downloads, checksums, and version resolution to GitHub Releases.

## Build from source

Requires Rust (edition 2024 toolchain).

```sh
git clone https://github.com/FlowUp-Official/axiom.git
cd axiom
cargo build --release
```

The compiled binary is written to `target/release/axiom` (`axiom.exe` on Windows). To run the test suite:

```sh
cargo test
cargo clippy --all-targets
```

For editor integration, build the language server with `cargo build -p axiom-lsp --release` (writes `target/release/axiom-lsp`) and add it to your `PATH`, or use the bundled [Zed extension](extensions/axiom).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
