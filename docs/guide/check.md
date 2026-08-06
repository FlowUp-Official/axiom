# `axiom check`

`axiom check` verifies workspace correctness and keeps generated output in
sync with your sources, all in one command. It reuses the same BLAKE3
content-addressed cache as `axiom generate`, so repeat runs are fast.

```sh
axiom check              # verify the whole workspace
axiom check --fix        # rewrite out-of-sync generated files, then verify
```

## What it checks

| Phase | What is verified | Diagnostic codes |
| ----- | ---------------- | ---------------- |
| Schema parse | Every SQL schema input parses with the configured dialect | `check.sql-parse` |
| Query syntax | Every `-- @fn` annotation and query body parses | `check.query-annotation`, `check.query-sql` |
| Query ↔ schema | Referenced tables and columns exist; placeholders resolve to declared parameters; return types resolve to a table or model | `check.missing-table`, `check.missing-column`, `check.query-placeholder`, `check.query-return-type` |
| Models | Imports resolve, no duplicate models, no import cycles, models parse | `check.model-*` |
| Generated output | The output that `axiom generate` would write matches what is on disk | `check.output-outdated` |

## Generated-output synchronization

The synchronization phase recomputes every configured output in memory and
compares it byte-for-byte with the file on disk. Anything that differs is
reported as an `error[check.output-outdated]`:

```text
gen/api.ts: error[check.output-outdated]: generated output `api` is out of date
  (run `axiom generate`, or `axiom check --fix` to rewrite it)
```

Pass `--fix` to have `axiom check` rewrite the out-of-sync files itself:

```sh
axiom check --fix
# fixed 2 output files (run `axiom generate` next)
```

The rewrite is performed in memory and committed with an atomic rename, so a
failed run never leaves partially written output behind.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | All checks passed |
| `1` | One or more errors were found |
| `2` | `--fix` rewrote at least one out-of-sync output, and nothing else is broken |

## Caching

Query results are cached in the configured [`ToolCache`](/guide/configuration)
keyed by the query file's content hash plus the aggregate schema hash. Editing
a schema invalidates stale query results while untouched query files stay
cached; the generated-output comparison is always recomputed so it can never
go stale.
