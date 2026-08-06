# Configuration

Axiom is configured through an `axiom.json` file, one per project directory.
The file is validated against a generated JSON schema at load time, so invalid
configuration fails fast with a readable diagnostic instead of a runtime
surprise.

## The `$schema` key

The first key in an `axiom.json` is usually `$schema`, which points at the JSON
schema for the running Axiom version:

```json
{
  "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json",
  "project": { "name": "api", "dialect": "postgres" }
}
```

This gives you autocompletion, hover documentation, and inline validation in
**VS Code**, **Neovim**, and any other editor with JSON Schema support.

- Schema URLs are version-pinned (`v<version>`), so every release has a stable,
  immutable schema.
- The `$schema` key is optional and is skipped when absent.

## Minimal configuration

The config is auto-detected in the current directory, or selected explicitly
with `--config <PATH>`.

## Full reference

An `axiom init` template looks like this:

```json
{
  "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json",
  "project": { "name": "my-project", "dialect": "postgres" },
  "cache": { "enabled": true, "path": ".axiom.cache" },
  "inputs": {
    "schema": ["./schema.sql"],
    "queries": ["./queries/**/*.sql"]
  },
  "validation": { "on_error": "fail" },
  "outputs": {
    "api": { "type": "typescript", "path": "./gen/api.ts" },
    "core": { "type": "rust", "path": "./gen/api.rs" }
  }
}
```

### `project`

| Field     | Type   | Description                               |
| --------- | ------ | ----------------------------------------- |
| `name`    | string | Project name, used in output and messages |
| `dialect` | string | SQL dialect, e.g. `postgres`              |

### `cache`

| Field     | Type    | Default          | Description                       |
| --------- | ------- | ---------------- | --------------------------------- |
| `enabled` | boolean | `true`           | Enable the incremental build cache |
| `path`    | string  | `".axiom.cache"` | Cache file location                |

The cache stores BLAKE3 digests of the config and every input file. When all
digests match, generation is skipped entirely. See
[Performance](/guide/performance).

### `inputs`

| Field    | Type     | Description                                   |
| -------- | -------- | --------------------------------------------- |
| `schema` | string[] | Glob patterns for schema SQL files            |
| `queries`| string[] | Glob patterns for annotated query SQL files   |

Paths are resolved relative to the directory containing the config file.

### `validation`

| Field     | Type   | Description                                        |
| --------- | ------ | -------------------------------------------------- |
| `on_error`| string | Failure policy for validation, e.g. `fail`         |

### `outputs`

A map of target names to output configurations. Each entry selects a generator
with `type` and an output location with `path`:

| `type`       | Description                                        |
| ------------ | -------------------------------------------------- |
| `typescript` | Emit a TypeScript module using the `postgres` driver |
| `rust`       | Emit a Rust module using `sqlx`                    |

## Schema validation

Every loaded `axiom.json` is validated against the generated JSON schema before
it is used. Common failure modes include:

- Wrong types, e.g. `"enabled": "yes"` instead of `true`.
- Unknown output types.
- Missing required sections.

Failures render as colorized diagnostics (code `axiom::config::validation_failed`)
pointing at the offending keys.
