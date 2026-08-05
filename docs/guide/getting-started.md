# Getting Started

Axiom is a single-binary CLI that turns SQL schemas and annotated query files
into type-safe, validated client code for **TypeScript** and **Rust**, and keeps
Postgres databases in sync with your source of truth.

## Install

### Prebuilt binaries

Binaries are published for `x86_64-unknown-linux-gnu` and
`x86_64-pc-windows-msvc` on every [release](https://github.com/FlowUp-Official/axiom/releases):

- Download the archive for your platform (`axiom-v<version>-<target>.tar.gz` or `.zip`).
- Verify its `.sha256` checksum file.
- Extract and place the `axiom` binary on your `PATH`.

### proto

If you use the [proto](https://moonrepo.dev/docs/proto) version manager, Axiom
is available as a proto plugin. The bundled `axiom-plugin.toml` wires archive
downloads, SHA-256 checksum verification, and GitHub tag version resolution.

### Build from source

Requires a recent Rust toolchain (edition 2024).

```sh
git clone https://github.com/FlowUp-Official/axiom.git
cd axiom
cargo build --release
```

The binary is written to `target/release/axiom` (`axiom.exe` on Windows).

## Quick start

Axiom bootstraps a fully-populated, schema-validated configuration for you:

```sh
axiom init
```

This writes an `axiom.json` into the current directory with sensible defaults,
including a versioned `$schema` URL so your editor can autocomplete the file
immediately. Rerunning `axiom init` refuses to overwrite an existing file unless
you pass `--force`.

From there, drop a `schema.sql` beside it, and run:

```sh
axiom generate
```

Generated clients appear at the output paths configured in `axiom.json`
(`gen/api.ts` and `gen/api.rs` by default).

## Core workflow

1. **`axiom init`** — bootstrap an `axiom.json` with defaults.
2. **`axiom generate`** — compile SQL schemas and queries into typed clients.
3. **`axiom push`** — sync your schema to a Postgres database.
4. **`axiom schema`** — print the JSON schema for `axiom.json`.

See the [CLI Reference](/guide/cli) for the full command surface and
[Configuration](/guide/configuration) for the `axiom.json` format.
