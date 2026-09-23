//! Scoped local Git operations. This service never accepts arbitrary Git argv.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use thiserror::Error;

use crate::git_status::PathStatus;

#[derive(Debug, Error)]
pub enum GitServiceError {
    #[error("workspace must be the root of a non-bare Git worktree")]
    InvalidRoot,
    #[error("path is outside the worktree or crosses a symlink: {0}")]
    InvalidPath(PathBuf),
    #[error("invalid local branch: {0}")]
    InvalidBranch(String),
    #[error("no upstream is configured for this branch")]
    NoUpstream,
    #[error("git {operation} could not start: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("git {operation} failed: {message}")]
    Git {
        operation: &'static str,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn linked_worktree_is_accepted() {
        let dir = repo();
        let linked = TempDir::new().unwrap();
        let path = linked.path().join("tree");
        git(
            dir.path(),
            &["worktree", "add", "-qb", "linked", path.to_str().unwrap()],
        );
        let service = LocalGit::new(&path).unwrap();
        assert_eq!(service.root(), path.canonicalize().unwrap());
        fs::write(path.join("linked-file"), "linked\n").unwrap();
        service.stage(Path::new("linked-file")).unwrap();
        assert!(service.status().unwrap()[Path::new("linked-file")]
            .staged
            .is_some());
    }

    fn repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "forge@example.test"]);
        git(dir.path(), &["config", "user.name", "Forge Test"]);
        fs::write(dir.path().join("a"), "base\n").unwrap();
        git(dir.path(), &["add", "a"]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        dir
    }

    #[test]
    fn root_and_path_scope() {
        let dir = repo();
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        assert!(matches!(
            LocalGit::new(&nested),
            Err(GitServiceError::InvalidRoot)
        ));
        let service = LocalGit::new(dir.path()).unwrap();
        for path in ["../outside", ".git/config", "a/../../outside"] {
            assert!(matches!(
                service.stage(Path::new(path)),
                Err(GitServiceError::InvalidPath(_))
            ));
        }
        let outside = TempDir::new().unwrap();
        assert!(matches!(
            service.stage(outside.path()),
            Err(GitServiceError::InvalidPath(_))
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
            assert!(matches!(
                service.stage(Path::new("link/file")),
                Err(GitServiceError::InvalidPath(_))
            ));
        }
    }

    #[test]
    fn status_stage_unstage_commit_and_upstream() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        fs::write(dir.path().join("new file"), "new\n").unwrap();
        service.stage(Path::new("new file")).unwrap();
        assert!(service.status().unwrap()[Path::new("new file")]
            .staged
            .is_some());
        service.unstage(Path::new("new file")).unwrap();
        assert!(service.status().unwrap()[Path::new("new file")]
            .staged
            .is_none());
        service.stage(Path::new("new file")).unwrap();
        service.commit("add new file").unwrap();
        assert!(service.status().unwrap().is_empty());
        assert_eq!(service.branch_status().unwrap().upstream, None);
        assert!(matches!(service.pull(), Err(GitServiceError::NoUpstream)));
        assert!(matches!(service.push(), Err(GitServiceError::NoUpstream)));
        let branch = service.branch_status().unwrap().branch.unwrap();
        git(dir.path(), &["branch", "tracking"]);
        git(
            dir.path(),
            &["branch", "--set-upstream-to", "tracking", &branch],
        );
        fs::write(dir.path().join("a"), "ahead\n").unwrap();
        service.stage(Path::new("a")).unwrap();
        service.commit("ahead").unwrap();
        assert_eq!(
            (
                service.branch_status().unwrap().ahead,
                service.branch_status().unwrap().behind
            ),
            (1, 0)
        );
    }

    #[test]
    fn merge_reports_conflicts_and_rejects_revisions() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        let main = service.branch_status().unwrap().branch.unwrap();
        git(dir.path(), &["checkout", "-qb", "side"]);
        fs::write(dir.path().join("a"), "side\n").unwrap();
        git(dir.path(), &["commit", "-qam", "side"]);
        git(dir.path(), &["checkout", "-q", &main]);
        fs::write(dir.path().join("a"), "main\n").unwrap();
        service.stage(Path::new("a")).unwrap();
        service.commit("main").unwrap();
        assert!(matches!(
            service.merge("HEAD~1"),
            Err(GitServiceError::InvalidBranch(_))
        ));
        assert!(service.merge("side").is_err());
        let state = service.conflict_state().unwrap();
        assert!(state.merge_in_progress);
        assert_eq!(state.paths, vec![PathBuf::from("a")]);
    }

    #[test]
    fn branches_and_current_branch_report_where_head_is() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        let main = service.current_branch().unwrap().unwrap();
        git(dir.path(), &["branch", "alpha"]);
        git(dir.path(), &["branch", "feature/x"]);
        // A remote-tracking ref is not a branch this can switch to.
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/remote", "HEAD"],
        );
        assert_eq!(
            service.branches().unwrap(),
            vec!["alpha".to_string(), "feature/x".to_string(), main.clone()]
        );
        assert_eq!(service.current_branch().unwrap().as_deref(), Some(&*main));

        // A branch sharing its name with a tag. `%(refname:short)` would
        // disambiguate it against the tag and print `heads/alpha`, which is not
        // a name `switch` can land on — the whole reason this reads `lstrip=2`.
        git(dir.path(), &["tag", "alpha", &main]);
        assert!(
            service.branches().unwrap().contains(&"alpha".to_string()),
            "a name that collides with a tag must still read as itself: {:?}",
            service.branches().unwrap()
        );

        // Detached: no branch, and that is not an error.
        git(dir.path(), &["checkout", "-q", "--detach"]);
        assert_eq!(service.current_branch().unwrap(), None);
        assert!(service.branches().unwrap().contains(&main));

        // An unborn branch has no commit for `rev-parse` to resolve, but its
        // name is still the answer.
        let fresh = TempDir::new().unwrap();
        git(fresh.path(), &["init", "-q", "-b", "trunk"]);
        let unborn = LocalGit::new(fresh.path()).unwrap();
        assert_eq!(unborn.current_branch().unwrap().as_deref(), Some("trunk"));
        assert!(unborn.branches().unwrap().is_empty());
    }

    #[test]
    fn create_branch_creates_and_switches() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        let main = service.current_branch().unwrap().unwrap();
        service.create_branch("feature/one").unwrap();
        assert_eq!(
            service.current_branch().unwrap().as_deref(),
            Some("feature/one")
        );
        assert_eq!(
            service.branches().unwrap(),
            vec!["feature/one".to_string(), main]
        );
        // The commit it was created from is untouched: the new branch starts
        // where `HEAD` was.
        assert_eq!(service.status().unwrap().len(), 0);
        // Git refuses a duplicate rather than moving the existing branch.
        assert!(service.create_branch("feature/one").is_err());
    }

    #[test]
    fn switch_branch_moves_head() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        let main = service.current_branch().unwrap().unwrap();
        git(dir.path(), &["branch", "other"]);
        service.switch_branch("other").unwrap();
        assert_eq!(service.current_branch().unwrap().as_deref(), Some("other"));
        service.switch_branch(&main).unwrap();
        assert_eq!(service.current_branch().unwrap().as_deref(), Some(&*main));
        // A branch that does not exist fails in Git's own words.
        assert!(service.switch_branch("missing").is_err());
    }

    /// The guard is pure, so it can be pinned down case by case without a
    /// repository — and nothing it refuses can reach a subprocess, because it
    /// runs before the only call that spawns one.
    #[test]
    fn branch_name_validation_refuses_options_and_illegal_refs() {
        for name in [
            "",
            "   ",
            "\t",
            "-branch",
            "--force",
            "-",
            "feat/x~1",
            "feat^x",
            "feat:x",
            "feat?x",
            "feat*x",
            "feat[x",
            "feat\\x",
            "feat x",
            "feat..x",
            "feat@{x",
            "feat//x",
            "feat/",
            "feat.lock",
            "feat\nx",
            "feat\u{7}x",
        ] {
            assert!(
                matches!(
                    valid_branch_name(name),
                    Err(GitServiceError::InvalidBranch(_))
                ),
                "{name:?} must be refused"
            );
        }
        for name in ["main", "feat/x", "release-1.0", "a-b", "user/fix-1", "v1.2"] {
            assert!(valid_branch_name(name).is_ok(), "{name:?} must be allowed");
        }
    }

    #[test]
    fn branch_operations_refuse_an_option_shaped_name_before_spawning() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        let before = service.current_branch().unwrap();
        for name in ["--force", "-d", "feat/x~1", ""] {
            assert!(matches!(
                service.create_branch(name),
                Err(GitServiceError::InvalidBranch(_))
            ));
            assert!(matches!(
                service.switch_branch(name),
                Err(GitServiceError::InvalidBranch(_))
            ));
        }
        // `--detach` is the sharp case: had it reached Git as argv, HEAD would
        // now be detached. It is still on the branch, and no branch was made.
        assert!(matches!(
            service.switch_branch("--detach"),
            Err(GitServiceError::InvalidBranch(_))
        ));
        assert_eq!(service.current_branch().unwrap(), before);
        assert_eq!(service.branches().unwrap(), vec![before.unwrap()]);
    }

    #[test]
    fn abort_merge_restores_the_tree_after_a_conflict() {
        let dir = repo();
        let service = LocalGit::new(dir.path()).unwrap();
        // Nothing to abort yet: Git's own refusal, which the sync cache also
        // guards against before it spawns anything.
        assert!(service.abort_merge().is_err());

        let main = service.current_branch().unwrap().unwrap();
        git(dir.path(), &["switch", "-qc", "side"]);
        fs::write(dir.path().join("a"), "side\n").unwrap();
        git(dir.path(), &["commit", "-qam", "side"]);
        git(dir.path(), &["switch", "-q", &main]);
        fs::write(dir.path().join("a"), "main\n").unwrap();
        service.stage(Path::new("a")).unwrap();
        service.commit("main").unwrap();

        let conflict = service.merge("side").unwrap_err();
        // The reason a conflict gives lives on Git's stdout, so the message
        // must not come back empty.
        assert!(conflict.to_string().contains("CONFLICT"), "{conflict}");
        assert!(service.conflict_state().unwrap().merge_in_progress);
        // Both sides are in the working tree, marked, and the merge is not
        // finished: `abort` has something to undo.
        assert!(fs::read_to_string(dir.path().join("a"))
            .unwrap()
            .starts_with("<<<<<<< HEAD\nmain\n=======\nside\n>>>>>>> "));

        service.abort_merge().unwrap();
        let state = service.conflict_state().unwrap();
        assert!(!state.merge_in_progress);
        assert!(state.paths.is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("a")).unwrap(), "main\n");
        assert!(service.status().unwrap().is_empty());
        assert_eq!(service.current_branch().unwrap().as_deref(), Some(&*main));
    }

    #[test]
    fn pull_and_push_use_configured_upstream() {
        let dir = repo();
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        let remote_path = remote.path().to_str().unwrap();
        git(dir.path(), &["remote", "add", "origin", remote_path]);
        let service = LocalGit::new(dir.path()).unwrap();
        let branch = service.branch_status().unwrap().branch.unwrap();
        git(dir.path(), &["push", "-u", "origin", &branch]);
        fs::write(dir.path().join("a"), "pushed\n").unwrap();
        service.stage(Path::new("a")).unwrap();
        service.commit("pushed").unwrap();
        service.push().unwrap();

        let other = TempDir::new().unwrap();
        git(
            other.path(),
            &["clone", "-q", "-b", &branch, remote_path, "."],
        );
        git(
            other.path(),
            &["config", "user.email", "forge@example.test"],
        );
        git(other.path(), &["config", "user.name", "Forge Test"]);
        fs::write(other.path().join("a"), "pulled\n").unwrap();
        git(other.path(), &["commit", "-qam", "pulled"]);
        git(other.path(), &["push", "-q"]);
        service.pull().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a")).unwrap(),
            "pulled\n"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictState {
    pub paths: Vec<PathBuf>,
    pub merge_in_progress: bool,
}

#[derive(Debug, Clone)]
pub struct LocalGit {
    root: PathBuf,
}

impl LocalGit {
    /// Refuse a nested workspace: repo-wide operations must not affect files
    /// outside the selected workspace. Git resolves linked worktrees itself.
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, GitServiceError> {
        let root = workspace
            .as_ref()
            .canonicalize()
            .map_err(|_| GitServiceError::InvalidRoot)?;
        if !root.is_dir() {
            return Err(GitServiceError::InvalidRoot);
        }
        let output = scoped_command(&root)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|source| GitServiceError::Io {
                operation: "rev-parse",
                source,
            })?;
        let top = String::from_utf8_lossy(&output.stdout);
        if !output.status.success()
            || Path::new(top.trim_end_matches(['\r', '\n']))
                .canonicalize()
                .ok()
                .as_deref()
                != Some(root.as_path())
        {
            return Err(GitServiceError::InvalidRoot);
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run(&self, operation: &'static str, args: &[&str]) -> Result<String, GitServiceError> {
        output_text(operation, self.spawn(operation, args)?)
    }

    /// The one place a Git subprocess is started, so every call gets the same
    /// scrubbed environment. Callers that must read a non-zero exit themselves
    /// (a detached `HEAD`) use this instead of [`Self::run`].
    fn spawn(&self, operation: &'static str, args: &[&str]) -> Result<Output, GitServiceError> {
        scoped_command(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .output()
            .map_err(|source| GitServiceError::Io { operation, source })
    }

    /// Paths may be relative to the root or absolute within it. Reject
    /// symlinks, including symlinked ancestors of missing/deleted paths.
    fn path(&self, path: &Path) -> Result<PathBuf, GitServiceError> {
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| GitServiceError::InvalidPath(path.into()))?
        } else {
            path
        };
        if rel.as_os_str().is_empty()
            || rel.components().any(|c| !matches!(c, Component::Normal(_)))
            || rel
                .components()
                .next()
                .is_some_and(|c| c.as_os_str() == ".git")
        {
            return Err(GitServiceError::InvalidPath(path.into()));
        }
        let mut current = self.root.clone();
        for component in rel.components() {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(GitServiceError::InvalidPath(path.into()));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(GitServiceError::InvalidPath(path.into())),
            }
        }
        Ok(rel.into())
    }

    pub fn status(&self) -> Result<HashMap<PathBuf, PathStatus>, GitServiceError> {
        let output = self.run(
            "status",
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=1",
                "-z",
                "-uall",
            ],
        )?;
        // Parse bytes rather than text so unusual filenames and NUL separators
        // retain the same semantics as the workspace status cache.
        crate::git_status::parse_null_terminated(output.as_bytes()).map_err(|message| {
            GitServiceError::Git {
                operation: "status",
                message,
            }
        })
    }

    pub fn branch_status(&self) -> Result<BranchStatus, GitServiceError> {
        let branch = self
            .run("branch", &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_owned());
        let upstream = self
            .run(
                "upstream",
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            )
            .ok()
            .map(|s| s.trim().to_owned());
        let (ahead, behind) = if upstream.is_some() {
            let counts = self.run(
                "rev-list",
                &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
            )?;
            let mut parts = counts.split_whitespace();
            (
                parts.next().unwrap_or("0").parse().unwrap_or(0),
                parts.next().unwrap_or("0").parse().unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        Ok(BranchStatus {
            detached: branch.is_none(),
            branch,
            upstream,
            ahead,
            behind,
        })
    }

    /// Local branch names, sorted. Remote-tracking refs are deliberately left
    /// out: every name here is something `switch` can actually land on.
    pub fn branches(&self) -> Result<Vec<String>, GitServiceError> {
        // `lstrip=2` rather than `:short`: `:short` disambiguates against
        // tags and remote-tracking refs, so a branch that shares its name with
        // either prints as `heads/<name>` — a name `switch` cannot land on.
        // Stripping the fixed `refs/heads/` prefix yields the branch name
        // itself, always.
        let output = self.run(
            "for-each-ref",
            &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"],
        )?;
        let mut names: Vec<String> = output
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// `HEAD`'s branch, or `None` when detached.
    ///
    /// `symbolic-ref` rather than `rev-parse`: it still answers on an unborn
    /// branch, where there is no commit for `rev-parse` to resolve.
    pub fn current_branch(&self) -> Result<Option<String>, GitServiceError> {
        let output = self.spawn(
            "symbolic-ref",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        if output.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            ));
        }
        // A detached `HEAD` is the one expected non-zero exit, and `--quiet`
        // keeps it silent. Anything that says why it failed is a real failure.
        if output.stderr.is_empty() {
            return Ok(None);
        }
        Err(GitServiceError::Git {
            operation: "symbolic-ref",
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    /// Create `name` and switch to it.
    ///
    /// The name is a single argv element, so it can never be split into extra
    /// arguments — but Git would still read a leading `-` as an option, which
    /// is why the name is validated before anything is spawned.
    pub fn create_branch(&self, name: &str) -> Result<String, GitServiceError> {
        valid_branch_name(name)?;
        self.run("switch", &["switch", "-c", name])
    }

    /// Switch to an existing local branch, under the same guard as
    /// [`Self::create_branch`].
    pub fn switch_branch(&self, name: &str) -> Result<String, GitServiceError> {
        valid_branch_name(name)?;
        // `--` ends option parsing, so even a name this guard lets through can
        // only be read as a ref.
        self.run("switch", &["switch", "--", name])
    }

    /// Abandon an in-progress merge, restoring the pre-merge working tree.
    /// Git itself refuses this when there is nothing to abort.
    pub fn abort_merge(&self) -> Result<String, GitServiceError> {
        self.run("merge", &["merge", "--abort"])
    }

    pub fn conflict_state(&self) -> Result<ConflictState, GitServiceError> {
        let mut paths: Vec<_> = self
            .status()?
            .into_iter()
            .filter_map(|(path, status)| status.is_conflicted().then_some(path))
            .collect();
        paths.sort();
        let merge_head = self.run("merge state", &["rev-parse", "--git-path", "MERGE_HEAD"])?;
        let merge_head = Path::new(merge_head.trim());
        let merge_head = if merge_head.is_absolute() {
            merge_head.to_path_buf()
        } else {
            self.root.join(merge_head)
        };
        Ok(ConflictState {
            paths,
            merge_in_progress: merge_head.exists(),
        })
    }

    pub fn stage(&self, path: &Path) -> Result<(), GitServiceError> {
        let path = self.path(path)?;
        self.run("add", &["add", "--", &path.to_string_lossy()])?;
        Ok(())
    }

    pub fn unstage(&self, path: &Path) -> Result<(), GitServiceError> {
        let path = self.path(path)?;
        // `reset` is deliberately not used. `restore --staged` also works
        // for initial commits when the index has no HEAD tree.
        if self
            .run("restore", &["rev-parse", "--verify", "HEAD"])
            .is_ok()
        {
            self.run(
                "restore",
                &["restore", "--staged", "--", &path.to_string_lossy()],
            )?;
        } else {
            self.run(
                "rm",
                &["rm", "--cached", "-r", "--", &path.to_string_lossy()],
            )?;
        }
        Ok(())
    }

    pub fn commit(&self, message: &str) -> Result<String, GitServiceError> {
        if message.trim().is_empty() || message.contains('\0') {
            return Err(GitServiceError::Git {
                operation: "commit",
                message: "commit message must not be empty or contain NUL".into(),
            });
        }
        self.run("commit", &["commit", "-m", message])
    }

    /// Pull from the configured upstream, retaining ordinary merge conflicts.
    pub fn pull(&self) -> Result<String, GitServiceError> {
        self.upstream()?;
        self.run("pull", &["pull", "--no-rebase", "--no-edit"])
    }

    pub fn push(&self) -> Result<String, GitServiceError> {
        self.upstream()?;
        self.run("push", &["push"])
    }

    fn upstream(&self) -> Result<(), GitServiceError> {
        if self.branch_status()?.upstream.is_none() {
            Err(GitServiceError::NoUpstream)
        } else {
            Ok(())
        }
    }

    /// Merge only an existing local branch; never interpret a user string as
    /// an option, remote, or arbitrary revision expression.
    pub fn merge(&self, branch: &str) -> Result<String, GitServiceError> {
        if branch.is_empty() || branch.starts_with('-') || branch.contains('\0') {
            return Err(GitServiceError::InvalidBranch(branch.into()));
        }
        let refname = format!("refs/heads/{branch}");
        if self
            .run(
                "branch lookup",
                &["show-ref", "--verify", "--quiet", &refname],
            )
            .is_err()
        {
            return Err(GitServiceError::InvalidBranch(branch.into()));
        }
        self.run("merge", &["merge", "--no-edit", "--", &refname])
    }
}

/// A branch name handed to Git as one argv element still has to be refused when
/// Git would read it as an option, or when it is not a legal ref name at all.
///
/// Pure by construction: it takes no service and starts nothing, which is what
/// makes "refused before spawning" checkable in a test.
fn valid_branch_name(name: &str) -> Result<(), GitServiceError> {
    let illegal = name
        .chars()
        .any(|c| matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\') || c.is_control());
    if name.trim().is_empty()
        || name.starts_with('-')
        || illegal
        || name.ends_with('/')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
    {
        return Err(GitServiceError::InvalidBranch(name.to_owned()));
    }
    Ok(())
}

fn scoped_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    command
}

fn output_text(operation: &'static str, output: Output) -> Result<String, GitServiceError> {
    if !output.status.success() {
        // Git's `fatal:` lines go to stderr, but its ordinary progress does
        // not: a conflicting `merge` explains itself on stdout, so an empty
        // stderr must not become an empty message.
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let message = if stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        } else {
            stderr
        };
        return Err(GitServiceError::Git { operation, message });
    }
    String::from_utf8(output.stdout).map_err(|e| GitServiceError::Git {
        operation,
        message: e.to_string(),
    })
}
