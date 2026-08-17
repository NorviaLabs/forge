//! Git repository-topology detection, via Git plumbing rather than
//! assuming a fixed `.git/info/exclude`-style layout. Distinguishes a
//! normal repository from a linked worktree, a bare repository, and a
//! plain non-Git directory — each needs different storage handling.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTopology {
    /// A normal repository (or a submodule, which looks the same from
    /// `rev-parse`'s perspective once scoped to its own root).
    Normal,
    /// A linked worktree of some main repository — shares a common `.git`
    /// directory with other worktrees but has its own `git-dir`.
    LinkedWorktree,
    /// A bare repository: no working tree to store repository-local state in.
    Bare,
    /// Not inside a Git repository at all.
    NotAGitRepo,
}

/// Repository identity as resolved by Git itself for `workspace`.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub topology: GitTopology,
    /// The working tree root (`git rev-parse --show-toplevel`). `None` for
    /// bare repositories and non-Git directories.
    pub worktree_root: Option<PathBuf>,
    /// The `.git` directory shared by every worktree of this repository
    /// (`git rev-parse --git-common-dir`) — stable across linked worktrees,
    /// useful as an identity anchor.
    pub git_common_dir: Option<PathBuf>,
    /// This worktree's own Git administrative directory
    /// (`git rev-parse --git-dir`) — differs from `git_common_dir` only for
    /// linked worktrees.
    pub git_dir: Option<PathBuf>,
}

fn git_rev_parse(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("rev-parse")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let first = text.lines().next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

fn normalize(workspace: &Path, raw: Option<String>) -> Option<PathBuf> {
    let path = PathBuf::from(raw?);
    Some(if path.is_relative() {
        workspace.join(path)
    } else {
        path
    })
}

/// Detect repository topology for `workspace` using only Git itself — never
/// assumes `.git` is a fixed-layout directory (it may be a file, as in
/// worktrees and submodules).
pub fn detect_repo_info(workspace: &Path) -> RepoInfo {
    let not_a_repo = RepoInfo {
        topology: GitTopology::NotAGitRepo,
        worktree_root: None,
        git_common_dir: None,
        git_dir: None,
    };

    let Some(inside_work_tree) = git_rev_parse(workspace, &["--is-inside-work-tree"]) else {
        // `rev-parse` itself failed (no git binary, or genuinely not a repo).
        // A bare repository also fails `--is-inside-work-tree` with a
        // non-error "false", handled below via `--is-bare-repository`.
        return check_bare(workspace).unwrap_or(not_a_repo);
    };

    if inside_work_tree != "true" {
        return check_bare(workspace).unwrap_or(not_a_repo);
    }

    let worktree_root = normalize(workspace, git_rev_parse(workspace, &["--show-toplevel"]));
    let git_common_dir = normalize(workspace, git_rev_parse(workspace, &["--git-common-dir"]));
    let git_dir = normalize(workspace, git_rev_parse(workspace, &["--git-dir"]));

    let topology = match (&git_common_dir, &git_dir) {
        (Some(common), Some(own)) if common != own => GitTopology::LinkedWorktree,
        _ => GitTopology::Normal,
    };

    RepoInfo {
        topology,
        worktree_root,
        git_common_dir,
        git_dir,
    }
}

/// Separate check for a bare repository — `--is-inside-work-tree` reports
/// `false` for both a bare repo and a non-Git directory, so bare-ness needs
/// its own query.
fn check_bare(workspace: &Path) -> Option<RepoInfo> {
    let is_bare = git_rev_parse(workspace, &["--is-bare-repository"])?;
    if is_bare != "true" {
        return None;
    }
    let git_dir = normalize(workspace, git_rev_parse(workspace, &["--git-dir"]));
    Some(RepoInfo {
        topology: GitTopology::Bare,
        worktree_root: None,
        git_common_dir: git_dir.clone(),
        git_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_test_support::{git as run, init_repo};
    use tempfile::TempDir;

    #[test]
    fn detects_a_normal_repository() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let info = detect_repo_info(dir.path());
        assert_eq!(info.topology, GitTopology::Normal);
        assert!(info.worktree_root.is_some());
        assert_eq!(info.git_common_dir, info.git_dir);
    }

    #[test]
    fn detects_a_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let info = detect_repo_info(dir.path());
        assert_eq!(info.topology, GitTopology::NotAGitRepo);
        assert!(info.worktree_root.is_none());
    }

    #[test]
    fn detects_a_bare_repository() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "--bare", "-q"]);
        let info = detect_repo_info(dir.path());
        assert_eq!(info.topology, GitTopology::Bare);
        assert!(info.worktree_root.is_none());
    }

    #[test]
    fn detects_a_linked_worktree() {
        let main_dir = TempDir::new().unwrap();
        init_repo(main_dir.path());
        std::fs::write(main_dir.path().join("f.txt"), "x").unwrap();
        run(main_dir.path(), &["add", "."]);
        run(main_dir.path(), &["commit", "-q", "-m", "init"]);

        let worktree_dir = TempDir::new().unwrap();
        // Reuse the tempdir's path but let `git worktree add` create the leaf.
        let leaf = worktree_dir.path().join("wt");
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

        let info = detect_repo_info(&leaf);
        assert_eq!(info.topology, GitTopology::LinkedWorktree);
        assert!(info.worktree_root.is_some());
        assert_ne!(info.git_common_dir, info.git_dir);
    }

    #[test]
    fn detects_a_submodule_as_its_own_repository_boundary() {
        let parent = TempDir::new().unwrap();
        init_repo(parent.path());
        std::fs::write(parent.path().join("f.txt"), "x").unwrap();
        run(parent.path(), &["add", "."]);
        run(parent.path(), &["commit", "-q", "-m", "init"]);

        let sub_source = TempDir::new().unwrap();
        init_repo(sub_source.path());
        std::fs::write(sub_source.path().join("s.txt"), "y").unwrap();
        run(sub_source.path(), &["add", "."]);
        run(sub_source.path(), &["commit", "-q", "-m", "sub init"]);

        run(
            parent.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub_source.path().to_str().unwrap(),
                "sub",
            ],
        );

        let sub_path = parent.path().join("sub");
        let info = detect_repo_info(&sub_path);
        // The submodule resolves as its own Normal-topology repository,
        // rooted at the submodule directory — never the parent's.
        assert_eq!(info.topology, GitTopology::Normal);
        assert_eq!(
            info.worktree_root
                .as_deref()
                .map(|p| p.canonicalize().unwrap()),
            Some(sub_path.canonicalize().unwrap())
        );
    }
}
