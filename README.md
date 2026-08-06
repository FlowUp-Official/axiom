# Axiom

> A high-performance code generator for SQL schemas and queries, built for large monorepos.

Axiom turns your SQL schema and query definitions into type-safe, validated client code — for both **TypeScript** and **Rust** — keeps the databases that mirror them in sync, and brings the whole compiler into your editor via a Language Server. It is written entirely in Rust.

## Why Axiom?

Hand-written database layers rot. SQL drifts from the application code that talks to it, validation logic gets reimplemented per service, and every new API endpoint means copy-pasting the same fragile types.

Axiom makes your database the **single source of truth**:

- **Declarative, validated configuration.** `axiom.json` is checked against a generated JSON schema at load time, so misconfiguration fails fast with a readable diagnostic — not a runtime surprise.
- **One schema, many languages.** Generate consistent, type-safe TypeScript and Rust client code from the same SQL inputs, so a table change propagates everywhere at once.
- **Embedded input validation.** Column and parameter rules (email, UUID, range, length, regex, normalization, and more) are declared in SQL comments and compiled straight into the generated code.
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
- **Glob-driven inputs.** Schema and query files are resolved with flexible glob patterns, so your inputs stay aligned with your directory layout.
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
- TypeScript and Rust code generation from SQL schemas and annotated query files.
- Typed, async query functions generated from `@fn` annotations, with **named parameters** (`$email`) or positional placeholders (`$1`), per-parameter `@validate` rules, and case-insensitive return-type resolution (`: users` → `Users`).
- Compiler-grade `check` with diagnostics for unresolved tables/columns, missing models, return-type mismatches, and invalid placeholders — the exact same engine the editor runs.
- A **Language Server** (`axiom-lsp`) with diagnostics, completion, hover, go-to-definition, rename, and formatting, plus a **Zed extension**.
- Deterministic `format` for `.axm` models and SQL inputs, and static-analysis `lint` rules.
- Column and parameter validation rules compiled into the output: `email`, `url`, `uuid`, `ulid`, `ipv4`, `ipv6`, `isodate`, `alphanumeric`, `trim`, `lower`, `upper`, `min_len`, `max_len`, `min`, `max`, and custom `regex`.
- JSON Schema validation of `axiom.json` at load time, with colorized `miette` diagnostics on failure.
- Postgres schema synchronization with flexible URL resolution from CLI flags, `.env` files, and environment variables.
- Release artifacts signed with SHA-256 checksums for both Linux and Windows.

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
