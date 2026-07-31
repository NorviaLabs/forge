//! Centralized runtime-storage resolver. No runtime component should
//! construct a `.forge/local/...` (or application-data fallback) path
//! independently — everything goes through [`RuntimeStorage::path_for`].

use std::cell::{OnceCell, RefCell};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::exclude::{ensure_managed_block, resolve_exclude_path, ExcludeError};
use crate::git::{detect_repo_info, GitTopology, RepoInfo};
use crate::migrate::{migrate_legacy_runtime_files, MigrationOutcome, MigrationRecord};

/// The Forge-managed exclude pattern. Deliberately narrow: excludes only
/// the runtime-owned subtree, never all of `.forge/` (which would also
/// hide project-owned `.forge/rules|agents|skills|workflows`).
pub const EXCLUDE_PATTERN: &str = "/.forge/local/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    /// Inside the repository's own worktree, at `.forge/local/`.
    RepositoryLocal,
    /// The platform application-data directory — used when repository-local
    /// storage can't be set up safely, or there is no repository at all.
    ApplicationData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RuntimeDataKind {
    Session,
    Lifecycle,
    Queue,
    UiState,
    Cache,
    Log,
    Index,
    Checkpoint,
}

impl RuntimeDataKind {
    fn subdir(self) -> &'static str {
        match self {
            Self::Session => "sessions",
            Self::Lifecycle => "lifecycle",
            Self::Queue => "queue",
            Self::UiState => "ui-state",
            Self::Cache => "cache",
            Self::Log => "logs",
            Self::Index => "index",
            Self::Checkpoint => "checkpoints",
        }
    }
}

/// Stable identity for the workspace this storage instance is scoped to.
/// Matters only for the application-data fallback: two linked worktrees, or
/// two unrelated repositories that happen to share a folder basename, must
/// not collide in a single shared fallback directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    key: String,
}

impl RuntimeIdentity {
    fn from_repo_info(workspace: &Path, info: &RepoInfo) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Prefer the common git dir (stable across linked worktrees of the
        // same repository) and the worktree root (distinguishes worktrees
        // from each other); fall back to the raw workspace path outside a
        // repository.
        info.git_common_dir
            .as_deref()
            .unwrap_or(workspace)
            .hash(&mut hasher);
        info.worktree_root
            .as_deref()
            .unwrap_or(workspace)
            .hash(&mut hasher);
        let hash = hasher.finish();
        let label = workspace
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("workspace");
        Self {
            key: format!("{label}-{hash:016x}"),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    #[error("no repository worktree root could be resolved")]
    NoRoot,
    #[error("resolved path would escape the storage root")]
    PathEscapesRoot,
    #[error(transparent)]
    ExcludeSetupFailed(#[from] ExcludeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no application-data directory is available on this platform")]
    NoAppDataDir,
}

pub trait RuntimeStorage {
    /// The repository worktree root, if any (`None` for a bare repo, a
    /// non-Git directory, or before detection has occurred).
    fn repository_root(&self) -> Option<&Path>;
    /// Which mode is (or would be) in effect. Before `ensure_ready`/`root`
    /// have run, this reflects the *desired* mode from topology alone; a
    /// desired `RepositoryLocal` can still fall back to `ApplicationData`
    /// once `ensure_ready` actually attempts setup.
    fn storage_mode(&self) -> StorageMode;
    /// Stable identity for this workspace (used for the fallback directory name).
    fn identity(&self) -> &RuntimeIdentity;
    /// User-facing explanation, set only when fallback storage is in use.
    fn fallback_reason(&self) -> Option<String>;
    /// Idempotently perform whatever setup this mode needs (exclude rule +
    /// directory creation for repository-local; directory creation for
    /// application-data) — safe to call repeatedly, and safe to call before
    /// any actual write. Does not create anything until first called.
    fn ensure_ready(&self) -> Result<(), StorageError>;
    /// The runtime-storage root directory, ensuring it's ready first.
    fn root(&self) -> Result<PathBuf, StorageError>;
    /// A category subdirectory under the root, created on demand. Also
    /// establishes Git exclusion (repository-local mode) or the
    /// application-data directory (fallback mode) as a side effect — use
    /// this only when actually about to write.
    fn path_for(&self, kind: RuntimeDataKind) -> Result<PathBuf, StorageError>;
    /// The path a category *would* resolve to, computed from topology alone
    /// — no directory is created, no Git exclude file is touched. For
    /// read-only callers (does this file exist yet?) that must not trigger
    /// lazy initialization just by checking.
    fn path_for_read(&self, kind: RuntimeDataKind) -> Option<PathBuf>;
}

pub struct LocalRuntimeStorage {
    workspace: PathBuf,
    repo_info: RepoInfo,
    identity: RuntimeIdentity,
    effective_mode: OnceCell<StorageMode>,
    fallback_reason: RefCell<Option<String>>,
    migration_report: RefCell<Vec<MigrationRecord>>,
}

impl LocalRuntimeStorage {
    /// Detection happens here, but nothing is created on disk yet —
    /// `ensure_ready`/`root`/`path_for` are the lazy-initialization points.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        let repo_info = detect_repo_info(&workspace);
        let identity = RuntimeIdentity::from_repo_info(&workspace, &repo_info);
        Self {
            workspace,
            repo_info,
            identity,
            effective_mode: OnceCell::new(),
            fallback_reason: RefCell::new(None),
            migration_report: RefCell::new(Vec::new()),
        }
    }

    /// Outcome of the one-time legacy-file migration attempted during
    /// repository-local setup. Empty until `ensure_ready`/`root`/`path_for`
    /// has run at least once.
    pub fn migration_report(&self) -> Vec<MigrationRecord> {
        self.migration_report.borrow().clone()
    }

    /// Legacy runtime paths that were left in place because they're tracked
    /// by Git — surfaced so a caller can warn the operator ("Forge did not
    /// modify the Git index; review the tracked files before migration"),
    /// never silently altered.
    pub fn tracked_migration_conflicts(&self) -> Vec<PathBuf> {
        self.migration_report
            .borrow()
            .iter()
            .filter(|r| r.outcome == MigrationOutcome::Tracked)
            .map(|r| r.source.clone())
            .collect()
    }

    fn desired_mode(&self) -> StorageMode {
        match self.repo_info.topology {
            GitTopology::Normal | GitTopology::LinkedWorktree => StorageMode::RepositoryLocal,
            GitTopology::Bare | GitTopology::NotAGitRepo => StorageMode::ApplicationData,
        }
    }

    fn try_repository_local(&self) -> Result<PathBuf, StorageError> {
        let worktree_root = self
            .repo_info
            .worktree_root
            .as_deref()
            .ok_or(StorageError::NoRoot)?;
        let exclude_path = resolve_exclude_path(worktree_root)?;
        ensure_managed_block(&exclude_path, EXCLUDE_PATTERN)?;
        let root = worktree_root.join(".forge").join("local");
        std::fs::create_dir_all(&root)?;
        set_restrictive_permissions(&root);
        // Best-effort: a migration failure never blocks storage setup — the
        // new location works regardless of whether legacy files moved.
        *self.migration_report.borrow_mut() = migrate_legacy_runtime_files(worktree_root);
        Ok(root)
    }

    fn app_data_root(&self) -> Result<PathBuf, StorageError> {
        let base = dirs::data_dir().ok_or(StorageError::NoAppDataDir)?;
        let root = base.join("forge").join(self.identity.as_str());
        std::fs::create_dir_all(&root)?;
        set_restrictive_permissions(&root);
        Ok(root)
    }
}

impl LocalRuntimeStorage {
    /// The workspace directory this instance was constructed for — the
    /// repository-local root when one was detected, otherwise the plain
    /// directory Forge was opened in. Useful for diagnostics/logging.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

impl RuntimeStorage for LocalRuntimeStorage {
    fn repository_root(&self) -> Option<&Path> {
        self.repo_info.worktree_root.as_deref()
    }

    fn storage_mode(&self) -> StorageMode {
        self.effective_mode
            .get()
            .copied()
            .unwrap_or_else(|| self.desired_mode())
    }

    fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }

    fn fallback_reason(&self) -> Option<String> {
        self.fallback_reason.borrow().clone()
    }

    fn ensure_ready(&self) -> Result<(), StorageError> {
        if self.effective_mode.get().is_some() {
            return Ok(());
        }
        if self.desired_mode() == StorageMode::RepositoryLocal {
            match self.try_repository_local() {
                Ok(_) => {
                    // Ignore a racing concurrent initializer — either value
                    // is the correct, already-attempted mode.
                    let _ = self.effective_mode.set(StorageMode::RepositoryLocal);
                    return Ok(());
                }
                Err(err) => {
                    *self.fallback_reason.borrow_mut() = Some(format!(
                        "Forge could not safely use repository-local storage ({err}). \
                         Runtime state is being stored in Forge's application-data directory instead."
                    ));
                }
            }
        }
        self.app_data_root()?;
        let _ = self.effective_mode.set(StorageMode::ApplicationData);
        Ok(())
    }

    fn root(&self) -> Result<PathBuf, StorageError> {
        self.ensure_ready()?;
        match self.storage_mode() {
            StorageMode::RepositoryLocal => Ok(self
                .repo_info
                .worktree_root
                .as_deref()
                .ok_or(StorageError::NoRoot)?
                .join(".forge")
                .join("local")),
            StorageMode::ApplicationData => self.app_data_root(),
        }
    }

    fn path_for(&self, kind: RuntimeDataKind) -> Result<PathBuf, StorageError> {
        let root = self.root()?;
        let path = root.join(kind.subdir());
        std::fs::create_dir_all(&path)?;
        validate_within_root(&root, &path)?;
        Ok(path)
    }

    fn path_for_read(&self, kind: RuntimeDataKind) -> Option<PathBuf> {
        let root = match self.storage_mode() {
            StorageMode::RepositoryLocal => self
                .repo_info
                .worktree_root
                .as_deref()?
                .join(".forge")
                .join("local"),
            StorageMode::ApplicationData => {
                dirs::data_dir()?.join("forge").join(self.identity.as_str())
            }
        };
        Some(root.join(kind.subdir()))
    }
}

fn validate_within_root(root: &Path, candidate: &Path) -> Result<(), StorageError> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    if candidate.starts_with(&root) {
        Ok(())
    } else {
        Err(StorageError::PathEscapesRoot)
    }
}

#[cfg(unix)]
fn set_restrictive_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_restrictive_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn init_repo(dir: &Path) {
        run(dir, &["init", "--initial-branch=main", "-q"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        run(dir, &["config", "user.name", "Test"]);
    }

    #[test]
    fn resolves_forge_local_for_a_normal_repository() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        let root = storage.root().unwrap();
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.path()
                .join(".forge")
                .join("local")
                .canonicalize()
                .unwrap()
        );
        assert_eq!(storage.storage_mode(), StorageMode::RepositoryLocal);
        assert!(storage.fallback_reason().is_none());
    }

    #[test]
    fn does_not_create_anything_during_construction() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let _storage = LocalRuntimeStorage::new(dir.path());
        assert!(!dir.path().join(".forge").exists());
    }

    #[test]
    fn creates_storage_lazily_on_first_required_write() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        assert!(!dir.path().join(".forge").exists());
        storage.path_for(RuntimeDataKind::Session).unwrap();
        assert!(dir
            .path()
            .join(".forge")
            .join("local")
            .join("sessions")
            .is_dir());
    }

    #[test]
    fn path_for_read_never_creates_anything_or_touches_exclude() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        let path = storage.path_for_read(RuntimeDataKind::UiState).unwrap();
        assert!(path.ends_with("ui-state"));
        assert!(!dir.path().join(".forge").exists());
        // `git init` itself may pre-populate `.git/info/exclude` with a
        // template — what matters is that Forge's own managed block was
        // never added by a mere read.
        let exclude_path = resolve_exclude_path(dir.path()).unwrap();
        assert!(!crate::exclude::has_managed_block(
            &exclude_path,
            EXCLUDE_PATTERN
        ));
    }

    #[test]
    fn path_for_read_matches_the_side_effecting_path_for_once_created() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        let read_path = storage.path_for_read(RuntimeDataKind::Session).unwrap();
        let write_path = storage.path_for(RuntimeDataKind::Session).unwrap();
        assert_eq!(
            read_path.canonicalize().unwrap_or(read_path),
            write_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn establishes_exclusion_before_the_first_repository_local_write() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        storage.ensure_ready().unwrap();
        let exclude_path = resolve_exclude_path(dir.path()).unwrap();
        assert!(crate::exclude::has_managed_block(
            &exclude_path,
            EXCLUDE_PATTERN
        ));
    }

    /// Git-authority test: after storage is set up and runtime files are
    /// written, real `git status --porcelain` (Forge's own exclude
    /// mechanism, not any post-filtering — there is none) must report
    /// nothing under `.forge/local/`. A normal project file must still be
    /// reported as authoritative Git considers it.
    #[test]
    fn native_git_status_never_reports_forge_local_but_reports_real_changes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        // A real, uncommitted project file — must show up in git status.
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();

        let storage = LocalRuntimeStorage::new(dir.path());
        storage.path_for(RuntimeDataKind::Session).unwrap();
        storage.path_for(RuntimeDataKind::UiState).unwrap();
        std::fs::write(
            dir.path()
                .join(".forge")
                .join("local")
                .join("sessions")
                .join("abc.db"),
            "data",
        )
        .unwrap();

        let output = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["status", "--porcelain", "-uall"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let status = String::from_utf8(output.stdout).unwrap();

        assert!(
            !status.contains(".forge"),
            "runtime state leaked into git status: {status:?}"
        );
        assert!(
            status.contains("README.md"),
            "a real project change must still be reported: {status:?}"
        );
    }

    #[test]
    fn ensure_ready_migrates_legacy_runtime_files_and_reports_the_outcome() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::create_dir_all(dir.path().join(".forge/sessions")).unwrap();
        std::fs::write(dir.path().join(".forge/sessions/old.db"), "data").unwrap();

        let storage = LocalRuntimeStorage::new(dir.path());
        storage.ensure_ready().unwrap();

        assert!(dir.path().join(".forge/local/sessions/old.db").is_file());
        assert!(!dir.path().join(".forge/sessions").exists());
        assert!(storage.tracked_migration_conflicts().is_empty());
        assert!(storage
            .migration_report()
            .iter()
            .any(|r| r.source.ends_with("sessions")
                && r.outcome == crate::MigrationOutcome::Migrated));
    }

    #[test]
    fn ensure_ready_surfaces_tracked_legacy_files_as_conflicts_not_migrated() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        std::fs::write(dir.path().join(".forge/ui-state.json"), "{}").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", ".forge/ui-state.json"])
            .status()
            .unwrap();
        assert!(status.success());

        let storage = LocalRuntimeStorage::new(dir.path());
        storage.ensure_ready().unwrap();

        assert!(dir.path().join(".forge/ui-state.json").is_file());
        assert!(!dir
            .path()
            .join(".forge/local/ui-state/ui-state.json")
            .exists());
        assert_eq!(storage.tracked_migration_conflicts().len(), 1);
    }

    #[test]
    fn uses_application_data_fallback_for_a_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let storage = LocalRuntimeStorage::new(dir.path());
        let root = storage.root().unwrap();
        assert_eq!(storage.storage_mode(), StorageMode::ApplicationData);
        assert!(!root.starts_with(dir.path()));
        // Never pollutes the (non-)repository.
        assert!(!dir.path().join(".forge").exists());
    }

    #[test]
    fn uses_application_data_fallback_for_a_bare_repository() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "--bare", "-q"]);
        let storage = LocalRuntimeStorage::new(dir.path());
        assert_eq!(storage.storage_mode(), StorageMode::ApplicationData);
        storage.root().unwrap();
        assert!(!dir.path().join(".forge").exists());
    }

    #[test]
    fn produces_distinct_worktree_identities() {
        let main_dir = TempDir::new().unwrap();
        init_repo(main_dir.path());
        std::fs::write(main_dir.path().join("f.txt"), "x").unwrap();
        run(main_dir.path(), &["add", "."]);
        run(main_dir.path(), &["commit", "-q", "-m", "init"]);

        let worktree_parent = TempDir::new().unwrap();
        let leaf = worktree_parent.path().join("wt");
        run(
            main_dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                leaf.to_str().unwrap(),
                "-b",
                "wt-branch",
            ],
        );

        let main_storage = LocalRuntimeStorage::new(main_dir.path());
        let wt_storage = LocalRuntimeStorage::new(&leaf);
        assert_ne!(main_storage.identity(), wt_storage.identity());

        // And each worktree's repository-local root is genuinely its own.
        let main_root = main_storage.root().unwrap();
        let wt_root = wt_storage.root().unwrap();
        assert_ne!(main_root, wt_root);
    }

    #[test]
    fn produces_stable_repository_identities() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let a = LocalRuntimeStorage::new(dir.path());
        let b = LocalRuntimeStorage::new(dir.path());
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn separates_runtime_data_categories() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        let sessions = storage.path_for(RuntimeDataKind::Session).unwrap();
        let queue = storage.path_for(RuntimeDataKind::Queue).unwrap();
        assert_ne!(sessions, queue);
        assert!(sessions.ends_with("sessions"));
        assert!(queue.ends_with("queue"));
    }

    #[test]
    fn prevents_path_traversal_outside_the_storage_root() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let escaped = outside.path().join("evil");
        assert!(validate_within_root(root.path(), &escaped).is_err());
    }

    #[test]
    fn handles_paths_with_spaces_and_unicode() {
        let base = TempDir::new().unwrap();
        let dir = base.path().join("has space and 日本語");
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);
        let storage = LocalRuntimeStorage::new(&dir);
        let root = storage.root().unwrap();
        assert!(root.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn applies_restrictive_permissions_where_supported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let storage = LocalRuntimeStorage::new(dir.path());
        let root = storage.root().unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
