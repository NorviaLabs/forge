//! Git as Forge itself runs it: confined to the repository it is run on.
//!
//! A repository's `.git/config` and hooks decide what git executes (hooks,
//! `core.fsmonitor`, filter and diff drivers, aliases), and a confined agent
//! can write them. A git that Forge ran unconfined on that repository would
//! execute whatever the agent planted, outside the sandbox. So no git Forge
//! starts on a repository is unconfined: it reads the disk like any confined
//! process, writes only the worktree and its git dirs, and has no network.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{availability, wrap_command, SandboxPolicy};

/// `git -C <repo>`, confined to the worktree containing `repo` and its git
/// dirs. Add the subcommand and its arguments to the returned command.
///
/// On a host that cannot confine at all, the command is plain `git`: there
/// the agent's own commands cannot be confined either, and every one of them
/// is already put to the user.
pub fn command(repo: &Path) -> Command {
    command_writing(repo, &[])
}

/// As [`command`], also allowed to write `also`: the directory a worktree is
/// created in, or the worktree being removed.
pub fn command_writing(repo: &Path, also: &[&Path]) -> Command {
    let args = vec!["-C".to_string(), repo.to_string_lossy().into_owned()];
    let wrapped = availability().ok().and_then(|()| {
        let mut policy = SandboxPolicy::for_workspace(worktree_root(repo)).with_git_open();
        for path in also {
            policy = policy.with_write_path(path);
        }
        wrap_command("git", &args, &policy)
    });
    let (program, args) = wrapped.unwrap_or_else(|| ("git".to_string(), args));
    let mut command = Command::new(program);
    command.args(args);
    command
}

/// `git -C <repo>` on the host, not confined.
///
/// For the operations that cannot work confined yet: the ones that contact
/// a remote or check files out (credentials, SSH keys, an LFS download) and
/// the ones that create a commit (signing keys). Each call of this function
/// is a place where a hook or a program named by the repository's config
/// still runs outside the sandbox, so the agent must not be able to write
/// those, and no new call should be added without that reason.
pub fn unconfined(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo);
    command
}

/// The root of the worktree `dir` is in: the nearest ancestor holding a
/// `.git` entry, or `dir` itself when there is none (a bare repository, or
/// not a repository).
fn worktree_root(dir: &Path) -> PathBuf {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    dir.ancestors()
        .find(|candidate| candidate.join(".git").symlink_metadata().is_ok())
        .unwrap_or(&dir)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        command(repo).args(args).output().unwrap()
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.test"],
            &["config", "user.name", "T"],
            &["commit", "-q", "--allow-empty", "-m", "first"],
        ] {
            let out = git(dir.path(), args);
            assert!(out.status.success(), "{args:?}: {out:?}");
        }
        dir
    }

    #[test]
    fn worktree_root_is_the_nearest_ancestor_with_a_git_entry() {
        let repo = repository();
        let nested = repo.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(worktree_root(&nested), repo.path().canonicalize().unwrap());

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            worktree_root(plain.path()),
            plain.path().canonicalize().unwrap()
        );
    }

    /// The point of the module: a program the repository's own config names
    /// runs inside the sandbox. Here `core.fsmonitor` names a script that
    /// tries to write outside the repository; `git status` runs it, and the
    /// write is refused.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_program_named_by_repository_config_runs_confined() {
        if availability().is_err() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;

        let repo = repository();
        let outside = tempfile::tempdir().unwrap();
        let escaped = outside.path().join("escaped");
        let inside = repo.path().join("ran");
        let script = repo.path().join("monitor.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n: > {}\n: > {}\n",
                inside.display(),
                escaped.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = git(
            repo.path(),
            &["config", "core.fsmonitor", script.to_str().unwrap()],
        );
        assert!(out.status.success(), "{out:?}");

        let _ = git(repo.path(), &["status", "--porcelain"]);
        assert!(inside.exists(), "the configured program must have run");
        assert!(
            !escaped.exists(),
            "and must not have written outside the repository"
        );
    }

    /// Creating and removing a linked worktree elsewhere works once the
    /// destination is named, and git run inside that worktree reaches the
    /// shared git dir.
    #[test]
    fn a_linked_worktree_can_be_created_used_and_removed() {
        let repo = repository();
        let elsewhere = tempfile::tempdir().unwrap();
        let worktree = elsewhere.path().join("wt");

        let out = command_writing(repo.path(), &[&worktree])
            .args(["worktree", "add", "-q", "-b", "side"])
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");

        std::fs::write(worktree.join("file.txt"), "x").unwrap();
        for args in [&["add", "file.txt"][..], &["commit", "-q", "-m", "second"]] {
            let out = git(&worktree, args);
            assert!(out.status.success(), "{args:?}: {out:?}");
        }

        let out = command_writing(repo.path(), &[&worktree])
            .args(["worktree", "remove"])
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(!worktree.exists());
    }

    /// Forge keeps managed worktrees under the repository's `.forge/`, which
    /// is otherwise read-only. Naming the worktree is what lets git create
    /// its administrative files there and delete it again.
    #[test]
    fn a_worktree_under_dot_forge_can_be_removed() {
        let repo = repository();
        let worktree = repo.path().join(".forge/local/worktrees/session-1");
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let out = unconfined(repo.path())
            .args(["worktree", "add", "-q", "-b", "managed"])
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");

        let out = command_writing(repo.path(), &[&worktree])
            .args(["worktree", "remove", "--force"])
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(!worktree.exists());
    }

    /// A `.git` pointer redirected at a directory that does not name this
    /// worktree back gives no access to it.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_redirected_gitdir_pointer_grants_nothing() {
        if availability().is_err() {
            return;
        }
        let victim = tempfile::tempdir().unwrap();
        let fake = victim.path().join("worktrees/x");
        std::fs::create_dir_all(&fake).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join(".git"),
            format!("gitdir: {}\n", fake.display()),
        )
        .unwrap();

        let policy = SandboxPolicy::for_workspace(workspace.path()).with_git_open();
        let profile = crate::seatbelt_profile(&policy).unwrap();
        let victim = victim.path().canonicalize().unwrap();
        assert!(
            !profile.contains(victim.to_str().unwrap()),
            "the redirected directory must not appear in the profile: {profile}"
        );
    }
}
