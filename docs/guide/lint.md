# `axiom lint`

`axiom lint` runs a set of static-analysis rules over your schema, query, and
model files and reports problems before they reach the database or your
generated clients. Warnings do not fail the command; errors do.

```sh
axiom lint                      # run every configured rule
axiom lint --rules select-star  # run only the named rules
```

## Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | No lint errors (warnings are allowed) |
| `1` | One or more lint errors were found |

## Rules

Rules are grouped by the file type they analyze. Their names are stable and can
be passed individually to `--rules`.

### SQL rules

| Rule | Severity | What it reports |
| ---- | -------- | --------------- |
| `missing-where-clause` | error | `DELETE` (or `UPDATE`) without a `WHERE` clause — the classic footgun that wipes every row |
| `select-star` | warning | `SELECT *` selects every column; explicit column lists are clearer and pin the contract |
| `unindexed-foreign-key` | warning | Foreign-key columns that have no matching index, which slows down joins and cascades |

### `.axm` rules

| Rule | Severity | What it reports |
| ---- | -------- | --------------- |
| `unused-import` | warning | An `import` that is never referenced by any model |
| `dead-model` | warning | A model that is neither exported nor referenced by another model |
| `redundant-validator` | warning | A `.nonempty()` / `.min(..)`-style validator that is already implied by the type |

## Example

```text
$ axiom lint
queries/delete_all.sql: error[lint.missing-where-clause]:
  `delete` without a `WHERE` clause will delete every row
  (add a `WHERE` clause, or explicitly guard it with `WHERE true` if intended)
queries/list_users.sql: warning[lint.select-star]:
  `SELECT *` selects every column; list columns explicitly
1 warning, 1 error found
```

## Caching

Rule results are cached in the configured `ToolCache` keyed by a hash of the
rule plus the file's content, so a re-run after fixing one file only
re-analyzes that file.
