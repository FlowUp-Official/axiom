---
layout: home

hero:
  name: Axiom
  text: Code generator for SQL schemas and queries
  tagline: Type-safe TypeScript and Rust clients generated from your SQL, with sub-millisecond incremental rebuilds — built for large monorepos.
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: View on GitHub
      link: https://github.com/FlowUp-Official/axiom

features:
  - title: Fast by design
    details: A native Rust pipeline, BLAKE3 content hashing, and a zero-copy rkyv/memmap2 cache keep unchanged runs well under a millisecond.
  - title: Polyglot from one source
    details: The same SQL schemas and query files generate consistent, type-safe TypeScript and Rust client code.
  - title: Monorepo-friendly
    details: Per-directory configuration, glob-driven inputs, and independent hashed caching keep large repositories fast.
  - title: Declarative validation
    details: Email, UUID, range, length, regex, and normalization rules declared in SQL comments are compiled straight into the generated code.
  - title: IDE-grade config
    details: axiom.json is validated against a versioned JSON schema with autocompletion and inline errors in VS Code and Neovim.
  - title: Database sync
    details: Push schemas to Postgres so your live database and generated clients never diverge from the source of truth.
---
