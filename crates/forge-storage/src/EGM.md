# forge-storage source

## Overview

Single owner of *where* Forge runtime state lives, plus the git worktree
lifecycle used for session/subagent isolation. Repository-local storage under
`.forge/local/` when Git is available and can be safely excluded, otherwise
the platform application-data directory. Project-owned `.forge/` content
(`rules/`, `agents/`, `skills/`, `workflows/`) is deliberately untouched.

## Interface

`resolver.rs`:

- `EXCLUDE_PATTERN: &str = "/.forge/local/"`.
- `StorageMode::{RepositoryLocal, ApplicationData}`.
- `RuntimeDataKind` (`#[non_exhaustive]`): `Control`, `Session`, `Lifecycle`, `Queue`, `UiState`, `Cache`, `Log`, `Index`, `Checkpoint`, `Attachment`, `Worktree`.
- `RuntimeStorage` trait: `repository_root`, `storage_mode`, `identity`, `fallback_reason`, `ensure_ready`, `root`, `path_for(kind)`, `path_for_read(kind)`.
- `LocalRuntimeStorage::new(workspace)` — one worktree; also `workspace()`, `migration_report()` (empty until setup has run), and `tracked_migration_conflicts()` (sources left in place because Git tracks them).
- `RepositoryRuntimeStorage::new(workspace)` + `main_worktree()` — resolves Git's main worktree and delegates, so every linked worktree shares one roster and journal root.
- `RuntimeIdentity` (`as_str()`), `StorageError` (`NoRoot`, `PathEscapesRoot`, `ExcludeSetupFailed`, `Io`, `NoAppDataDir`, `RepositoryTopology`).

`git.rs`: `GitTopology::{Normal, LinkedWorktree, Bare, NotAGitRepo}`, `RepoInfo { topology, worktree_root, git_common_dir, git_dir }`, `detect_repo_info(&Path) -> RepoInfo`.

`exclude.rs`: `ExcludeError`, `resolve_exclude_path(workspace)`, `ensure_managed_block(exclude_path, pattern)`, `has_managed_block(exclude_path, pattern)`.

`migrate.rs`: `MigrationOutcome::{Migrated, NotPresent, Tracked, DestinationCollision, Failed}`, `MigrationRecord { source, destination, outcome, detail }`, `migrate_legacy_runtime_files(workspace) -> Vec<MigrationRecord>`.

`worktree.rs` (also exposed as the public module `forge_storage::worktree`): `WorktreeError` (`AddFailed`, `BranchCreateFailed`, `RemoveFailed`, `ListFailed`, `Dirty`, `NotRegistered`, `BranchMismatch`, `DetachedHead`, `Io`), `WorktreeRecord { path, branch, head, prunable }`, `SubagentWorktree { path, branch }`, `DetachedWorktree { path, base_sha }`; `main_worktree`, `create_session_worktree`, `create_detached_session_worktree`, `create_worktree`, `list_worktrees`, `list_worktree_records`, `worktree_head`, `worktree_is_dirty`, `materialize_branch`, `branch_exists`, `session_branch_slug`, `remove_worktree`, `remove_clean_worktree`, `remove_clean_worktree_if_branch`, `remove_clean_worktree_if_detached`, `remove_dirty_worktree`.

## Invariants

- The exclude pattern is deliberately narrow (`/.forge/local/`): excluding all of `.forge/` would hide project-owned rules/agents/skills/workflows.
- Git topology is discovered only through `git rev-parse` plumbing, never by assuming `.git` is a directory (it may be a file in worktrees/submodules).
- The managed exclude block is idempotent: unrelated lines are preserved verbatim, a missing trailing newline is tolerated, duplicate Forge blocks are reconciled into one, and the write is atomic (temp file in the same directory, then rename). `.gitignore` is never modified.
- Storage creation is lazy: `LocalRuntimeStorage::new` only detects, and nothing is created on disk until `ensure_ready`/`root`/`path_for` runs.
- `path_for_read` computes the path from topology alone and creates nothing, for callers that must not trigger initialization.
- `storage_mode` reflects the *desired* mode before setup; a desired `RepositoryLocal` may still fall back to `ApplicationData` when `ensure_ready` fails, and `fallback_reason` is set only in that case.
- `RuntimeIdentity` hashes the git common dir plus worktree root (falling back to the workspace path) and appends a label, so unrelated repos with the same basename and distinct linked worktrees never collide in a shared fallback directory.
- Migration is conservative and idempotent: only the fixed `CANDIDATES` list, only untracked legacy paths, and never over existing destination content (`DestinationCollision`). `fs::rename` failure leaves the source untouched (`Failed`). Migration runs best-effort during repository-local setup and never blocks it; tracked legacy files are surfaced, never altered in the Git index.
- A missing legacy path resolves to `NotPresent` with a single `exists()` check and no Git subprocess.
- `session_branch_slug` reduces arbitrary (possibly model-authored) text to `[A-Za-z0-9_-]`, trims `-`, caps at 40 chars, and falls back to `"task"` — this is what prevents `../` or ref-reserved characters from escaping `base_dir` or producing an invalid ref.
- User-session cleanup helpers never pass `--force`; a dirty worktree is refused (`Dirty`) rather than discarded. `remove_dirty_worktree` is the separate explicit opt-in.
- A worktree removal requires the path to be registered with Git and, for the branch-checked variants, to be on the expected branch.
- `main_worktree` relies on `git worktree list` ordering (main worktree first).

## Side Effects

- Spawns `git` subprocesses (`rev-parse`, `worktree add/list/remove`, `ls-files`, `check-ignore`).
- Creates directories (`.forge/local/<kind>` or `<data_dir>/forge/<identity>`), sets the storage root to mode `0700` on Unix (best-effort), and writes the repository-local exclude file.
- Writes/moves files during migration.
- Creates and deletes git worktrees and branches on disk.

## Verification

- Run: `cargo test --package forge-storage --locked`
