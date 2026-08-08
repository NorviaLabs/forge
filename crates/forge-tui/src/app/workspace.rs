//! Workspace view navigation for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Conversation, file and diff-review views
//! share one navigation stack; these methods push, replace and validate views.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn current_workspace_is_file(&self) -> bool {
        matches!(
            self.workspace_navigation.current,
            Some(WorkspaceView::File(_))
        )
    }

    pub(super) fn current_workspace_is_diff(&self) -> bool {
        matches!(
            self.workspace_navigation.current,
            Some(WorkspaceView::Diff(_))
        )
    }

    pub(super) fn toggle_files_panel(&mut self) {
        self.workspace_files.visible = !self.workspace_files.visible;
        self.save_ui_state();
        if self.workspace_files.visible {
            self.focus_block(FocusBlock::Files);
        } else {
            self.restore_focus_after_closing(FocusBlock::Files);
        }
        self.normalize_focus();
    }

    pub(super) fn workspace_view_is_valid(&self, view: &WorkspaceView) -> bool {
        match view {
            WorkspaceView::File(path) => path.is_file() || path.is_symlink(),
            WorkspaceView::Diff(DiffCommandContext::Current) => true,
        }
    }

    pub(super) fn apply_workspace_view(&mut self, view: &WorkspaceView) {
        match view {
            WorkspaceView::File(path) => {
                self.show_file_in_editor(path);
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => {
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
            self.explorer_dialog.current = Some(ExplorerDialog::DirtyExit);
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
            self.explorer_dialog.current = Some(ExplorerDialog::DirtyExit);
            return;
        }
        let mut next = None;
        while let Some(candidate) = self.workspace_navigation.history.pop() {
            if self.workspace_view_is_valid(&candidate) {
                next = Some(candidate);
                break;
            }
        }
        self.workspace_navigation.current = next.clone();
        match &next {
            Some(view) => self.apply_workspace_view(view),
            None => self.normalize_focus(),
        }
    }
}
