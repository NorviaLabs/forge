//! Workspace view navigation for [`TuiApp`].
//!
//! Session-local tab selection and retained resource navigation.

use super::*;

impl TuiApp {
    /// Reveal a retained workspace without reopening its buffers or closing
    /// its resources. First entry into Git initializes the existing review.
    pub(super) fn select_workspace_tab(&mut self, tab: WorkspaceTab) {
        self.workspace_navigation.select_tab(tab);
        match tab {
            WorkspaceTab::Agent => self.focus_block(FocusBlock::Composer),
            WorkspaceTab::Files => {
                self.navigator_tab = crate::widgets::NavigatorTab::Files;
                self.navigator_tab_explicit = true;
                self.workspace_files.visible = true;
                self.git_grouped_list = false;
                self.workspace_files.explorer.set_diff_filter(None);
                self.focus_block(FocusBlock::Search);
            }
            WorkspaceTab::Git => {
                self.navigator_tab = crate::widgets::NavigatorTab::Git;
                self.navigator_tab_explicit = true;
                self.workspace_files.visible = true;
                if self.workspace_navigation.current().is_none() {
                    self.open_git_view();
                } else if self.diff_view_is_open() {
                    self.git_grouped_list =
                        self.diff_view.source == crate::diff_view::DiffSource::WorkingTree;
                    if self.workspace_is_git_repository() {
                        self.workspace_files.explorer.refresh_git_status();
                        self.refresh_diff_entries();
                    }
                }
                self.focus_block(FocusBlock::Files);
            }
        }
        self.normalize_focus();
    }

    /// Reveal the other retained pane. This changes presentation and focus,
    /// never the navigation stack or the open editor buffer.
    pub(super) fn switch_workspace_pane(&mut self) {
        if self.workspace_navigation.current().is_none() {
            return;
        }
        let next = if self.workspace_navigation.resource_selected() {
            FocusBlock::Sidebar
        } else {
            FocusBlock::Workspace
        };
        self.focus_block(next);
    }

    pub(super) fn current_workspace_is_file(&self) -> bool {
        matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::File(_))
        )
    }

    /// `Ctrl+E` means "take me to Files", and only closes the pane when you
    /// are already there.
    ///
    /// It used to toggle blindly on visibility. That made the common case
    /// dangerous: with the pane already open but focus elsewhere, pressing
    /// `Ctrl+E` to reach the file list *closed* it and handed focus back to
    /// the editor — which is modal, so the filter you started typing was
    /// executed as vim commands and silently edited the open file. `i` opens
    /// INSERT; the rest of the word lands in the buffer. Nothing on screen
    /// says focus moved, and the Unsaved Changes dialog defaults to Save.
    ///
    /// Matching the editor convention (VS Code's `Ctrl+Shift+E`) removes the
    /// hazard rather than papering over it: the direction that loses focus to
    /// a text-mutating surface is now only reachable deliberately, from the
    /// explorer itself.
    pub(super) fn toggle_files_panel(&mut self) {
        let already_in_files = matches!(self.focus.block(), FocusBlock::Files | FocusBlock::Search);

        if self.workspace_files.visible && !already_in_files {
            self.focus_block(FocusBlock::Search);
            self.normalize_focus();
            return;
        }

        self.workspace_files.visible = !self.workspace_files.visible;
        self.save_ui_state();
        if self.workspace_files.visible {
            self.focus_block(FocusBlock::Search);
        } else {
            self.restore_focus_after_closing(FocusBlock::Files);
        }
        self.normalize_focus();
    }

    pub(super) fn workspace_view_is_valid(view: &WorkspaceView) -> bool {
        match view {
            WorkspaceView::File(path) => path.is_file() || path.is_symlink(),
            // Always valid: the diff view holds no path that can go stale.
            WorkspaceView::Diff | WorkspaceView::GithubIssues => true,
        }
    }

    pub(super) fn apply_workspace_view(&mut self, view: &WorkspaceView) {
        match view {
            WorkspaceView::File(path) => {
                self.show_file_in_editor(path);
            }
            WorkspaceView::Diff | WorkspaceView::GithubIssues => {
                self.focus_block(FocusBlock::Workspace);
            }
        }
        self.normalize_focus();
    }

    pub(super) fn push_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.push_view(view.clone());
        self.apply_workspace_view(&view);
    }

    pub(super) fn replace_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.replace_view(view.clone());
        self.apply_workspace_view(&view);
    }

    pub(super) fn navigate_to_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.navigate_to(view.clone());
        self.apply_workspace_view(&view);
    }

    pub(super) fn go_home_workspace(&mut self) {
        if self
            .editor_session
            .as_ref()
            .is_some_and(|editor| editor.is_dirty())
        {
            self.pending_editor_home = true;
            self.explorer_dialog.show(ExplorerDialog::DirtyExit);
            return;
        }
        self.pending_editor_home = false;
        self.workspace_navigation.home();
        self.normalize_focus();
    }

    pub(super) fn complete_dirty_editor_exit(&mut self) {
        if self.pending_editor_quit {
            self.pending_editor_quit = false;
            // The buffer is resolved; the quit itself still goes through the
            // gate, which may have other sessions' work to account for.
            self.begin_quit();
        } else if self.pending_editor_home {
            self.pending_editor_home = false;
            self.go_home_workspace();
        } else {
            self.go_back_workspace();
        }
    }

    /// Global quit (`Ctrl+D`, and `/quit` on the primary session). A dirty
    /// editor buffer raises the existing Save/Discard/Cancel dialog first;
    /// work in flight elsewhere raises the quit-all confirm (#647).
    pub(super) fn request_quit(&mut self) {
        if self
            .editor_session
            .as_ref()
            .is_some_and(|editor| editor.is_dirty())
        {
            self.pending_editor_quit = true;
            self.explorer_dialog.show(ExplorerDialog::DirtyExit);
            return;
        }
        self.begin_quit();
    }

    /// Finish a `/quit` whose session close could not be completed.
    ///
    /// Closing the selected session before quitting is cleanup, not a
    /// precondition for quitting: retirement is best-effort, so it can fail
    /// (a turn that ignores cancellation past the retirement deadline, a full
    /// command queue) or be rejected outright. Leaving the app running in that
    /// case strands the operator in the session they explicitly asked to
    /// leave, with no visible way out. Exit anyway — the leftover actor and
    /// worktree are reconciled by the supervisor on the next launch.
    pub(super) fn request_quit_after_deferred_cleanup(&mut self) {
        self.exit.request();
        self.status_state.message = "quitting · cleanup deferred".into();
    }

    pub(super) fn go_back_workspace(&mut self) {
        if matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::GithubIssues)
        ) {
            self.close_github_issues();
            return;
        }
        if self
            .editor_session
            .as_ref()
            .is_some_and(|editor| editor.is_dirty())
        {
            self.explorer_dialog.show(ExplorerDialog::DirtyExit);
            return;
        }
        let next = self
            .workspace_navigation
            .pop_previous_valid(Self::workspace_view_is_valid);
        match &next {
            Some(view) => self.apply_workspace_view(view),
            None => self.normalize_focus(),
        }
    }
}
