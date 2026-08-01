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
//! The rest of `.forge/` (`rules/`, `agents/`, `skills/`, `workflows/`) is
//! project-owned and untouched by this crate — it stays Git-visible.

mod exclude;
mod git;
mod migrate;
mod resolver;
pub mod worktree;

pub use exclude::{ensure_managed_block, has_managed_block, resolve_exclude_path, ExcludeError};
pub use git::{detect_repo_info, GitTopology, RepoInfo};
pub use migrate::{migrate_legacy_runtime_files, MigrationOutcome, MigrationRecord};
pub use resolver::{
    LocalRuntimeStorage, RuntimeDataKind, RuntimeIdentity, RuntimeStorage, StorageError,
    StorageMode, EXCLUDE_PATTERN,
};
pub use worktree::{
    create_worktree, list_worktrees, remove_worktree, SubagentWorktree, WorktreeError,
};
