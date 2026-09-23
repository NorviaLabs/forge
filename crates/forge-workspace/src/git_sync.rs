//! Branch state and the network operations, kept off the render path.
//!
//! [`crate::git_status::GitStatusCache`] is the model: a worker thread owns the
//! subprocess and the event loop only polls a receiver. `pull` and `push` are
//! the reason this module exists — either can block for seconds on a network
//! round trip, which is exactly what must never happen inside a frame.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::git_service::{BranchStatus, LocalGit};

/// The operations that talk to a remote. Serialized by construction: Git takes
/// its own lock, so a second concurrent operation could only wait or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOperation {
    Pull,
    Push,
}

impl SyncOperation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
        }
    }

    /// Present tense, for the row that says what is happening now.
    pub fn active(self) -> &'static str {
        match self {
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
        }
    }

    pub fn finished(self) -> &'static str {
        match self {
            Self::Pull => "Pulled",
            Self::Push => "Pushed",
        }
    }

    fn run(self, git: &LocalGit) -> Result<String, String> {
        match self {
            Self::Pull => git.pull(),
            Self::Push => git.push(),
        }
        .map_err(|error| error.to_string())
    }
}

/// A finished `pull` or `push`, ready to report.
#[derive(Debug)]
pub struct SyncOutcome {
    pub operation: SyncOperation,
    pub result: Result<String, String>,
}

/// Branch/upstream/ahead-behind, plus at most one in-flight operation.
///
/// The last branch read is kept rather than cleared on failure, and the error
/// is kept beside it, so a repository this code cannot read renders as
/// *unknown* — never as "in sync", which an empty `BranchStatus` would claim.
#[derive(Default)]
pub struct GitSyncCache {
    pub branch: Option<BranchStatus>,
    pub error: Option<String>,
    pub loading: bool,
    /// The operation running now, if any. Rendering reads this for the row.
    pub running: Option<SyncOperation>,
    pending_branch: Option<Receiver<Result<BranchStatus, String>>>,
    /// Latest root requested while a read is still running.
    queued_branch: Option<PathBuf>,
    pending_operation: Option<Receiver<SyncOutcome>>,
}

impl GitSyncCache {
    /// Re-read the branch. Coalesces onto an in-flight read, keeping only the
    /// latest root, the way `GitStatusCache` does.
    pub fn start_branch_refresh(&mut self, root: PathBuf) {
        if self.pending_branch.is_some() {
            self.queued_branch = Some(root);
            return;
        }
        self.spawn_branch(root);
    }

    fn spawn_branch(&mut self, root: PathBuf) {
        self.loading = true;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = LocalGit::new(&root)
                .and_then(|git| git.branch_status())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.pending_branch = Some(receiver);
    }

    /// Non-blocking, safe from the render loop. Returns `true` when a branch
    /// read resolved this call.
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = self.pending_branch.take() else {
            return false;
        };
        let resolved = match receiver.try_recv() {
            Ok(Ok(branch)) => {
                self.loading = false;
                self.error = None;
                self.branch = Some(branch);
                true
            }
            Ok(Err(error)) => {
                self.loading = false;
                self.error = Some(error);
                true
            }
            Err(TryRecvError::Empty) => {
                self.pending_branch = Some(receiver);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.loading = false;
                self.error = Some("branch refresh disconnected".into());
                true
            }
        };
        if resolved {
            if let Some(root) = self.queued_branch.take() {
                self.spawn_branch(root);
            }
        }
        resolved
    }

    /// Whether a remote is known, which is what `pull`/`push` need. A branch
    /// with no upstream cannot be pushed to or pulled from, and saying so
    /// before the round trip is the point of reading this first.
    pub fn upstream(&self) -> Option<&str> {
        self.branch
            .as_ref()
            .and_then(|branch| branch.upstream.as_deref())
    }

    /// Start `pull` or `push`. Refused while another is running and refused
    /// with no upstream: both would otherwise only fail after a round trip.
    pub fn start_operation(
        &mut self,
        root: PathBuf,
        operation: SyncOperation,
    ) -> Result<(), String> {
        if let Some(running) = self.running {
            return Err(format!(
                "a {} is already running; wait for it to finish",
                running.label()
            ));
        }
        if self.error.is_some() {
            return Err("the branch could not be read, so its upstream is unknown".into());
        }
        if self.upstream().is_none() {
            return Err(format!(
                "no upstream for this branch — {} has nowhere to go",
                operation.label()
            ));
        }
        self.running = Some(operation);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = match LocalGit::new(&root) {
                Ok(git) => operation.run(&git),
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(SyncOutcome { operation, result });
        });
        self.pending_operation = Some(receiver);
        Ok(())
    }

    /// Non-blocking. `Some` once the running operation has finished.
    pub fn poll_operation(&mut self) -> Option<SyncOutcome> {
        let receiver = self.pending_operation.take()?;
        match receiver.try_recv() {
            Ok(outcome) => {
                self.running = None;
                Some(outcome)
            }
            Err(TryRecvError::Empty) => {
                self.pending_operation = Some(receiver);
                None
            }
            Err(TryRecvError::Disconnected) => {
                let operation = self.running.take()?;
                Some(SyncOutcome {
                    operation,
                    result: Err("the Git operation disconnected".into()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) {
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
    }

    /// A repository with a real remote and an upstream, so `pull`/`push` can
    /// actually be attempted.
    fn repo_with_remote() -> (TempDir, TempDir) {
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "forge@example.test"]);
        git(dir.path(), &["config", "user.name", "Forge Test"]);
        std::fs::write(dir.path().join("a"), "one\n").unwrap();
        git(dir.path(), &["add", "a"]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        let remote_path = remote.path().to_str().unwrap();
        git(dir.path(), &["remote", "add", "origin", remote_path]);
        git(dir.path(), &["push", "-q", "-u", "origin", "main"]);
        (dir, remote)
    }

    fn settle_branch(cache: &mut GitSyncCache, root: &Path) {
        cache.start_branch_refresh(root.to_path_buf());
        for _ in 0..400 {
            if cache.poll() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn branch_read_reports_upstream_and_rejects_an_operation_without_one() {
        let (dir, _remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());

        let branch = cache.branch.as_ref().expect("branch read");
        assert_eq!(branch.branch.as_deref(), Some("main"));
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!(cache.upstream(), Some("origin/main"));

        // A repository with no upstream refuses before spawning anything.
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "-q", "-b", "main"]);
        let mut lonely = GitSyncCache::default();
        settle_branch(&mut lonely, bare.path());
        assert!(lonely.upstream().is_none());
        let refused = lonely.start_operation(bare.path().to_path_buf(), SyncOperation::Push);
        assert!(refused.is_err(), "a push with no upstream must be refused");
        assert!(lonely.running.is_none(), "and must not mark itself running");
    }

    #[test]
    fn push_and_pull_run_off_the_calling_thread_and_report_once() {
        let (dir, remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());

        std::fs::write(dir.path().join("a"), "two\n").unwrap();
        git(dir.path(), &["commit", "-qam", "second"]);

        cache
            .start_operation(dir.path().to_path_buf(), SyncOperation::Push)
            .unwrap();
        assert_eq!(cache.running, Some(SyncOperation::Push));

        // A second operation while one is in flight is refused rather than
        // queued: two racing subprocesses on one repository help nobody.
        assert!(cache
            .start_operation(dir.path().to_path_buf(), SyncOperation::Pull)
            .is_err());

        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, SyncOperation::Push);
        outcome.result.as_ref().unwrap();
        assert!(cache.running.is_none());
        // It really reached the remote. The bare repo's own `HEAD` still points
        // at its default branch, so name the ref that was pushed.
        let head = Command::new("git")
            .args([
                "-C",
                remote.path().to_str().unwrap(),
                "log",
                "-1",
                "--pretty=%s",
                "refs/heads/main",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "second");

        // The outcome is delivered exactly once.
        assert!(cache.poll_operation().is_none());
    }

    #[test]
    fn ahead_counts_move_from_the_cached_branch() {
        let (dir, _remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branch.as_ref().unwrap().ahead, 0);

        std::fs::write(dir.path().join("a"), "ahead\n").unwrap();
        git(dir.path(), &["commit", "-qam", "ahead"]);
        // The cached read is stale until refreshed — the row must not guess.
        assert_eq!(cache.branch.as_ref().unwrap().ahead, 0);
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branch.as_ref().unwrap().ahead, 1);
    }

    /// Poll until the operation lands, failing the test rather than hanging.
    fn loop_with(cache: &mut GitSyncCache) -> SyncOutcome {
        for _ in 0..600 {
            if let Some(outcome) = cache.poll_operation() {
                return outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the operation never reported");
    }
}
