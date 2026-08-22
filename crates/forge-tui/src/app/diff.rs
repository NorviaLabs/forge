//! `/diff` — opening, refreshing and closing the workspace pane's review mode.
//!
//! The view itself is state-only (`crate::diff_view`); this is the wiring that
//! feeds it from git status, the transcript, and the async patch cache.

use std::path::{Path, PathBuf};

use forge_transcript::{ChatItem, ConversationModel, ConversationViewOpts};
use forge_workspace::git_review::parse_file_diff;

use super::*;
use crate::diff_view::{entries_from_changed_files, DiffEntry, DiffSource, DiffStatus, Patch};
use crate::overlays::StatusRow;

impl TuiApp {
    /// Enter `/diff`. Focuses the workspace pane and filters the explorer to
    /// the changed files, remembering nothing else so `Esc` can put both back.
    pub(super) fn open_diff_view(&mut self, source: DiffSource) {
        self.diff_view = crate::diff_view::DiffView::new(source);
        if !self.workspace_is_git_repository() {
            self.diff_view.status = DiffStatus::NotARepo;
            self.workspace_navigation.navigate_to(WorkspaceView::Diff);
            self.focus_block(FocusBlock::Workspace);
            self.status_state.message = "Not a git repository".into();
            return;
        }
        self.workspace_navigation.navigate_to(WorkspaceView::Diff);
        self.focus_block(FocusBlock::Workspace);
        self.refresh_diff_entries();
        self.status_state.message = format!("Reviewing changes · {}", source.label());
    }

    /// Leave `/diff`, restoring the full explorer listing and the pane's
    /// previous contents. One `Esc` closes the mode outright — it does not
    /// unwind level by level.
    pub(super) fn close_diff_view(&mut self) {
        self.workspace_files.explorer.set_diff_filter(None);
        let restored = self
            .workspace_navigation
            .pop_previous_valid(|view| !matches!(view, WorkspaceView::Diff));
        if let Some(WorkspaceView::File(path)) = restored {
            self.show_file_in_editor(&path.clone());
        } else {
            self.status_state.message = "Ready".into();
        }
    }

    pub(super) fn diff_view_is_open(&self) -> bool {
        matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::Diff)
        )
    }

    fn workspace_is_git_repository(&self) -> bool {
        self.session_view.workspace_root().join(".git").exists()
    }

    /// Recompute the changed-file list for the active source and push it to
    /// both the view and the explorer.
    pub(super) fn refresh_diff_entries(&mut self) {
        // A git-status failure has to surface here, not as an empty list that
        // reads as "no changes".
        if self.diff_view.source == DiffSource::WorkingTree {
            match self.workspace_files.explorer.git_status.error.clone() {
                Some(error) => {
                    self.diff_view.status = DiffStatus::Failed(error);
                    return;
                }
                None if matches!(self.diff_view.status, DiffStatus::Failed(_)) => {
                    self.diff_view.status = DiffStatus::Ready;
                }
                None => {}
            }
        }
        let entries = match self.diff_view.source {
            DiffSource::WorkingTree => entries_from_changed_files(
                &self.workspace_files.explorer.git_status.changed_files(),
            ),
            DiffSource::LastTurn => self.last_turn_diff_entries(),
        };
        let paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
        self.diff_view.set_entries(entries);
        self.workspace_files.explorer.set_diff_filter(Some(paths));
    }

    /// The files the most recent assistant turn wrote, taken from the
    /// transcript's own diff cards. Using the cards rather than `git` keeps
    /// "last turn" honest when the tree has moved on since.
    fn last_turn_diff_entries(&self) -> Vec<DiffEntry> {
        self.last_turn_diff_cards()
            .into_iter()
            .map(|(path, _)| DiffEntry {
                path: PathBuf::from(path),
                marker: "M",
                untracked: false,
            })
            .collect()
    }

    fn last_turn_diff_cards(&self) -> Vec<(String, Vec<String>)> {
        let model = ConversationModel::from_messages(
            self.transcript_view.messages(),
            self.transcript_view.events(),
            self.session_view.lifecycle,
            ConversationViewOpts::default(),
        );
        // Walk back to the last thing the user said; everything after it is
        // the turn in question.
        let start = model
            .items
            .iter()
            .rposition(|item| matches!(item, ChatItem::User { .. }))
            .map(|index| index + 1)
            .unwrap_or(0);
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();
        for item in &model.items[start..] {
            if let ChatItem::DiffCard { path, lines, .. } = item {
                match seen.iter_mut().find(|(seen_path, _)| seen_path == path) {
                    // A file written twice in one turn shows its latest patch.
                    Some(entry) => entry.1 = lines.clone(),
                    None => seen.push((path.clone(), lines.clone())),
                }
            }
        }
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        seen
    }

    /// Drive the patch for the selected file: request it when missing, collect
    /// it when it lands. Called from the event-loop tick, never from `draw`.
    pub(super) fn pump_diff_view(&mut self) {
        if !self.diff_view_is_open() {
            return;
        }
        if matches!(
            self.diff_view.status,
            DiffStatus::NotARepo | DiffStatus::Failed(_)
        ) {
            return;
        }
        self.refresh_diff_entries();
        if self.diff_view.is_empty() {
            return;
        }
        let Some(path) = self.diff_view.selected_path().map(Path::to_path_buf) else {
            return;
        };
        if self.diff_view.source == DiffSource::LastTurn {
            let card = self
                .last_turn_diff_cards()
                .into_iter()
                .find(|(card_path, _)| Path::new(card_path) == path);
            if let Some((card_path, lines)) = card {
                if !self.diff_view.patch_is_current(0) {
                    let patch = Patch::from_lines(PathBuf::from(card_path), &lines);
                    self.diff_view.set_patch(0, path, patch);
                }
            }
            return;
        }

        let revision = self.workspace_files.explorer.git_status.revision();
        if self.diff_view.patch_is_current(revision) {
            return;
        }
        let root = self.session_view.workspace_root().to_path_buf();
        match self
            .workspace_files
            .explorer
            .git_status
            .get_combined_diff(&path)
        {
            Some(Ok(text)) => {
                let untracked = self
                    .diff_view
                    .selected_entry()
                    .is_some_and(|entry| entry.untracked);
                let parsed = parse_file_diff(path.clone(), &text, untracked);
                self.diff_view
                    .set_patch(revision, path, Patch::from_file_diff(&parsed));
            }
            Some(Err(error)) => {
                self.diff_view.set_patch_error(revision, path, error);
            }
            None => {
                self.workspace_files
                    .explorer
                    .git_status
                    .request_combined_diff(root, path);
            }
        }
    }

    /// `d` — swap between the working tree and the last turn.
    pub(super) fn toggle_diff_source(&mut self) {
        self.diff_view.source = self.diff_view.source.toggled();
        self.diff_view.status = DiffStatus::Ready;
        self.diff_view.patch = crate::diff_view::PatchState::Loading;
        self.diff_view.loaded_for = None;
        self.diff_view.scroll = 0;
        self.refresh_diff_entries();
        self.status_state.message =
            format!("Reviewing changes · {}", self.diff_view.source.label());
    }

    /// `o` — open the file under the cursor in the source viewer, at the line
    /// the cursor is sitting on. The payoff of not being a modal overlay.
    pub(super) fn open_selected_diff_file(&mut self) {
        let Some(relative) = self.diff_view.selected_path().map(Path::to_path_buf) else {
            return;
        };
        let line = self.diff_view.new_file_line_at_scroll();
        let absolute = self.session_view.workspace_root().join(&relative);
        if !absolute.is_file() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("{} no longer exists", relative.display()),
            );
            return;
        }
        self.workspace_files.explorer.set_diff_filter(None);
        // `navigate_to_workspace_view` (via `open_file_in_editor`) is what
        // actually swaps the pane out of Diff; `show_file_in_editor` alone
        // only loads the buffer.
        self.open_file_in_editor(&absolute);
        if !self.current_workspace_is_file() {
            return;
        }
        let Some(line) = line else {
            return;
        };
        // Both surfaces have to move: the editor owns the cursor for editable
        // files, the viewer owns it for read-only ones, and the header reads
        // whichever is active.
        let max_line = self.source_viewer.lines.len().max(1);
        let target = line.clamp(1, max_line);
        self.source_viewer.current_line = target - 1;
        self.source_viewer.top_line = target - 1;
        if let Some(editor) = self.editor_session.as_mut() {
            let row = (target - 1).min(editor.line_count().saturating_sub(1));
            editor.set_cursor(row, 0);
        }
    }
}

impl TuiApp {
    /// Keys owned by the diff pane. Returns `false` for anything it does not
    /// use, so unhandled keys fall through to the composer rather than being
    /// swallowed — the failure that lost a pasted message during the
    /// competitive benchmark.
    pub(super) fn handle_diff_key(&mut self, key: event::KeyEvent) -> bool {
        use event::{KeyCode, KeyModifiers};

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('d') => {
                    self.diff_view.scroll_page(1);
                    true
                }
                KeyCode::Char('u') => {
                    self.diff_view.scroll_page(-1);
                    true
                }
                _ => false,
            };
        }
        if !(key.modifiers & !KeyModifiers::SHIFT).is_empty() {
            return false;
        }

        match key.code {
            KeyCode::Esc => {
                self.close_diff_view();
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.diff_view.scroll_by(1);
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.diff_view.scroll_by(-1);
                true
            }
            KeyCode::PageDown => {
                self.diff_view.scroll_page(2);
                true
            }
            KeyCode::PageUp => {
                self.diff_view.scroll_page(-2);
                true
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.diff_view.scroll_to_top();
                true
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.diff_view.scroll_to_bottom();
                true
            }
            KeyCode::Char(']') => {
                self.diff_view.next_hunk();
                true
            }
            KeyCode::Char('[') => {
                self.diff_view.prev_hunk();
                true
            }
            KeyCode::Char('n') => {
                self.diff_view.select_next_file();
                true
            }
            KeyCode::Char('p') => {
                self.diff_view.select_prev_file();
                true
            }
            KeyCode::Char('d') => {
                self.toggle_diff_source();
                true
            }
            // `o` only, deliberately not Enter. Enter already means "show me
            // this file's patch" in the explorer half of this mode; giving it
            // a second, opposite meaning in the patch half — leave review,
            // open the editor — would make one key do two things in one mode.
            KeyCode::Char('o') => {
                self.open_selected_diff_file();
                true
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::StatusReport {
                    title: "Diff shortcuts".into(),
                    rows: diff_shortcut_rows(),
                });
                true
            }
            _ => false,
        }
    }
}

/// Shown by `?`. A footer hint line is not discoverable enough for a keymap
/// this size — OpenCode's dedicated table is the right pattern here.
pub(super) fn diff_shortcut_rows() -> Vec<StatusRow> {
    let mut rows = Vec::new();
    rows.extend(
        [
            ("j / k · ↑ / ↓", "Scroll one line"),
            ("Ctrl+d / Ctrl+u", "Half page down / up"),
            ("g / G", "Top / bottom of the patch"),
            ("] / [", "Next / previous hunk"),
            ("n / p", "Next / previous changed file"),
            ("o", "Open this file at the cursor's line"),
            ("d", "Switch working tree / last turn"),
            ("Esc", "Close the diff view"),
        ]
        .into_iter()
        .map(|(key, action)| StatusRow::field(key, action)),
    );
    rows
}

impl TuiApp {
    /// Point the patch pane at `path` (absolute) and move focus to it, so
    /// Enter in the explorer reads as "show me this change".
    pub(super) fn select_diff_path(&mut self, path: &Path) {
        let root = self.session_view.workspace_root().to_path_buf();
        let relative = path.strip_prefix(&root).unwrap_or(path);
        self.diff_view.select_path(relative);
        self.workspace_files.explorer.selected_path = Some(path.to_path_buf());
        self.focus_block(FocusBlock::Workspace);
    }
}
