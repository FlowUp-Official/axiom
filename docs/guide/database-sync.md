# Database Sync

`axiom push` synchronizes your schema SQL files to a target Postgres database,
executing them as a batch on a single connection.

```sh
axiom push --db-url postgres://user:pass@host:5432/db
```

Schema files are resolved from the `inputs.schema` globs in `axiom.json`, in
configured order. If no schema files match, the command reports a notice and
does nothing rather than failing.

## URL resolution

The database URL is resolved in a strict order, so an explicit value always wins
and environment-based configuration is the fallback:

1. `--db-url` CLI flag.
2. `DATABASE_URL` environment variable.
3. `--env-file <FILE>` dotenv file, if provided (loaded before resolution).

If no URL can be resolved, Axiom fails with a diagnostic (code
`axiom::push::db_error`) that lists both the `--db-url` flag and the
`DATABASE_URL` variable, so the fix is obvious.

## Usage in CI and monorepos

Because URL resolution is environment-driven, you can keep credentials out of
`axiom.json` entirely:

```sh
DATABASE_URL=postgres://... axiom push
```

Or load them from a per-environment dotenv file that is not committed:

```sh
axiom push --env-file .env.production
```

The push pipeline only touches the schemas declared for the directory it runs
in, so different packages in a monorepo can sync independently.
