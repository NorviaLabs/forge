//! Git worktree lifecycle for subagent isolation.
//!
//! Each subagent gets its own working directory and branch, created via
//! `git worktree add` off the parent's current `HEAD`. This is what makes
//! concurrent write-classed tool calls from the parent session and any
//! number of subagents safe without a shared-workspace write lock — they
//! simply never touch the same files, because they're not in the same
//! directory.
//!
//! This module only knows *how* to create/remove a worktree — *where* it
//! lives is the caller's decision (`forge-core` resolves the base directory
//! via [`crate::RuntimeDataKind::Worktree`], keeping this module free of any
//! opinion about storage layout).
//!
//! Deliberately out of scope: merging a finished subagent's branch back into
//! the parent branch. The worktree and branch are left in place for manual
//! review — see the M2a plan note this module was built against.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    #[error("git worktree add failed: {0}")]
    AddFailed(String),
    #[error("git worktree remove failed: {0}")]
    RemoveFailed(String),
    #[error("git worktree list failed: {0}")]
    ListFailed(String),
    #[error("worktree is dirty: {0}")]
    Dirty(String),
    #[error("worktree has no branch checked out")]
    DetachedHead,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One worktree registered with Git, including the branch identity Forge uses
/// to validate an immutable task/worktree binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub prunable: bool,
}

/// One subagent's isolated worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Resolve the main worktree for the repository containing `workspace`.
/// `git worktree list` guarantees the main worktree is first; linked
/// worktrees follow it.
pub fn main_worktree(workspace: &Path) -> Result<PathBuf, WorktreeError> {
    list_worktree_records(workspace)?
        .into_iter()
        .next()
        .map(|record| record.path)
        .ok_or_else(|| WorktreeError::ListFailed("git returned no worktrees".into()))
}

/// Create a user-visible managed task worktree and branch from
/// `source_worktree`'s committed `HEAD`.
pub fn create_task_worktree(
    source_worktree: &Path,
    base_dir: &Path,
    id: u64,
    label: &str,
) -> Result<SubagentWorktree, WorktreeError> {
    std::fs::create_dir_all(base_dir)?;
    let slug = sanitize_label(label);
    let name = format!("task-{id}-{slug}");
    let branch = format!("forge/{slug}-{id}");
    let path = base_dir.join(&name);
    let output = Command::new("git")
        .arg("-C")
        .arg(source_worktree)
        .args(["worktree", "add", "-q"])
        .arg(&path)
        .args(["-b", &branch, "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::AddFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(SubagentWorktree { path, branch })
}

/// Reduce arbitrary (possibly model-authored) text to a safe path component
/// and git ref segment: ASCII alphanumerics/`-`/`_` only, no leading/
/// trailing `-`, capped length. Without this, a label containing `../` or
/// git ref-reserved characters (space, `~^:?*[`, a leading `-`) could escape
/// `base_dir` or fail/misbehave as a branch name.
fn sanitize_label(label: &str) -> String {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    let truncated: String = trimmed.chars().take(40).collect();
    if truncated.is_empty() {
        "task".to_string()
    } else {
        truncated
    }
}

/// Remove a clean worktree without deleting its branch. Unlike the subagent
/// cleanup helper above, this never passes `--force`; user-task cleanup must
/// refuse uncommitted work rather than discarding it.
pub fn remove_clean_worktree(repo_root: &Path, worktree_path: &Path) -> Result<(), WorktreeError> {
    let status = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()?;
    if !status.status.success() {
        return Err(WorktreeError::RemoveFailed(
            String::from_utf8_lossy(&status.stderr).into_owned(),
        ));
    }
    let dirty = String::from_utf8_lossy(&status.stdout);
    if !dirty.trim().is_empty() {
        return Err(WorktreeError::Dirty(dirty.into_owned()));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove"])
        .arg(worktree_path)
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::RemoveFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(())
}

/// Create a new worktree for a subagent, on a fresh branch off `repo_root`'s
/// current `HEAD`, at `<base_dir>/subagent-<id>-<slug>`. `id` guarantees
/// uniqueness even when two subagents share a label (e.g. two "test-fixer"
/// runs); `label` is sanitized before use in either the path or branch name.
pub fn create_worktree(
    repo_root: &Path,
    base_dir: &Path,
    id: u64,
    label: &str,
) -> Result<SubagentWorktree, WorktreeError> {
    std::fs::create_dir_all(base_dir)?;
    let slug = sanitize_label(label);
    let name = format!("subagent-{id}-{slug}");
    let branch = format!("forge/subagent/{name}");
    let path = base_dir.join(&name);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "-q"])
        .arg(&path)
        .args(["-b", &branch])
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::AddFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(SubagentWorktree { path, branch })
}

/// Remove a worktree created by `create_worktree`. `--force` discards any
/// uncommitted changes in it — the branch itself is untouched (not deleted),
/// so committed work is never lost, only the working-directory checkout.
pub fn remove_worktree(repo_root: &Path, worktree_path: &Path) -> Result<(), WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::RemoveFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(())
}

/// List every worktree path currently registered for `repo_root` (including
/// the main worktree itself), via `git worktree list --porcelain`.
pub fn list_worktrees(repo_root: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
    Ok(list_worktree_records(repo_root)?
        .into_iter()
        .map(|record| record.path)
        .collect())
}

pub fn list_worktree_records(repo_root: &Path) -> Result<Vec<WorktreeRecord>, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::ListFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut records = Vec::new();
    let mut path = None;
    let mut branch = None;
    let mut head = None;
    let mut prunable = false;
    let finish = |records: &mut Vec<WorktreeRecord>,
                  path: &mut Option<PathBuf>,
                  branch: &mut Option<String>,
                  head: &mut Option<String>,
                  prunable: &mut bool| {
        if let Some(path) = path.take() {
            records.push(WorktreeRecord {
                path,
                branch: branch.take(),
                head: head.take(),
                prunable: std::mem::take(prunable),
            });
        }
    };
    for line in text.lines() {
        if line.is_empty() {
            finish(
                &mut records,
                &mut path,
                &mut branch,
                &mut head,
                &mut prunable,
            );
        } else if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
            branch = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("HEAD ") {
            head = Some(value.to_string());
        } else if line.starts_with("prunable") {
            prunable = true;
        }
    }
    finish(
        &mut records,
        &mut path,
        &mut branch,
        &mut head,
        &mut prunable,
    );
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_test_support::init_repo_with_commit as init_repo;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    #[test]
    fn sanitize_label_keeps_only_safe_characters() {
        assert_eq!(sanitize_label("test-fixer"), "test-fixer");
        assert_eq!(sanitize_label("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_label("  spaced out!!  "), "spaced-out");
        assert_eq!(sanitize_label(""), "task");
        assert_eq!(sanitize_label("---"), "task");
        let long = "a".repeat(100);
        assert_eq!(sanitize_label(&long).len(), 40);
    }

    #[test]
    fn create_worktree_checks_out_a_fresh_branch_at_the_expected_path() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let wt = create_worktree(repo.path(), base.path(), 1, "test-fixer").unwrap();
        assert_eq!(wt.path, base.path().join("subagent-1-test-fixer"));
        assert_eq!(wt.branch, "forge/subagent/subagent-1-test-fixer");
        assert!(wt.path.join("f.txt").exists());

        let info = crate::detect_repo_info(&wt.path);
        assert_eq!(info.topology, crate::GitTopology::LinkedWorktree);
    }

    #[test]
    fn create_worktree_sanitizes_a_hostile_label() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let wt = create_worktree(repo.path(), base.path(), 1, "../../escape").unwrap();
        // Must stay confined under `base`, never escape via `../`.
        assert!(wt.path.starts_with(base.path()));
        assert_eq!(wt.path, base.path().join("subagent-1-escape"));
    }

    #[test]
    fn two_subagents_with_the_same_label_get_distinct_worktrees() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let a = create_worktree(repo.path(), base.path(), 1, "test-fixer").unwrap();
        let b = create_worktree(repo.path(), base.path(), 2, "test-fixer").unwrap();
        assert_ne!(a.path, b.path);
        assert_ne!(a.branch, b.branch);
    }

    #[test]
    fn remove_worktree_deletes_the_directory_but_keeps_the_branch() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let wt = create_worktree(repo.path(), base.path(), 1, "cleanup-me").unwrap();
        assert!(wt.path.exists());

        remove_worktree(repo.path(), &wt.path).unwrap();
        assert!(!wt.path.exists());

        // Branch survives removal — only the checkout is gone.
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "--list", &wt.branch])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains(&wt.branch), "branch listing was: {text:?}");
    }

    #[test]
    fn list_worktrees_includes_the_main_and_every_created_worktree() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let a = create_worktree(repo.path(), base.path(), 1, "alpha").unwrap();
        let b = create_worktree(repo.path(), base.path(), 2, "beta").unwrap();

        let listed = list_worktrees(repo.path()).unwrap();
        let canon = |p: &Path| p.canonicalize().unwrap();
        assert!(listed.iter().any(|p| canon(p) == canon(repo.path())));
        assert!(listed.iter().any(|p| canon(p) == canon(&a.path)));
        assert!(listed.iter().any(|p| canon(p) == canon(&b.path)));
    }

    #[test]
    fn repository_main_worktree_is_stable_from_a_linked_worktree() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();
        let linked = create_task_worktree(repo.path(), base.path(), 4, "linked").unwrap();

        assert_eq!(
            main_worktree(&linked.path).unwrap().canonicalize().unwrap(),
            repo.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn task_worktree_uses_the_initiating_head_and_user_facing_names() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();

        let worktree =
            create_task_worktree(repo.path(), base.path(), 13, "Scheduler fairness").unwrap();
        assert_eq!(worktree.branch, "forge/Scheduler-fairness-13");
        assert_eq!(
            worktree.path,
            base.path().join("task-13-Scheduler-fairness")
        );
    }

    #[test]
    fn clean_only_removal_refuses_uncommitted_work_and_keeps_branch() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = TempDir::new().unwrap();
        let worktree = create_task_worktree(repo.path(), base.path(), 5, "dirty").unwrap();
        std::fs::write(worktree.path.join("uncommitted.txt"), "keep me").unwrap();

        assert!(matches!(
            remove_clean_worktree(repo.path(), &worktree.path),
            Err(WorktreeError::Dirty(_))
        ));
        assert!(worktree.path.exists());

        std::fs::remove_file(worktree.path.join("uncommitted.txt")).unwrap();
        remove_clean_worktree(repo.path(), &worktree.path).unwrap();
        assert!(!worktree.path.exists());
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "--list", &worktree.branch])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains(&worktree.branch));
    }
}
