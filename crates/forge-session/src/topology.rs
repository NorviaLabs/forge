//! What kind of project a workspace belongs to, and where its runtime data
//! lives.
//!
//! A project is either a Git repository (every linked worktree shares the main
//! worktree's storage and owner lock) or a plain directory (the folder itself
//! is the root, and its data lives in `<root>/.forge/local`). Every place in
//! this crate that builds runtime storage or lists worktrees goes through
//! [`ProjectTopology`], so the two kinds never need separate code paths beyond
//! the Git-only operations.
//!
//! The topology is plain data on purpose: the storage types hold `OnceCell`
//! and `RefCell` and are `!Sync`, so they are built per call rather than
//! stored in shared supervisor state.

use std::path::{Path, PathBuf};

use forge_storage::{
    GitTopology, LocalRuntimeStorage, RepositoryRuntimeStorage, RuntimeStorage, StorageError,
    WorktreeError, WorktreeRecord,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Git,
    Directory,
}

#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("{0} is a bare Git repository; open one of its worktrees instead")]
    Bare(PathBuf),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTopology {
    kind: ProjectKind,
    root: PathBuf,
}

impl ProjectTopology {
    /// Detect the project `workspace` belongs to. The kind is detected on
    /// every call, never stored, so `git init` or deleting `.git` takes
    /// effect on the next launch.
    pub fn detect(workspace: &Path) -> Result<Self, TopologyError> {
        match forge_storage::detect_repo_info(workspace).topology {
            GitTopology::Normal | GitTopology::LinkedWorktree => Ok(Self {
                kind: ProjectKind::Git,
                root: forge_storage::main_worktree(workspace)?,
            }),
            GitTopology::NotAGitRepo => Ok(Self {
                kind: ProjectKind::Directory,
                root: workspace.to_path_buf(),
            }),
            GitTopology::Bare => Err(TopologyError::Bare(workspace.to_path_buf())),
        }
    }

    pub fn kind(&self) -> ProjectKind {
        self.kind
    }

    /// The project root: the main worktree for Git, the folder itself for a
    /// directory project.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn supports_git(&self) -> bool {
        self.kind == ProjectKind::Git
    }

    /// Runtime storage for this project, built fresh on each call.
    pub fn storage(&self) -> Result<Box<dyn RuntimeStorage + Send>, TopologyError> {
        Ok(match self.kind {
            ProjectKind::Git => Box::new(RepositoryRuntimeStorage::new(&self.root)?),
            ProjectKind::Directory => Box::new(LocalRuntimeStorage::directory_local(&self.root)),
        })
    }

    /// The project's worktrees. A directory project has exactly one: the
    /// root, with no branch.
    pub fn worktrees(&self) -> Result<Vec<WorktreeRecord>, TopologyError> {
        match self.kind {
            ProjectKind::Git => Ok(forge_storage::list_worktree_records(&self.root)?),
            ProjectKind::Directory => Ok(vec![WorktreeRecord {
                path: self.root.clone(),
                branch: None,
                head: None,
                prunable: false,
            }]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_storage::{RuntimeDataKind, StorageMode};
    use forge_test_support::init_repo;
    use tempfile::TempDir;

    #[test]
    fn plain_folder_is_a_directory_project() {
        let dir = TempDir::new().unwrap();
        let topology = ProjectTopology::detect(dir.path()).unwrap();
        assert_eq!(topology.kind(), ProjectKind::Directory);
        assert_eq!(topology.root(), dir.path());
        assert!(!topology.supports_git());

        let worktrees = topology.worktrees().unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, dir.path());
        assert_eq!(worktrees[0].branch, None);

        let storage = topology.storage().unwrap();
        assert_eq!(storage.storage_mode(), StorageMode::DirectoryLocal);
        assert_eq!(
            storage.path_for(RuntimeDataKind::Control).unwrap(),
            dir.path().join(".forge/local/control")
        );
    }

    #[test]
    fn repository_subfolder_resolves_to_the_main_worktree() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();

        let topology = ProjectTopology::detect(&sub).unwrap();
        assert_eq!(topology.kind(), ProjectKind::Git);
        assert_eq!(
            topology.root().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert!(topology.supports_git());
        assert_eq!(
            topology.storage().unwrap().storage_mode(),
            StorageMode::RepositoryLocal
        );
    }

    #[test]
    fn kind_follows_git_init() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            ProjectTopology::detect(dir.path()).unwrap().kind(),
            ProjectKind::Directory
        );
        init_repo(dir.path());
        assert_eq!(
            ProjectTopology::detect(dir.path()).unwrap().kind(),
            ProjectKind::Git
        );
    }
}
