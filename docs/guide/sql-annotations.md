# SQL Annotations

Axiom reads validation rules directly from SQL comments. Annotations attach to
the column definition they precede, and are compiled into the generated
TypeScript and Rust clients as real, runnable validation code — no runtime
config, no duplicated logic.

## Column annotations

Place a `-- @validate` comment on the line directly above a column:

```sql
CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    -- @validate email[msg="Bad Email"], min_len=3[msg="Too short"], trim, lower
    email VARCHAR(255) NOT NULL,
    -- @validate uuid
    external_id UUID,
    -- @validate trim, lower, alphanumeric
    username VARCHAR(32)
);
```

Rules are a comma-separated list. Each rule can carry a custom failure message
in brackets: `rule[msg="..."]`. A trailing `msg="..."` segment (with no rule
name) sets the fallback message for rules that do not define their own.

`-- @override` is accepted as an alias for `-- @validate`.

## Rule reference

| Rule            | Example                          | Description                              |
| --------------- | -------------------------------- | ---------------------------------------- |
| `email`         | `email`                          | Must be a valid email address            |
| `url`           | `url`                            | Must be a valid URL                      |
| `uuid`          | `uuid`                           | Must be a valid UUID                     |
| `ulid`          | `ulid`                           | Must be a valid ULID                     |
| `ipv4`          | `ipv4`                           | Must be a valid IPv4 address             |
| `ipv6`          | `ipv6`                           | Must be a valid IPv6 address             |
| `isodate`       | `isodate`                        | Must be an ISO 8601 date                 |
| `alphanumeric`  | `alphanumeric`                   | Must contain only letters and digits     |
| `trim`          | `trim`                           | Trim surrounding whitespace              |
| `lower`         | `lower`                          | Normalize to lowercase                   |
| `upper`         | `upper`                          | Normalize to uppercase                   |
| `min_len`       | `min_len=3`                      | Minimum string length                    |
| `max_len`       | `max_len=255`                    | Maximum string length                    |
| `min`           | `min=0`                          | Minimum numeric value                    |
| `max`           | `max=100`                        | Maximum numeric value                    |
| `regex`         | `regex="^[a-z]+$"`               | Must match the custom regular expression  |

Rule names are case-insensitive, and a few have short aliases: `minlen` /
`maxlen`, `lowercase` / `uppercase`, `iso_date`, and `alnum`.

## Custom messages

Each rule can override its default message, and a line-level fallback covers
everything else:

```sql
-- @validate email[msg="Please enter a valid email"], min_len=5[msg="Too short"], msg="Invalid value"
email VARCHAR(255) NOT NULL
```

- `email[msg="..."]` — message for the `email` rule only.
- A bare trailing `msg="..."` — fallback for every other rule on the line.
