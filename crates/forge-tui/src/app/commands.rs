//! Slash-command and semantic-command dispatch for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Key presses are turned into a
//! `SemanticCommand` here, and `execute_semantic_command` and `dispatch_line`
//! carry them out. Methods are moved verbatim — no signature, field access or
//! logic changed.

use super::*;

impl TuiApp {
    /// Filtered slash suggestions for the current textbox (empty if not in slash mode).
    pub fn slash_suggestions(&self) -> Vec<PaletteItem> {
        let t = self.input.text.trim();
        if !t.starts_with('/') {
            return Vec::new();
        }
        // Filter by text after leading `/`
        let filter = t.trim_start_matches('/');
        filter_palette(filter)
    }

    pub(super) fn clamp_slash_suggest(&mut self) {
        let n = self.slash_suggestions().len();
        if n == 0 {
            self.slash_suggest_idx = 0;
        } else {
            self.slash_suggest_idx = self.slash_suggest_idx.min(n - 1);
        }
    }

    pub(super) fn complete_slash_suggestion(&mut self) {
        let items = self.slash_suggestions();
        if items.is_empty() {
            return;
        }
        let idx = self.slash_suggest_idx.min(items.len() - 1);
        let cmd = items[idx].cmd.clone();
        // If user already typed more than the bare cmd (has args), don't clobber args
        let cur = self.input.text.trim();
        if cur == cmd || cur.starts_with(&(cmd.clone() + " ")) {
            return;
        }
        self.input.set_text(format!("{cmd} "));
        self.slash_suggest_idx = 0;
        self.clamp_slash_suggest();
    }

    pub(super) fn tab_nav_command(&self, key: event::KeyEvent) -> Option<TabNavCommand> {
        let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
        let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left if shifted && plain => Some(TabNavCommand::PreviousTab),
            KeyCode::Right if shifted && plain => Some(TabNavCommand::NextTab),
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
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            }
            KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::OpenRun(RunCommandTarget::Current))
            }
            KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => Some(
                SemanticCommand::OpenBottomPanel(BottomPanelTab::Diagnostics),
            ),
            KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::OpenBottomPanel(BottomPanelTab::Terminal))
            }
            KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::OpenBottomPanel(BottomPanelTab::Activity))
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
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleFiles)
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleInspector)
            }
            KeyCode::Char('[') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::CycleInspectorTab { forward: false })
            }
            KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::CycleInspectorTab { forward: true })
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::OpenQuickOpen)
            }
            KeyCode::Char('`') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::ToggleBottomPanel)
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::QuickSwitchModel)
            }
            KeyCode::F(1) if self.overlay.is_none() => Some(SemanticCommand::OpenHelp),
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_file_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        if !self.files_visible {
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
            KeyCode::Char('r') if key.modifiers.is_empty() => Some(SemanticCommand::RefreshFiles),
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
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::StartSourceSearch)
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::StartJumpToLine)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => Some(SemanticCommand::RefreshEditor),
            KeyCode::Char('e') if key.modifiers.is_empty() => {
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
            Some(TabNavCommand::NextTab) => {
                return Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current));
            }
            None => {}
        }
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() && self.current_workspace_is_diff() => {
                Some(SemanticCommand::SelectPreviousChange)
            }
            KeyCode::Down if key.modifiers.is_empty() && self.current_workspace_is_diff() => {
                Some(SemanticCommand::SelectNextChange)
            }
            KeyCode::Char('r' | 'R')
                if key.modifiers.is_empty() && self.current_workspace_is_diff() =>
            {
                Some(SemanticCommand::RefreshDiff)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                if self.current_workspace_is_conversation() {
                    Some(SemanticCommand::CancelCurrentInteraction)
                } else {
                    Some(SemanticCommand::GoBack)
                }
            }
            KeyCode::Enter
                if key.modifiers.is_empty()
                    && self.current_workspace_is_conversation()
                    && self.activity_summary_command().is_some() =>
            {
                Some(SemanticCommand::ActivateActivitySummary)
            }
            KeyCode::Enter if key.modifiers.is_empty() && self.current_workspace_is_run() => {
                Some(SemanticCommand::RunOrCancel)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() && self.current_workspace_is_run() => {
                Some(SemanticCommand::Rerun)
            }
            KeyCode::Char('e') if key.modifiers.is_empty() && self.current_workspace_is_run() => {
                Some(SemanticCommand::EditAndRerun)
            }
            _ if self.current_workspace_is_file() => self.semantic_command_for_editor_key(key),
            _ => None,
        }
    }

    pub(super) fn semantic_command_for_inspector_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        match self.tab_nav_command(key) {
            Some(TabNavCommand::PreviousTab) => {
                Some(SemanticCommand::CycleInspectorTab { forward: false })
            }
            Some(TabNavCommand::NextTab) => {
                Some(SemanticCommand::CycleInspectorTab { forward: true })
            }
            None => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    Some(SemanticCommand::CancelCurrentInteraction)
                }
                _ => None,
            },
        }
    }

    pub(super) fn semantic_command_for_bottom_panel_key(
        &self,
        key: event::KeyEvent,
    ) -> Option<SemanticCommand> {
        if !self.bottom_panel.open {
            return None;
        }
        match self.tab_nav_command(key) {
            Some(TabNavCommand::PreviousTab) => {
                return Some(SemanticCommand::CycleBottomPanelTab { forward: false });
            }
            Some(TabNavCommand::NextTab) => {
                return Some(SemanticCommand::CycleBottomPanelTab { forward: true });
            }
            None => {}
        }
        match key.code {
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::CycleBottomPanelTab { forward: false })
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(SemanticCommand::CycleBottomPanelTab { forward: true })
            }
            KeyCode::Enter if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::RunOrCancel)
            }
            KeyCode::Char('r') if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::Rerun)
            }
            KeyCode::Char('e') if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::EditAndRerun)
            }
            KeyCode::Char('m') if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::ToggleRunExecutionMode)
            }
            KeyCode::Char('i') if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::EditRunCommand)
            }
            KeyCode::Char('d') if self.bottom_panel.active == BottomPanelTab::Run => {
                Some(SemanticCommand::EditRunDirectory)
            }
            KeyCode::Up if self.bottom_panel.active == BottomPanelTab::Tasks => {
                Some(SemanticCommand::MoveTasksSelection(-1))
            }
            KeyCode::Down if self.bottom_panel.active == BottomPanelTab::Tasks => {
                Some(SemanticCommand::MoveTasksSelection(1))
            }
            KeyCode::Char('x') if self.bottom_panel.active == BottomPanelTab::Tasks => {
                Some(SemanticCommand::CancelSelectedBackgroundTask)
            }
            KeyCode::Char('a') if self.bottom_panel.active == BottomPanelTab::Tasks => {
                Some(SemanticCommand::ApproveSelectedBackgroundTask)
            }
            KeyCode::Char('d') if self.bottom_panel.active == BottomPanelTab::Tasks => {
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
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SemanticCommand::InsertComposerNewline)
            }
            // Help's own advertised shortcut ("? Help") only applies to a
            // truly empty composer, so a real message that happens to end in
            // "?" is never intercepted — F1 remains the fallback either way.
            KeyCode::Char('?') if key.modifiers.is_empty() && self.input.text.is_empty() => {
                Some(SemanticCommand::OpenHelp)
            }
            _ => None,
        }
    }

    pub(super) async fn execute_semantic_command(
        &mut self,
        command: SemanticCommand,
    ) -> Result<bool, TuiError> {
        match command {
            SemanticCommand::GoHome => self.go_home_workspace(),
            SemanticCommand::GoBack => self.go_back_workspace(),
            SemanticCommand::PushView(view) => self.push_workspace_view(view),
            SemanticCommand::ReplaceView(view) => self.replace_workspace_view(view),
            SemanticCommand::CancelCurrentInteraction => {
                // While a turn is running, Esc is the scoped, low-risk way to
                // interrupt just this turn — mirrors the graceful first press
                // of Ctrl+C (`QuitOrInterrupt`) without its second-press quit
                // escalation. Previously nothing bound Esc to this at all, so
                // the only way to stop a stuck turn was to kill the whole app.
                if self.busy {
                    if !self.cancel_requested {
                        self.cancel_requested = true;
                        self.push_toast("interrupt requested");
                    }
                }
                // An open slash-command palette/suggestion is its own
                // interaction level: the first Esc must close *that* and
                // keep composer focus, not silently move focus away while
                // leaving the "/" text and dropdown rendered but orphaned
                // (nothing left routes Backspace/Enter/Tab back to them).
                else if self.focus.block == FocusBlock::Composer
                    && self.input.text.starts_with('/')
                {
                    self.input.clear();
                    self.slash_suggest_idx = 0;
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
            SemanticCommand::ReviewChanges(DiffCommandContext::Current) => {
                self.capture_diff_snapshot();
                self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current))
            }
            SemanticCommand::OpenRun(target) => match target {
                RunCommandTarget::Current => {
                    if let Some(id) = self.current_run_id() {
                        self.navigate_to_workspace_view(WorkspaceView::Run(id));
                    } else {
                        self.open_bottom_panel(Some(BottomPanelTab::Run));
                    }
                }
                RunCommandTarget::Id(id) => {
                    if self.run_exists(&id) {
                        self.navigate_to_workspace_view(WorkspaceView::Run(id));
                    } else {
                        self.set_feedback(
                            FeedbackSeverity::Warn,
                            format!("Run is no longer available: {id}"),
                        );
                    }
                }
            },
            SemanticCommand::ToggleFiles => self.toggle_files_panel(),
            SemanticCommand::CloseOverlay => {
                self.overlay = None;
                self.explorer_dialog = None;
            }
            SemanticCommand::FocusComposer => self.enter_chat_composer(),
            SemanticCommand::FocusPane(block) => self.focus_block(block),
            SemanticCommand::SubmitMessage => self.submit_composer_message().await?,
            SemanticCommand::InsertComposerNewline => self.input.insert_newline(),
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
            SemanticCommand::ActivateActivitySummary => self.activate_activity_summary(),
            SemanticCommand::SelectEntry(path) => {
                if self
                    .file_explorer
                    .visible_nodes()
                    .iter()
                    .any(|node| node.path == path)
                {
                    self.file_explorer.selected_path = Some(path);
                    if self.files_visible {
                        self.focus_block(FocusBlock::Files);
                    }
                }
            }
            SemanticCommand::MoveFileSelection(delta) => self.file_explorer.move_selection(delta),
            SemanticCommand::ExpandSelectedDirectory => self.file_explorer.expand_selected(),
            SemanticCommand::CollapseSelectedDirectory => self.file_explorer.collapse_selected(),
            SemanticCommand::ToggleDirectory(path) => {
                if self.file_explorer.visible_nodes().iter().any(|node| {
                    node.path == path && node.kind == crate::file_explorer::FileKind::Directory
                }) {
                    self.file_explorer.selected_path = Some(path);
                    self.file_explorer.activate_selected();
                }
            }
            SemanticCommand::OpenSelectedEntry | SemanticCommand::ConfirmCurrentInteraction => {
                if let Some(path) = self.file_explorer.selected_file_path() {
                    if path.is_file() || path.is_symlink() {
                        self.open_file_in_editor(&path);
                    } else {
                        self.set_feedback(
                            FeedbackSeverity::Warn,
                            format!("File is no longer available: {}", path.display()),
                        );
                    }
                } else {
                    self.file_explorer.activate_selected();
                }
            }
            SemanticCommand::DispatchSlash { origin, line } => {
                match origin {
                    SlashCommandOrigin::Composer | SlashCommandOrigin::GlobalPalette => {}
                }
                self.dispatch_line(&line).await?;
            }
            SemanticCommand::CycleFocus { forward } => self.cycle_focus_block(forward),
            SemanticCommand::ToggleInspector => {
                self.sidebar_visible = !self.sidebar_visible;
                if self.sidebar_visible {
                    self.focus_block(FocusBlock::Inspector);
                } else {
                    self.restore_focus_after_closing(FocusBlock::Inspector);
                    self.normalize_focus();
                }
            }
            SemanticCommand::CycleInspectorTab { forward } => {
                self.inspector_view = if forward {
                    self.inspector_view.next()
                } else {
                    self.inspector_view.previous()
                };
            }
            SemanticCommand::ToggleBottomPanel => self.toggle_bottom_panel(),
            SemanticCommand::OpenQuickOpen => self.open_quick_open(),
            SemanticCommand::QuickSwitchModel => self.quick_switch_model(),
            SemanticCommand::CycleBottomPanelTab { forward } => {
                if forward {
                    self.bottom_panel.next_tab();
                } else {
                    self.bottom_panel.previous_tab();
                }
            }
            SemanticCommand::OpenBottomPanel(tab) => self.open_bottom_panel(Some(tab)),
            SemanticCommand::RefreshFiles => self.file_explorer.refresh_selected(),
            SemanticCommand::RefreshEditor => {
                self.source_viewer.refresh(self.session.workspace_root());
                self.file_explorer.refresh_git_status();
                if self.current_workspace_is_diff() {
                    self.refresh_diff_review();
                }
            }
            SemanticCommand::RefreshDiff => self.refresh_diff_review(),
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
            SemanticCommand::SelectPreviousChange => {
                self.diff_selected = self.diff_selected.saturating_sub(1);
            }
            SemanticCommand::SelectNextChange => {
                let count = self.file_explorer.git_status.changed_files().len();
                self.diff_selected = self
                    .diff_selected
                    .saturating_add(1)
                    .min(count.saturating_sub(1));
            }
            SemanticCommand::StartSourceSearch => {
                self.source_viewer.start_search();
                self.enter_transient(TransientOwner::SourceSearch);
            }
            SemanticCommand::StartJumpToLine => {
                self.source_viewer.start_jump();
                self.enter_transient(TransientOwner::JumpToLine);
            }
            SemanticCommand::OpenExternalEditor => self.pending_external_editor = true,
            SemanticCommand::ToggleCurrentFileAttachment => self.toggle_file_attachment(),
            SemanticCommand::ToggleToolDetails => self.tool_expanded = !self.tool_expanded,
            SemanticCommand::MoveQueueSelection(delta) => self.move_queue_selection(delta),
            SemanticCommand::CancelSelectedQueueMessage => self.cancel_selected_queue().await,
            SemanticCommand::MoveTasksSelection(delta) => self.move_tasks_selection(delta),
            SemanticCommand::CancelSelectedBackgroundTask => self.cancel_selected_task().await,
            SemanticCommand::ApproveSelectedBackgroundTask => {
                self.resolve_selected_task_hitl(HitlDecision::Approve)
            }
            SemanticCommand::DenySelectedBackgroundTask => {
                self.resolve_selected_task_hitl(HitlDecision::Deny)
            }
            SemanticCommand::QuitOrInterrupt => {
                if self.busy {
                    if self.cancel_requested {
                        self.should_quit = true;
                        self.last_exit = ExitCode::Canceled;
                    } else {
                        self.cancel_requested = true;
                        self.push_toast("interrupt requested · Ctrl+C again to quit");
                    }
                } else {
                    self.should_quit = true;
                    self.last_exit = ExitCode::Canceled;
                }
            }
            SemanticCommand::Quit => self.should_quit = true,
            SemanticCommand::RunOrCancel => {
                if self
                    .run
                    .current
                    .as_ref()
                    .is_some_and(|record| record.state == RunState::Running)
                {
                    self.cancel_run();
                } else {
                    self.run_current_draft();
                }
            }
            SemanticCommand::Rerun => self.rerun_current(),
            SemanticCommand::EditAndRerun => self.edit_and_rerun_current(),
            SemanticCommand::ToggleRunExecutionMode => {
                self.run.draft.execution_mode = match self.run.draft.execution_mode {
                    RunExecutionMode::Direct => RunExecutionMode::Shell,
                    RunExecutionMode::Shell => RunExecutionMode::Direct,
                };
            }
            SemanticCommand::EditRunCommand => {
                self.run.editing = true;
                self.run.editing_directory = false;
            }
            SemanticCommand::EditRunDirectory => {
                self.run.editing = true;
                self.run.editing_directory = true;
            }
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
        self.status_message = "pick a theme".into();
    }

    async fn handle_model_command(&mut self, provider: Option<&str>, model: Option<&str>) {
        if provider.is_none() && model.is_none() {
            self.overlay = Some(self.build_connect_model_overlay(ConnectModelColumn::Models));
            self.status_message = "pick a model (live catalog when connected)".into();
            return;
        }

        let connected_prefix = self.connect.profile.as_deref().and_then(|id| {
            self.connect
                .registry
                .get(id)
                .map(|profile| profile.model_provider_prefix.as_str())
        });
        let model_id = normalize_model_id(provider.unwrap_or(""), model, connected_prefix);
        if model_id.trim().is_empty() {
            self.set_feedback(FeedbackSeverity::Warn, "usage: /model <provider/model>");
            return;
        }
        let target_prefix = Self::model_prefix(&model_id);
        let matching_profile = self.connected_profile_for_model_prefix(target_prefix);
        let Some(matching_profile) = matching_profile else {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("connect `{target_prefix}` first before selecting {model_id}"),
            );
            self.push_notice(vec![
                format!("No connected provider matches `{target_prefix}`."),
                "Use /connect, or pick a model from the current provider catalog.".into(),
            ]);
            return;
        };
        // The provider is connected, but that alone doesn't mean `model_id` is
        // a real model — free-text `/model <name>` (including the picker's own
        // "no catalog match" fallback) previously applied *any* string here as
        // long as its prefix matched a connected provider, corrupting the
        // active selection to non-existent models (e.g. `xai/connect`).
        // Reject when this provider has a known, non-empty catalog and the
        // typed id isn't in it; only fall through for providers whose catalog
        // can't be enumerated (genuinely unlisted/custom models).
        let catalog = self.model_picker_items(false);
        let profile_catalog: Vec<&crate::overlays::ModelItem> = catalog
            .iter()
            .filter(|item| item.profile_id.as_deref() == Some(matching_profile.as_str()))
            .collect();
        if !profile_catalog.is_empty()
            && !profile_catalog
                .iter()
                .any(|item| item.model.eq_ignore_ascii_case(&model_id))
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("`{model_id}` is not in {target_prefix}'s model catalog"),
            );
            self.push_notice(vec![
                format!("`{model_id}` was not found for the connected `{target_prefix}` provider."),
                "Pick a model from the catalog with /model, or check the spelling.".into(),
            ]);
            return;
        }
        self.apply_model_selection("native", &model_id, None);
        if self.resolve_effort_for_model(&model_id) {
            self.overlay = Some(self.build_connect_model_overlay(ConnectModelColumn::Effort));
        }
    }

    pub async fn dispatch_line(&mut self, line: &str) -> Result<(), TuiError> {
        if let Some(cmd_res) = parse_slash(line) {
            let slash_name = line.split_whitespace().next().unwrap_or("/");
            self.push_activity(ActivityKind::Slash, FeedbackSeverity::Info, slash_name);
            match cmd_res {
                Ok(SlashCommand::Help) => {
                    self.overlay = Some(Overlay::welcome());
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "Help · press Enter to get started or Esc to dismiss",
                    );
                }
                Ok(SlashCommand::Quit) => {
                    self.should_quit = true;
                    self.status_message = "quitting…".into();
                }
                Ok(SlashCommand::Compact) => {
                    self.queue_context_reset();
                    if cfg!(test) {
                        let _ = self.drain_pending_context_reset(None).await;
                    }
                }
                Ok(SlashCommand::Model { provider, model }) => {
                    self.handle_model_command(provider.as_deref(), model.as_deref())
                        .await
                }
                Ok(SlashCommand::ResumeList) => {
                    match recent_resume_sessions(
                        self.session.journal_dir(),
                        self.session.session_id,
                        10,
                    ) {
                        Ok(sessions) if sessions.is_empty() => {
                            self.status_message = "no previous sessions".into();
                            self.push_notice(vec![
                                "No previous sessions found for this workspace.".into(),
                            ]);
                        }
                        Ok(sessions) => {
                            self.status_message = format!("{} resumable sessions", sessions.len());
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
                            self.notices.clear();
                            self.overlay = Some(Overlay::resume_picker(items));
                        }
                        Err(error) => {
                            self.report_error(&format!("Could not list previous sessions: {error}"))
                        }
                    }
                }
                Ok(SlashCommand::Resume { session_id }) => {
                    match self.session.resume_session(session_id).await {
                        Ok(_report) => {
                            self.overlay = None;
                            self.notices.clear();
                            self.busy = false;
                            self.busy_phase = BusyPhase::Idle;
                            self.last_exit = match self.session.active_task.lifecycle {
                                forge_types::TaskLifecycle::Failed => ExitCode::Failed,
                                forge_types::TaskLifecycle::Waiting => ExitCode::AwaitingHitl,
                                forge_types::TaskLifecycle::Cancelled => ExitCode::Canceled,
                                _ => ExitCode::Success,
                            };
                            // Stale Working with no live runtime becomes Interrupted.
                            if let Err(error) = self.session.mark_interrupted_if_stale().await {
                                self.report_error(&error.to_string());
                            }
                            if self.session.active_task.lifecycle
                                == forge_types::TaskLifecycle::Interrupted
                            {
                                self.status_message = "session interrupted".into();
                                self.set_feedback(
                                    FeedbackSeverity::Warn,
                                    "previous task interrupted · ready for a new request",
                                );
                            } else {
                                self.status_message = "session resumed".into();
                                self.set_feedback(
                                    FeedbackSeverity::Ok,
                                    "session restored · ready for the next action",
                                );
                            }
                            self.push_toast(format!("resumed {session_id}"));
                            self.push_activity(
                                ActivityKind::System,
                                FeedbackSeverity::Ok,
                                format!("session resumed · {session_id}"),
                            );
                            self.ui_banners.clear();
                            // `resume_session` already restored the durable queue for
                            // the target session — do not clear it out from under
                            // that restoration.
                            self.queue_selected = None;
                            self.stream_preview.clear();
                            self.stream_thinking.clear();
                            self.conversation_view.message_start = 0;
                            self.conversation_view.event_start = 0;
                            self.conversation_view.scroll = 0;
                            self.conversation_view.follow = true;
                            self.hitl_session_allow.clear();
                            self.maybe_open_hitl();
                        }
                        Err(error) => {
                            self.report_error(&format!(
                                "Could not resume session {session_id}: {error}"
                            ));
                        }
                    }
                }
                Ok(SlashCommand::Clear) => {
                    // Hide everything currently in the transcript without deleting session
                    // context, so subsequent model turns still see the full conversation.
                    self.conversation_view.message_start = self.session.messages.len();
                    self.conversation_view.event_start = self.session.events.len();
                    self.ui_banners.clear();
                    self.notices.clear();
                    self.clear_error_chrome();
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                    self.toast = None;
                    self.conversation_view.scroll = 0;
                    self.conversation_view.follow = true;
                }
                Ok(SlashCommand::Disconnect { profile_id }) => {
                    let msg = self.disconnect_auth(profile_id.as_deref())?;
                    self.open_connect_picker();
                    self.status_message = msg;
                }
                Ok(SlashCommand::Connect(action)) => {
                    self.handle_connect(action);
                }
                Ok(SlashCommand::Refresh) => {
                    if self.current_workspace_is_diff() {
                        self.refresh_diff_review();
                    } else {
                        self.note_workspace_changed();
                    }
                    self.status_message = "Refreshing git status...".into();
                }
                Ok(SlashCommand::Edit) => {
                    self.pending_external_editor = true;
                }
                Ok(SlashCommand::ContextFile) => {
                    self.toggle_file_attachment();
                }
                Ok(SlashCommand::Theme { name }) => {
                    self.handle_theme_command(name.as_deref());
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.set_feedback(FeedbackSeverity::Warn, msg.clone());
                    self.notices.clear();
                    self.push_toast(msg);
                }
            }
            return Ok(());
        }

        // Queue user message — the event loop drains this so the YOU bubble paints
        // before the model call, and so stream deltas can redraw each frame.
        self.clear_error_chrome();

        // Build the final message text, prepending file context if attached.
        let mut final_line = line.to_string();
        let attachment = self.pending_attachment.take();
        if let Some(ref att) = attachment {
            if let Some(p) = self.source_viewer.path.as_ref() {
                match crate::file_context::build_attachment_text(
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

        self.pending_prompt = Some(final_line);
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.turn_started = Some(Instant::now());
        // A new user turn should always follow the live conversation tail.
        // This also ensures its thinking block is visible after the user has
        // previously scrolled up to inspect an older response.
        self.conversation_view.follow = true;
        self.conversation_view.scroll = 0;
        self.stream_preview.clear();
        self.stream_thinking.clear();
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
    use crate::widgets::bottom_panel::BottomPanelTab;
    use forge_core::{AgentSession, LoopConfig};
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

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
                validation_command: None,
                file_icons: forge_config::FileIconMode::Unicode,
                mouse_capture: true,
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
    async fn tab_nav_needs_shift_and_rejects_control_or_alt() {
        let (_d, app) = app().await;
        assert_eq!(
            app.tab_nav_command(key(KeyCode::Left, SHIFT)),
            Some(TabNavCommand::PreviousTab)
        );
        assert_eq!(
            app.tab_nav_command(key(KeyCode::Right, SHIFT)),
            Some(TabNavCommand::NextTab)
        );
        // Unshifted arrows are ordinary navigation, not tab switching.
        assert_eq!(app.tab_nav_command(key(KeyCode::Left, NONE)), None);
        assert_eq!(app.tab_nav_command(key(KeyCode::Right, NONE)), None);
        // Shift combined with a chord modifier belongs to another binding.
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
                key(KeyCode::Right, ALT),
                SemanticCommand::ReviewChanges(DiffCommandContext::Current),
            ),
            (
                key(KeyCode::Char('1'), ALT),
                SemanticCommand::OpenRun(RunCommandTarget::Current),
            ),
            (
                key(KeyCode::Char('2'), ALT),
                SemanticCommand::OpenBottomPanel(BottomPanelTab::Diagnostics),
            ),
            (
                key(KeyCode::Char('3'), ALT),
                SemanticCommand::OpenBottomPanel(BottomPanelTab::Terminal),
            ),
            (
                key(KeyCode::Char('4'), ALT),
                SemanticCommand::OpenBottomPanel(BottomPanelTab::Activity),
            ),
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
                key(KeyCode::Char('b'), CTRL),
                SemanticCommand::ToggleInspector,
            ),
            (
                key(KeyCode::Char('['), ALT),
                SemanticCommand::CycleInspectorTab { forward: false },
            ),
            (
                key(KeyCode::Char(']'), ALT),
                SemanticCommand::CycleInspectorTab { forward: true },
            ),
            (
                key(KeyCode::Char('p'), CTRL),
                SemanticCommand::OpenQuickOpen,
            ),
            (
                key(KeyCode::Char('`'), CTRL),
                SemanticCommand::ToggleBottomPanel,
            ),
        ];
        for (k, expected) in cases {
            assert_eq!(
                app.semantic_command_for_global_key(k),
                Some(expected.clone()),
                "{k:?} should map to {expected:?}"
            );
        }

        // Unmodified keys are not global bindings.
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Left, NONE)),
            None
        );
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::Char('c'), NONE)),
            None
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
            lines: vec![],
        });
        assert_eq!(
            app.semantic_command_for_global_key(key(KeyCode::F(1), NONE)),
            None
        );
    }

    #[tokio::test]
    async fn file_pane_bindings_require_a_visible_pane() {
        let (_d, mut app) = app().await;
        app.files_visible = false;
        // Every binding is inert while the pane is hidden.
        assert_eq!(
            app.semantic_command_for_file_key(key(KeyCode::Up, NONE)),
            None
        );
        assert_eq!(
            app.semantic_command_for_file_key(key(KeyCode::Char('n'), NONE)),
            None
        );

        app.files_visible = true;
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
            (key(KeyCode::Char('r'), NONE), SemanticCommand::RefreshFiles),
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
        app.files_visible = true;
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
    async fn inspector_bindings_cycle_tabs_and_cancel() {
        let (_d, app) = app().await;
        assert_eq!(
            app.semantic_command_for_inspector_key(key(KeyCode::Left, SHIFT)),
            Some(SemanticCommand::CycleInspectorTab { forward: false })
        );
        assert_eq!(
            app.semantic_command_for_inspector_key(key(KeyCode::Right, SHIFT)),
            Some(SemanticCommand::CycleInspectorTab { forward: true })
        );
        assert_eq!(
            app.semantic_command_for_inspector_key(key(KeyCode::Esc, NONE)),
            Some(SemanticCommand::CancelCurrentInteraction)
        );
        assert_eq!(
            app.semantic_command_for_inspector_key(key(KeyCode::Char('x'), NONE)),
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
        // Tab cycling is available on any tab, by shifted arrow or Alt+arrow.
        for (k, forward) in [
            (key(KeyCode::Left, SHIFT), false),
            (key(KeyCode::Right, SHIFT), true),
            (key(KeyCode::Left, ALT), false),
            (key(KeyCode::Right, ALT), true),
        ] {
            assert_eq!(
                app.semantic_command_for_bottom_panel_key(k),
                Some(SemanticCommand::CycleBottomPanelTab { forward }),
                "{k:?}"
            );
        }
        assert_eq!(
            app.semantic_command_for_bottom_panel_key(key(KeyCode::Esc, NONE)),
            Some(SemanticCommand::CancelCurrentInteraction)
        );
    }

    #[tokio::test]
    async fn run_tab_bindings_are_scoped_to_the_run_tab() {
        let (_d, mut app) = app().await;
        app.bottom_panel.open = true;
        app.bottom_panel.active = BottomPanelTab::Run;
        let cases: Vec<(event::KeyEvent, SemanticCommand)> = vec![
            (key(KeyCode::Enter, NONE), SemanticCommand::RunOrCancel),
            (key(KeyCode::Char('r'), NONE), SemanticCommand::Rerun),
            (key(KeyCode::Char('e'), NONE), SemanticCommand::EditAndRerun),
            (
                key(KeyCode::Char('m'), NONE),
                SemanticCommand::ToggleRunExecutionMode,
            ),
            (
                key(KeyCode::Char('i'), NONE),
                SemanticCommand::EditRunCommand,
            ),
            (
                key(KeyCode::Char('d'), NONE),
                SemanticCommand::EditRunDirectory,
            ),
        ];
        for (k, expected) in &cases {
            assert_eq!(
                app.semantic_command_for_bottom_panel_key(*k),
                Some(expected.clone()),
                "{k:?} on the Run tab"
            );
        }

        // The same keys are inert on the other tabs, so they stay available to
        // whatever those tabs want them for.
        for tab in [
            BottomPanelTab::Diagnostics,
            BottomPanelTab::Terminal,
            BottomPanelTab::Activity,
        ] {
            app.bottom_panel.active = tab;
            for (k, _) in &cases {
                assert_eq!(
                    app.semantic_command_for_bottom_panel_key(*k),
                    None,
                    "{k:?} should be inert on {tab:?}"
                );
            }
        }
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

        app.slash_suggest_idx = count + 10;
        app.clamp_slash_suggest();
        assert_eq!(app.slash_suggest_idx, count - 1);

        // With no suggestions the index collapses to zero rather than staying
        // out of range.
        app.input.set_text("not a slash command");
        assert!(app.slash_suggestions().is_empty());
        app.slash_suggest_idx = 5;
        app.clamp_slash_suggest();
        assert_eq!(app.slash_suggest_idx, 0);
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

    #[tokio::test]
    async fn theme_command_without_a_name_opens_the_picker() {
        let (_d, mut app) = app().await;
        app.handle_theme_command(None);
        assert!(matches!(app.overlay, Some(Overlay::Theme { .. })));
        assert_eq!(app.status_message, "pick a theme");

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
        app.busy = true;
        assert!(!app.cancel_requested);
        app.execute_semantic_command(SemanticCommand::QuitOrInterrupt)
            .await
            .unwrap();
        assert!(
            app.cancel_requested,
            "first Ctrl+C while busy should request a graceful cancel, not quit"
        );
        assert!(
            !app.should_quit,
            "first Ctrl+C while busy must not quit the whole app"
        );

        app.execute_semantic_command(SemanticCommand::QuitOrInterrupt)
            .await
            .unwrap();
        assert!(
            app.should_quit,
            "second Ctrl+C while still busy and already cancel-requested should quit"
        );
    }

    #[tokio::test]
    async fn esc_requests_cancel_while_busy_instead_of_navigating_focus() {
        let (_d, mut app) = app().await;
        app.busy = true;
        app.focus.block = FocusBlock::Composer;
        assert!(!app.cancel_requested);
        app.execute_semantic_command(SemanticCommand::CancelCurrentInteraction)
            .await
            .unwrap();
        assert!(
            app.cancel_requested,
            "Esc while a turn is busy should request cancellation"
        );
        assert!(
            !app.should_quit,
            "Esc must never quit the app, unlike a second Ctrl+C"
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
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "forge test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: forge_config::FileIconMode::Unicode,
                mouse_capture: true,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );

        app.pending_prompt = Some("push it".into());
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            !app.busy,
            "drain_pending_prompt exits (busy=false) once a tool call needs approval"
        );
        assert!(app.session.pending_hitl().is_some());

        app.resolve_hitl_overlay(HitlDecision::Approve, false)
            .await
            .unwrap();
        assert!(
            app.busy && app.pending_turn_continue,
            "approving the tool call must re-arm the turn loop, not leave the session idle \
             while still displaying a busy/Working state"
        );

        // Mirrors `run_loop` noticing `pending_turn_continue` on its next tick.
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            !app.busy,
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
