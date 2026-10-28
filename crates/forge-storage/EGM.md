# forge-storage

## Overview

Crate root for runtime-storage resolution and git worktree management. No
source code at this level: all logic lives in `src/`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — `RuntimeStorage` resolver (repository-local `.forge/local/` vs application-data fallback), git topology/exclude handling, conservative legacy migration, and worktree lifecycle.

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — public symbols
  re-exported from `src/lib.rs` (`RuntimeStorage`, `LocalRuntimeStorage`,
  `RepositoryRuntimeStorage`, `RuntimeDataKind`, `StorageMode`,
  `RuntimeIdentity`, `StorageError`, `EXCLUDE_PATTERN`, `forge_data_dir`, git
  topology, exclude, migration, and the public `worktree` module).
- `forge_data_dir() -> Option<PathBuf>` — the application-data root:
  `$FORGE_DATA_DIR` when set and non-empty, else `<platform data dir>/forge`.
  The repo's `.cargo/config.toml` sets `FORGE_DATA_DIR` to `target/forge-data`
  for `cargo test`/`cargo run`; an exported value still wins.

## Invariants

- Runtime state is resolved through `RuntimeStorage` only; the exclude pattern
  covers `/.forge/local/` and never the project-owned remainder of `.forge/`.
- The `remove_clean_worktree*` helpers never discard uncommitted work (they
  refuse with `Dirty`). `remove_worktree` and its alias `remove_dirty_worktree`
  always pass `--force` and discard it. No removal deletes the branch.

## Side Effects

- Spawns `git` subprocesses, creates runtime directories (mode `0700` on Unix),
  writes the repository-local Git exclude file, moves legacy runtime files,
  creates/removes git worktrees, and creates (never deletes) branches. See [`./src/EGM.md`](./src/EGM.md).

## Notes

- No other component may construct `.forge/local/...` or fallback paths independently; everything goes through `RuntimeStorage::path_for`.
- This module knows *how* to create/remove worktrees but not *where* they live — callers resolve the base directory via `RuntimeDataKind::Worktree`.
- Merging a finished subagent branch back into the parent is out of scope by design.

## Verification

- Run: `cargo test --package forge-storage --locked`
