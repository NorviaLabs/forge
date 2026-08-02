//! Filesystem change watching for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Watches the workspace for external edits,
//! refreshes the file tree and open source viewer, and keeps diff review snapshots
//! current. Methods are moved verbatim.

use std::path::Path;

use super::*;

pub(super) fn path_is_under_dot_forge(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".forge")
}

impl TuiApp {
    pub(super) fn init_file_watcher(&mut self) {
        let tx = self.file_watch.change_tx.clone();
        let mut watcher = match RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        for path in event.paths {
                            // Journal/progress/ui-state under .forge churn constantly
                            // during exploration and must not thrash the Files tree.
                            if path_is_under_dot_forge(&path) {
                                continue;
                            }
                            let _ = tx.send(FileChangeEvent { path });
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };
        let _ = watcher.watch(self.session.workspace_root(), RecursiveMode::Recursive);
        self.file_watch.watcher = Some(watcher);
    }

    pub(super) fn poll_file_changes(&mut self) {
        let mut active_file_changed = false;
        let mut workspace_changed = false;
        while let Ok(change) = self.file_watch.change_rx.try_recv() {
            workspace_changed = true;
            if let Some(path) = &self.source_viewer.path {
                if change.path == *path {
                    active_file_changed = true;
                }
            }
        }
        if workspace_changed {
            self.refresh_after_filesystem_change(active_file_changed);
        }
    }

    pub(super) fn note_workspace_changed(&mut self) {
        self.clear_pending_double_click();
        self.mark_diff_stale_if_reviewing();
        self.file_explorer.refresh_workspace();
    }

    fn tool_may_mutate_workspace(name: &str) -> bool {
        matches!(name, "write_file" | "apply_patch" | "bash" | "git" | "run")
    }

    pub(super) fn maybe_note_workspace_changed_from_recent_tools(&mut self) {
        // Only the latest assistant step matters — older writes already refreshed the tree.
        let mutated = self
            .session
            .messages
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

    pub(super) fn current_changed_paths(&self) -> Vec<PathBuf> {
        self.file_explorer
            .git_status
            .changed_files()
            .into_iter()
            .map(|file| file.path)
            .collect()
    }

    pub(super) fn capture_diff_snapshot(&mut self) {
        self.diff_snapshot.paths = self.current_changed_paths();
        self.diff_snapshot.stale = false;
    }

    /// Whether the set of changed paths currently known to git status differs
    /// from the set captured when the review was last (re)opened. Order
    /// doesn't matter — only membership does.
    fn diff_review_paths_changed(&self) -> bool {
        let mut current = self.current_changed_paths();
        let mut captured = self.diff_snapshot.paths.clone();
        current.sort();
        captured.sort();
        current != captured
    }

    /// Called on every raw filesystem-watch event. A single external write
    /// can fire several watch events (and, on some platforms, replay recent
    /// history once the watcher attaches) well before the async git-status
    /// refresh they trigger has actually landed — so this only flags the
    /// review as stale when the *currently known* changed-path set already
    /// disagrees with what's under review, not on every raw notification.
    /// [`reconcile_diff_staleness`] catches the remaining case where the
    /// disagreement only becomes visible once that refresh completes.
    pub(super) fn mark_diff_stale_if_reviewing(&mut self) {
        if self.current_workspace_is_diff() && self.diff_review_paths_changed() {
            self.diff_snapshot.stale = true;
        }
    }

    /// Re-checks diff staleness once a git-status refresh has actually
    /// resolved. Call after `git_status.poll()` returns `true`.
    pub(super) fn reconcile_diff_staleness(&mut self) {
        if self.current_workspace_is_diff()
            && !self.diff_snapshot.stale
            && self.diff_review_paths_changed()
        {
            self.diff_snapshot.stale = true;
        }
    }

    pub(super) fn refresh_diff_review(&mut self) {
        self.file_explorer.refresh_git_status();
        self.capture_diff_snapshot();
    }

    pub(super) fn refresh_after_filesystem_change(&mut self, active_file_changed: bool) {
        let renamed_open_file = self.reconcile_open_file_external_rename();
        let renamed_notice = renamed_open_file.then(|| "File renamed externally".to_string());
        if active_file_changed {
            self.refresh_active_source_viewer();
            self.notices.clear();
        } else if renamed_open_file {
            self.notices.clear();
        }
        if self.focus.block == FocusBlock::Files && self.focus.mode == FocusMode::Navigation {
            self.file_explorer.refresh_git_status();
        } else {
            self.note_workspace_changed();
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
