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
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One subagent's isolated worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentWorktree {
    pub path: PathBuf,
    pub branch: String,
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
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git must be on PATH for these tests");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn init_repo(dir: &Path) {
        run(dir, &["init", "--initial-branch=main", "-q"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        run(dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-q", "-m", "init"]);
    }

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
}
