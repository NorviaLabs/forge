//! Git repository-topology detection, via Git plumbing rather than
//! assuming a fixed `.git/info/exclude`-style layout. Distinguishes a
//! normal repository from a linked worktree, a bare repository, and a
//! plain non-Git directory — each needs different storage handling.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    let output = forge_sandbox::git::command(workspace)
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

/// The names of the files directly in `dir` that Git tracks: `None` when
/// `dir` is not inside a working tree (nothing there can have been
/// committed); an error when it is, but Git cannot list them.
pub fn tracked_files(dir: &Path) -> Result<Option<HashSet<OsString>>, String> {
    tracked_files_with(dir, &mut |dir: &Path, args: &[&str]| {
        forge_sandbox::git::command(dir).args(args).output()
    })
}

/// [`tracked_files`], running Git with `git`. One Git process in the usual
/// case (a journal directory inside a repository): the listing first, and
/// what kind of directory this is only when the listing fails.
fn tracked_files_with(
    dir: &Path,
    git: &mut dyn FnMut(&Path, &[&str]) -> std::io::Result<std::process::Output>,
) -> Result<Option<HashSet<OsString>>, String> {
    let output = git(dir, &["ls-files", "-z", "--", "."])
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        if !matches!(
            detect_repo_info(dir).topology,
            GitTopology::Normal | GitTopology::LinkedWorktree
        ) {
            return Ok(None);
        }
        return Err(format!(
            "git could not list the tracked files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // Paths relative to `dir`; only the files directly in it matter.
    Ok(Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty() && !path.contains(&b'/'))
            .map(os_string)
            .collect(),
    ))
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
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

#[cfg(test)]
mod tracked_files_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// `tracked_files`, counting the Git processes it starts.
    fn counted(dir: &Path) -> (Result<Option<HashSet<OsString>>, String>, usize) {
        let mut runs = 0;
        let result = tracked_files_with(dir, &mut |dir: &Path, args: &[&str]| {
            runs += 1;
            forge_sandbox::git::command(dir).args(args).output()
        });
        (result, runs)
    }

    /// Inside a repository — where Forge keeps its journals — one Git
    /// process, not the four or five a topology check first would take.
    #[test]
    fn listing_the_tracked_files_of_a_repository_runs_git_once() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "--initial-branch=main"]);
        git(repo.path(), &["config", "user.email", "forge@example.com"]);
        git(repo.path(), &["config", "user.name", "Forge Test"]);
        let sessions = repo.path().join(".forge/local/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("committed.db"), "x").unwrap();
        std::fs::write(sessions.join("local.db"), "x").unwrap();
        git(
            repo.path(),
            &["add", "-f", ".forge/local/sessions/committed.db"],
        );
        git(repo.path(), &["commit", "-q", "-m", "x"]);

        let (result, runs) = counted(&sessions);
        assert_eq!(runs, 1);
        assert_eq!(
            result.unwrap(),
            Some(HashSet::from([OsString::from("committed.db")]))
        );
    }

    #[test]
    fn outside_a_repository_nothing_is_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let (result, _) = counted(dir.path());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn a_repository_git_cannot_list_is_an_error() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "--initial-branch=main"]);
        std::fs::write(repo.path().join(".git/index"), "not an index").unwrap();
        let (result, _) = counted(repo.path());
        assert!(result.is_err(), "{result:?}");
    }
}
