//! Slash-command and semantic-command dispatch for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Key presses are turned into a
//! `SemanticCommand` here, and `execute_semantic_command` and `dispatch_line`
//! carry them out. Methods are moved verbatim — no signature, field access or
//! logic changed.

use super::*;

const MAX_SKILL_DESC_CHARS: usize = 100;

fn truncate_skill_description(desc: &str) -> String {
    if desc.chars().count() <= MAX_SKILL_DESC_CHARS {
        desc.to_string()
    } else {
        desc.chars().take(MAX_SKILL_DESC_CHARS).collect::<String>() + "…"
    }
}

impl TuiApp {
    pub(super) fn open_task_switcher(&mut self) {
        if let Some(supervisor) = self.supervisor.as_ref() {
            let selected_task_id = self.selected_task_id;
            let items = supervisor
                .snapshots
                .values()
                .map(|snapshot| {
                    use forge_session::{SessionLifecycle, SupervisorTurnState};
                    // "Needs you" is a turn that stopped for a reason the
                    // operator has to answer; a task the operator is already
                    // looking at is never in it.
                    let attention = snapshot.task.session_id != selected_task_id
                        && matches!(
                            snapshot.task.turn_state,
                            SupervisorTurnState::Waiting | SupervisorTurnState::Failed
                        );
                    let group = match snapshot.task.lifecycle {
                        SessionLifecycle::Archived => TaskSwitcherGroup::Archived,
                        SessionLifecycle::Unavailable => TaskSwitcherGroup::Unavailable,
                        SessionLifecycle::Active if attention => TaskSwitcherGroup::Attention,
                        SessionLifecycle::Active => TaskSwitcherGroup::Active,
                    };
                    TaskSwitcherItem {
                        session_id: snapshot.task.session_id.to_string(),
                        label: snapshot.task.label.clone(),
                        branch: snapshot.task.branch.clone(),
                        workspace: snapshot.task.workspace.display().to_string(),
                        state: snapshot.task.turn_state.label().into(),
                        attention,
                        group,
                        managed: snapshot.task.ownership
                            == forge_session::WorktreeOwnership::Managed,
                    }
                })
                .collect();
            self.overlay = Some(Overlay::task_switcher(items));
            self.set_feedback(FeedbackSeverity::Info, "Tasks · Enter switch · Esc close");
            return;
        }
        let sessions =
            match recent_resume_sessions(self.session.journal_dir(), self.session.session_id, 32) {
                Ok(sessions) => sessions,
                Err(error) => {
                    self.report_error(&format!("Could not list tasks: {error}"));
                    return;
                }
            };
        let current = ResumeSessionItem {
            id: self.session.session_id.to_string(),
            modified: "current".into(),
            title: Some("Active task · current worktree".into()),
        };
        let mut items = vec![current];
        for session in sessions {
            let timestamp: chrono::DateTime<chrono::Local> = session.modified.into();
            items.push(ResumeSessionItem {
                id: session.id.to_string(),
                modified: timestamp.format("%Y-%m-%d %H:%M").to_string(),
                title: None,
            });
        }
        self.overlay = Some(Overlay::resume_picker(items));
        self.set_feedback(FeedbackSeverity::Info, "Tasks · Enter switch · Esc close");
    }

    /// Filtered slash suggestions for the current textbox (empty if not in slash mode).
    pub fn slash_suggestions(&self) -> Vec<PaletteItem> {
        let t = self.input.text.trim();
        if !t.starts_with('/') {
            return Vec::new();
        }
        // Filter by text after leading `/`
        let filter = t.trim_start_matches('/');
        let mut items = filter_palette(filter);
        items.extend(
            self.session
                .loaded_skills()
                .into_iter()
                .map(|skill| PaletteItem {
                    cmd: format!("/{}", skill.name),
                    desc: truncate_skill_description(&skill.description),
                    is_skill: true,
                })
                .filter(|item| {
                    let filter = filter.to_ascii_lowercase();
                    item.cmd.to_ascii_lowercase().contains(&filter)
                        || item.desc.to_ascii_lowercase().contains(&filter)
                }),
        );
        items.sort_by_key(|item| {
            let skill_offset = if item.is_skill { 1 } else { 0 };
            let command = item.cmd.trim_start_matches('/').to_ascii_lowercase();
            if command.starts_with(&filter.to_ascii_lowercase()) {
                (skill_offset, 0)
            } else if command.contains(&filter.to_ascii_lowercase()) {
                (skill_offset, 1)
            } else {
                (skill_offset, 2)
            }
        });
        items
    }

    pub(super) fn clamp_slash_suggest(&mut self) {
        let n = self.slash_suggestions().len();
        if n == 0 {
            self.slash_suggestions.selected = 0;
        } else {
            self.slash_suggestions.selected = self.slash_suggestions.selected.min(n - 1);
        }
    }

    pub(super) fn complete_slash_suggestion(&mut self) {
        let items = self.slash_suggestions();
        if items.is_empty() {
            return;
        }
        let idx = self.slash_suggestions.selected.min(items.len() - 1);
        let cmd = items[idx].cmd.clone();
        // If user already typed more than the bare cmd (has args), don't clobber args
        let cur = self.input.text.trim();
        if cur == cmd || cur.starts_with(&(cmd.clone() + " ")) {
            return;
        }
        self.input.set_text(format!("{cmd} "));
        self.slash_suggestions.selected = 0;
        self.clamp_slash_suggest();
    }

    pub(super) fn tab_nav_command(&self, key: event::KeyEvent) -> Option<TabNavCommand> {
        // Plain Left is in-panel back. Right is unbound after Review removal.
        let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Left if plain => Some(TabNavCommand::PreviousTab),
            KeyCode::Right if plain => Some(TabNavCommand::NextTab),
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_global_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        match key.code {
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::GoBack)
            }

            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::MoveQueueSelection(-1))
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::MoveQueueSelection(1))
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::CancelSelectedQueueMessage)
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::QuitOrInterrupt)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::Quit)
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleToolDetails)
            }
            KeyCode::Char('t')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                Some(SemanticCommand::OpenTaskSwitcher)
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleFiles)
            }
            KeyCode::Char('`') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleBottomPanel)
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::QuickSwitchModel)
            }
            KeyCode::F(4) if key.modifiers.is_empty() => Some(SemanticCommand::OpenModelControl(
                ConnectModelColumn::Models,
            )),
            KeyCode::Char(',') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::StepReasoningEffort(false))
            }
            KeyCode::Char('.') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::StepReasoningEffort(true))
            }
            KeyCode::F(1) if self.overlay.is_none() => Some(SemanticCommand::OpenHelp),
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_file_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        if !self.workspace_files.visible {
            return None;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                Some(SemanticCommand::CancelCurrentInteraction)
            }
            KeyCode::Up if key.modifiers.is_empty() => Some(SemanticCommand::MoveFileSelection(-1)),
            KeyCode::Down if key.modifiers.is_empty() => {
                Some(SemanticCommand::MoveFileSelection(1))
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                Some(SemanticCommand::ExpandSelectedDirectory)
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                Some(SemanticCommand::CollapseSelectedDirectory)
            }
            KeyCode::Enter if key.modifiers.is_empty() => Some(SemanticCommand::OpenSelectedEntry),
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                Some(SemanticCommand::BeginCreateFile)
            }
            KeyCode::Char('N')
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                Some(SemanticCommand::BeginCreateDirectory)
            }
            KeyCode::Char('R')
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                Some(SemanticCommand::BeginRename)
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => Some(SemanticCommand::RequestDelete),
            _ => None,
        }
    }

    fn semantic_command_for_editor_key(&self, key: event::KeyEvent) -> Option<SemanticCommand> {
        if !self.current_workspace_is_file() {
            return None;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                Some(SemanticCommand::CancelCurrentInteraction)
            }
            KeyCode::Char('f')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.editor_session.is_none() =>
            {
                Some(SemanticCommand::StartSourceSearch)
            }
            KeyCode::Char('g')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.editor_session.is_none() =>
            {
                Some(SemanticCommand::StartJumpToLine)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() && self.editor_session.is_none() => {
                Some(SemanticCommand::RefreshEditor)
            }
            KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => {
                Some(SemanticCommand::SaveEditor)
            }
            KeyCode::Char('e') if key.modifiers.is_empty() && self.editor_session.is_none() => {
                Some(SemanticCommand::OpenExternalEditor)
            }
            KeyCode::Char('e') if key.modifiers == KeyModifiers::ALT => {
                Some(SemanticCommand::OpenExternalEditor)
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleCurrentFileAttachment)
            }
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_workspace_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        match self.tab_nav_command(key) {
            Some(TabNavCommand::PreviousTab) => {
                return Some(SemanticCommand::GoBack);
            }
            Some(TabNavCommand::NextTab) | None => {}
        }
        match key.code {
            KeyCode::Esc
                if key.modifiers.is_empty()
                    && self.current_workspace_is_file()
                    && self.editor_handles_escape() =>
            {
                // Let `handle_editor_key` consume this Esc to drop back to
                // NORMAL mode instead of navigating away; a second Esc (now
                // in NORMAL) falls through to the GoBack arm below.
                None
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(SemanticCommand::GoBack),
            _ if self.current_workspace_is_file() => self.semantic_command_for_editor_key(key),
            _ => None,
        }
    }

    fn editor_handles_escape(&self) -> bool {
        self.editor_session.as_ref().map_or(
            self.source_viewer.mode == crate::source_viewer::ViewerMode::Insert,
            |editor| {
                matches!(
                    editor.mode(),
                    edtui::EditorMode::Insert | edtui::EditorMode::Search
                )
            },
        )
    }

    pub(super) fn semantic_command_for_bottom_panel_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        if !self.bottom_panel.open {
            return None;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => Some(SemanticCommand::ToggleBottomPanel),
            _ => None,
        }
    }

    /// Background-task selection/cancel/approve/deny — moved out of the
    /// bottom panel's (now-deleted) Tasks tab; gated on the sidebar itself
    /// having focus, since the task strip lives there now.
    pub(super) fn semantic_command_for_sidebar_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                Some(SemanticCommand::MoveTasksSelection(-1))
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                Some(SemanticCommand::MoveTasksSelection(1))
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                Some(SemanticCommand::CancelSelectedBackgroundTask)
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => {
                Some(SemanticCommand::ApproveSelectedBackgroundTask)
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                Some(SemanticCommand::DenySelectedBackgroundTask)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                Some(SemanticCommand::CancelCurrentInteraction)
            }
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_composer_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                Some(SemanticCommand::CancelCurrentInteraction)
            }
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Some(SemanticCommand::InsertComposerNewline)
            }
            KeyCode::Enter if key.modifiers.is_empty() => Some(SemanticCommand::SubmitMessage),
            KeyCode::Tab if key.modifiers.is_empty() && self.busy_state.is_active() => {
                Some(SemanticCommand::QueueMessage)
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::EditLastQueuedMessage)
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(SemanticCommand::EditLastQueuedMessage)
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::InsertComposerNewline)
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::OpenHistorySearch)
            }
            KeyCode::Char('v')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                Some(SemanticCommand::PasteClipboardImage)
            }
            _ => None,
        }
    }

    pub(super) async fn execute_semantic_command(
        &mut self,
        command: SemanticCommand,
    ) -> Result<bool, TuiError> {
        if self.busy_state.is_active() && !command.available_while_busy() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "unavailable while Forge is working · wait or Esc to interrupt",
            );
            return Ok(true);
        }
        match command {
            SemanticCommand::GoHome => self.go_home_workspace(),
            SemanticCommand::GoBack => self.go_back_workspace(),
            SemanticCommand::PushView(view) => self.push_workspace_view(view),
            SemanticCommand::ReplaceView(view) => self.replace_workspace_view(view),
            SemanticCommand::CancelCurrentInteraction => {
                // A mouse selection takes priority: Esc's first job is to
                // deselect (mirrors browser/native-terminal behavior), not
                // interrupt a turn or move focus.
                if self.selection.is_active() {
                    self.selection.clear();
                    return Ok(true);
                }
                // While a turn is running, Esc is the scoped, low-risk way to
                // interrupt just this turn — mirrors the graceful first press
                // of Ctrl+C (`QuitOrInterrupt`) without its second-press quit
                // escalation. Previously nothing bound Esc to this at all, so
                // the only way to stop a stuck turn was to kill the whole app.
                if self.busy_state.is_active() {
                    if !self.cancellation.is_requested() {
                        self.cancellation.request();
                        self.push_toast("interrupt requested");
                    }
                }
                // History Up/Down now wrap indefinitely rather than exiting
                // to the live draft at an edge, so this is the only way
                // back to it — checked before the slash-clear branch since
                // a recalled entry may itself start with '/'.
                else if self.focus.block() == FocusBlock::Composer && self.history.browsing() {
                    if let Some(draft) = self.history.leave_browse() {
                        self.apply_history_text(draft);
                    }
                }
                // An open slash-command palette/suggestion is its own
                // interaction level: the first Esc must close *that* and
                // keep composer focus, not silently move focus away while
                // leaving the "/" text and dropdown rendered but orphaned
                // (nothing left routes Backspace/Enter/Tab back to them).
                else if self.focus.block() == FocusBlock::Composer
                    && self.input.text.starts_with('/')
                {
                    self.input.clear();
                    self.slash_suggestions.selected = 0;
                } else {
                    self.escape_navigation();
                }
            }
            SemanticCommand::OpenFile(path) => {
                if path.is_file() || path.is_symlink() {
                    self.open_file_in_editor(&path);
                } else {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!("File is no longer available: {}", path.display()),
                    );
                }
            }
            SemanticCommand::ToggleFiles => self.toggle_files_panel(),
            SemanticCommand::CloseOverlay => {
                self.dismiss_overlay();
                self.explorer_dialog.clear();
            }
            SemanticCommand::FocusComposer => self.enter_chat_composer(),
            SemanticCommand::FocusPane(block) => self.focus_block(block),
            SemanticCommand::SubmitMessage => self.submit_composer_message().await?,
            SemanticCommand::QueueMessage => self.queue_composer_message().await?,
            SemanticCommand::EditLastQueuedMessage => self.edit_last_queued_message().await,
            SemanticCommand::InsertComposerNewline => self.input.insert_newline(),
            SemanticCommand::OpenHistorySearch => {
                self.overlay = Some(Overlay::history_search(
                    self.history.entries().to_vec(),
                    self.input.text.clone(),
                ));
                self.input.history_browse = false;
                self.history.reset_browse();
            }
            SemanticCommand::OpenSlashCommands => {
                self.enter_chat_composer();
                if self.input.text.is_empty() {
                    self.input.insert('/');
                    self.clamp_slash_suggest();
                }
            }
            SemanticCommand::OpenHelp => {
                self.overlay = Some(Overlay::welcome());
                self.set_feedback(
                    FeedbackSeverity::Info,
                    "Help · press Enter to get started or Esc to dismiss",
                );
            }
            SemanticCommand::SelectEntry(path) => {
                if self.workspace_files.explorer.is_visible(&path) {
                    self.workspace_files.explorer.selected_path = Some(path);
                    if self.workspace_files.visible {
                        self.focus_block(FocusBlock::Files);
                    }
                }
            }
            SemanticCommand::MoveFileSelection(delta) => {
                self.workspace_files.explorer.move_selection(delta)
            }
            SemanticCommand::ExpandSelectedDirectory => {
                self.workspace_files.explorer.expand_selected()
            }
            SemanticCommand::CollapseSelectedDirectory => {
                self.workspace_files.explorer.collapse_selected()
            }
            SemanticCommand::ToggleDirectory(path) => {
                if self.workspace_files.explorer.is_visible_directory(&path) {
                    self.workspace_files.explorer.selected_path = Some(path);
                    self.workspace_files.explorer.activate_selected();
                }
            }
            SemanticCommand::OpenSelectedEntry | SemanticCommand::ConfirmCurrentInteraction => {
                if let Some(path) = self.workspace_files.explorer.selected_file_path() {
                    // In `/diff` the explorer is the changed-file list, so
                    // Enter points the patch pane at that file instead of
                    // dropping out of review into the editor.
                    if self.diff_view_is_open() {
                        self.select_diff_path(&path);
                    } else if path.is_file() || path.is_symlink() {
                        self.open_file_in_editor(&path);
                    } else {
                        self.set_feedback(
                            FeedbackSeverity::Warn,
                            format!("File is no longer available: {}", path.display()),
                        );
                    }
                } else {
                    self.workspace_files.explorer.activate_selected();
                }
            }
            SemanticCommand::DispatchSlash { origin, line } => {
                match origin {
                    SlashCommandOrigin::Composer | SlashCommandOrigin::GlobalPalette => {}
                }
                self.dispatch_line(&line).await?;
            }
            SemanticCommand::CycleFocus { forward } => self.cycle_focus_block(forward),
            SemanticCommand::ToggleBottomPanel => self.toggle_bottom_panel(),
            SemanticCommand::QuickSwitchModel => self.quick_switch_model(),
            SemanticCommand::OpenModelControl(focus) => self.open_connect_picker_compact(focus),
            SemanticCommand::OpenBottomPanel => self.open_bottom_panel(),
            SemanticCommand::RefreshFiles => self.workspace_files.explorer.refresh_selected(),
            SemanticCommand::RefreshEditor => {
                self.source_viewer
                    .refresh(self.session_view.workspace_root());
                self.workspace_files.explorer.refresh_git_status();
            }
            SemanticCommand::SaveEditor => self.save_active_editor(),
            SemanticCommand::BeginCreateFile => {
                self.open_explorer_name_dialog(ExplorerNameAction::CreateFile)
            }
            SemanticCommand::BeginCreateDirectory => {
                self.open_explorer_name_dialog(ExplorerNameAction::CreateDirectory)
            }
            SemanticCommand::BeginRename => {
                self.open_explorer_name_dialog(ExplorerNameAction::Rename)
            }
            SemanticCommand::RequestDelete => self.open_explorer_delete_dialog(),
            SemanticCommand::StartSourceSearch => {
                self.source_viewer.start_search();
                self.enter_transient(TransientOwner::SourceSearch);
            }
            SemanticCommand::StartJumpToLine => {
                self.source_viewer.start_jump();
                self.enter_transient(TransientOwner::JumpToLine);
            }
            SemanticCommand::OpenExternalEditor => self.external_editor.requested = true,
            SemanticCommand::ToggleCurrentFileAttachment => self.toggle_file_attachment(),
            SemanticCommand::PasteClipboardImage => {
                self.paste_clipboard_image();
            }
            SemanticCommand::ToggleToolDetails => self.tool_detail.toggle(),
            SemanticCommand::OpenTaskSwitcher => self.open_task_switcher(),
            SemanticCommand::StepReasoningEffort(forward) => {
                let stepped = self
                    .reasoning_effort
                    .value
                    .step(&self.runtime.model_label, forward);
                if stepped != self.reasoning_effort.value {
                    self.reasoning_effort.value = stepped;
                    self.sync_effort_to_session();
                    self.record_deliberate_selection();
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        format!("effort: {}", stepped.label()),
                    );
                }
            }
            SemanticCommand::MoveQueueSelection(delta) => self.move_queue_selection(delta),
            // The sidebar lists the primary session's own queue and
            // background tasks. A sibling's are held by its supervisor actor
            // and are not reachable from here yet, so refuse rather than act
            // on the primary's list under a sibling's name.
            SemanticCommand::CancelSelectedQueueMessage => {
                if self.require_primary_task("cancelling a queued message") {
                    self.cancel_selected_queue().await
                }
            }
            SemanticCommand::MoveTasksSelection(delta) => self.move_tasks_selection(delta),
            SemanticCommand::CancelSelectedBackgroundTask => {
                if self.require_primary_task("cancelling a background task") {
                    self.cancel_selected_task().await
                }
            }
            SemanticCommand::ApproveSelectedBackgroundTask => {
                if self.require_primary_task("approving a background task") {
                    self.resolve_selected_task_hitl(HitlDecision::Approve)
                }
            }
            SemanticCommand::DenySelectedBackgroundTask => {
                if self.require_primary_task("denying a background task") {
                    self.resolve_selected_task_hitl(HitlDecision::Deny)
                }
            }
            SemanticCommand::QuitOrInterrupt => {
                if self.busy_state.is_active() {
                    if self.cancellation.is_requested() {
                        self.exit.request_with_code(ExitCode::Canceled);
                    } else {
                        self.cancellation.request();
                        let queued = self.session.queue().len();
                        self.push_toast(match queued {
                            0 => "interrupt requested · Ctrl+C again to quit".into(),
                            count => format!(
                                "interrupt requested · {count} queued preserved · Ctrl+C again to quit"
                            ),
                        });
                    }
                } else {
                    self.exit.request_with_code(ExitCode::Canceled);
                }
            }
            SemanticCommand::Quit => self.exit.request(),
        }
        Ok(true)
    }

    pub(super) fn handle_theme_command(&mut self, name: Option<&str>) {
        if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
            let theme_id = forge_config::normalize_theme_id(name);
            if crate::theme::registry().contains(&theme_id) {
                self.apply_theme(theme_id, true);
            } else {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    format!("unknown theme `{name}` (use /theme to pick from installed themes)"),
                );
            }
            return;
        }
        self.overlay = Some(Overlay::theme_open(&crate::theme::active()));
        self.status_state.message = "pick a theme".into();
    }

    async fn handle_model_command(&mut self) {
        self.overlay = Some(self.build_connect_model_overlay(ConnectModelColumn::Models, false));
        self.status_state.message = "pick a model (live catalog when connected)".into();
        self.start_catalog_refresh();
    }

    pub async fn dispatch_line(&mut self, line: &str) -> Result<(), TuiError> {
        let skill_line = if let Some(skill_name) = line
            .split_whitespace()
            .next()
            .and_then(|token| token.strip_prefix('/'))
        {
            let is_skill = self
                .session
                .loaded_skills()
                .iter()
                .any(|skill| skill.name == skill_name);
            if is_skill {
                let request = line.strip_prefix('/').unwrap_or(line).trim().to_string();
                Some(format!(
                    "Use the `{skill_name}` skill for this task.\n\n{request}"
                ))
            } else {
                None
            }
        } else {
            None
        };
        let line = skill_line.as_deref().unwrap_or(line);
        if let Some(cmd_res) = parse_slash(line) {
            let slash_name = line.split_whitespace().next().unwrap_or("/");
            if let Ok(command) = &cmd_res {
                if self.busy_state.is_active() && !command.available_while_busy() {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!(
                            "{slash_name} unavailable while Forge is working · wait or Esc to interrupt"
                        ),
                    );
                    return Ok(());
                }
            }
            self.push_activity(ActivityKind::Slash, FeedbackSeverity::Info, slash_name);
            match cmd_res {
                Ok(SlashCommand::Help) => {
                    self.overlay = Some(Overlay::welcome());
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "Help · press Enter to get started or Esc to dismiss",
                    );
                }
                Ok(SlashCommand::Continue) => {
                    let session = recent_resume_sessions(
                        self.session.journal_dir(),
                        self.session.session_id,
                        1,
                    )?
                    .first()
                    .map(|session| session.id);
                    if let Some(session_id) = session {
                        Box::pin(self.dispatch_line(&format!("/resume {session_id}"))).await?;
                    } else {
                        self.set_feedback(
                            FeedbackSeverity::Info,
                            "no previous session to continue",
                        );
                    }
                }
                Ok(SlashCommand::Fork) => {
                    if !self.require_primary_task("/fork") {
                        return Ok(());
                    }
                    match self.session.fork().await {
                        Ok(session) => {
                            let session_id = session.session_id;
                            let old_session_id = self.session.session_id;
                            self.task_view_states.remove(&old_session_id);
                            self.session = session;
                            self.selected_task_id = session_id;
                            self.task_chrome
                                .retain(|task| task.session_id == session_id);
                            if self.task_chrome.is_empty() {
                                self.task_chrome.push(TaskChromeItem {
                                    session_id,
                                    slot: Some(1),
                                    label: self
                                        .runtime
                                        .cwd
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("primary")
                                        .to_string(),
                                    branch: "HEAD".into(),
                                    lifecycle: self.session.active_task.lifecycle,
                                    selected: true,
                                    secondary: None,
                                    attention: false,
                                });
                            }
                            self.session_view = SessionSnapshot::capture(&self.session);
                            self.transcript_view = TranscriptSnapshot::capture(&self.session);
                            self.conversation_view.message_start = 0;
                            self.conversation_view.event_start = 0;
                            self.conversation_view.scroll = 0;
                            self.conversation_view.follow = true;
                            self.render_cache.conversation = None;
                            self.set_feedback(
                                FeedbackSeverity::Ok,
                                "forked session · ready for a new task",
                            );
                            self.push_toast(format!("forked {session_id}"));
                        }
                        Err(error) => {
                            self.report_error(&format!("Could not fork session: {error}"))
                        }
                    }
                }
                Ok(SlashCommand::Quit) => {
                    self.exit.request();
                    self.status_state.message = "quitting…".into();
                }
                Ok(SlashCommand::Compact) => {
                    self.queue_context_reset();
                    if cfg!(test) {
                        let _ = self.drain_pending_context_reset(None).await;
                    }
                }
                Ok(SlashCommand::Model) => self.handle_model_command().await,
                Ok(SlashCommand::ResumeList) => {
                    match recent_resume_sessions(
                        self.session.journal_dir(),
                        self.session.session_id,
                        10,
                    ) {
                        Ok(sessions) if sessions.is_empty() => {
                            self.status_state.message = "no previous sessions".into();
                            self.set_feedback(
                                FeedbackSeverity::Info,
                                "No previous sessions found for this workspace.",
                            );
                        }
                        Ok(sessions) => {
                            self.status_state.message =
                                format!("{} resumable sessions", sessions.len());
                            let journal_dir = self.session.journal_dir().to_path_buf();
                            let mut items = Vec::with_capacity(sessions.len());
                            for session in sessions {
                                let timestamp: chrono::DateTime<chrono::Local> =
                                    session.modified.into();
                                let title =
                                    forge_core::session_title_hint(&journal_dir, session.id).await;
                                items.push(ResumeSessionItem {
                                    id: session.id.to_string(),
                                    modified: timestamp.format("%Y-%m-%d %H:%M").to_string(),
                                    title,
                                });
                            }
                            self.overlay = Some(Overlay::resume_picker(items));
                        }
                        Err(error) => {
                            self.report_error(&format!("Could not list previous sessions: {error}"))
                        }
                    }
                }
                Ok(SlashCommand::Tasks) => {
                    self.open_task_switcher();
                }
                Ok(SlashCommand::Resume { session_id }) => {
                    // Resume rebinds the session this app owns. A task's
                    // session/worktree binding is immutable, so this can only
                    // ever mean the primary — never the selected sibling.
                    if !self.require_primary_task("/resume") {
                        return Ok(());
                    }
                    match self.session.resume_session(session_id).await {
                        Ok(_report) => {
                            self.overlay = None;
                            self.busy_state.stop();
                            self.exit
                                .set_code(match self.session.active_task.lifecycle {
                                    forge_types::TaskLifecycle::Failed => ExitCode::Failed,
                                    forge_types::TaskLifecycle::Waiting => ExitCode::AwaitingHitl,
                                    forge_types::TaskLifecycle::Cancelled => ExitCode::Canceled,
                                    _ => ExitCode::Success,
                                });
                            // Stale Working with no live runtime becomes Interrupted.
                            if let Err(error) = self.session.mark_interrupted_if_stale().await {
                                self.report_error(&error.to_string());
                            }
                            if self.session.active_task.lifecycle
                                == forge_types::TaskLifecycle::Interrupted
                            {
                                self.status_state.message = "session interrupted".into();
                                self.set_feedback(
                                    FeedbackSeverity::Warn,
                                    "previous task interrupted · ready for a new request",
                                );
                            } else {
                                self.status_state.message = "session resumed".into();
                                let queued = self.session.queue().len();
                                self.set_feedback(
                                    FeedbackSeverity::Ok,
                                    match queued {
                                        0 => "session restored · ready for the next action".into(),
                                        count => {
                                            format!("session restored · {count} queued messages")
                                        }
                                    },
                                );
                            }
                            self.push_toast(format!("resumed {session_id}"));
                            self.push_activity(
                                ActivityKind::System,
                                FeedbackSeverity::Ok,
                                format!("session resumed · {session_id}"),
                            );
                            self.banner_state.items.clear();
                            // `resume_session` already restored the durable queue for
                            // the target session — do not clear it out from under
                            // that restoration.
                            self.task_selection.clear_queue();
                            self.stream.clear_preview();
                            self.stream.thinking.clear();
                            self.conversation_view.message_start = 0;
                            self.conversation_view.event_start = 0;
                            self.conversation_view.scroll = 0;
                            self.conversation_view.follow = true;
                            self.clear_session_approvals();
                        }
                        Err(error) => {
                            self.report_error(&format!(
                                "Could not resume session {session_id}: {error}"
                            ));
                        }
                    }
                }
                Ok(SlashCommand::Clear) => {
                    // The clear marks index into the primary session's own
                    // message/event vectors; applying them while a sibling is
                    // shown would hide the wrong transcript.
                    if !self.require_primary_task("/clear") {
                        return Ok(());
                    }
                    // Hide everything currently in the transcript without deleting session
                    // context, so subsequent model turns still see the full conversation.
                    self.conversation_view.message_start = self.session.messages.len();
                    self.conversation_view.event_start = self.session.events.len();
                    self.banner_state.items.clear();
                    self.clear_error_chrome();
                    self.feedback = FeedbackModel::default();
                    self.status_state.message.clear();
                    self.toast.clear();
                    self.conversation_view.scroll = 0;
                    self.conversation_view.follow = true;
                }
                Ok(SlashCommand::Disconnect { profile_id }) => {
                    let msg = self.disconnect_auth(profile_id.as_deref())?;
                    self.open_connect_picker();
                    self.status_state.message = msg;
                }
                Ok(SlashCommand::Connect) => {
                    self.handle_connect(ConnectAction::Open);
                }
                Ok(SlashCommand::Refresh) => {
                    self.note_workspace_changed();
                    self.status_state.message = "Refreshing git status...".into();
                }
                Ok(SlashCommand::Edit) => {
                    self.external_editor.requested = true;
                }
                Ok(SlashCommand::ContextFile) => {
                    self.toggle_file_attachment();
                }
                Ok(SlashCommand::Theme { name }) => {
                    self.handle_theme_command(name.as_deref());
                }
                Ok(SlashCommand::Status) => {
                    self.overlay = Some(Overlay::StatusReport {
                        title: "Status".into(),
                        rows: self.status_report_rows(),
                    });
                }
                Ok(SlashCommand::Context) => {
                    self.overlay = Some(Overlay::StatusReport {
                        title: "Context".into(),
                        rows: self.context_report_rows(),
                    });
                }
                Ok(SlashCommand::Effort) => {
                    if self.require_primary_task("changing effort") {
                        self.open_connect_picker_compact(ConnectModelColumn::Effort);
                    }
                }
                Ok(SlashCommand::Thinking { enabled }) => {
                    if self.require_primary_task("changing thinking") {
                        self.thinking_enabled = enabled.unwrap_or(!self.thinking_enabled);
                        self.sync_effort_to_session();
                        let label = if self.thinking_enabled { "on" } else { "off" };
                        self.set_feedback(FeedbackSeverity::Info, format!("thinking: {label}"));
                    }
                }
                Ok(SlashCommand::Terminal) => {
                    // Open rather than toggle: the user asked for the terminal
                    // by name, so closing an already-open one would be the
                    // opposite of the request. `open_bottom_panel` also focuses
                    // it, which is the point of asking for it.
                    self.open_bottom_panel();
                }
                Ok(SlashCommand::Diff { source }) => {
                    self.open_diff_view(source);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.set_feedback(FeedbackSeverity::Warn, msg.clone());
                    self.push_toast(msg);
                }
            }
            self.expire_info_feedback();
            return Ok(());
        }

        // Queue user message — the event loop drains this so the YOU bubble paints
        // before the model call, and so stream deltas can redraw each frame.
        self.clear_error_chrome();

        if self.attachment.has_images() {
            if !self.session.image_input_supported() {
                self.input.set_text(line);
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "active model does not support image inputs — switch model or dismiss the image",
                );
                return Ok(());
            }
            if matches!(
                input_route::classify_input(
                    &self.session.active_task,
                    self.overlay.is_some(),
                    line
                ),
                input_route::InputRoute::QueueFutureTask
            ) {
                self.input.set_text(line);
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "wait for the current turn to finish before sending images",
                );
                return Ok(());
            }
        }

        // Build the final message text, prepending file context if attached.
        let mut final_line = line.to_string();
        let attachment = self.attachment.take_file();
        if let Some(ref att) = attachment {
            if let Some(p) = self.source_viewer.path.as_ref() {
                match forge_workspace::file_context::build_attachment_text(
                    p,
                    att.cursor_line,
                    &att.rel_path,
                    150,
                ) {
                    Ok(ctx) => {
                        final_line = format!("{}\n\n{}", ctx, final_line);
                    }
                    Err(e) => {
                        self.set_feedback(FeedbackSeverity::Warn, e.to_string());
                    }
                }
            }
        }
        // Re-apply credentials (with silent refresh) before each turn so sessions stay signed in.
        if !self.connect.auth_suspended {
            if let Some(pid) = self.connect.profile.clone() {
                self.apply_connect_credentials(&pid);
            } else {
                // Try restore mid-session if credentials appeared (e.g. /connect in another terminal)
                let restored = {
                    let svc = ConnectService {
                        registry: &self.connect.registry,
                        store: &self.connect.store,
                        preferences: &self.connect.preferences,
                        active_profile_id: None,
                        active_model: None,
                    };
                    svc.connected_profiles().ok().and_then(|v| {
                        v.iter()
                            .find(|p| p.id == "xai")
                            .cloned()
                            .or_else(|| v.into_iter().next())
                    })
                };
                if let Some(p) = restored {
                    self.connect.profile = Some(p.id.clone());
                    self.apply_connect_credentials(&p.id);
                    if let Some(m) = p.default_model() {
                        self.runtime.model_label = m.to_string();
                        self.session.set_active_model(m);
                        self.sync_model_capabilities();
                    }
                    self.refresh_connection_ui();
                }
            }
        }

        // Gate: no LLM chat without a live provider (slash commands already returned above).
        if !self.is_provider_connected() {
            self.input.set_text(line);
            let msg = self.disconnected_message();
            self.report_error(&msg);
            self.refresh_connection_ui();
            return Ok(());
        }

        self.pending_turn
            .queue(final_line, self.attachment.take_images());
        self.busy_state.start(BusyPhase::Model);
        self.timing.started = Some(Instant::now());
        if self.timing.turn_started.is_none() {
            self.timing.turn_started = Some(Instant::now());
            self.timing.completion_tokens_at_start =
                self.session.token_usage_report().api.completion_tokens;
        }
        // Counters are per user turn, not per model step: a turn that runs
        // three tools reports one summary covering all of it.
        self.timing.chars = 0;
        self.timing.tools = 0;
        // A new user turn should always follow the live conversation tail.
        // This also ensures its thinking block is visible after the user has
        // previously scrolled up to inspect an older response.
        self.conversation_view.follow = true;
        self.conversation_view.scroll = 0;
        self.stream.clear_preview();
        self.stream.thinking.clear();
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::{AgentSession, LoopConfig};
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn busy_turns_allow_model_settings_but_gate_other_runtime_mutations() {
        for command in [
            SemanticCommand::OpenExternalEditor,
            SemanticCommand::SaveEditor,
            SemanticCommand::BeginCreateFile,
            SemanticCommand::BeginCreateDirectory,
            SemanticCommand::BeginRename,
            SemanticCommand::RequestDelete,
        ] {
            assert!(!command.available_while_busy(), "{command:?}");
        }
        for command in [
            SemanticCommand::QuickSwitchModel,
            SemanticCommand::OpenModelControl(ConnectModelColumn::Models),
            SemanticCommand::StepReasoningEffort(true),
            SemanticCommand::FocusComposer,
            SemanticCommand::SubmitMessage,
            SemanticCommand::ToggleBottomPanel,
            SemanticCommand::ToggleToolDetails,
            SemanticCommand::QuitOrInterrupt,
        ] {
            assert!(command.available_while_busy(), "{command:?}");
        }
    }

    #[tokio::test]
    async fn model_settings_can_change_while_a_turn_is_running() {
        let (_dir, mut app) = app().await;
        app.busy_state.start(BusyPhase::Model);

        app.dispatch_line("/thinking off").await.unwrap();
        assert!(!app.thinking_enabled);
        assert!(!app.session.build_model_request().thinking_enabled);

        app.dispatch_line("/effort").await.unwrap();
        assert!(matches!(
            app.overlay,
            Some(Overlay::ConnectModel {
                focus: ConnectModelColumn::Effort,
                ..
            })
        ));

        app.overlay = None;
        app.dispatch_line("/model").await.unwrap();
        assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));

        app.overlay = None;
        app.dispatch_line("/connect").await.unwrap();
        assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
    }

    /// Git-initializes `dir` so writes routed through the runtime-storage
    /// resolver (UI state, run history, context offload/progress) resolve
    /// repository-locally inside the tempdir, instead of falling back to
    /// the real platform application-data directory.
    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "--initial-branch=main", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    async fn app() -> (TempDir, TuiApp) {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,
                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("/tmp"),
                version: "forge test".into(),
                startup_notices: Vec::new(),
                file_icons: forge_config::FileIconMode::Unicode,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );
        (dir, app)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent::new(code, modifiers)
    }

    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[tokio::test]
    async fn tab_nav_uses_plain_arrows_and_rejects_shift_or_chords() {
        let (_d, app) = app().await;
        // Plain Left/Right is the unified in-panel horizontal nav: unshifted.
        assert_eq!(
            app.tab_nav_command(key(KeyCode::Left, NONE)),
            Some(TabNavCommand::PreviousTab)
        );
        assert_eq!(
            app.tab_nav_command(key(KeyCode::Right, NONE)),
            Some(TabNavCommand::NextTab)
        );
        // Shift is no longer required for tab switching; chords belong to
        // other bindings.
        assert_eq!(app.tab_nav_command(key(KeyCode::Left, SHIFT)), None);
        assert_eq!(app.tab_nav_command(key(KeyCode::Right, SHIFT)), None);
        assert_eq!(app.tab_nav_command(key(KeyCode::Left, SHIFT | CTRL)), None);
        assert_eq!(app.tab_nav_command(key(KeyCode::Right, SHIFT | ALT)), None);
        assert_eq!(app.tab_nav_command(key(KeyCode::Up, SHIFT)), None);
    }

    #[tokio::test]
    async fn global_key_bindings_map_to_their_commands() {
        let (_d, app) = app().await;
        let cases: Vec<(event::KeyEvent, SemanticCommand)> = vec![
            (key(KeyCode::Left, ALT), SemanticCommand::GoBack),
            (
                key(KeyCode::Up, CTRL),
                SemanticCommand::MoveQueueSelection(-1),
            ),
            (
                key(KeyCode::Down, CTRL),
                SemanticCommand::MoveQueueSelection(1),
            ),
            (
                key(KeyCode::Backspace, CTRL),
                SemanticCommand::CancelSelectedQueueMessage,
            ),
            (
                key(KeyCode::Char('c'), CTRL),
                SemanticCommand::QuitOrInterrupt,
            ),
            (key(KeyCode::Char('d'), CTRL), SemanticCommand::Quit),
            (
                key(KeyCode::Char('o'), CTRL),
                SemanticCommand::ToggleToolDetails,
            ),
            (key(KeyCode::Char('e'), CTRL), SemanticCommand::ToggleFiles),
            (
                key(KeyCode::Char('`'), CTRL),
                SemanticCommand::ToggleBottomPanel,
            ),
            (
                key(KeyCode::F(4), NONE),
                SemanticCommand::OpenModelControl(ConnectModelColumn::Models),
            ),
            (
                key(KeyCode::Char(','), ALT),
                SemanticCommand::StepReasoningEffort(false),
            ),
            (
                key(KeyCode::Char('.'), ALT),
                SemanticCommand::StepReasoningEffort(true),
            ),
        ];
        for (k, expected) in cases {
            assert_eq!(
                app.semantic_command_for_global_key(k),
                Some(expected.clone()),
                "{k:?} should map to {expected:?}"
            );
        }
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Right, ALT)),
            None,
            "Alt+Right is unbound after Review removal"
        );

        // Unmodified keys are not global bindings.
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Left, NONE)),
            None
        );
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Char('c'), NONE)),
            None
        );
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Char('p'), ALT)),
            None
        );
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Char('c'), ALT)),
            None
        );
        // F3 no longer opens a side-channel footer focus — reaching the
        // footer's controls is an ordinary Tab stop now (FocusBlock::Footer).
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::F(3), NONE)),
            None
        );
    }

    #[tokio::test]
    async fn composer_uses_codex_queue_bindings_while_busy() {
        let (_d, mut app) = app().await;
        app.busy_state.start(BusyPhase::Model);

        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Tab, NONE)),
            Some(SemanticCommand::QueueMessage)
        );
        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Up, ALT)),
            Some(SemanticCommand::EditLastQueuedMessage)
        );
        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Left, SHIFT)),
            Some(SemanticCommand::EditLastQueuedMessage)
        );
    }

    #[tokio::test]
    async fn help_binding_yields_to_an_open_overlay() {
        let (_d, mut app) = app().await;
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::F(1), NONE)),
            Some(SemanticCommand::OpenHelp)
        );
        // An overlay already owns the screen; F1 must not stack another.
        app.overlay = Some(Overlay::StatusReport {
            title: "t".into(),
            rows: vec![],
        });
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::F(1), NONE)),
            None
        );
    }

    #[tokio::test]
    async fn file_pane_bindings_require_a_visible_pane() {
        let (_d, mut app) = app().await;
        app.workspace_files.visible = false;
        // Every binding is inert while the pane is hidden.
        assert_eq!(
            app.semantic_command_for_file_key(key(KeyCode::Up, NONE)),
            None
        );
        assert_eq!(
            app.semantic_command_for_file_key(key(KeyCode::Char('n'), NONE)),
            None
        );

        app.workspace_files.visible = true;
        let cases: Vec<(event::KeyEvent, SemanticCommand)> = vec![
            (
                key(KeyCode::Esc, NONE),
                SemanticCommand::CancelCurrentInteraction,
            ),
            (
                key(KeyCode::Up, NONE),
                SemanticCommand::MoveFileSelection(-1),
            ),
            (
                key(KeyCode::Down, NONE),
                SemanticCommand::MoveFileSelection(1),
            ),
            (
                key(KeyCode::Right, NONE),
                SemanticCommand::ExpandSelectedDirectory,
            ),
            (
                key(KeyCode::Left, NONE),
                SemanticCommand::CollapseSelectedDirectory,
            ),
            (
                key(KeyCode::Enter, NONE),
                SemanticCommand::OpenSelectedEntry,
            ),
            (
                key(KeyCode::Char('n'), NONE),
                SemanticCommand::BeginCreateFile,
            ),
            (
                key(KeyCode::Char('d'), NONE),
                SemanticCommand::RequestDelete,
            ),
        ];
        for (k, expected) in cases {
            assert_eq!(
                app.semantic_command_for_file_key(k),
                Some(expected.clone()),
                "{k:?} should map to {expected:?}"
            );
        }
    }

    /// The capital-letter bindings accept the key both with and without an
    /// explicit SHIFT modifier, because terminals disagree on whether they
    /// report it alongside an already-uppercased character.
    #[tokio::test]
    async fn shifted_file_bindings_accept_either_modifier_report() {
        let (_d, mut app) = app().await;
        app.workspace_files.visible = true;
        for modifiers in [NONE, SHIFT] {
            assert_eq!(
                app.semantic_command_for_file_key(key(KeyCode::Char('N'), modifiers)),
                Some(SemanticCommand::BeginCreateDirectory),
                "N with {modifiers:?}"
            );
            assert_eq!(
                app.semantic_command_for_file_key(key(KeyCode::Char('R'), modifiers)),
                Some(SemanticCommand::BeginRename),
                "R with {modifiers:?}"
            );
        }
        // A chord modifier is a different binding entirely.
        assert_eq!(
            app.semantic_command_for_file_key(key(KeyCode::Char('N'), CTRL)),
            None
        );
    }

    #[tokio::test]
    async fn bottom_panel_bindings_require_an_open_panel() {
        let (_d, mut app) = app().await;
        app.bottom_panel.open = false;
        assert_eq!(
            app.semantic_command_for_bottom_panel_key(key(KeyCode::Esc, NONE)),
            None
        );

        app.bottom_panel.open = true;
        assert_eq!(
            app.semantic_command_for_bottom_panel_key(key(KeyCode::Esc, NONE)),
            Some(SemanticCommand::ToggleBottomPanel)
        );
    }

    #[tokio::test]
    async fn sidebar_key_bindings_drive_background_task_selection() {
        let (_d, app) = app().await;
        for (k, expected) in [
            (
                key(KeyCode::Up, NONE),
                SemanticCommand::MoveTasksSelection(-1),
            ),
            (
                key(KeyCode::Down, NONE),
                SemanticCommand::MoveTasksSelection(1),
            ),
            (
                key(KeyCode::Char('x'), NONE),
                SemanticCommand::CancelSelectedBackgroundTask,
            ),
            (
                key(KeyCode::Char('a'), NONE),
                SemanticCommand::ApproveSelectedBackgroundTask,
            ),
            (
                key(KeyCode::Char('d'), NONE),
                SemanticCommand::DenySelectedBackgroundTask,
            ),
        ] {
            assert_eq!(
                app.semantic_command_for_sidebar_key(k),
                Some(expected.clone()),
                "{k:?}"
            );
        }
        assert_eq!(
            app.semantic_command_for_sidebar_key(key(KeyCode::Esc, NONE)),
            Some(SemanticCommand::CancelCurrentInteraction)
        );
    }

    #[tokio::test]
    async fn composer_submits_on_plain_enter_and_newlines_on_modified_enter() {
        let (_d, app) = app().await;
        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Enter, NONE)),
            Some(SemanticCommand::SubmitMessage)
        );
        // Shift, Alt and Ctrl+J all insert a newline instead of sending.
        for k in [
            key(KeyCode::Enter, SHIFT),
            key(KeyCode::Enter, ALT),
            key(KeyCode::Char('j'), CTRL),
        ] {
            assert_eq!(
                app.semantic_command_for_composer_key(k),
                Some(SemanticCommand::InsertComposerNewline),
                "{k:?}"
            );
        }
        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Esc, NONE)),
            Some(SemanticCommand::CancelCurrentInteraction)
        );
        assert_eq!(
            app.semantic_command_for_composer_key(key(KeyCode::Char('a'), NONE)),
            None
        );
    }

    #[tokio::test]
    async fn slash_suggestion_index_is_clamped_to_the_available_items() {
        let (_d, mut app) = app().await;
        app.input.set_text("/");
        let count = app.slash_suggestions().len();
        assert!(count > 0, "a bare slash should suggest commands");

        app.slash_suggestions.selected = count + 10;
        app.clamp_slash_suggest();
        assert_eq!(app.slash_suggestions.selected, count - 1);

        // With no suggestions the index collapses to zero rather than staying
        // out of range.
        app.input.set_text("not a slash command");
        assert!(app.slash_suggestions().is_empty());
        app.slash_suggestions.selected = 5;
        app.clamp_slash_suggest();
        assert_eq!(app.slash_suggestions.selected, 0);
    }

    #[tokio::test]
    async fn completing_a_slash_suggestion_preserves_already_typed_arguments() {
        let (_d, mut app) = app().await;
        app.input.set_text("/");
        let first = app.slash_suggestions()[0].cmd.clone();

        app.complete_slash_suggestion();
        assert_eq!(app.input.text, format!("{first} "));

        // Completing again is a no-op: the buffer already holds the bare command.
        app.input.set_text(first.clone());
        app.complete_slash_suggestion();
        assert_eq!(app.input.text, first);

        // Arguments the user typed must not be clobbered by a re-completion.
        let with_args = format!("{first} some argument");
        app.input.set_text(with_args.clone());
        app.complete_slash_suggestion();
        assert_eq!(app.input.text, with_args);
    }

    #[test]
    fn skill_palette_label_is_display_only() {
        let item = PaletteItem {
            cmd: "/launch-it".into(),
            desc: "Plan a developer launch".into(),
            is_skill: true,
        };

        assert_eq!(item.display_cmd(), "/skill:launch-it");
        assert_eq!(item.cmd, "/launch-it");
    }

    #[tokio::test]
    async fn theme_command_without_a_name_opens_the_picker() {
        let (_d, mut app) = app().await;
        app.handle_theme_command(None);
        assert!(matches!(app.overlay, Some(Overlay::Theme { .. })));
        assert_eq!(app.status_state.message, "pick a theme");

        // A blank name is treated as "no name" rather than as an invalid theme.
        app.overlay = None;
        app.handle_theme_command(Some("   "));
        assert!(matches!(app.overlay, Some(Overlay::Theme { .. })));
    }

    #[tokio::test]
    async fn theme_command_reports_an_unknown_name_instead_of_switching() {
        let (_d, mut app) = app().await;
        app.handle_theme_command(Some("no-such-theme"));
        // No picker: the name was supplied, it was simply wrong.
        assert!(app.overlay.is_none());
        assert!(
            !app.feedback.is_empty(),
            "an unknown theme name should surface feedback"
        );
        assert_eq!(app.feedback.severity, FeedbackSeverity::Warn);
    }

    #[tokio::test]
    async fn quit_or_interrupt_requests_cancel_before_quitting_while_busy() {
        let (_d, mut app) = app().await;
        app.busy_state.activate();
        assert!(!app.cancellation.is_requested());
        app.execute_semantic_command(SemanticCommand::QuitOrInterrupt)
            .await
            .unwrap();
        assert!(
            app.cancellation.is_requested(),
            "first Ctrl+C while busy should request a graceful cancel, not quit"
        );
        assert!(
            !app.exit.is_requested(),
            "first Ctrl+C while busy must not quit the whole app"
        );

        app.execute_semantic_command(SemanticCommand::QuitOrInterrupt)
            .await
            .unwrap();
        assert!(
            app.exit.is_requested(),
            "second Ctrl+C while still busy and already cancel-requested should quit"
        );
    }

    #[tokio::test]
    async fn esc_requests_cancel_while_busy_instead_of_navigating_focus() {
        let (_d, mut app) = app().await;
        app.busy_state.activate();
        app.focus.set_navigation(FocusBlock::Composer);
        assert!(!app.cancellation.is_requested());
        app.execute_semantic_command(SemanticCommand::CancelCurrentInteraction)
            .await
            .unwrap();
        assert!(
            app.cancellation.is_requested(),
            "Esc while a turn is busy should request cancellation"
        );
        assert!(
            !app.exit.is_requested(),
            "Esc must never quit the app, unlike a second Ctrl+C"
        );
    }

    #[tokio::test]
    async fn esc_clears_an_active_selection_before_interrupting_or_navigating() {
        let (_d, mut app) = app().await;
        app.busy_state.activate();
        app.focus.set_navigation(FocusBlock::Composer);
        app.selection.start_in(
            crate::selection::CopyPane::Conversation,
            crate::selection::Cell { row: 1, col: 1 },
        );
        app.selection
            .update(crate::selection::Cell { row: 2, col: 5 });
        assert!(app.selection.is_active());

        app.execute_semantic_command(SemanticCommand::CancelCurrentInteraction)
            .await
            .unwrap();

        assert!(
            !app.selection.is_active(),
            "Esc's first job is to clear an active mouse selection"
        );
        assert!(
            !app.cancellation.is_requested(),
            "Esc must not also interrupt a busy turn when it only cleared a selection"
        );
        assert_eq!(
            app.focus.block(),
            FocusBlock::Composer,
            "Esc must not also navigate focus when it only cleared a selection"
        );
    }

    /// Before this fix, `resolve_hitl_overlay` only executed the approved
    /// tool and returned — nothing re-armed the turn loop, so the follow-up
    /// model call never happened. The header kept reading "Working" forever
    /// (reflecting the core's lifecycle, not the TUI's own idle `busy: false`)
    /// while the session sat permanently stalled, uncancellable because
    /// nothing was actually running to cancel.
    #[tokio::test]
    async fn approving_a_hitl_gated_tool_resumes_the_turn_instead_of_stalling_forever() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![forge_types::ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "git push origin main"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "pushed".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut tools = ToolRegistry::new();
        for tool in forge_tools::default_builtins() {
            tools.register(tool);
        }
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: false,
                enable_governance: true,
                ..Default::default()
            },
            model,
            tools,
        )
        .await
        .unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "forge-test/hitl-resume".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "forge test".into(),
                startup_notices: Vec::new(),
                file_icons: forge_config::FileIconMode::Unicode,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );
        app.session
            .set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));

        app.pending_turn.queue("push it".into(), Vec::new());
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            !app.busy_state.is_active(),
            "drain_pending_prompt exits (busy=false) once a tool call needs approval"
        );
        assert!(app.session.pending_hitl().is_some());

        app.resolve_hitl_overlay(HitlDecision::Approve, ApprovalGrant::Once)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.pending_approved_tool.is_some() {
            if Instant::now() > deadline {
                panic!("approved tool did not finish");
            }
            app.poll_approved_hitl().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            app.busy_state.is_active() && app.pending_turn.continue_requested(),
            "approving the tool call must re-arm the turn loop, not leave the session idle \
             while still displaying a busy/Working state"
        );

        // Mirrors `run_loop` noticing `pending_turn_continue` on its next tick.
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            !app.busy_state.is_active(),
            "the turn must reach a terminal state, not stay stuck on Working forever"
        );
        assert_ne!(
            app.session.active_task.lifecycle,
            forge_types::TaskLifecycle::Working,
            "a Working lifecycle with busy=false is exactly the misleading stuck state this fixes"
        );
        // The mock's second scripted response only gets consumed if a real
        // follow-up model call happened — proof the turn actually resumed
        // rather than the session silently going idle after approval.
        assert!(
            app.session
                .messages
                .iter()
                .any(|m| m.content.contains("pushed")),
            "the follow-up model call's response should be recorded"
        );
    }
}
