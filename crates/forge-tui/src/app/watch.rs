//! Filesystem change watching for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Watches the workspace for external edits
//! and refreshes the file tree and open source viewer. Methods are moved verbatim.

use std::path::Path;

use super::*;

pub(super) fn path_is_ignored_by_file_watcher(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component.as_os_str().to_str(), Some(".forge" | ".git")))
}

impl TuiApp {
    pub(super) fn init_file_watcher(&mut self) {
        let tx = self.file_watch.sender();
        let mut watcher = match RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        // Ordinary data/metadata writes can affect source and Git
                        // status, but cannot alter the explorer's path structure.
                        let tree_changed = !matches!(
                            event.kind,
                            EventKind::Modify(
                                notify::event::ModifyKind::Data(_)
                                    | notify::event::ModifyKind::Metadata(_)
                            )
                        );
                        for path in event.paths {
                            // Runtime state and Git internals churn during refreshes
                            // and must not retrigger the Files tree refresh.
                            if path_is_ignored_by_file_watcher(&path) {
                                continue;
                            }
                            let _ = tx.send(FileChangeEvent {
                                path,
                                tree_changed,
                                immediate: false,
                            });
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };
        let _ = watcher.watch(self.session_view.workspace_root(), RecursiveMode::Recursive);
        self.file_watch.install(watcher);
    }

    pub(super) fn poll_file_changes(&mut self) {
        let files_are_active = matches!(self.focus.block(), FocusBlock::Files | FocusBlock::Search)
            && self.focus.mode() == FocusMode::Navigation;
        if !files_are_active && self.file_watch.take_deferred_tree_refresh() {
            self.note_workspace_changed();
        }
        let Some(batch) = self.file_watch.take_ready_batch() else {
            return;
        };
        let active_file_changed = self.source_viewer.path.as_ref().is_some_and(|open_path| {
            batch
                .paths
                .iter()
                .any(|change_path| same_file_identity(change_path, open_path))
        });
        self.refresh_after_filesystem_change(active_file_changed, batch.tree_changed);
    }

    pub(super) fn note_workspace_changed(&mut self) {
        self.workspace_files.explorer.refresh_workspace();
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
            self.notice_state.items.clear();
        } else if renamed_open_file {
            self.notice_state.items.clear();
        }
        let files_are_active = matches!(self.focus.block(), FocusBlock::Files | FocusBlock::Search)
            && self.focus.mode() == FocusMode::Navigation;
        if tree_changed && !files_are_active {
            self.note_workspace_changed();
        } else if tree_changed {
            // Rebuilding while the operator moves through Files can shift the
            // selected row underneath them. Preserve the current tree now,
            // but do not consume the change forever: the next application
            // tick after focus leaves Files performs one coalesced refresh.
            self.file_watch.defer_tree_refresh();
            self.workspace_files.explorer.refresh_git_status();
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
