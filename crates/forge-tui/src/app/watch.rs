//! Filesystem change watching for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Watches the workspace for external edits
//! and refreshes the file tree and open source viewer. Methods are moved verbatim.

use std::path::{Path, PathBuf};

use super::*;

pub(super) fn path_is_ignored_by_file_watcher(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component.as_os_str().to_str(), Some(".forge" | ".git")))
}

/// The per-worktree Git index for `root`, when the workspace is a repository.
///
/// A plain checkout keeps it at `.git/index`. A linked worktree's `.git` is a
/// `gitdir:` pointer *file*, and its index lives in that gitdir (each worktree
/// has its own), which sits outside the workspace root — so a recursive watch
/// of the root never sees it and it has to be watched by path.
pub(super) fn git_index_path(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.join("index"));
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let target = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let gitdir = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        root.join(target)
    };
    Some(gitdir.join("index"))
}

/// Whether a watcher event `path` names the Git `index` we watch. Compares the
/// file name first so the canonicalizing fallback — for a workspace reached
/// through a symlinked root — stays off the hot path for ordinary file events.
pub(super) fn is_git_index(path: &Path, index: &Path) -> bool {
    path.file_name() == index.file_name() && (path == index || same_file_identity(path, index))
}

/// Classify one watcher event path: `None` when it is ignored outright,
/// otherwise the `tree_changed` flag to enqueue it with.
///
/// Internal churn (`.forge` runtime state, `.git` refs and locks) is dropped.
/// The Git index is kept but downgraded to a status-only signal, so an external
/// `git add` refreshes the changed-file list without rebuilding the file tree.
pub(super) fn classify_watch_path(
    path: &Path,
    index_path: Option<&Path>,
    tree_changed: bool,
) -> Option<bool> {
    let is_index = index_path.is_some_and(|index| is_git_index(path, index));
    if !is_index && path_is_ignored_by_file_watcher(path) {
        return None;
    }
    Some(tree_changed && !is_index)
}

impl TuiApp {
    pub(super) fn init_file_watcher(&mut self) {
        let tx = self.file_watch.sender();
        let overflow = self.file_watch.overflow_handle();
        let root = self.session_view.workspace_root().to_path_buf();
        // Git rewrites the index on every stage, commit, checkout and reset.
        // Watching it (rather than the whole `.git`, which churns far more) is
        // what surfaces an external `git add` in the changed-file list without
        // a periodic poll. `git status` runs with `--no-optional-locks`, so our
        // own status refreshes never rewrite the index and cannot feed back
        // into this watch.
        let index_path = git_index_path(&root);
        let mut watcher = match RecommendedWatcher::new(
            {
                let index_path = index_path.clone();
                move |result: notify::Result<notify::Event>| {
                    if let Ok(event) = result {
                        if matches!(
                            event.kind,
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                        ) {
                            // Ordinary data/metadata writes can affect source and
                            // Git status, but cannot alter the explorer's path
                            // structure.
                            let tree_changed = !matches!(
                                event.kind,
                                EventKind::Modify(
                                    notify::event::ModifyKind::Data(_)
                                        | notify::event::ModifyKind::Metadata(_)
                                )
                            );
                            for path in event.paths {
                                let Some(path_tree_changed) =
                                    classify_watch_path(&path, index_path.as_deref(), tree_changed)
                                else {
                                    // Internal churn must not retrigger the Files tree.
                                    continue;
                                };
                                if tx
                                    .try_send(FileChangeEvent {
                                        path,
                                        tree_changed: path_tree_changed,
                                        immediate: false,
                                    })
                                    .is_err()
                                {
                                    overflow.store(true, std::sync::atomic::Ordering::Release);
                                }
                            }
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };
        let _ = watcher.watch(&root, RecursiveMode::Recursive);
        if let Some(parent) = index_path.as_deref().and_then(Path::parent) {
            // Watch the directory: Git atomically replaces index via index.lock,
            // so watching the file inode would stop working after the first write.
            let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
        }
        self.file_watch.install(watcher);
    }

    pub(super) fn poll_file_changes(&mut self) {
        // Install any refresh the blocking worker finished since the last tick.
        self.workspace_files.explorer.poll_workspace_refresh();
        self.drain_inactive_file_watchers();
        let Some(batch) = self.file_watch.take_ready_batch() else {
            return;
        };
        let active_file_changed = batch.overflowed
            || self.source_viewer.path.as_ref().is_some_and(|open_path| {
                batch
                    .paths
                    .iter()
                    .any(|change_path| same_file_identity(change_path, open_path))
            });
        self.refresh_after_filesystem_change(active_file_changed, batch.tree_changed);
    }

    pub(super) fn drain_inactive_file_watchers(&mut self) {
        for state in self.session_view_states.values_mut() {
            state.file_watch.drain_events();
        }
        for state in self.retiring_session_view_states.values_mut() {
            state.file_watch.drain_events();
        }
    }

    pub(super) fn note_workspace_changed(&mut self) {
        // Walking every loaded directory is filesystem-bound, so it runs on a
        // blocking worker and lands on a later tick via
        // `poll_workspace_refresh` — never on the terminal thread.
        self.workspace_files.explorer.request_workspace_refresh();
        // A commit, checkout or merge moves the branch too.
        self.refresh_git_branch();
    }

    fn tool_may_mutate_workspace(name: &str) -> bool {
        matches!(
            name,
            "write_file"
                | "apply_patch"
                | "edit"
                | "search_replace"
                | "edit_file"
                | "bash"
                | "git"
                | "run"
        )
    }

    pub(super) fn maybe_note_workspace_changed_from_recent_tools(&mut self) {
        // Only the latest assistant step matters — older writes already refreshed the tree.
        let mutated = self
            .transcript_view
            .messages()
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| {
                message
                    .tool_calls
                    .iter()
                    .any(|call| Self::tool_may_mutate_workspace(&call.name))
            })
            .unwrap_or(false);
        if mutated {
            self.note_workspace_changed();
        }
    }

    pub(super) fn refresh_after_filesystem_change(
        &mut self,
        active_file_changed: bool,
        tree_changed: bool,
    ) {
        let renamed_open_file = self.reconcile_open_file_external_rename();
        let renamed_notice = renamed_open_file.then(|| "File renamed externally".to_string());
        if active_file_changed {
            let deleted = self
                .source_viewer
                .path
                .as_ref()
                .is_some_and(|path| !path.exists());
            if self.editor_session.is_some() && !deleted {
                // The editor owns its in-memory buffer. A watcher event only
                // records the external change; save/reload resolves it later.
                self.source_viewer.notice =
                    Some("File changed on disk · save, reload, or force-save".into());
            } else {
                self.refresh_active_source_viewer();
            }
        }
        if tree_changed {
            self.note_workspace_changed();
        } else {
            // Content writes do not justify recursively refreshing every
            // loaded directory.
            self.workspace_files.explorer.refresh_git_status();
        }
        if let Some(notice) = renamed_notice {
            self.source_viewer.notice = Some(notice);
        }
    }

    pub(super) fn apply_history_text(&mut self, text: String) {
        self.input.set_text(text);
        self.input.history_browse = self.history.browsing();
        self.clamp_slash_suggest();
    }
}

fn same_file_identity(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}
