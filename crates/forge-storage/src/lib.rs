//! Centralized runtime-storage resolver for Forge.
//!
//! Forge-generated runtime state (sessions, lifecycle/queue persistence, UI
//! state, caches, logs, indexes, checkpoints) must never appear as a
//! user-authored project change. This crate is the single owner of *where*
//! that state lives: repository-local at `.forge/local/` when a Git
//! repository is available and can be safely excluded via its native
//! exclude mechanism, falling back to the platform application-data
//! directory otherwise. No other component should construct these paths
//! independently — see [`RuntimeStorage`].
//!
//! A `.forge/local` that holds anything Git tracks is not used at all: the
//! exclude rule keeps Forge's files out of commits, but cannot stop someone
//! committing files there (`git add -f`), and a clone would then read them
//! as this machine's own sessions. Such a repository's runtime state goes
//! to application data instead, with a notice (see `local_dir`).
//!
//! The rest of `.forge/` (`rules/`, `agents/`, `skills/`, `workflows/`) is
//! project-owned and untouched by this crate — it stays Git-visible.

mod exclude;
mod git;
mod local_dir;
mod migrate;
mod resolver;
pub mod worktree;

pub use exclude::{ensure_managed_block, has_managed_block, resolve_exclude_path, ExcludeError};
pub use git::{detect_repo_info, tracked_files, GitTopology, RepoInfo};
pub use migrate::{migrate_legacy_runtime_files, MigrationOutcome, MigrationRecord};
pub use resolver::{
    forge_data_dir, LocalRuntimeStorage, RepositoryRuntimeStorage, RuntimeDataKind,
    RuntimeIdentity, RuntimeStorage, StorageError, StorageMode, EXCLUDE_PATTERN,
};
pub use worktree::{
    branch_exists, create_detached_session_worktree, create_session_worktree, create_worktree,
    head_is_on_a_branch, list_worktree_records, list_worktrees, main_worktree, materialize_branch,
    project_root, remove_clean_worktree, remove_clean_worktree_if_branch,
    remove_clean_worktree_if_detached, remove_dirty_worktree, remove_worktree, repair_worktrees,
    session_branch_slug, worktree_head, worktree_is_dirty, DetachedWorktree, SubagentWorktree,
    WorktreeError, WorktreeRecord,
};
