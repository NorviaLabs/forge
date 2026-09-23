//! Branch state and the network operations, kept off the render path.
//!
//! [`crate::git_status::GitStatusCache`] is the model: a worker thread owns the
//! subprocess and the event loop only polls a receiver. `pull` and `push` are
//! the reason this module exists — either can block for seconds on a network
//! round trip, which is exactly what must never happen inside a frame. A merge
//! rewrites the working tree and can conflict, so it runs here too.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::git_service::{BranchStatus, ConflictState, LocalGit};

/// The operations the sync row can run. Serialized by construction: Git takes
/// its own lock, so a second concurrent operation could only wait or fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOperation {
    Pull,
    Push,
    /// Merge a local branch into the current one. Conflicts are a normal
    /// outcome, not a refusal: the tree is left for the operator to resolve.
    Merge {
        branch: String,
    },
    /// Move HEAD to an existing local branch.
    Switch {
        branch: String,
    },
    /// Create a local branch from `HEAD` and move to it.
    Create {
        branch: String,
    },
    /// Fetch the configured upstream's remote. Moves the remote-tracking refs,
    /// so a finished fetch leaves the cached ahead/behind stale — the caller
    /// refreshes the branch, exactly as it does after a pull or a push.
    Fetch,
    /// Delete a local branch. `force` is the difference between Git refusing to
    /// lose commits that are not merged yet and losing them.
    DeleteBranch {
        branch: String,
        force: bool,
    },
    /// Rename a local branch in place.
    RenameBranch {
        from: String,
        to: String,
    },
    /// Give up on the merge in progress and restore the pre-merge tree.
    AbortMerge,
}

impl SyncOperation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
            Self::Merge { .. } => "merge",
            Self::Switch { .. } => "switch",
            Self::Create { .. } => "create",
            Self::Fetch => "fetch",
            Self::DeleteBranch { .. } => "delete branch",
            Self::RenameBranch { .. } => "rename branch",
            Self::AbortMerge => "abort merge",
        }
    }

    /// Present tense, for the row that says what is happening now.
    pub fn active(&self) -> &'static str {
        match self {
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
            Self::Merge { .. } => "Merging",
            Self::Switch { .. } => "Switching",
            Self::Create { .. } => "Creating",
            Self::Fetch => "Fetching",
            Self::DeleteBranch { .. } => "Deleting",
            Self::RenameBranch { .. } => "Renaming",
            Self::AbortMerge => "Aborting merge",
        }
    }

    pub fn finished(&self) -> &'static str {
        match self {
            Self::Pull => "Pulled",
            Self::Push => "Pushed",
            Self::Merge { .. } => "Merged",
            Self::Switch { .. } => "Switched",
            Self::Create { .. } => "Created",
            Self::Fetch => "Fetched",
            Self::DeleteBranch { .. } => "Deleted",
            Self::RenameBranch { .. } => "Renamed",
            Self::AbortMerge => "Aborted merge",
        }
    }

    fn run(&self, git: &LocalGit) -> Result<String, String> {
        match self {
            Self::Pull => git.pull(),
            Self::Push => git.push(),
            Self::Merge { branch } => git.merge(branch),
            Self::Switch { branch } => git.switch_branch(branch),
            Self::Create { branch } => git.create_branch(branch),
            Self::Fetch => git.fetch(),
            Self::DeleteBranch { branch, force } => git.delete_branch(branch, *force),
            Self::RenameBranch { from, to } => git.rename_branch(from, to),
            Self::AbortMerge => git.abort_merge(),
        }
        .map_err(|error| error.to_string())
    }
}

/// A finished sync operation, ready to report.
#[derive(Debug)]
pub struct SyncOutcome {
    pub operation: SyncOperation,
    pub result: Result<String, String>,
}

/// The three reads the refresh worker publishes together: one repository, read
/// once in one thread. Publishing them apart could show a branch, a branch list
/// and a conflict set from three different moments.
struct BranchRead {
    branch: BranchStatus,
    branches: Vec<String>,
    conflicts: ConflictState,
}

/// Branch/upstream/ahead-behind, plus at most one in-flight operation.
///
/// The last branch read is kept rather than cleared on failure, and the error
/// is kept beside it, so a repository this code cannot read renders as
/// *unknown* — never as "in sync", which an empty `BranchStatus` would claim.
#[derive(Default)]
pub struct GitSyncCache {
    pub branch: Option<BranchStatus>,
    /// Local branch names from the same read as `branch`, for the picker.
    pub branches: Vec<String>,
    /// Whether a merge is in progress, from the same read as `branch`. `None`
    /// means unknown, which is not the same as "no merge".
    pub conflicts: Option<ConflictState>,
    pub error: Option<String>,
    pub loading: bool,
    /// The operation running now, if any. Rendering reads this for the row.
    pub running: Option<SyncOperation>,
    pending_branch: Option<Receiver<Result<BranchRead, String>>>,
    /// Latest root requested while a read is still running.
    queued_branch: Option<PathBuf>,
    pending_operation: Option<Receiver<SyncOutcome>>,
}

impl GitSyncCache {
    /// Re-read the branch, the local branch list and the conflict state.
    /// Coalesces onto an in-flight read, keeping only the latest root, the way
    /// `GitStatusCache` does.
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
            // Three reads, one thread, one publish: the branch list and the
            // conflict state describe the same moment as the branch itself.
            let result = LocalGit::new(&root)
                .and_then(|git| {
                    Ok(BranchRead {
                        branch: git.branch_status()?,
                        branches: git.branches()?,
                        conflicts: git.conflict_state()?,
                    })
                })
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
            Ok(Ok(read)) => {
                self.loading = false;
                self.error = None;
                self.branch = Some(read.branch);
                self.branches = read.branches;
                self.conflicts = Some(read.conflicts);
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

    /// Start a sync operation. Refused while another is running, and refused
    /// for the reasons Git itself would only report after a subprocess: no
    /// upstream to reach, or a merge that is (or is not) in progress.
    pub fn start_operation(
        &mut self,
        root: PathBuf,
        operation: SyncOperation,
    ) -> Result<(), String> {
        if let Some(running) = &self.running {
            return Err(format!(
                "a {} is already running; wait for it to finish",
                running.label()
            ));
        }
        match &operation {
            // A remote is what these need, and reading the branch is how this
            // cache knows one exists. `fetch` is in here rather than with the
            // branch verbs because the remote it reaches comes from the same
            // upstream a push and a pull use.
            SyncOperation::Pull | SyncOperation::Push | SyncOperation::Fetch => {
                if self.error.is_some() {
                    return Err("the branch could not be read, so its upstream is unknown".into());
                }
                if self.upstream().is_none() {
                    return Err(format!(
                        "no upstream for this branch — {} has nowhere to go",
                        operation.label()
                    ));
                }
            }
            SyncOperation::Merge { .. } => {
                if self.merge_in_progress() {
                    return Err("a merge is already in progress".into());
                }
            }
            SyncOperation::AbortMerge => {
                if !self.merge_in_progress() {
                    return Err("no merge is in progress to abort".into());
                }
            }
            // A branch verb needs a readable repository, which is the same
            // condition the picker was opened under — re-checked here because
            // the picker can be left open while the branch state moves.
            SyncOperation::Switch { .. } | SyncOperation::Create { .. } => {
                if self.error.is_some() {
                    return Err(
                        "the branch could not be read, so there is nothing to move to".into(),
                    );
                }
                // Git refuses these itself while the index is unmerged, but
                // saying so before the worker starts costs nothing and reads
                // better than git's own wording.
                if matches!(operation, SyncOperation::Switch { .. }) && self.merge_in_progress() {
                    return Err("a merge is in progress — finish or abort it first".into());
                }
            }
            // Both act on the branch list the same read publishes, so an
            // unreadable repository is the only refusal they can make before
            // spawning. Deleting the branch `HEAD` is on is the service's own
            // refusal, made before it runs the verb — duplicating it here would
            // only be a second place to keep true.
            SyncOperation::DeleteBranch { .. } | SyncOperation::RenameBranch { .. } => {
                if self.error.is_some() {
                    return Err(
                        "the branch could not be read, so there is no branch list to act on".into(),
                    );
                }
            }
        }
        // The cache keeps a copy for the row while the worker thread owns the
        // original to report back with.
        self.running = Some(operation.clone());
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

    /// Whether the last read saw a merge to resolve. Unknown reads as "no
    /// merge", which is what the refusal texts need to say either way.
    fn merge_in_progress(&self) -> bool {
        self.conflicts
            .as_ref()
            .is_some_and(|conflicts| conflicts.merge_in_progress)
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

    /// A commit pushed to `remote` from a clone of it, so a fetch in the
    /// repository under test has something real to bring back.
    fn push_to_remote(remote: &Path, contents: &str) {
        let other = TempDir::new().unwrap();
        git(
            other.path(),
            &["clone", "-q", "-b", "main", remote.to_str().unwrap(), "."],
        );
        git(
            other.path(),
            &["config", "user.email", "forge@example.test"],
        );
        git(other.path(), &["config", "user.name", "Forge Test"]);
        std::fs::write(other.path().join("a"), contents).unwrap();
        git(other.path(), &["commit", "-qam", contents]);
        git(other.path(), &["push", "-q", "origin", "HEAD:main"]);
    }

    /// A ref's commit, for asserting that an operation moved it.
    fn read_ref(root: &Path, refname: &str) -> String {
        let output = Command::new("git")
            .args(["-C", root.to_str().unwrap(), "rev-parse", refname])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "rev-parse {refname}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
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

        // A fetch names a remote the same way, so it is refused on the same
        // read rather than left to fail on the network.
        let refused = lonely.start_operation(bare.path().to_path_buf(), SyncOperation::Fetch);
        assert_eq!(
            refused.unwrap_err(),
            "no upstream for this branch — fetch has nowhere to go"
        );
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
    fn fetch_runs_off_the_calling_thread_and_moves_the_tracking_ref() {
        let (dir, remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        push_to_remote(remote.path(), "remote\n");
        let tracking_before = read_ref(dir.path(), "refs/remotes/origin/main");
        let head_before = read_ref(dir.path(), "HEAD");

        cache
            .start_operation(dir.path().to_path_buf(), SyncOperation::Fetch)
            .unwrap();
        assert_eq!(cache.running, Some(SyncOperation::Fetch));

        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, SyncOperation::Fetch);
        outcome.result.as_ref().unwrap();
        assert!(cache.running.is_none());
        assert_ne!(
            read_ref(dir.path(), "refs/remotes/origin/main"),
            tracking_before,
            "the fetch must have moved the remote-tracking ref"
        );
        // A fetch brings refs back, not work: what `HEAD` names is untouched.
        assert_eq!(read_ref(dir.path(), "HEAD"), head_before);

        // The outcome is delivered exactly once, like every operation.
        assert!(cache.poll_operation().is_none());
    }

    /// A finished fetch moves the remote-tracking refs, so the cached
    /// ahead/behind is stale the moment it lands. `poll_operation` deliberately
    /// does not refresh — the caller owns the one refresh after every operation
    /// — and this pins that down rather than leaving it to be assumed.
    #[test]
    fn a_finished_fetch_leaves_the_cached_branch_stale_for_the_caller_to_refresh() {
        let (dir, remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branch.as_ref().unwrap().behind, 0);

        // A commit this branch has not seen, so `origin/main` is ahead of it.
        push_to_remote(remote.path(), "remote\n");

        cache
            .start_operation(dir.path().to_path_buf(), SyncOperation::Fetch)
            .unwrap();
        loop_with(&mut cache).result.unwrap();
        assert_eq!(
            cache.branch.as_ref().unwrap().behind,
            0,
            "the cache must not refresh itself behind the caller's back"
        );

        // The caller's refresh is what turns the moved ref into a count.
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branch.as_ref().unwrap().behind, 1);
    }

    #[test]
    fn delete_and_rename_operations_run_off_the_calling_thread() {
        let (dir, _remote) = repo_with_remote();
        git(dir.path(), &["branch", "doomed"]);
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        assert_eq!(
            cache.branches,
            vec!["doomed".to_string(), "main".to_string()]
        );

        let delete = SyncOperation::DeleteBranch {
            branch: "doomed".into(),
            force: false,
        };
        cache
            .start_operation(dir.path().to_path_buf(), delete.clone())
            .unwrap();
        assert_eq!(cache.running.as_ref(), Some(&delete));
        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, delete);
        outcome.result.unwrap();
        assert!(cache.running.is_none());
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branches, vec!["main".to_string()]);

        // Renaming the branch `HEAD` is on moves it; the cache only learns
        // that on its next read.
        let rename = SyncOperation::RenameBranch {
            from: "main".into(),
            to: "trunk".into(),
        };
        cache
            .start_operation(dir.path().to_path_buf(), rename.clone())
            .unwrap();
        assert_eq!(cache.running.as_ref(), Some(&rename));
        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, rename);
        outcome.result.unwrap();
        assert!(cache.running.is_none());
        settle_branch(&mut cache, dir.path());
        assert_eq!(
            cache.branch.as_ref().unwrap().branch.as_deref(),
            Some("trunk")
        );
        assert_eq!(cache.branches, vec!["trunk".to_string()]);
    }

    /// Deleting the branch `HEAD` is on is the service's refusal, so it is
    /// reported by the operation rather than turned away here — and it costs
    /// nothing, because the service refuses before it spawns the verb.
    #[test]
    fn deleting_the_current_branch_is_reported_by_the_operation() {
        let (dir, _remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());

        let delete = SyncOperation::DeleteBranch {
            branch: "main".into(),
            force: true,
        };
        cache
            .start_operation(dir.path().to_path_buf(), delete.clone())
            .unwrap();
        assert_eq!(
            cache.running.as_ref(),
            Some(&delete),
            "the cache must not duplicate the service's refusal"
        );
        let error = loop_with(&mut cache).result.unwrap_err();
        assert!(error.contains("HEAD is on it"), "{error}");

        settle_branch(&mut cache, dir.path());
        assert_eq!(
            cache.branch.as_ref().unwrap().branch.as_deref(),
            Some("main")
        );
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

    /// A repository whose `main` and `side` both edit the same line, so merging
    /// `side` conflicts. No remote: a merge needs none.
    fn repo_with_conflicting_branch() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "forge@example.test"]);
        git(dir.path(), &["config", "user.name", "Forge Test"]);
        std::fs::write(dir.path().join("a"), "base\n").unwrap();
        git(dir.path(), &["add", "a"]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        git(dir.path(), &["switch", "-qc", "side"]);
        std::fs::write(dir.path().join("a"), "side\n").unwrap();
        git(dir.path(), &["commit", "-qam", "side"]);
        git(dir.path(), &["switch", "-q", "main"]);
        std::fs::write(dir.path().join("a"), "main\n").unwrap();
        git(dir.path(), &["commit", "-qam", "main"]);
        dir
    }

    #[test]
    fn operation_labels_name_each_operation() {
        let merge = SyncOperation::Merge {
            branch: "side".into(),
        };
        assert_eq!(
            (merge.label(), merge.active(), merge.finished()),
            ("merge", "Merging", "Merged")
        );
        assert_eq!(
            (
                SyncOperation::AbortMerge.label(),
                SyncOperation::AbortMerge.active(),
                SyncOperation::AbortMerge.finished()
            ),
            ("abort merge", "Aborting merge", "Aborted merge")
        );
        assert_eq!(
            (
                SyncOperation::Fetch.label(),
                SyncOperation::Fetch.active(),
                SyncOperation::Fetch.finished()
            ),
            ("fetch", "Fetching", "Fetched")
        );
        let delete = SyncOperation::DeleteBranch {
            branch: "side".into(),
            force: false,
        };
        assert_eq!(
            (delete.label(), delete.active(), delete.finished()),
            ("delete branch", "Deleting", "Deleted")
        );
        let rename = SyncOperation::RenameBranch {
            from: "side".into(),
            to: "renamed".into(),
        };
        assert_eq!(
            (rename.label(), rename.active(), rename.finished()),
            ("rename branch", "Renaming", "Renamed")
        );
    }

    #[test]
    fn branch_refresh_publishes_branches_and_conflicts_with_the_branch() {
        let (dir, _remote) = repo_with_remote();
        git(dir.path(), &["branch", "feature/x"]);
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());

        assert_eq!(
            cache.branch.as_ref().unwrap().branch.as_deref(),
            Some("main")
        );
        assert_eq!(
            cache.branches,
            vec!["feature/x".to_string(), "main".to_string()]
        );
        // A clean repository is not "unknown": the read says so explicitly.
        let conflicts = cache.conflicts.as_ref().expect("a conflict read");
        assert!(!conflicts.merge_in_progress);
        assert!(conflicts.paths.is_empty());
    }

    #[test]
    fn merge_conflict_is_published_then_abort_merge_restores_the_tree() {
        let dir = repo_with_conflicting_branch();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        assert_eq!(cache.branches, vec!["main".to_string(), "side".to_string()]);
        assert!(!cache.conflicts.as_ref().unwrap().merge_in_progress);

        // No upstream anywhere, and the merge still runs: nothing about it
        // needs a remote.
        let merge = SyncOperation::Merge {
            branch: "side".into(),
        };
        cache
            .start_operation(dir.path().to_path_buf(), merge.clone())
            .unwrap();
        assert_eq!(cache.running.as_ref(), Some(&merge));
        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, merge);
        // A conflict is a reported failure, not a silent one, and the cache is
        // free to run the next operation.
        let error = outcome.result.unwrap_err();
        assert!(error.contains("CONFLICT"), "{error}");
        assert!(cache.running.is_none());

        settle_branch(&mut cache, dir.path());
        assert!(cache.conflicts.as_ref().unwrap().merge_in_progress);
        assert!(std::fs::read_to_string(dir.path().join("a"))
            .unwrap()
            .contains("<<<<<<<"));

        // A second merge while one is unresolved is refused before spawning:
        // Git would only say the same thing after starting.
        let refused = cache.start_operation(
            dir.path().to_path_buf(),
            SyncOperation::Merge {
                branch: "side".into(),
            },
        );
        assert_eq!(refused.unwrap_err(), "a merge is already in progress");
        assert!(cache.running.is_none());

        // Switching away mid-merge is refused too: git would refuse it itself,
        // but saying so before the worker starts reads better and leaves the
        // tree untouched. `Create` is allowed — a new branch starts from HEAD
        // and cannot disturb an unmerged index the way a checkout can.
        let refused = cache.start_operation(
            dir.path().to_path_buf(),
            SyncOperation::Switch {
                branch: "main".into(),
            },
        );
        assert_eq!(
            refused.unwrap_err(),
            "a merge is in progress — finish or abort it first"
        );
        assert!(cache.running.is_none());

        cache
            .start_operation(dir.path().to_path_buf(), SyncOperation::AbortMerge)
            .unwrap();
        let outcome = loop_with(&mut cache);
        assert_eq!(outcome.operation, SyncOperation::AbortMerge);
        outcome.result.unwrap();
        settle_branch(&mut cache, dir.path());
        assert!(!cache.conflicts.as_ref().unwrap().merge_in_progress);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).unwrap(),
            "main\n"
        );
    }

    #[test]
    fn branch_verbs_are_refused_when_the_branch_cannot_be_read() {
        // Not a repository at all: the picker is opened from a cached read, so
        // the refusal has to be re-checked here rather than trusted from
        // whatever the picker saw when it opened.
        let dir = TempDir::new().unwrap();
        let mut cache = GitSyncCache::default();
        // A real read of a real non-repository, so this covers the path that
        // actually sets the error rather than a hand-set field.
        settle_branch(&mut cache, dir.path());
        assert!(
            cache.error.is_some(),
            "a plain directory is not a repository"
        );
        assert!(cache
            .start_operation(
                dir.path().to_path_buf(),
                SyncOperation::Switch {
                    branch: "main".into()
                }
            )
            .is_err());
        assert!(cache
            .start_operation(
                dir.path().to_path_buf(),
                SyncOperation::Create {
                    branch: "new".into()
                }
            )
            .is_err());
        // The delete and rename verbs act on that same branch list, so an
        // unreadable repository refuses them too.
        assert!(cache
            .start_operation(
                dir.path().to_path_buf(),
                SyncOperation::DeleteBranch {
                    branch: "doomed".into(),
                    force: true
                }
            )
            .is_err());
        assert!(cache
            .start_operation(
                dir.path().to_path_buf(),
                SyncOperation::RenameBranch {
                    from: "one".into(),
                    to: "two".into()
                }
            )
            .is_err());
        assert!(cache.running.is_none(), "neither may mark itself running");
    }

    #[test]
    fn abort_merge_is_refused_when_no_merge_is_in_progress() {
        let (dir, _remote) = repo_with_remote();
        let mut cache = GitSyncCache::default();
        settle_branch(&mut cache, dir.path());
        let refused = cache.start_operation(dir.path().to_path_buf(), SyncOperation::AbortMerge);
        assert_eq!(refused.unwrap_err(), "no merge is in progress to abort");
        assert!(cache.running.is_none(), "and must not mark itself running");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).unwrap(),
            "one\n"
        );
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
