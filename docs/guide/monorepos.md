# Monorepos

Monorepos multiply the pain of database-driven development: many services, many
schemas, many languages, all sharing one repository. Axiom is designed around
that reality.

## Per-directory configuration

Each package or service owns its own `axiom.json` in its directory. There is no
global state — Axiom auto-detects the configuration in the current directory,
or you point any command at a specific file with `--config <PATH>`.

```text
repo/
├── services/
│   ├── api/
│   │   ├── axiom.json
│   │   └── schema.sql
│   └── worker/
│       ├── axiom.json
│       └── schema.sql
├── packages/
│   └── shared/
│       ├── axiom.json
│       └── queries/
└── ...
```

Run `axiom generate` inside any of these directories and only *that* package is
processed, with its own inputs and outputs.

## Glob-driven inputs

Schema and query inputs are resolved with flexible glob patterns, keeping your
inputs aligned with your directory layout without hand-listing files:

```json
{
  "inputs": {
    "schema": ["./schema.sql", "./migrations/**/*.sql"],
    "queries": ["./queries/**/*.sql"]
  }
}
```

Paths resolve relative to the config file's directory, so patterns never depend
on where the command is invoked from.

## Independent, hashed caching

Every project caches against its own configuration and inputs:

- Digests are scoped per directory, so a change in one package never
  invalidates another.
- A wide, no-op sweep across many packages costs only hashing, because each
  package's cache hit is sub-millisecond.

This keeps incremental builds fast at monorepo scale, where a single `generate`
across dozens of packages would otherwise be prohibitively slow.

## Polyglot consistency

The same SQL drives both TypeScript and Rust targets. In a monorepo where
services are written in different languages, a schema change propagates to every
consumer at once — no duplicated effort, no drift between language bindings.

## Versioned schemas everywhere

Every release's `axiom.json` schema is pinned to a versioned `$schema` URL. The
whole repository can be configured against a known-good schema version, and
`axiom push` can sync each package's schemas to its own database independently.
