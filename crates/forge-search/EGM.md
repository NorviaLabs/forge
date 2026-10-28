# forge-search

## Overview

Crate root for workspace indexing and search. No source code at this level:
all logic lives in `src/`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — `WorkspaceIndex` (find/grep/merged search over `fff-search`), Quick Open path scoring, and the serializable search result types.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — `WorkspaceIndex`,
  `WorkspaceIndexOptions`, `SearchError`, `merge_search_results`,
  `score_quick_open`/`rerank_quick_open_hits`, and the result types
  (`FileSearchHit`, `GrepSearchHit`, `FindResponse`, `GrepResponse`,
  `MergedSearch`, `GrepQueryMode`).

## Invariants

- Every query waits for the initial scan; a slow scan surfaces as
  `SearchError::ScanTimeout`, never as partial results.
- `relevance` is normalized against the top hit of the same result set.
- Path/include filters are applied before pagination.

## Side Effects

- Scans and (by default) watches the workspace filesystem via `fff-search`
  background threads; reads file contents for grep; mutates in-memory frecency
  state. No writes, network, or subprocesses.

## Notes

- Result types are `serde`-serializable because the agent tools and the TUI consume the same shapes.
- This crate has no dependency on session or model state; it only needs a workspace root.

## Verification

- Run: `cargo test --package forge-search --locked`
