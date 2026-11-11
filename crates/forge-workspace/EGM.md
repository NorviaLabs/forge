# forge-workspace

## Overview

Workspace data services for Forge: Git status parsing, local Git operations
and review, active-file attachment context, and read/write GitHub
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

- `git_review` confines paths to a canonicalized workspace root and rejects
  `..` and absolute escapes; it rejects symlinks on the relative path, after
  canonicalizing an absolute input.
  `file_context::build_attachment_text` reads the path it is given with no
  confinement check.
- `LocalGit` operations are scoped to a validated worktree root and never
  accept arbitrary Git argv; external processes are started without a shell.
  The `gh.rs` helpers run `git -C <workspace>` on the caller's path without a
  `LocalGit` root check.
- Every `gh.rs` function runs synchronously on the caller's thread: each `gh`
  and local `git` call is bounded by a 30 s timeout and `push_branch`'s
  `git push` by 300 s (process group killed at the deadline).
- Read-only GitHub discovery stays separate from write workflows: `github.rs`
  only reads; `gh.rs` performs mutations through explicit command functions.
- No credentials are read or stored here; authentication belongs to the `gh`
  CLI and Git.

## Side Effects

- Spawns `git` and `gh` subprocesses (including network-reaching `push`,
  `pull`, `fetch`, and `gh` API calls).
- Mutates the worktree: stages, commits, merges, restores, and discards hunks.
- `git_review::discard_hunk` writes a temporary patch file under
  `std::env::temp_dir()` and removes it afterward.
- `gh.rs` spawns short-lived threads that drain each subprocess's stdout and
  stderr while the caller waits on the deadline.

## Verification

- Run: `cargo test --package forge-workspace --locked`
