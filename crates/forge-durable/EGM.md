# forge-durable

## Overview

Durable execution journal and crash recovery for Forge. One SQLite database per
session records append-only journal events; replay reconstructs the session's
messages, tool results, queue, background tasks, and pending approval state.
A separate durable audit table records governance and egress decisions.

## Subdirectories & System Boundaries

- `src`: Component package with the journal/replay engine (`lib.rs`) and the
  durable audit table (`audit.rs`). Contract: [`./src/EGM.md`](./src/EGM.md).
- `tests`: Integration tests only. No contract file is maintained here.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- Events are append-only: `seq` is monotonically increasing per database and a
  written event is never updated or deleted.
- Record-before-side-effect: callers must await the journal append before
  performing the model/tool side effect it describes.
- Replay is deterministic over the event stream; checkpoints are only an
  accelerator and never change the reconstructed state.
- A checkpoint or snapshot failure never fails an append or a user operation.
- Schema changes stay backward compatible: journal rows carry
  `schema_version`, and replay ignores unknown/unrelated data safely.
- The session database lives at `<dir>/<session_id>.db`; opening creates the
  directory and the database if missing.

## Side Effects

- Creates directories and SQLite database/WAL files on disk.
- Spawns a dedicated checkpoint thread per journal on first checkpoint need.
- Reads directory entries to find the latest session.
- No network access.

## Verification

- Run: `cargo test --package forge-durable --locked`
