# `axiom format`

`axiom format` rewrites `.axm` models and SQL inputs to a single canonical
style. Formatting is deterministic and idempotent: formatting an already
formatted file is a no-op, and identical input always produces identical
output. It is AST/token-based — never regex — so comments, strings, and
structure are preserved.

```sh
axiom format            # rewrite all configured inputs
axiom format --check    # report would-reformat files without touching them
axiom format path.axm   # format only the given files or globs
```

## Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Everything is formatted (or formatting was applied) |
| `1` | A file could not be read or parsed |
| `2` | `--check` found files that would be reformatted |

With `--check`, each unformatted file is reported as
`warning[format.would-reformat]` and the command exits `2` — the same
convention `cargo fmt --check` uses, so it drops straight into CI.

## `.axm` rules

- Two-space indentation.
- A block is laid out inline when it fits within 60 columns, otherwise each
  field moves to its own line.
- Calls with four or more arguments break onto continuation lines, one `.rule()`
  per line; calls with three or fewer stay inline.
- Transformations are listed before validations.
- Trailing whitespace is removed.

```text
# before
model Address { street: String .nonempty() .trim() city: String .nonempty() .trim() state: String }

# after
model Address {
  street: String.trim().nonempty()
  city: String.trim().nonempty()
  state: String
}
```

## SQL rules

- Curated keywords are uppercased (`SELECT`, `FROM`, `WHERE`, `INSERT`, ...);
  identifiers and strings keep their casing.
- Whitespace is normalized and trailing whitespace stripped.
- Blank lines collapse to a single separator.

```sql
-- before
select id, email from users where id > 5;

-- after
SELECT id, email
FROM users
WHERE id > 5;
```

## Idempotence in CI

Because the formatter is idempotent, `axiom format --check` is safe to gate
merges on: if it exits `0`, running `axiom format` changes nothing.
