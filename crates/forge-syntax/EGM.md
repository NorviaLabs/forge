# forge-syntax

## Overview

Tree-sitter powered syntax highlighting for Forge. The crate parses source
text into styled spans and per-line highlighted segments, and detects a
language from file path, extension, or content. It has no terminal or UI
dependency: callers receive plain data (spans, RGB triples, bold/italic flags)
and render it themselves.

The crate root is a thin facade; all implementation lives in `src/`.

## Subdirectories & System Boundaries

- `src`: Component package holding language detection (`lang.rs`), tree-sitter
  queries (`queries.rs` plus `kotlin.scm` / `dockerfile.scm` assets), span
  highlighting (`highlight.rs`), and the process-global result cache
  (`cache.rs`). Contract: [`./src/EGM.md`](./src/EGM.md).
- `tests`: Integration tests only. No contract file is maintained here.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- Highlighting is a pure function of `(language, source text, theme)`; the only
  stateful layer is the memoisation cache.
- Unsupported or unknown languages never fail the render path: they degrade to
  a single default-styled span covering the whole input.
- Grammar and highlight-query support is fixed at compile time by the
  tree-sitter grammar crates listed in `Cargo.toml`; adding a language requires
  a grammar dependency, a `SyntaxLanguage` variant, a query source, and an
  extension/shebang entry.

## Side Effects

- A process-global, lock-protected highlight cache (bounded by total bytes,
  LRU-evicted) retains computed segments for the life of the process.
- No filesystem writes, network calls, or subprocesses.

## Verification

- Run: `cargo test --package forge-syntax --locked`
