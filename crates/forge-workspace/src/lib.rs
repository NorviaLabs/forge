//! Workspace isolation via git worktree (workspace-isolation.md) — CTX-03.

use std::path::{Path, PathBuf};
use std::process::Command;

use forge_types::SessionId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a git repository: {0}")]
    NotGit(String),
    #[error("git failed: {0}")]
    Git(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    Off,
    Worktree,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub isolation: IsolationMode,
    #[serde(default = "default_wt_dir")]
    pub worktree_dir: String,
}

fn default_wt_dir() -> String {
    ".forge/worktrees".into()
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            isolation: IsolationMode::Off,
            worktree_dir: default_wt_dir(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeManager {
    pub primary_root: PathBuf,
    pub worktree_dir: PathBuf,
    pub session_id: SessionId,
    pub branch: String,
    pub path: Option<PathBuf>,
    pub base_branch: String,
}

impl WorktreeManager {
    pub fn new(primary_root: PathBuf, session_id: SessionId) -> Self {
        let short = session_id.to_string();
        let short = &short[..8.min(short.len())];
        Self {
            worktree_dir: primary_root.join(".forge/worktrees"),
            primary_root,
            session_id,
            branch: format!("forge/{short}"),
            path: None,
            base_branch: "HEAD".into(),
        }
    }

    pub fn is_git_repo(root: &Path) -> bool {
        root.join(".git").exists()
            || Command::new("git")
                .args(["-C", &root.display().to_string(), "rev-parse", "--git-dir"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }

    fn git(&self, args: &[&str]) -> Result<String, WorktreeError> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.primary_root)
            .output()?;
        if !out.status.success() {
            return Err(WorktreeError::Git(format!(
                "{:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Create worktree if needed; return active root for tools.
    pub fn ensure(&mut self) -> Result<PathBuf, WorktreeError> {
        if !Self::is_git_repo(&self.primary_root) {
            return Err(WorktreeError::NotGit(
                self.primary_root.display().to_string(),
            ));
        }
        if let Some(ref p) = self.path {
            if p.exists() {
                return Ok(p.clone());
            }
        }
        std::fs::create_dir_all(&self.worktree_dir)?;
        let wt_path = self.worktree_dir.join(self.session_id.to_string());
        // capture base
        self.base_branch = self
            .git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "main".into());
        if self.base_branch == "HEAD" {
            self.base_branch = "main".into();
        }

        if wt_path.exists() {
            self.path = Some(wt_path.clone());
            return Ok(wt_path);
        }

        // git worktree add -b branch path
        let path_s = wt_path.display().to_string();
        self.git(&["worktree", "add", "-b", &self.branch, &path_s, "HEAD"])?;
        self.path = Some(wt_path.clone());
        Ok(wt_path)
    }

    pub fn active_root(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| self.primary_root.clone())
    }

    pub fn status(&self) -> String {
        format!(
            "isolation=worktree primary={} worktree={:?} branch={}",
            self.primary_root.display(),
            self.path,
            self.branch
        )
    }

    /// Merge worktree branch into base branch (or current).
    pub fn merge(&mut self) -> Result<(), WorktreeError> {
        let Some(ref wt) = self.path else {
            return Err(WorktreeError::Other("no worktree".into()));
        };
        // checkout base and merge
        let base = self.base_branch.clone();
        self.git(&["checkout", &base])?;
        let r = self.git(&["merge", "--no-edit", &self.branch]);
        if let Err(e) = r {
            return Err(e);
        }
        // remove worktree
        let _ = self.git(&["worktree", "remove", "--force", &wt.display().to_string()]);
        let _ = self.git(&["branch", "-D", &self.branch]);
        self.path = None;
        Ok(())
    }

    pub fn discard(&mut self) -> Result<(), WorktreeError> {
        let Some(ref wt) = self.path else {
            return Ok(());
        };
        let _ = self.git(&["worktree", "remove", "--force", &wt.display().to_string()]);
        let _ = self.git(&["branch", "-D", &self.branch]);
        self.path = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn init_git(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("README"), "hi").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn not_git_errors() {
        let dir = tempdir().unwrap();
        let mut m = WorktreeManager::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert!(matches!(m.ensure(), Err(WorktreeError::NotGit(_))));
    }

    #[test]
    fn ensure_worktree_and_discard() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let mut m = WorktreeManager::new(dir.path().to_path_buf(), Uuid::new_v4());
        let wt = m.ensure().unwrap();
        assert!(wt.exists());
        assert_ne!(wt, dir.path());
        // primary still has only README
        std::fs::write(wt.join("agent.txt"), "x").unwrap();
        assert!(!dir.path().join("agent.txt").exists());
        m.discard().unwrap();
        assert!(!wt.exists());
    }

    #[test]
    fn merge_brings_file_to_primary() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let mut m = WorktreeManager::new(dir.path().to_path_buf(), Uuid::new_v4());
        let wt = m.ensure().unwrap();
        std::fs::write(wt.join("from-agent.txt"), "ok").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&wt)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "agent"])
            .current_dir(&wt)
            .output()
            .unwrap();
        m.merge().unwrap();
        assert!(dir.path().join("from-agent.txt").exists());
    }
}
