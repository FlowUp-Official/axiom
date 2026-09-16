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
| `.axm` resolution | Model/type files parse; imports resolve; no duplicate names; no import cycles; `.regex(...)` patterns compile; every query placeholder is declared | `check.axm-parse`, `check.model-resolution`, `check.duplicate-model`, `check.import-cycle`, `check.regex-invalid`, `check.query-placeholder`, `check.model` |
| Query ↔ schema | Query bodies parse; referenced tables and columns exist; return types resolve to a table, model, or type alias; the SQL body honors the declared return contract | `check.query-sql`, `check.missing-table`, `check.missing-column`, `check.query-return-type`, `check.query-contract` |
| Generated output | The output that `axiom generate` would write exists and matches what is on disk | `check.output-missing`, `check.output-outdated`, `check.output-unreadable` |

`check.model` is the fallback code for model-file errors that don't map to a
more specific variant; the specific codes above (`check.axm-parse`,
`check.model-resolution`, `check.duplicate-model`, `check.import-cycle`,
`check.regex-invalid`) are used whenever the cause is known.

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
keyed by each `.axm` file's content hash plus the aggregate schema hash. Editing
a schema invalidates stale query results while untouched files stay cached; the
generated-output comparison is always recomputed so it can never go stale.
