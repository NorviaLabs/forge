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

- `WorkspaceFileOps` and `git_review` confine paths to a canonicalized
  workspace root and reject `..` and absolute escapes. `WorkspaceFileOps`
  resolves symlinked ancestors with `canonicalize()` and accepts them when the
  target lies inside the root; `git_review` rejects symlinks on the relative
  path, after canonicalizing an absolute input.
  `file_context::build_attachment_text` reads the path it is given with no
  confinement check.
- `LocalGit` operations are scoped to a validated worktree root and never
  accept arbitrary Git argv; external processes are started without a shell.
  The `gh.rs` helpers run `git -C <workspace>` on the caller's path without a
  `LocalGit` root check.
- Long-running Git work in `git_status` and `git_sync` (`pull`, `push`,
  `fetch`, status refresh, diffs) runs on worker threads behind a channel, and
  the caller polls. Every `gh.rs` function runs synchronously on the caller's
  thread: each `gh` call is bounded by a 30 s timeout, `wait_for_checks`
  sleeps 10 s between polls, and `push_branch` runs `git push` inline.
- Read-only GitHub discovery stays separate from write workflows: `github.rs`
  only reads; `gh.rs` performs mutations through explicit command functions.
- No credentials are read or stored here; authentication belongs to the `gh`
  CLI and Git.

## Side Effects

- Spawns `git` and `gh` subprocesses (including network-reaching `push`,
  `pull`, `fetch`, and `gh` API calls).
- Mutates the worktree: creates/renames/deletes/trashes entries, stages,
  commits, merges, restores, and discards hunks.
- Trashing moves the entry out of the workspace into `~/.Trash` (macOS) or
  `$XDG_DATA_HOME/Trash/files` (Linux), creating that directory if needed.
- `git_review::discard_hunk` writes a temporary patch file under
  `std::env::temp_dir()` and removes it afterward.
- Spawns background threads holding subprocess handles and channels.

## Verification

- Run: `cargo test --package forge-workspace --locked`
