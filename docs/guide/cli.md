# CLI Reference

Axiom ships as a single binary with seven commands. A global `--config <FILE>`
flag lets you point any command at a specific `axiom.json` (it is auto-detected
in the current directory when omitted).

## `axiom init`

Bootstrap a new `axiom.json` in the current directory.

| Option           | Default      | Description                                             |
| ---------------- | ------------ | ------------------------------------------------------- |
| `-o, --output`   | `axiom.json` | Path to write the configuration file to                 |
| `-f, --force`    |              | Overwrite an existing configuration file                |

Writes a fully-populated configuration with sensible defaults, a versioned
`$schema` URL, and both a TypeScript and a Rust output target. Refuses to
overwrite an existing file unless `--force` is passed (diagnostic code
`axiom::config::already_exists`).

## `axiom generate`

Generate typed output from the configured inputs.

| Option                 | Description                                                        |
| ---------------------- | ------------------------------------------------------------------ |
| `--db-url`             | Database URL, used when generation needs a live database          |
| `--env-file <FILE>`    | Load environment variables from a dotenv file before running       |

Parses every input schema and query file, then writes the configured outputs.
When nothing has changed since the last run, generation is skipped entirely and
the run reports `Everything up to date` in under a millisecond.

## `axiom push`

Push your schema to a target database.

| Option                 | Description                                                        |
| ---------------------- | ------------------------------------------------------------------ |
| `--db-url`             | Database URL to push to                                            |
| `--env-file <FILE>`    | Load environment variables from a dotenv file before running       |

See [Database Sync](/guide/database-sync) for URL resolution order.

## `axiom schema`

Print the generated JSON schema for `axiom.json` to stdout. Useful for
integrating with tooling, editor plugins, or release packaging.

```sh
axiom schema > schemas/axiom.schema.json
```

## `axiom check`

Verify workspace correctness and generated-output synchronization. With `--fix`,
out-of-sync generated files are rewritten in place.

| Option  | Description                                            |
| ------- | ------------------------------------------------------ |
| `--fix` | Rewrite out-of-sync generated files on disk            |

See [Check](/guide/check).

## `axiom format`

Format `.axm` models and SQL inputs using canonical, deterministic rules.

| Option        | Description                                                |
| ------------- | ---------------------------------------------------------- |
| `--check`     | Only report files that would be reformatted; exit `2` if any |
| `FILE`...     | Format only the given files or globs, instead of the configured inputs |

See [Format](/guide/format).

## `axiom lint`

Run static-analysis rules over `.axm` models and SQL inputs.

| Option          | Description                     |
| --------------- | ------------------------------- |
| `--rules RULE`  | Only run the given rules (repeatable) |

See [Lint](/guide/lint).

## Exit codes and diagnostics

Axiom exits with status `0` on success and `1` on any failure. `axiom format
--check` and `axiom check --fix` additionally use `2` to signal that files
*would* be reformatted or *were* fixed (the same convention as `cargo fmt
--check`). Failures are rendered as colorized
[miette](https://docs.rs/miette/latest/miette/) reports with stable diagnostic
codes, e.g.:

- `axiom::config::missing` — no `axiom.json` found.
- `axiom::config::validation_failed` — the config does not match the schema.
- `axiom::config::already_exists` — `axiom init` hit an existing file.
- `axiom::query::annotation_error` — a `-- @fn` line is malformed.
- `axiom::push::db_error` — a push failed.
- `check.output-outdated` — generated output is out of sync (see [Check](/guide/check)).
- `check.missing-table` / `check.missing-column` — query/schema mismatch.
- `format.would-reformat` — a file is not formatted (see [Format](/guide/format)).
- `lint.missing-where-clause` — a `DELETE`/`UPDATE` without `WHERE` (see [Lint](/guide/lint)).
