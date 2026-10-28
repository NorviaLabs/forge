# forge-types

## Overview

Shared data types for the Forge agent harness: messages, tool calls and
outputs, model responses, journal events, session/task identifiers, governance
payloads, plan and question tool schemas, and image references. The crate has
no behavior beyond small pure helpers and exists so every other crate agrees on
one wire and in-memory representation.

## Subdirectories & System Boundaries

- `src`: Component package with the core type module (`lib.rs`), image metadata
  (`image.rs`), and shared git facts (`git.rs`). Contract:
  [`./src/EGM.md`](./src/EGM.md).

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- Persisted and wire-visible enums use `snake_case` serialization; renames are
  forbidden where journals already contain the old tag (`SessionStatus` keeps
  `"session_status"`). Exception: `WaitReason` has no `rename_all`, so its
  variant tags serialize in PascalCase (`Approval`, `MissingConfiguration`).
- Types are `serde`-round-trippable unless explicitly documented otherwise;
  additive fields that need compatibility use `#[serde(default)]`.
- `TaskId`, `AttemptId`, `QueueItemId`, and `BackgroundTaskId` are `u64`
  newtypes and are never reinterpreted across domains. `SessionId` is a plain
  `Uuid` type alias, so the type system does not enforce this for it.
- Helpers (`strip_protocol_markers`, `title_from_prompt`,
  `session_name_from_prompt`, `is_readonly_git_subcommand`, image inspection)
  are pure and allocation-bounded.
- `is_readonly_git_subcommand` is deliberately conservative and shared by
  governance and transcript so their read/write judgments cannot drift.

## Side Effects

- No filesystem, network, subprocess, or global mutable state. The only clock
  read is `ProgressDocument::new`, which stamps `updated_at` with `Utc::now()`.

## Verification

- Run: `cargo test --package forge-types --locked`
