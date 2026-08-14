//! Keep / Discard decisions on the Review changes pane.

use super::*;

impl TuiApp {
    pub(super) fn review_file_paths(&self) -> Vec<PathBuf> {
        if self.diff_view.snapshot.stale && !self.diff_view.snapshot.paths.is_empty() {
            self.diff_view.snapshot.paths.clone()
        } else {
            self.current_changed_paths()
        }
    }

    fn review_selected_path(&self) -> Option<PathBuf> {
        let paths = self.review_file_paths();
        let idx = self.diff_view.selected.min(paths.len().saturating_sub(1));
        paths.get(idx).cloned()
    }

    fn review_file_diff(&self, path: &Path) -> Result<FileDiff, String> {
        combined_diff(self.session_view.workspace_root(), path)
    }

    pub(super) fn review_status(&self, path: &Path) -> forge_workspace::git_status::PathStatus {
        self.workspace_files
            .explorer
            .git_status
            .path_status(path)
            .unwrap_or_default()
    }

    fn reviewability_for(&self, path: &Path, diff: &FileDiff) -> Reviewability {
        reviewability(self.review_status(path), diff)
    }

    fn review_actions_frozen(&self) -> bool {
        self.diff_view.snapshot.stale || self.diff_view.pending_untracked_delete.is_some()
    }

    pub(super) fn select_previous_hunk(&mut self) {
        self.diff_view.hunk = self.diff_view.hunk.saturating_sub(1);
    }

    pub(super) fn select_next_hunk(&mut self) {
        let Some(path) = self.review_selected_path() else {
            return;
        };
        let Ok(diff) = self.review_file_diff(&path) else {
            return;
        };
        if diff.hunks.is_empty() {
            return;
        }
        self.diff_view.hunk = self
            .diff_view
            .hunk
            .saturating_add(1)
            .min(diff.hunks.len().saturating_sub(1));
    }

    pub(super) fn keep_selected_hunk(&mut self) {
        if self.review_actions_frozen() {
            self.review_frozen_feedback();
            return;
        }
        let Some(path) = self.review_selected_path() else {
            return;
        };
        let Ok(diff) = self.review_file_diff(&path) else {
            return;
        };
        if self.reviewability_for(&path, &diff) != Reviewability::Reviewable {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "This file is view-only in review.".to_string(),
            );
            return;
        }
        let Some(hunk) = diff
            .hunks
            .get(self.diff_view.hunk.min(diff.hunks.len().saturating_sub(1)))
        else {
            return;
        };
        self.diff_view.kept.insert((path, hunk.header.clone()));
        self.select_next_hunk();
    }

    pub(super) fn keep_rest_of_file(&mut self) {
        if self.review_actions_frozen() {
            self.review_frozen_feedback();
            return;
        }
        let Some(path) = self.review_selected_path() else {
            return;
        };
        let Ok(diff) = self.review_file_diff(&path) else {
            return;
        };
        if self.reviewability_for(&path, &diff) != Reviewability::Reviewable {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "This file is view-only in review.".to_string(),
            );
            return;
        }
        for hunk in &diff.hunks {
            self.diff_view
                .kept
                .insert((path.clone(), hunk.header.clone()));
        }
    }

    pub(super) fn discard_selected_hunk(&mut self) {
        if self.review_actions_frozen() {
            self.review_frozen_feedback();
            return;
        }
        let Some(path) = self.review_selected_path() else {
            return;
        };
        let Ok(diff) = self.review_file_diff(&path) else {
            return;
        };
        if self.reviewability_for(&path, &diff) != Reviewability::Reviewable {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "This file is view-only in review.".to_string(),
            );
            return;
        }
        if diff.untracked {
            self.diff_view.pending_untracked_delete = Some(path);
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Delete this untracked file? y to delete, n to cancel.".to_string(),
            );
            return;
        }
        let idx = self.diff_view.hunk.min(diff.hunks.len().saturating_sub(1));
        self.expect_own_review_change();
        match discard_hunk(self.session_view.workspace_root(), &path, idx) {
            Ok(()) => {
                self.finish_own_review_change();
                self.set_feedback(FeedbackSeverity::Info, "Discarded hunk.".to_string());
            }
            Err(err) => {
                self.diff_view.expect_own_change = false;
                self.set_feedback(FeedbackSeverity::Error, err);
            }
        }
    }

    pub(super) fn discard_rest_of_file(&mut self) {
        if self.review_actions_frozen() {
            self.review_frozen_feedback();
            return;
        }
        let Some(path) = self.review_selected_path() else {
            return;
        };
        let Ok(diff) = self.review_file_diff(&path) else {
            return;
        };
        if self.reviewability_for(&path, &diff) != Reviewability::Reviewable {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "This file is view-only in review.".to_string(),
            );
            return;
        }
        if diff.untracked {
            self.diff_view.pending_untracked_delete = Some(path);
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Delete this untracked file? y to delete, n to cancel.".to_string(),
            );
            return;
        }
        self.expect_own_review_change();
        match restore_path(self.session_view.workspace_root(), &path) {
            Ok(()) => {
                self.diff_view
                    .kept
                    .retain(|(kept_path, _)| kept_path != &path);
                self.finish_own_review_change();
                self.set_feedback(FeedbackSeverity::Info, "Restored file to HEAD.".to_string());
            }
            Err(err) => {
                self.diff_view.expect_own_change = false;
                self.set_feedback(FeedbackSeverity::Error, err);
            }
        }
    }

    pub(super) fn confirm_review_delete(&mut self) {
        let Some(path) = self.diff_view.pending_untracked_delete.take() else {
            return;
        };
        self.expect_own_review_change();
        match delete_untracked(self.session_view.workspace_root(), &path) {
            Ok(()) => {
                self.diff_view
                    .kept
                    .retain(|(kept_path, _)| kept_path != &path);
                self.finish_own_review_change();
                self.set_feedback(
                    FeedbackSeverity::Info,
                    "Deleted untracked file.".to_string(),
                );
            }
            Err(err) => {
                self.diff_view.expect_own_change = false;
                self.set_feedback(FeedbackSeverity::Error, err);
            }
        }
    }

    pub(super) fn cancel_review_delete(&mut self) {
        self.diff_view.pending_untracked_delete = None;
        self.set_feedback(FeedbackSeverity::Info, "Delete cancelled.".to_string());
    }

    fn review_frozen_feedback(&mut self) {
        if self.diff_view.pending_untracked_delete.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Confirm or cancel the pending delete first.".to_string(),
            );
        } else {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Review is stale · press r to refresh before Keep/Discard.".to_string(),
            );
        }
    }

    fn expect_own_review_change(&mut self) {
        self.diff_view.expect_own_change = true;
    }

    fn finish_own_review_change(&mut self) {
        self.workspace_files.explorer.refresh_git_status();
        // Poll once if the refresh is already done; otherwise snapshot updates
        // when reconcile sees expect_own_change.
        let _ = self.workspace_files.explorer.git_status.poll();
        self.capture_diff_snapshot();
        self.diff_view.expect_own_change = true;
    }

    pub(super) fn hunk_is_kept(&self, path: &Path, header: &str) -> bool {
        self.diff_view
            .kept
            .contains(&(path.to_path_buf(), header.to_string()))
    }
}
