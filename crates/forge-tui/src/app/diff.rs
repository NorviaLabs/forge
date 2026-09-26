//! `/diff` — opening, refreshing and closing the workspace pane's review mode.
//!
//! The view itself is state-only (`crate::diff_view`); this is the wiring that
//! feeds it from git status, the transcript, and the async patch cache.

use std::path::{Path, PathBuf};

use forge_transcript::{ChatItem, ConversationModel, ConversationViewOpts};
use forge_workspace::git_review::parse_file_diff;

use super::*;
use crate::diff_view::{entries_for_source, DiffEntry, DiffSource, DiffStatus, Patch};
use crate::overlays::StatusRow;

impl TuiApp {
    /// Git uses the existing live working-tree review pane; no separate
    /// navigation state or patch cache is needed.
    pub(super) fn open_git_view(&mut self) {
        self.open_diff_view(DiffSource::WorkingTree);
        self.git_grouped_list = true;
        self.refresh_diff_entries();
        self.focus_block(FocusBlock::Files);
        if self.workspace_is_git_repository() {
            self.status_state.message = "Git workspace · staged and unstaged changes".into();
        }
    }

    /// Enter or leave the navigator's `Git` tab. Its changed-file list is split
    /// into staged and unstaged groups; the patch renders in the Workspace pane.
    pub(super) fn apply_navigator_git_tab(&mut self, on_git: bool) {
        if on_git {
            self.open_git_view();
        } else if self.diff_view_is_open() {
            self.git_grouped_list = false;
            self.close_diff_view();
        }
    }

    /// Enter `/diff`. Focuses the workspace pane and filters the explorer to
    /// the changed files, remembering nothing else so `Esc` can put both back.
    // Retained as the seam for the Git workflow redesign.
    #[allow(dead_code)]
    pub(super) fn open_diff_view(&mut self, source: DiffSource) {
        self.git_grouped_list = false;
        self.diff_view = crate::diff_view::DiffView::new(source);
        self.workspace_files.explorer.set_diff_filter(None);
        if !self.workspace_is_git_repository() {
            self.diff_view.status = DiffStatus::NotARepo;
            self.workspace_navigation.navigate_to(WorkspaceView::Diff);
            self.focus_block(FocusBlock::Workspace);
            self.status_state.message = "Not a git repository".into();
            return;
        }
        self.workspace_files.explorer.refresh_git_status();
        self.refresh_git_branch();
        self.workspace_navigation.navigate_to(WorkspaceView::Diff);
        self.focus_block(FocusBlock::Workspace);
        self.refresh_diff_entries();
        self.status_state.message = format!("Reviewing changes · {}", source.label());
    }

    /// Leave `/diff`, restoring the full explorer listing and the pane's
    /// previous contents. One `Esc` closes the mode outright — it does not
    /// unwind level by level.
    pub(super) fn close_diff_view(&mut self) {
        if let Some(previous) = self.diff_explorer_was_visible.take() {
            self.workspace_files.visible = previous;
        }
        if !self.git_grouped_list {
            self.workspace_files.explorer.set_diff_filter(None);
        }
        self.git_grouped_list = false;
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

    // Retained with the diff-view seam for the Git workflow redesign.
    // A workspace is a repository when its root holds `.git` — a directory in a
    // plain checkout, a file in a linked worktree. Both count.
    #[allow(dead_code)]
    pub(super) fn workspace_is_git_repository(&self) -> bool {
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
        let entries = if self.git_grouped_list {
            crate::diff_view::entries_for_sides(
                &self.workspace_files.explorer.git_status.changed_files(),
            )
        } else {
            match self.diff_view.source {
                DiffSource::WorkingTree | DiffSource::Staged => entries_for_source(
                    &self.workspace_files.explorer.git_status.changed_files(),
                    self.diff_view.source,
                ),
                DiffSource::LastTurn => self.last_turn_diff_entries(),
            }
        };
        let previous = self.diff_view.selected_path().map(Path::to_path_buf);
        let previous_side = self.diff_view.selected_entry().and_then(|entry| entry.side);
        let paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
        self.diff_view.set_entries(entries);
        if self.git_grouped_list {
            let root = self.session_view.workspace_root().to_path_buf();
            self.workspace_files.explorer.set_git_change_nodes(
                paths,
                previous.and_then(|path| {
                    self.diff_view
                        .entries
                        .iter()
                        .find(|entry| entry.path == path && entry.side == previous_side)
                        .map(|entry| root.join(&entry.path))
                }),
            );
        } else {
            self.workspace_files.explorer.set_diff_filter(Some(paths));
        }
    }

    /// The files the most recent assistant turn wrote, taken from the
    /// transcript's own diff cards. Using the cards rather than `git` keeps
    /// "last turn" honest when the tree has moved on since.
    pub(super) fn last_turn_diff_entries(&self) -> Vec<DiffEntry> {
        self.last_turn_diff_cards()
            .into_iter()
            .map(|(path, _)| DiffEntry {
                path: PathBuf::from(path),
                marker: "M",
                untracked: false,
                side: None,
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
        if self.diff_view.patch_is_current(revision)
            && self
                .diff_view
                .loaded_for
                .as_ref()
                .is_some_and(|(_, loaded_path)| {
                    self.diff_view.selected_entry().is_some_and(|entry| {
                        entry.side
                            == self
                                .diff_view
                                .entries
                                .iter()
                                .find(|candidate| &candidate.path == loaded_path)
                                .and_then(|entry| entry.side)
                    })
                })
        {
            return;
        }
        let root = self.session_view.workspace_root().to_path_buf();
        // Grouped Git rows carry their own side, so the same path can show
        // index and worktree patches independently.
        let selected_side = self.diff_view.selected_entry().and_then(|entry| entry.side);
        let staged = selected_side == Some(crate::diff_view::DiffSide::Staged)
            || (selected_side.is_none() && self.diff_view.source == DiffSource::Staged);
        let cached = if staged {
            self.workspace_files
                .explorer
                .git_status
                .get_staged_diff(&path)
        } else {
            self.workspace_files
                .explorer
                .git_status
                .get_unstaged_diff(&path)
        };
        match cached {
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
                if staged {
                    self.workspace_files
                        .explorer
                        .git_status
                        .request_staged_diff(root, path);
                } else {
                    self.workspace_files
                        .explorer
                        .git_status
                        .request_unstaged_diff(root, path);
                }
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
        // A different set of changes is a different search.
        self.diff_view.clear_search();
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

        // The search prompt owns the keyboard while it is open: everything
        // printable is query text, so no diff key can fire underneath it.
        if self.diff_view.search.open {
            return match key.code {
                KeyCode::Esc => {
                    self.diff_view.close_search();
                    true
                }
                KeyCode::Enter => {
                    self.diff_view.next_match();
                    true
                }
                KeyCode::Backspace => {
                    self.diff_view.backspace_search();
                    true
                }
                KeyCode::Char(ch) => {
                    self.diff_view.push_search_char(ch);
                    true
                }
                _ => false,
            };
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
            KeyCode::Char('/') => {
                self.diff_view.open_search();
                true
            }
            KeyCode::Tab => {
                self.diff_view.next_match();
                true
            }
            KeyCode::BackTab => {
                self.diff_view.prev_match();
                true
            }
            KeyCode::Char('m') => {
                self.toggle_diff_reviewed();
                true
            }
            KeyCode::Char('v') => {
                self.toggle_diff_layout();
                true
            }
            KeyCode::Char('s') => {
                self.stage_selected_diff_file(true);
                true
            }
            KeyCode::Char('u') => {
                self.stage_selected_diff_file(false);
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
            KeyCode::Char('c') => {
                self.open_git_commit();
                true
            }
            // Directional, because `p` is already "previous changed file" and
            // stealing it would trade one muscle memory for another. Only the
            // working tree can be pulled into or pushed from.
            KeyCode::Char('>') => {
                self.start_git_sync(forge_workspace::git_sync::SyncOperation::Push);
                true
            }
            KeyCode::Char('<') => {
                self.start_git_sync(forge_workspace::git_sync::SyncOperation::Pull);
                true
            }
            // `f` fetches without merging, so the operator can look at what is
            // incoming before `<` takes it. That is the whole reason to have
            // both keys.
            KeyCode::Char('f') => {
                self.start_git_sync(forge_workspace::git_sync::SyncOperation::Fetch);
                true
            }
            KeyCode::Char('b') => {
                self.open_git_branch_picker(false);
                true
            }
            KeyCode::Char('M') => {
                self.open_git_merge_picker();
                true
            }
            // `a` only ever confirms or aborts; it never merges. Unbound in
            // spirit anywhere else in this view, and never the first thing a
            // destructive key press does.
            KeyCode::Char('a') => {
                self.abort_git_merge();
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
            ("/", "Search within the patch"),
            ("Tab / Shift+Tab", "Next / previous search hit"),
            ("m", "Mark this file reviewed"),
            ("v", "Unified / split layout"),
            ("s / u", "Stage / unstage this file"),
            ("c", "Commit the staged changes"),
            (">", "Push to the upstream branch"),
            ("<", "Pull from the upstream branch"),
            ("f", "Fetch from the upstream branch without merging"),
            (
                "b",
                "Switch branch, create, rename (`r`) or delete (`x`) one",
            ),
            ("M", "Merge a branch into the current one"),
            ("a", "Abort an in-progress merge (confirmed)"),
            ("o", "Open this file at the cursor's line"),
            ("d", "Cycle working tree / staged / last turn"),
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

impl TuiApp {
    /// `m`. Reports where the review stands rather than toggling silently —
    /// the whole value of the mark is knowing what is left.
    pub(super) fn toggle_diff_reviewed(&mut self) {
        let Some(path) = self.diff_view.selected_path().map(Path::to_path_buf) else {
            return;
        };
        let now_reviewed = self.diff_view.toggle_reviewed();
        let total = self.diff_view.entries.len();
        let done = self.diff_view.reviewed_count();
        self.status_state.message = if !now_reviewed {
            format!("{} unmarked · {done} of {total} reviewed", path.display())
        } else if self.diff_view.all_reviewed() {
            format!("All {total} files reviewed")
        } else {
            format!("{done} of {total} reviewed")
        };
        // Marking a file done is a cue to move on to the next one.
        if now_reviewed && !self.diff_view.all_reviewed() {
            self.select_next_unreviewed();
        }
    }

    fn select_next_unreviewed(&mut self) {
        let total = self.diff_view.entries.len();
        for step in 1..=total {
            let index = (self.diff_view.selected + step) % total;
            let path = self.diff_view.entries[index].path.clone();
            if !self.diff_view.reviewed.contains(&path) {
                self.diff_view.select(index);
                return;
            }
        }
    }

    /// `v`. Split needs room the patch pane never gets beside the explorer,
    /// so entering split hides the explorer and leaving it brings it back —
    /// while you are reading one patch the file list is redundant anyway, and
    /// the header already says which file this is.
    pub(super) fn toggle_diff_layout(&mut self) {
        self.diff_view.toggle_layout();
        let split = self.diff_view.layout == crate::diff_view::PatchLayout::Split;
        if split {
            self.diff_explorer_was_visible = Some(self.workspace_files.visible);
            self.workspace_files.visible = false;
        } else if let Some(previous) = self.diff_explorer_was_visible.take() {
            self.workspace_files.visible = previous;
        }
        self.normalize_focus();
        self.report_diff_layout();
    }

    pub(super) fn report_diff_layout(&mut self) {
        let width = self.diff_view.viewport_width;
        let selected = self.diff_view.layout;
        let effective = self.diff_view.effective_layout(width);
        self.status_state.message = match (selected, effective) {
            (crate::diff_view::PatchLayout::Split, crate::diff_view::PatchLayout::Unified) => {
                format!(
                    "Split view needs {} columns; showing unified",
                    crate::diff_view::SPLIT_MIN_WIDTH
                )
            }
            (crate::diff_view::PatchLayout::Split, _) => "Split view".into(),
            _ => "Unified view".into(),
        };
    }

    /// `c` in the working-tree review. Opens the message prompt, or explains
    /// why there is nothing to commit — an empty commit would fail in `git`
    /// anyway, and failing before the prompt is one less dead end.
    pub(super) fn open_git_commit(&mut self) {
        if self.diff_view.source == DiffSource::LastTurn {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Committing applies to the working tree; press d to switch",
            );
            return;
        }
        let staged = self.staged_change_count();
        if staged == 0 {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Nothing staged — press s on a file to stage it first",
            );
            return;
        }
        self.overlay = Some(Overlay::GitCommit {
            message: String::new(),
            error: None,
        });
        self.status_state.message = format!("Reviewing {staged} staged change(s)");
    }

    /// Staged paths in the current status snapshot, derived from the same
    /// status the file list draws so the count can never disagree with the
    /// markers on screen.
    pub(super) fn staged_change_count(&self) -> usize {
        self.workspace_files
            .explorer
            .git_status
            .details
            .values()
            .filter(|status| status.staged.is_some())
            .count()
    }

    /// Commit the index. Only staged changes go in: `git commit` without `-a`
    /// is the whole contract here, and the message came from the prompt above.
    pub(super) fn commit_staged_changes(&mut self, message: &str) {
        self.overlay = None;
        let root = self.session_view.workspace_root().to_path_buf();
        let service = match forge_workspace::git_service::LocalGit::new(&root) {
            Ok(service) => service,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.to_string());
                return;
            }
        };
        match service.commit(message) {
            // `git commit` reports `[main abc1234] subject` on its first line;
            // the rest is the file/insertion summary a status line has no room
            // for.
            Ok(output) => {
                let summary = output.lines().next().unwrap_or_default().trim();
                self.set_feedback(
                    FeedbackSeverity::Info,
                    if summary.is_empty() {
                        "Committed".to_string()
                    } else {
                        format!("Committed · {summary}")
                    },
                );
                // The index and `HEAD` both moved, so the status cache and every
                // cached patch are stale.
                self.note_workspace_changed();
            }
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.to_string()),
        }
    }

    /// Kick the branch read that the sync row and the `pull`/`push` refusals
    /// both depend on. Cheap to call repeatedly: the cache coalesces onto an
    /// in-flight read.
    pub(super) fn refresh_git_branch(&mut self) {
        let root = self.session_view.workspace_root().to_path_buf();
        self.git_sync.start_branch_refresh(root);
    }

    /// The right-aligned tag on the review pane's hint row.
    ///
    /// Priority: what is happening now beats what the branch is, and an
    /// in-progress merge beats the plain branch name — it is the state that
    /// decides what the next key should be (`FORGE-DESIGN §9.8`).
    pub(super) fn git_sync_tag(&self) -> Option<String> {
        let sync = &self.git_sync;
        if let Some(operation) = sync.running.as_ref() {
            use forge_workspace::git_sync::SyncOperation;
            // A branch verb is with a branch, not an upstream, so naming the
            // upstream would name the wrong thing. Aborting a merge is not
            // *with* anything, so it names no target at all.
            let target = match operation {
                SyncOperation::Merge { branch }
                | SyncOperation::Switch { branch }
                | SyncOperation::Create { branch }
                | SyncOperation::DeleteBranch { branch, .. } => Some(branch.clone()),
                SyncOperation::RenameBranch { from, .. } => Some(from.clone()),
                SyncOperation::AbortMerge => None,
                SyncOperation::Pull | SyncOperation::Push | SyncOperation::Fetch => {
                    Some(sync.upstream().unwrap_or("upstream").to_string())
                }
            };
            return Some(match target {
                Some(target) => format!("{} {target}…", operation.active()),
                None => format!("{}…", operation.active()),
            });
        }
        if sync
            .conflicts
            .as_ref()
            .is_some_and(|conflicts| conflicts.merge_in_progress)
        {
            return Some(format!(
                "merging · {} unresolved",
                self.git_conflict_count()
            ));
        }
        // A repository this code cannot read must not look like one that is in
        // sync: `?` says the state is unknown, where an empty tag would claim
        // there is nothing to report.
        if sync.error.is_some() {
            return Some("branch ?".into());
        }
        let branch = sync.branch.as_ref()?;
        let name = match (&branch.branch, branch.detached) {
            (Some(name), _) => name.as_str(),
            (None, true) => "detached",
            (None, false) => "?",
        };
        let mut tag = name.to_string();
        // Only what is actually waiting: `↓0 ↑0` is noise on every clean read.
        if branch.behind > 0 {
            tag.push_str(&format!(" ↓{}", branch.behind));
        }
        if branch.ahead > 0 {
            tag.push_str(&format!(" ↑{}", branch.ahead));
        }
        Some(tag)
    }

    /// Apply a finished `pull`/`push`: refresh what it moved and report it.
    pub(super) fn poll_git_sync(&mut self) {
        self.git_sync.poll();
        let Some(outcome) = self.git_sync.poll_operation() else {
            return;
        };
        // Both operations move the branch, and a pull moves the working tree
        // with it, so the status cache and every cached patch are stale either
        // way.
        self.refresh_git_branch();
        self.note_workspace_changed();
        let target = self
            .git_sync
            .upstream()
            .map(str::to_string)
            .unwrap_or_else(|| "upstream".into());
        match outcome.result {
            Ok(_) => self.set_feedback(
                FeedbackSeverity::Info,
                match &outcome.operation {
                    // Naming the upstream would name the wrong thing for a
                    // branch verb.
                    forge_workspace::git_sync::SyncOperation::Merge { branch } => {
                        format!("{} {branch}", outcome.operation.finished())
                    }
                    forge_workspace::git_sync::SyncOperation::Switch { branch }
                    | forge_workspace::git_sync::SyncOperation::Create { branch } => {
                        format!("{} · on {branch}", outcome.operation.finished())
                    }
                    forge_workspace::git_sync::SyncOperation::DeleteBranch { branch, .. } => {
                        format!("{} {branch}", outcome.operation.finished())
                    }
                    // Both ends, because a rename the operator did not intend
                    // is only visible in the pair.
                    forge_workspace::git_sync::SyncOperation::RenameBranch { from, to } => {
                        format!("{} {from} → {to}", outcome.operation.finished())
                    }
                    forge_workspace::git_sync::SyncOperation::AbortMerge => {
                        outcome.operation.finished().to_string()
                    }
                    _ => format!("{} {target}", outcome.operation.finished()),
                },
            ),
            // Git's own words: a rejected push and an offline pull fail for
            // different reasons and the operator needs to tell them apart.
            Err(error) => match &outcome.operation {
                // A merge that stops for conflicts exits non-zero, but nothing
                // failed: git is asking for a human. Git's first line names the
                // path, and the banner carries the next step once the refresh
                // lands — reporting "failed" here would train the operator to
                // ignore a message that usually means "do your job".
                forge_workspace::git_sync::SyncOperation::Merge { branch } => {
                    let first = error.lines().find(|line| !line.trim().is_empty());
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        match first {
                            Some(line) => format!("Merge of {branch} stopped · {line}"),
                            None => format!("Merge of {branch} stopped"),
                        },
                    );
                }
                _ => self.set_feedback(
                    FeedbackSeverity::Error,
                    format!("{} failed · {}", outcome.operation.label(), error),
                ),
            },
        }
    }

    /// `<` / `>`. The cache refuses a second operation and a missing upstream
    /// before either can spawn a subprocess that would only fail on the
    /// network.
    pub(super) fn start_git_sync(&mut self, operation: forge_workspace::git_sync::SyncOperation) {
        let root = self.session_view.workspace_root().to_path_buf();
        // Read before the operation is handed to the worker, which takes
        // ownership of it.
        let active = operation.active();
        match self.git_sync.start_operation(root, operation) {
            Ok(()) => {
                let target = self
                    .git_sync
                    .upstream()
                    .map(str::to_string)
                    .unwrap_or_else(|| "upstream".into());
                self.status_state.message = format!("{active} {target}…");
            }
            Err(reason) => self.set_feedback(FeedbackSeverity::Warn, reason),
        }
    }

    /// `b` / `M`. Both pickers read the same cached branch list, so they share
    /// every guard: a source that cannot switch branches, and a branch state
    /// this code could not read — opening a picker over stale data would invite
    /// a choice that then fails.
    fn open_git_branch_picker(&mut self, merge: bool) {
        if self.diff_view.source == DiffSource::LastTurn {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Branches apply to the working tree; press d to switch",
            );
            return;
        }
        if self.git_sync.error.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "The branch could not be read, so there is nothing to choose from",
            );
            return;
        }
        if self.git_sync.branches.is_empty() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "No local branches yet — commit once to create one",
            );
            return;
        }
        self.overlay = Some(Overlay::GitBranch {
            selected: 0,
            filter: String::new(),
            items: self.git_sync.branches.clone(),
            current: self
                .git_sync
                .branch
                .as_ref()
                .and_then(|branch| branch.branch.clone()),
            merge,
            error: None,
        });
    }

    pub(super) fn open_git_merge_picker(&mut self) {
        if self
            .git_sync
            .conflicts
            .as_ref()
            .is_some_and(|conflicts| conflicts.merge_in_progress)
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "A merge is already in progress — finish or abort it first",
            );
            return;
        }
        self.open_git_branch_picker(true);
    }

    /// Start one branch operation on the worker. Every branch verb shares this
    /// so a refusal always reads the same way and never leaves the overlay up
    /// over an action that did not start.
    fn start_branch_operation(
        &mut self,
        operation: forge_workspace::git_sync::SyncOperation,
        what: &str,
    ) {
        let root = self.session_view.workspace_root().to_path_buf();
        match self.git_sync.start_operation(root, operation) {
            Ok(()) => {
                self.overlay = None;
                self.status_state.message = format!("{what}…");
            }
            Err(reason) => {
                self.overlay = None;
                self.set_feedback(FeedbackSeverity::Warn, reason);
            }
        }
    }

    pub(super) fn switch_git_branch(&mut self, name: &str) {
        self.start_branch_operation(
            forge_workspace::git_sync::SyncOperation::Switch {
                branch: name.to_string(),
            },
            &format!("Switching to {name}"),
        );
    }

    pub(super) fn create_git_branch(&mut self, name: &str) {
        self.start_branch_operation(
            forge_workspace::git_sync::SyncOperation::Create {
                branch: name.to_string(),
            },
            &format!("Creating {name}"),
        );
    }

    pub(super) fn merge_git_branch(&mut self, name: &str) {
        self.start_branch_operation(
            forge_workspace::git_sync::SyncOperation::Merge {
                branch: name.to_string(),
            },
            &format!("Merging {name}"),
        );
    }

    /// `a`. Aborting throws the merge away, so it confirms first and the
    /// operation itself only runs from the confirmation (`FORGE-DESIGN §8`).
    /// Called both by the key and by the confirmed action, so the two can never
    /// diverge: no confirmation open → raise it; confirmation open → run it.
    pub(super) fn abort_git_merge(&mut self) {
        let conflicts = self.git_sync.conflicts.as_ref();
        let in_progress = conflicts.is_some_and(|conflicts| conflicts.merge_in_progress);
        if !in_progress {
            self.set_feedback(FeedbackSeverity::Warn, "No merge is in progress to abort");
            return;
        }
        if matches!(self.overlay, Some(Overlay::GitAbortMerge { .. })) {
            self.start_branch_operation(
                forge_workspace::git_sync::SyncOperation::AbortMerge,
                "Aborting merge",
            );
            return;
        }
        let unresolved = self.git_conflict_count();
        let detail = format!(
            "This abandons the merge and returns the working tree to its pre-merge state. \
             {unresolved} unresolved {} still on disk will be replaced by the pre-merge contents.",
            if unresolved == 1 { "file" } else { "files" }
        );
        self.overlay = Some(Overlay::GitAbortMerge { detail });
    }

    /// `x` in the picker, and the confirmed action. Deletion destroys the
    /// commits the branch holds, so it confirms first and only runs from the
    /// confirmation — the same dual role as `abort_git_merge`.
    ///
    /// There is deliberately no force path in the UI: `-d` refuses a branch
    /// whose commits are not merged anywhere, and that refusal is the feature.
    /// Escalating to `-D` from a TUI is how commits get lost by accident, so
    /// losing an unmerged branch stays a shell operation.
    pub(super) fn delete_git_branch(&mut self, name: &str) {
        if self.git_sync.error.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "The branch could not be read, so there is nothing to delete",
            );
            return;
        }
        if self
            .git_sync
            .branch
            .as_ref()
            .and_then(|branch| branch.branch.as_deref())
            == Some(name)
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("Cannot delete `{name}`: HEAD is on it"),
            );
            return;
        }
        if matches!(self.overlay, Some(Overlay::GitDeleteBranch { .. })) {
            self.start_branch_operation(
                forge_workspace::git_sync::SyncOperation::DeleteBranch {
                    branch: name.to_string(),
                    force: false,
                },
                &format!("Deleting {name}"),
            );
            return;
        }
        self.overlay = Some(Overlay::GitDeleteBranch {
            name: name.to_string(),
            detail: format!(
                "This deletes `{name}` and the commits that are only on it. Git refuses if \
                 they are not merged anywhere; Forge will not force it."
            ),
        });
    }

    /// `r` in the picker: ask for the new name. Renaming destroys nothing, so
    /// unlike deletion it needs no confirmation — only a valid name.
    pub(super) fn open_git_rename(&mut self, from: &str) {
        if self.git_sync.error.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "The branch could not be read, so there is nothing to rename",
            );
            return;
        }
        self.overlay = Some(Overlay::GitRenameBranch {
            from: from.to_string(),
            // Prefilled with the current name, so the edit is a correction
            // rather than a retype.
            name: from.to_string(),
            error: None,
        });
    }

    pub(super) fn rename_git_branch(&mut self, from: &str, to: &str) {
        if to.trim().is_empty() {
            if let Some(Overlay::GitRenameBranch { error, .. }) = self.overlay.as_mut() {
                *error = Some("A branch needs a name".into());
            }
            return;
        }
        self.start_branch_operation(
            forge_workspace::git_sync::SyncOperation::RenameBranch {
                from: from.to_string(),
                to: to.to_string(),
            },
            &format!("Renaming {from} to {to}"),
        );
    }

    /// Unresolved paths, counted from the same status the file list draws, so
    /// the banner and the `!` markers can never disagree.
    pub(super) fn git_conflict_count(&self) -> usize {
        self.workspace_files
            .explorer
            .git_status
            .details
            .values()
            .filter(|status| {
                status.staged == Some(forge_workspace::git_status::GitStatusKind::Conflicted)
                    || status.unstaged
                        == Some(forge_workspace::git_status::GitStatusKind::Conflicted)
            })
            .count()
    }

    /// The merge banner for the review pane, or `None` when no merge is in
    /// progress. Preformatted for the widget, like `git_sync_tag`.
    pub(super) fn git_merge_banner(&self) -> Option<String> {
        let conflicts = self.git_sync.conflicts.as_ref()?;
        if !conflicts.merge_in_progress {
            return None;
        }
        let unresolved = self.git_conflict_count();
        Some(if unresolved == 0 {
            // Every conflict is resolved but the merge commit has not been made
            // yet — committing with `c` is the last step, so say that rather
            // than leaving the operator to guess.
            "MERGE IN PROGRESS · all resolved · c commits the merge".to_string()
        } else {
            format!(
                "MERGE IN PROGRESS · {unresolved} unresolved · o opens, s marks resolved, a aborts"
            )
        })
    }

    /// `s` / `u`. Staging is reversible and touches only the index, so it runs
    /// without a confirmation; discarding work would not, and is not bound.
    pub(super) fn stage_selected_diff_file(&mut self, stage: bool) {
        if self.diff_view.source == DiffSource::LastTurn {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Staging applies to the working tree; press d to switch",
            );
            return;
        }
        let Some(relative) = self.diff_view.selected_path().map(Path::to_path_buf) else {
            return;
        };
        let root = self.session_view.workspace_root().to_path_buf();
        let result = if stage {
            forge_workspace::git_review::stage_path(&root, &relative)
        } else {
            forge_workspace::git_review::unstage_path(&root, &relative)
        };
        match result {
            Ok(()) => {
                let verb = if stage { "Staged" } else { "Unstaged" };
                self.status_state.message = format!("{verb} {}", relative.display());
                // The index moved, so the status cache and every cached patch
                // for this revision are stale.
                self.note_workspace_changed();
            }
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Warn, error);
            }
        }
    }
}
