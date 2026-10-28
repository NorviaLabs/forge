# forge-context

## Overview

Context-lifecycle crate (CTX-01/02): offloading oversized tool output, progress
checkpointing, skill discovery, and context compaction. It depends only on
`forge-types` and `forge-storage`, so the compaction rules stay pure and
reusable by frontends and tests. Source lives in `src/`.

## Subdirectories & Boundaries

- [src](./src/EGM.md) — `ContextEngine`, skill discovery, token estimation, and
  the compaction subsystem.

## Interface

- `ContextEngine`, `ContextConfig`, `OffloadResult`, `ContextError`,
  `SkillManifest`, `discover_skills`, `estimate_tokens`,
  `estimate_messages_tokens`, `reduction_ratio` ([src](./src/EGM.md)).
- `compaction` module: `Checkpoint`, `plan_compaction`, `CompactionPolicy`,
  `SessionContextState`, protected facts, and tail selection
  ([src/compaction](./src/compaction/EGM.md)).

## Invariants

- The compaction module is pure (no I/O); the journaled install lives in
  `forge-core`.
- Token estimation is a ~4-characters-per-token heuristic that returns at
  least 1.
- Default storage paths route through `forge-storage`'s runtime-storage
  resolver; explicit overrides are used verbatim.

## Side Effects

- `ContextEngine` writes offloaded tool bodies and the progress document, and
  reads `AGENTS.md` and `SKILL.md` files from the workspace and the global
  skills directory.
- Reads the `FORGE_GLOBAL_SKILLS_DIR` environment variable.

## Verification

- Run: `cargo test --package forge-context --locked`
