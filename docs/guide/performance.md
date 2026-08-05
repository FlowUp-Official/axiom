# Performance

Axiom is built around the philosophy that code generation should never slow you
down. Repeated runs are designed to be effectively free.

## Native pipeline

The entire toolchain — SQL tokenizing, catalog building, query parsing, and code
generation — is native Rust. There is no interpreter, no virtual machine, and no
runtime language dependency in the generation path.

## BLAKE3 content hashing

Every run hashes the configuration file and every resolved input file with
[BLAKE3](https://github.com/BLAKE3-team/BLAKE3), a SIMD-accelerated hash that
is substantially faster than SHA-256. Because hashing covers *content* (not
timestamps or sizes), an unchanged input is detected reliably regardless of
file system metadata changes.

## Zero-copy caching

The cache manifest is serialized with [rkyv](https://github.com/rkyv/rkyv)
(rendered, zero-copy) and memory-mapped back with
[memmap2](https://github.com/memmap2/memmap2):

- A cache hit is a handful of pointer reads over a memory-mapped region.
- No JSON or binary deserialization, no parsing.
- The archive is validated as a well-formed rkyv buffer before use, so a
  corrupted cache can never crash generation — it simply misses.

## Atomic writes

Cache updates are written to a temporary sibling file and atomically renamed
into place. An interrupted run can never leave a half-written cache behind.

## What that means in practice

- **Sub-millisecond no-ops.** When nothing changed, Axiom reports
  `Everything up to date` and exits in well under a millisecond.
- **Incremental rebuilds are cheap.** A single-file edit re-hashes the touched
  inputs and regenerates only what actually changed.
- **Safe to run everywhere.** Generation can be invoked freely in watch modes,
  pre-commit hooks, and CI without noticeable overhead.

## Measuring

Run `axiom generate` twice on an unchanged tree and observe the second run's
`(<0.5ms)` cache-hit timing. Add files, change a column, or bump the config, and
the cache invalidates by digest automatically.
