//! Workspace view navigation for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Conversation, file, diff-review and run views
//! share one navigation stack; these methods push, replace and validate views.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn current_workspace_is_conversation(&self) -> bool {
        matches!(
            self.workspace_navigation.current,
            WorkspaceView::Conversation
        )
    }

    pub(super) fn current_workspace_is_file(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::File(_))
    }

    pub(super) fn current_workspace_is_diff(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::Diff(_))
    }

    pub(super) fn current_workspace_is_run(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::Run(_))
    }
    pub(super) fn toggle_files_panel(&mut self) {
        self.files_visible = !self.files_visible;
        self.save_ui_state();
        if self.files_visible {
            self.focus_block(FocusBlock::Files);
        } else {
            self.restore_focus_after_closing(FocusBlock::Files);
        }
        self.normalize_focus();
    }

    pub(super) fn workspace_view_is_valid(&self, view: &WorkspaceView) -> bool {
        match view {
            WorkspaceView::Conversation => true,
            WorkspaceView::File(path) => path.is_file() || path.is_symlink(),
            WorkspaceView::Diff(DiffCommandContext::Current) => true,
            WorkspaceView::Run(id) => self.run_exists(id),
        }
    }

    pub(super) fn apply_workspace_view(&mut self, view: &WorkspaceView) {
        match view {
            WorkspaceView::Conversation => {}
            WorkspaceView::File(path) => {
                self.show_file_in_editor(path);
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => {
                self.focus_block(FocusBlock::Workspace);
            }
            WorkspaceView::Run(id) => {
                if self.run_exists(id) {
                    self.focus_block(FocusBlock::Workspace);
                } else {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!("Run is no longer available: {id}"),
                    );
                    self.workspace_navigation
                        .replace_view(WorkspaceView::Conversation);
                }
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
        self.workspace_navigation.home();
        self.apply_workspace_view(&WorkspaceView::Conversation);
    }

    pub(super) fn go_back_workspace(&mut self) {
        let mut next = WorkspaceView::Conversation;
        while let Some(candidate) = self.workspace_navigation.history.pop() {
            if self.workspace_view_is_valid(&candidate) {
                next = candidate;
                break;
            }
        }
        self.workspace_navigation.replace_view(next.clone());
        self.apply_workspace_view(&next);
    }
}
