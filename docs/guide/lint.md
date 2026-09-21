# `axiom lint`

`axiom lint` runs a set of static-analysis rules over your `.axm` models and
SQL schema inputs — including the SQL body of every `query` and `transaction`
declaration — and reports problems before they reach the database or your
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

SQL rules run over `.sql` schema inputs and the SQL body of every `query`
and `transaction` declaration in a `.axm` file:

| Rule | Severity | What it reports |
| ---- | -------- | --------------- |
| `missing-where-clause` | error | `DELETE` (or `UPDATE`) without a `WHERE` clause — the classic footgun that wipes every row |
| `select-star` | warning | An unqualified `SELECT *` (including `SELECT DISTINCT *` and `SELECT id, *`) selects every column; explicit column lists are clearer and pin the contract. Each SQL body is analyzed exactly once, and `users.*` is not reported |
| `unindexed-foreign-key` | warning | Foreign-key columns that have no matching index, which slows down joins and cascades. A column-level `PRIMARY KEY`/`UNIQUE` (or a table-level constraint over the column) also counts as indexing it |
| `missing-primary-key` | warning | A `CREATE TABLE` that declares neither a `PRIMARY KEY` nor a `UNIQUE` constraint, leaving rows with no stable identity |

### `.axm` rules

| Rule | Severity | What it reports |
| ---- | -------- | --------------- |
| `unused-import` | warning | An `import` whose names are never referenced by a model, field, transaction parameter, or return type; aliased imports are matched by the name written at the use site |
| `dead-model` | warning | A model that is never referenced anywhere in the workspace — by a model field, an import, a query or transaction parameter or return type, or a type-alias base (directly or transitively) |
| `redundant-validator` | warning | A duplicate validator, or a bound strictly weaker than one already established on the same field (e.g. `.min(10) .min(5)`); custom messages are ignored when matching |
| `unsatisfiable-validator` | warning | Validator combinations no value can satisfy: contradictory numeric bounds (`.min(10) .max(5)`), contradictory length bounds (`.min_length(10) .max_length(5)`), or `.nonempty()` with `.max_length(0)` |
| `naming-convention` | warning | Models, type aliases, queries, and transactions that are not PascalCase, or fields/parameters that are not camelCase; the whole identifier is validated (`user_name` is reported, not just a bad first letter). Quoted field names are exempt |
| `unused-query-param` | warning | A query or transaction parameter that the declaration's SQL body never references, whether by name (`$email`), position (`$1`), or as a structured-path base (`$input.field`) |

## Example

```text
$ axiom lint
models/user.axm: error[lint.missing-where-clause]:
  `delete` without a `WHERE` clause will delete every row
  (add a `WHERE` clause, or explicitly guard it with `WHERE true` if intended)
models/user.axm: warning[lint.select-star]:
  `SELECT *` selects every column; list columns explicitly
1 warning, 1 error found
```

## Caching

Rule results are cached in the configured `ToolCache` keyed by a hash of the
rule plus the file's content, so a re-run after fixing one file only
re-analyzes that file.
