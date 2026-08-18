//! Workspace view navigation for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Conversation and file views share one
//! navigation stack; these methods push, replace and validate views.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
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
        // Below the layout's width threshold the explorer is never rendered, so
        // toggling `visible` changes nothing on screen and focusing it parks the
        // cursor in an invisible pane. Say why instead of doing nothing: the
        // width requirement is otherwise undiscoverable.
        if self.last_frame_width > 0 && !crate::layout::files_fit(self.last_frame_width) {
            self.set_feedback(
                FeedbackSeverity::Info,
                format!(
                    "Files needs a wider terminal ({} columns; this one is {}).",
                    crate::layout::files_min_frame_width(),
                    self.last_frame_width
                ),
            );
            return;
        }
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
        }
    }

    pub(super) fn apply_workspace_view(&mut self, view: &WorkspaceView) {
        match view {
            WorkspaceView::File(path) => {
                self.show_file_in_editor(path);
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
        if self.pending_editor_home {
            self.pending_editor_home = false;
            self.go_home_workspace();
        } else {
            self.go_back_workspace();
        }
    }

    pub(super) fn go_back_workspace(&mut self) {
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
