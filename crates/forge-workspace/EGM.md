# forge-workspace

## Overview

Workspace data services for Forge: filesystem operations, Git status and
review, branch sync, active-file attachment context, and read/write GitHub
workflows via the `gh` CLI. The crate contains no terminal or rendering
concerns; a frontend consumes its data and decides how it looks.

## Subdirectories & System Boundaries

- `src`: Component package with the workspace modules. Contract:
  [`./src/EGM.md`](./src/EGM.md).

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- Every filesystem entry point is confined to a canonicalized workspace root;
  `..`, absolute escapes, and symlinked ancestors are rejected.
- Git operations are scoped to a validated worktree root and never accept
  arbitrary Git argv; external processes are started without a shell.
- Long-running Git work (`pull`, `push`, status refresh, diffs) runs on worker
  threads behind a channel, and the caller polls; nothing blocks a render
  frame.
- Read-only GitHub discovery stays separate from write workflows: `github.rs`
  only reads; `gh.rs` performs mutations through explicit command functions.
- No credentials are read or stored here; authentication belongs to the `gh`
  CLI and Git.

## Side Effects

- Spawns `git` and `gh` subprocesses (including network-reaching `push`,
  `pull`, `fetch`, and `gh` API calls).
- Mutates the worktree: creates/renames/deletes/trashes entries, stages,
  commits, merges, restores, and discards hunks.
- Spawns background threads holding subprocess handles and channels.

## Verification

- Run: `cargo test --package forge-workspace --locked`
