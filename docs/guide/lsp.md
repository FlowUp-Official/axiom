# Editor Integration (LSP)

`axiom-lsp` is a [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
implementation that brings Axiom's compiler checks straight into your editor.
It is built on the same parsing, resolution, and validation engine as the CLI,
so what you see in the editor is exactly what `axiom check` reports.

Supported features:

- **Diagnostics** — parse errors, unresolved tables/columns, missing models, and
  every rule the shared checker reports, updated as you type.
- **Go to definition** — jump from a table/column use in a query to its
  `CREATE TABLE` declaration, or from a model type to its declaration.
- **Hover** — column types, nullability, primary keys, and model fields.
- **Completion** — tables after `FROM`/`JOIN`/`UPDATE`, columns after
  `alias.`, model types and `.axm` validator chains.
- **Rename** — rename a table, column, or model across every file that
  references it.
- **Formatting** — format a buffer with the same engine as `axiom format`.

## Installation

Build the server from the repository:

```sh
cargo build -p axiom-lsp --release
```

The binary is `target/release/axiom-lsp`. Add it to your `PATH`, or point your
editor at the full path.

## Configuration

The server discovers `axiom.json` from the workspace root on startup and loads
every configured input (`schema`, `queries`, `models`), so cross-file
references resolve without opening each file. When no `axiom.json` is present
the server still reports single-file parse errors for open buffers.

The BLAKE3 `ToolCache` is reused: diagnostics are cached by content hash and
only the file that changed is re-analyzed, keeping keystroke latency in the
microsecond range for cached results.

## Zed

An extension lives at `extensions/axiom/`. To use it while developing:

1. `cargo build -p axiom-lsp` so `axiom-lsp` is on your `PATH` (or set the
   `AXIOM_LSP_BIN` environment variable to the built binary).
2. In Zed, run `zed: install dev extension` and select the `extensions/axiom`
   directory.

The extension maps `.sql` files to **Axiom SQL** and `.axm` files to
**Axiom Model**, both served by `axiom-lsp`. It expects the binary on the
worktree `$PATH` (extensions may not bundle language servers).

## Standalone (any LSP client)

Because `axiom-lsp` speaks plain LSP over stdio, it works with any client that
supports LSP: configure it as a custom language server for `.sql` and `.axm`
files with the command `axiom-lsp`.

## Incremental performance

`axiom-lsp` keeps a per-file analysis database. Editing a file re-parses only
that file, invalidates only the caches that depend on it (the catalog for
schema/query edits, the model registry for `.axm` edits), and republishes
diagnostics only for the files that can be affected. See the
[Performance](/guide/performance) page for the same design in the CLI.
