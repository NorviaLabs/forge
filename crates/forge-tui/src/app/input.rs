//! Keyboard input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `handle_key` is the entry point; the
//! `handle_*_key` family dispatches per focus target. Key-to-command mapping
//! lives in `app/commands.rs`; this module decides who handles a press.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    async fn handle_editor_command_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(command) = self.editor_command.as_mut() else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.editor_command = None;
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                command.pop();
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let command = std::mem::take(command);
                self.editor_command = None;
                match command.as_str() {
                    "w" | "write" => {
                        self.save_active_editor();
                        if self
                            .editor_session
                            .as_ref()
                            .is_some_and(|editor| !editor.is_dirty())
                        {
                            self.editor_message =
                                Some(format!("written {}", self.source_viewer.rel_path));
                        }
                    }
                    "q" | "quit" => self.go_back_workspace(),
                    "q!" | "quit!" => {
                        if let Some(editor) = self.editor_session.as_mut() {
                            editor.accept_current_text();
                        }
                        self.go_back_workspace();
                    }
                    "wq" | "x" => {
                        self.save_active_editor();
                        if self
                            .editor_session
                            .as_ref()
                            .is_some_and(|editor| !editor.is_dirty())
                        {
                            self.go_back_workspace();
                        }
                    }
                    command if command == "e" || command == "edit" => {
                        if self
                            .editor_session
                            .as_ref()
                            .is_some_and(|editor| editor.is_dirty())
                        {
                            self.explorer_dialog.show(ExplorerDialog::SaveConflict);
                        } else {
                            self.reload_active_editor_from_disk();
                            self.editor_message =
                                Some(format!("reloaded {}", self.source_viewer.rel_path));
                        }
                    }
                    command if command.starts_with("e ") || command.starts_with("edit ") => {
                        let path = command
                            .split_once(' ')
                            .map(|(_, path)| path.trim())
                            .unwrap_or_default();
                        match self.resolve_workspace_path(path) {
                            Ok(path) if path.is_file() => {
                                if self.source_viewer.path.as_deref() == Some(path.as_path())
                                    && self
                                        .editor_session
                                        .as_ref()
                                        .is_some_and(|editor| editor.is_dirty())
                                {
                                    self.explorer_dialog.show(ExplorerDialog::SaveConflict);
                                } else {
                                    self.open_file_in_editor(&path);
                                }
                            }
                            Ok(_) | Err(_) => {
                                self.editor_message = Some("E32: No file or directory".to_string());
                            }
                        }
                    }
                    command if command.starts_with('s') || command.starts_with('%') => {
                        match parse_editor_substitute(command) {
                            Some((all_lines, pattern, replacement, replace_all)) => {
                                let count = self
                                    .editor_session
                                    .as_mut()
                                    .map(|editor| {
                                        editor.substitute(
                                            &pattern,
                                            &replacement,
                                            all_lines,
                                            replace_all,
                                        )
                                    })
                                    .unwrap_or(0);
                                self.editor_message = Some(format!(
                                    "{count} substitution{}",
                                    if count == 1 { "" } else { "s" }
                                ));
                            }
                            None => {
                                self.editor_message = Some("E488: Trailing characters".to_string());
                            }
                        }
                    }
                    _ => {
                        self.editor_message =
                            Some(format!("E492: Not an editor command: {command}"));
                    }
                }
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                command.push(ch);
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) async fn handle_task_strip_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        let count = self.task_chrome.len();
        if count == 0 {
            return Ok(false);
        }
        match key.code {
            KeyCode::Left if key.modifiers.is_empty() => {
                self.task_strip_selection = self.task_strip_selection.saturating_sub(1);
                Ok(true)
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.task_strip_selection = (self.task_strip_selection + 1).min(count - 1);
                Ok(true)
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.task_strip_selection = 0;
                Ok(true)
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.task_strip_selection = count - 1;
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let item = self.task_chrome[self.task_strip_selection].clone();
                if item.session_id != self.session.session_id {
                    if self.supervisor.is_some() {
                        if !self
                            .send_task_command(forge_session::SupervisorCommand::SelectTask {
                                session_id: Some(item.session_id),
                            })
                            .await
                        {
                            return Ok(true);
                        }
                        if let Some(snapshot) = self
                            .supervisor
                            .as_ref()
                            .and_then(|supervisor| supervisor.snapshots.get(&item.session_id))
                            .cloned()
                        {
                            self.save_task_view_state(self.selected_task_id);
                            self.restore_task_view_state(item.session_id);
                            self.selected_task_id = item.session_id;
                            for task in &mut self.task_chrome {
                                task.selected = task.session_id == item.session_id;
                            }
                            self.session_view = snapshot.session.clone();
                            self.transcript_view = snapshot.transcript.clone();
                        }
                        self.set_feedback(
                            FeedbackSeverity::Info,
                            format!("task selected · {}", item.label),
                        );
                    }
                } else {
                    self.save_task_view_state(self.selected_task_id);
                    self.restore_task_view_state(item.session_id);
                    self.selected_task_id = item.session_id;
                    self.set_feedback(FeedbackSeverity::Info, "primary task selected");
                }
                Ok(true)
            }
            KeyCode::Char('s') if key.modifiers.is_empty() => {
                let session_id = self.task_chrome[self.task_strip_selection].session_id;
                if session_id == self.session.session_id {
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "use Esc or Ctrl+C to stop the primary task",
                    );
                    return Ok(true);
                }
                self.send_task_command(forge_session::SupervisorCommand::StopTurn { session_id })
                    .await;
                Ok(true)
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                let session_id = self.task_chrome[self.task_strip_selection].session_id;
                if session_id == self.session.session_id {
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "primary task is controlled by the composer",
                    );
                    return Ok(true);
                }
                self.send_task_command(forge_session::SupervisorCommand::ContinueTurn {
                    session_id,
                })
                .await;
                Ok(true)
            }
            KeyCode::Char('p') if key.modifiers.is_empty() => {
                let task = self.task_chrome[self.task_strip_selection].clone();
                self.send_task_command(forge_session::SupervisorCommand::PinTask {
                    session_id: task.session_id,
                    slot: task.slot.or(Some(1)),
                    swap: true,
                })
                .await;
                Ok(true)
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                let session_id = self.task_chrome[self.task_strip_selection].session_id;
                if session_id == self.session.session_id {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        "the primary task cannot be archived",
                    );
                    return Ok(true);
                }
                self.send_task_command(forge_session::SupervisorCommand::ArchiveTask {
                    session_id,
                })
                .await;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Move the current view out of the app, leaving a blank one behind.
    ///
    /// Moving rather than cloning keeps this correct for the state that holds
    /// buffers (an open editor, a rendered stream preview) and guarantees the
    /// two tasks never share a live copy of anything.
    fn take_task_view_state(&mut self) -> TaskLocalViewState {
        let blank = TaskLocalViewState::default();
        TaskLocalViewState {
            input: std::mem::replace(&mut self.input, blank.input),
            workspace_navigation: std::mem::take(&mut self.workspace_navigation),
            source_viewer: std::mem::replace(&mut self.source_viewer, blank.source_viewer),
            conversation_view: std::mem::replace(
                &mut self.conversation_view,
                blank.conversation_view,
            ),
            focus: std::mem::replace(&mut self.focus, blank.focus),
            overlay: self.overlay.take(),
            attachment: std::mem::take(&mut self.attachment),
            editor_session: self.editor_session.take(),
            editor_command: self.editor_command.take(),
            editor_message: self.editor_message.take(),
            editor_viewport: std::mem::replace(&mut self.editor_viewport, blank.editor_viewport),
            diff_view: std::mem::take(&mut self.diff_view),
            stream: std::mem::take(&mut self.stream),
            activity: std::mem::take(&mut self.activity),
            banner_state: std::mem::take(&mut self.banner_state),
            tool_detail: std::mem::take(&mut self.tool_detail),
            composer_chip_focus: self.composer_chip_focus.take(),
            approval_session: std::mem::take(&mut self.approval_session),
            question_session: std::mem::take(&mut self.question_session),
            busy_state: std::mem::take(&mut self.busy_state),
            status_state: std::mem::take(&mut self.status_state),
            search_status: std::mem::take(&mut self.search_status),
            pending_turn: std::mem::take(&mut self.pending_turn),
            pending_interaction: std::mem::take(&mut self.pending_interaction),
            task_selection: std::mem::take(&mut self.task_selection),
            timing: std::mem::take(&mut self.timing),
            reasoning_effort: std::mem::take(&mut self.reasoning_effort),
            // Copied, not moved: the incoming task may have no model recorded
            // yet, and a blank footer between the save and the restore would
            // be a visible flicker on every switch.
            model_label: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
        }
    }

    fn install_task_view_state(&mut self, state: TaskLocalViewState) {
        self.input = state.input;
        self.workspace_navigation = state.workspace_navigation;
        self.source_viewer = state.source_viewer;
        self.conversation_view = state.conversation_view;
        self.focus = state.focus;
        self.overlay = state.overlay;
        self.attachment = state.attachment;
        self.editor_session = state.editor_session;
        self.editor_command = state.editor_command;
        self.editor_message = state.editor_message;
        self.editor_viewport = state.editor_viewport;
        self.diff_view = state.diff_view;
        self.stream = state.stream;
        self.activity = state.activity;
        self.banner_state = state.banner_state;
        self.tool_detail = state.tool_detail;
        self.composer_chip_focus = state.composer_chip_focus;
        self.approval_session = state.approval_session;
        self.question_session = state.question_session;
        self.busy_state = state.busy_state;
        self.status_state = state.status_state;
        self.search_status = state.search_status;
        self.pending_turn = state.pending_turn;
        self.pending_interaction = state.pending_interaction;
        self.task_selection = state.task_selection;
        self.timing = state.timing;
        self.reasoning_effort = state.reasoning_effort;
        // A task that has never been shown has no model of its own recorded
        // yet; keep whatever is on screen rather than blanking the footer.
        if !state.model_label.is_empty() {
            self.runtime.model_label = state.model_label;
        }
        if !state.provider.is_empty() {
            self.runtime.provider = state.provider;
        }
        // Rendered conversation lines belong to the transcript that produced
        // them, so drop the cache rather than paint one task's rows over
        // another's.
        self.render_cache.conversation = None;
        self.normalize_focus();
    }

    pub(super) fn save_task_view_state(&mut self, session_id: uuid::Uuid) {
        let state = self.take_task_view_state();
        self.task_view_states.insert(session_id, state);
    }

    pub(super) fn restore_task_view_state(&mut self, session_id: uuid::Uuid) {
        let state = self
            .task_view_states
            .remove(&session_id)
            .unwrap_or_else(|| {
                // First visit: a blank view, but seeded with the model that is
                // already showing so the footer does not flicker to empty.
                TaskLocalViewState {
                    model_label: self.runtime.model_label.clone(),
                    provider: self.runtime.provider.clone(),
                    ..Default::default()
                }
            });
        self.install_task_view_state(state);
    }

    /// Send a supervisor command and report the outcome in the feedback strip.
    ///
    /// A rejected command — an attach that names the wrong branch, an archive
    /// of a running task — is operator error, not a TUI failure. Returning it
    /// as `TuiError` would tear the session down over a typo, so failures
    /// surface as feedback and the caller learns only whether it worked.
    pub(super) async fn send_task_command(
        &mut self,
        command: forge_session::SupervisorCommand,
    ) -> bool {
        let Some(handle) = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.handle.clone())
        else {
            self.set_feedback(FeedbackSeverity::Warn, "multi-task mode is unavailable");
            return false;
        };
        match handle.command(command).await {
            Ok(()) => true,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.to_string());
                false
            }
        }
    }

    fn handle_explorer_dialog_key(&mut self, key: event::KeyEvent) -> bool {
        let Some(dialog) = self.explorer_dialog.take() else {
            return false;
        };
        let next = match dialog {
            ExplorerDialog::Name {
                action,
                parent,
                source,
                mut input,
                ..
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    input.pop();
                    Some(ExplorerDialog::Name {
                        action,
                        parent,
                        source,
                        input,
                        error: None,
                    })
                }
                KeyCode::Char(c)
                    if (key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE)).is_empty()
                        && !c.is_control() =>
                {
                    input.push(c);
                    Some(ExplorerDialog::Name {
                        action,
                        parent,
                        source,
                        input,
                        error: None,
                    })
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    match self.prepare_explorer_name_operation(
                        action,
                        &parent,
                        source.as_deref(),
                        &input,
                    ) {
                        Ok(next) => Some(next),
                        Err(error) => Some(ExplorerDialog::Name {
                            action,
                            parent,
                            source,
                            input,
                            error: Some(error.actionable()),
                        }),
                    }
                }
                _ => Some(ExplorerDialog::Name {
                    action,
                    parent,
                    source,
                    input,
                    error: None,
                }),
            },
            ExplorerDialog::ConfirmCreate {
                action,
                parent,
                name,
                path,
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.apply_confirmed_create(action, &parent, &name);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmCreate {
                    action,
                    parent,
                    name,
                    path,
                }),
            },
            ExplorerDialog::ConfirmRename { source, path, name } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.apply_confirmed_rename(&source, &name);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmRename { source, path, name }),
            },
            ExplorerDialog::ConfirmDelete {
                source,
                name,
                kind,
                non_empty,
                permanent,
                error,
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Char('p') | KeyCode::Char('P') if error.is_some() && !permanent => {
                    Some(ExplorerDialog::ConfirmDelete {
                        source,
                        name,
                        kind,
                        non_empty,
                        permanent: true,
                        error: None,
                    })
                }
                KeyCode::Char('D') if permanent || non_empty => {
                    self.apply_confirmed_delete(
                        &source,
                        if permanent {
                            DeleteMode::Permanent
                        } else {
                            DeleteMode::Trash
                        },
                    );
                    None
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
                    if !permanent && !non_empty && error.is_none() =>
                {
                    self.apply_confirmed_delete(&source, DeleteMode::Trash);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmDelete {
                    source,
                    name,
                    kind,
                    non_empty,
                    permanent,
                    error,
                }),
            },
            ExplorerDialog::DirtyExit => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.pending_editor_home = false;
                    None
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.pending_editor_home = false;
                    None
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if let Some(editor) = self.editor_session.as_mut() {
                        editor.accept_current_text();
                    }
                    self.complete_dirty_editor_exit();
                    None
                }
                KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => {
                    self.save_active_editor();
                    if matches!(
                        self.explorer_dialog.current(),
                        Some(ExplorerDialog::SaveConflict)
                    ) {
                        Some(ExplorerDialog::SaveConflict)
                    } else if self
                        .editor_session
                        .as_ref()
                        .is_some_and(|editor| !editor.is_dirty())
                    {
                        self.complete_dirty_editor_exit();
                        None
                    } else {
                        Some(ExplorerDialog::DirtyExit)
                    }
                }
                _ => Some(ExplorerDialog::DirtyExit),
            },
            ExplorerDialog::DirtySwitch { path } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.pending_editor_path = None;
                    None
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.pending_editor_path = None;
                    None
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.complete_pending_editor_switch(true);
                    None
                }
                KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => {
                    self.save_active_editor();
                    if matches!(
                        self.explorer_dialog.current(),
                        Some(ExplorerDialog::SaveConflict)
                    ) {
                        Some(ExplorerDialog::SaveConflict)
                    } else if self
                        .editor_session
                        .as_ref()
                        .is_some_and(|editor| !editor.is_dirty())
                    {
                        self.complete_pending_editor_switch(false);
                        None
                    } else {
                        Some(ExplorerDialog::DirtySwitch { path })
                    }
                }
                _ => Some(ExplorerDialog::DirtySwitch { path }),
            },
            ExplorerDialog::SaveConflict => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.pending_editor_path = None;
                    self.pending_editor_home = false;
                    None
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.reload_active_editor_from_disk();
                    if self.pending_editor_path.is_some()
                        && self
                            .editor_session
                            .as_ref()
                            .is_some_and(|editor| !editor.is_dirty())
                    {
                        self.complete_pending_editor_switch(false);
                    } else if self
                        .editor_session
                        .as_ref()
                        .is_some_and(|editor| !editor.is_dirty())
                        && self.pending_editor_home
                    {
                        self.complete_dirty_editor_exit();
                    }
                    None
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.save_active_editor_with_force(true);
                    if self
                        .editor_session
                        .as_ref()
                        .is_some_and(|editor| !editor.is_dirty())
                    {
                        if self.pending_editor_path.is_some() {
                            self.complete_pending_editor_switch(false);
                        } else if self.pending_editor_home {
                            self.complete_dirty_editor_exit();
                        }
                        None
                    } else {
                        Some(ExplorerDialog::SaveConflict)
                    }
                }
                _ => Some(ExplorerDialog::SaveConflict),
            },
        };
        self.explorer_dialog.replace(next);
        true
    }

    /// Insert bracketed-paste text into the current explicit text owner.
    pub(super) fn handle_paste(&mut self, data: &str) {
        if let Some(ExplorerDialog::Name { input, error, .. }) = self.explorer_dialog.current_mut()
        {
            for ch in data.chars().filter(|ch| !ch.is_control()) {
                input.push(ch);
            }
            *error = None;
            return;
        }
        if self.focus.block() == FocusBlock::BottomPanel {
            if let Some(terminal) = self.interactive_terminal.as_mut() {
                match terminal.consume_input(data.as_bytes()) {
                    Ok(true) => self.toggle_bottom_panel(),
                    Ok(false) => {}
                    Err(error) => self.set_feedback(
                        FeedbackSeverity::Error,
                        format!("terminal paste failed: {error}"),
                    ),
                }
                return;
            }
        }
        if let Some(ref mut ov) = self.overlay {
            let _ = handle_overlay_key(ov, OverlayKey::Paste(data.to_string()));
            return;
        }
        self.normalize_focus();
        match self.focus.mode() {
            FocusMode::Navigation if self.focus.block() == FocusBlock::Composer => {
                self.input.history_browse = false;
                self.input.insert_paste(data);
                self.clamp_slash_suggest();
            }
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                for ch in data.chars().filter(|ch| !ch.is_control()) {
                    self.source_viewer.append_search_char(ch);
                }
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                for ch in data.chars().filter(|ch| ch.is_ascii_digit()) {
                    self.source_viewer.append_jump_char(ch);
                }
            }
            FocusMode::Navigation => {}
        }
    }

    /// Records a submitted composer line in both local Up/Down history and
    /// the session journal, so the two never drift apart. Journal recording
    /// is best-effort (`let _ =`) — a persistence hiccup here must never
    /// block sending the message — and gated by the same `should_store`
    /// policy `history.push` already applies internally, keeping the two
    /// stores policy-consistent (no secret-looking or empty lines in
    /// either).
    async fn record_submitted_line(&mut self, line: &str) {
        self.history.push(line);
        if InputHistory::should_store(line) {
            let _ = self.session.record_composer_line(line).await;
        }
    }

    pub(super) async fn submit_composer_message(&mut self) -> Result<(), TuiError> {
        // A pending approval normally rejects composer input as stale
        // (`classify_input`). The one exception is a note the operator was
        // explicitly asked for by picking "Don't run, and say why" — it is an
        // answer to the prompt, not a new instruction, so it is taken before
        // the line is classified at all.
        if self.approval_denial_note_pending() {
            let note = self.input.take();
            self.deny_pending_approval_with_note(&note).await?;
            return Ok(());
        }
        let suggestions = self.slash_suggestions();
        if self.input.text.starts_with('/')
            && !suggestions.is_empty()
            && !self.input.text.contains(' ')
        {
            let idx = self.slash_suggestions.selected.min(suggestions.len() - 1);
            let cmd = suggestions[idx].cmd.clone();
            let cur = self.input.text.trim();
            let line = if cur == cmd.as_str() || cur.starts_with(&(cmd.clone() + " ")) {
                self.input.take()
            } else {
                self.input.set_text(cmd);
                self.input.take()
            };
            if !line.is_empty() {
                self.record_submitted_line(&line).await;
                self.slash_suggestions.selected = 0;
                self.notice_state.items.clear();
                self.input.history_browse = false;
                self.dispatch_line(&line).await?;
            }
            return Ok(());
        }

        let line = self.input.take();
        if line.trim().is_empty() && !self.attachment.has_images() {
            if !self.busy_state.is_active() && !self.session.queue().is_empty() {
                self.dequeue_and_send_next().await;
            }
            return Ok(());
        }

        // Slash commands always dispatch immediately regardless of lifecycle.
        if line.trim_start().starts_with('/') {
            self.record_submitted_line(&line).await;
            self.slash_suggestions.selected = 0;
            self.notice_state.items.clear();
            self.input.history_browse = false;
            self.dispatch_line(&line).await?;
            return Ok(());
        }

        if line.starts_with('!') {
            let command = line.strip_prefix('!').unwrap_or_default();
            if command.is_empty() {
                self.open_bottom_panel();
                if self.interactive_terminal.is_none() {
                    self.input.set_text(line);
                } else {
                    self.record_submitted_line(&line).await;
                }
                return Ok(());
            }
            self.open_bottom_panel();
            let Some(terminal) = self.interactive_terminal.as_mut() else {
                self.input.set_text(line);
                return Ok(());
            };
            match terminal.start_command(command) {
                Ok(()) => {
                    self.record_submitted_line(&line).await;
                    self.slash_suggestions.selected = 0;
                    self.notice_state.items.clear();
                    self.input.history_browse = false;
                }
                Err(error) => {
                    self.input.set_text(line);
                    let severity = if error.kind() == std::io::ErrorKind::WouldBlock {
                        FeedbackSeverity::Warn
                    } else {
                        FeedbackSeverity::Error
                    };
                    self.set_feedback(severity, format!("terminal command failed: {error}"));
                }
            }
            return Ok(());
        }

        if self.selected_task_id != self.session.session_id {
            if let Some(handle) = self
                .supervisor
                .as_ref()
                .map(|supervisor| supervisor.handle.clone())
            {
                self.record_submitted_line(&line).await;
                handle
                    .command(forge_session::SupervisorCommand::SubmitPrompt {
                        session_id: self.selected_task_id,
                        text: line,
                    })
                    .await
                    .map_err(|error| TuiError::Other(error.to_string()))?;
                self.set_feedback(FeedbackSeverity::Info, "prompt queued for selected task");
                self.push_toast("sent to selected task");
                return Ok(());
            }
        }

        let route =
            input_route::classify_input(&self.session.active_task, self.overlay.is_some(), &line);
        let consumed = !matches!(route, input_route::InputRoute::RejectStaleResponse);
        if consumed {
            self.record_submitted_line(&line).await;
            self.slash_suggestions.selected = 0;
            self.notice_state.items.clear();
            self.input.history_browse = false;
        }
        match route {
            input_route::InputRoute::StartNewTask => {
                self.dispatch_line(&line).await?;
            }
            input_route::InputRoute::QueueFutureTask => {
                self.enqueue_user_message(line).await;
            }
            input_route::InputRoute::AnswerClarification => {
                if self.session.pending_question().is_some() {
                    self.apply_clarification_text(&line);
                } else {
                    self.set_feedback(FeedbackSeverity::Warn, "nothing pending to answer");
                }
            }
            input_route::InputRoute::ResolveSelection => {
                self.set_feedback(FeedbackSeverity::Warn, "nothing pending to answer");
            }
            input_route::InputRoute::RejectStaleResponse => {
                // Keep the operator's text so they can edit it into a valid
                // message instead of retyping from scratch.
                self.input.set_text(line);
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "resolve the pending approval first — ↑↓  Enter  Esc don't run",
                );
            }
        }
        Ok(())
    }

    fn handle_editor_key(&mut self, key: event::KeyEvent) -> bool {
        if !self.current_workspace_is_file() {
            return false;
        }

        if let Some(editor) = self.editor_session.as_mut() {
            if key.code == KeyCode::Char(':')
                && key.modifiers.is_empty()
                && editor.mode() == edtui::EditorMode::Normal
            {
                self.editor_command = Some(String::new());
                self.status_state.message = ":".into();
                return true;
            }
            let _ = editor.handle_key(key);
            self.source_viewer.current_line = editor.cursor_row();
            return true;
        }

        let height = self.editor_viewport.height.saturating_sub(2) as usize;
        // Navigation shortcuts are plain keys so modified combinations can
        // continue to control contextual workspace and chrome commands.
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(-(height as isize), height);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(height as isize, height);
                true
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.source_viewer.move_to_start_of_line();
                true
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.source_viewer.move_to_end_of_line();
                true
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
                true
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.source_viewer.move_to_first_line();
                true
            }
            KeyCode::Char('G')
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.source_viewer.move_to_last_line();
                true
            }
            KeyCode::Char('i') if key.modifiers.is_empty() => {
                self.source_viewer.enter_insert_mode();
                true
            }
            KeyCode::Esc
                if key.modifiers.is_empty()
                    && self.source_viewer.mode == crate::source_viewer::ViewerMode::Insert =>
            {
                self.source_viewer.enter_normal_mode();
                true
            }
            // A binary or invalid-UTF-8 preview is read-only, but it still
            // owns the workspace keyboard focus. Consume unsupported keys so
            // they cannot fall through to the chat composer.
            _ => self.editor_session.is_none(),
        }
    }

    async fn handle_sidebar_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if let Some(command) = self.semantic_command_for_sidebar_key(key) {
            Box::pin(self.execute_semantic_command(command)).await
        } else {
            Ok(false)
        }
    }

    async fn handle_bottom_panel_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if !self.bottom_panel.open {
            return Ok(false);
        }
        if let Some(bytes) = terminal_key_bytes(key) {
            if let Some(terminal) = self.interactive_terminal.as_mut() {
                match terminal.consume_input(&bytes) {
                    Ok(true) => {
                        self.toggle_bottom_panel();
                        return Ok(true);
                    }
                    Ok(false) => return Ok(true),
                    Err(error) => {
                        self.set_feedback(
                            FeedbackSeverity::Error,
                            format!("terminal input failed: {error}"),
                        );
                        return Ok(true);
                    }
                }
            }
        }
        if let Some(command) = self.semantic_command_for_bottom_panel_key(key) {
            Box::pin(self.execute_semantic_command(command)).await
        } else {
            Ok(false)
        }
    }

    fn handle_search_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_search();
                self.focus.set_navigation(FocusBlock::Workspace);
                self.normalize_focus();
                true
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.source_viewer.prev_match();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.next_match();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_search();
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.source_viewer.append_search_char(c);
                true
            }
            _ => true,
        }
    }

    fn handle_jump_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_jump();
                self.focus.set_navigation(FocusBlock::Workspace);
                self.normalize_focus();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.commit_jump();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_jump();
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.source_viewer.append_jump_char(c);
                true
            }
            _ => true,
        }
    }

    async fn handle_file_explorer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if self.workspace_files.explorer.search_focused {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U'))
            {
                self.workspace_files.explorer.clear_search();
                return Ok(true);
            }
            if key.modifiers.is_empty() && matches!(key.code, KeyCode::Backspace) {
                let mut query = self.workspace_files.explorer.search_query.clone();
                query.pop();
                self.workspace_files.explorer.set_search_query(query);
                return Ok(true);
            }
            if !key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
            {
                if let KeyCode::Char(c) = key.code {
                    if !c.is_control() {
                        let mut query = self.workspace_files.explorer.search_query.clone();
                        query.push(c);
                        self.workspace_files.explorer.set_search_query(query);
                        return Ok(true);
                    }
                }
            }
        }
        let Some(command) = self.semantic_command_for_file_key(key) else {
            return Ok(false);
        };
        Box::pin(self.execute_semantic_command(command)).await
    }

    async fn handle_workspace_navigation_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if self.diff_view_is_open() {
            return Ok(self.handle_diff_key(key));
        }
        if let Some(command) = self.semantic_command_for_workspace_key(key) {
            return Box::pin(self.execute_semantic_command(command)).await;
        }
        if self.current_workspace_is_file() {
            if let Some(command) = self.semantic_command_for_global_key(key) {
                return Box::pin(self.execute_semantic_command(command)).await;
            }
            return Ok(self.handle_editor_key(key));
        }
        Ok(false)
    }

    async fn handle_active_block_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        match self.focus.block() {
            FocusBlock::TaskStrip => self.handle_task_strip_key(key).await,
            FocusBlock::Search | FocusBlock::Files => self.handle_file_explorer_key(key).await,
            FocusBlock::Workspace => self.handle_workspace_navigation_key(key).await,
            FocusBlock::Sidebar => self.handle_sidebar_key(key).await,
            FocusBlock::Composer => Ok(false),
            FocusBlock::Footer => self.handle_footer_key(key).await,
            FocusBlock::BottomPanel => self.handle_bottom_panel_key(key).await,
            // Menu keys (↑↓ Enter Esc) are consumed by `handle_approval_menu_key`
            // before routing; everything else is ignored here.
            FocusBlock::Approval => Ok(false),
        }
    }

    async fn handle_global_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(command) = self.semantic_command_for_global_key(key) else {
            return Ok(false);
        };
        Box::pin(self.execute_semantic_command(command)).await
    }

    fn printable_chat_char(key: event::KeyEvent) -> Option<char> {
        let non_shift_modifiers = key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE);
        match key.code {
            KeyCode::Char(c) if non_shift_modifiers.is_empty() && !c.is_control() => Some(c),
            _ => None,
        }
    }

    async fn type_to_compose(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        // While an approval is pending the composer is not the answer input;
        // typing must neither move focus off the approval card nor accumulate
        // text behind the waiting state.
        if self.session.pending_hitl().is_some() {
            return Ok(false);
        }
        // Control surfaces own their own keystrokes. Falling through to the
        // composer from here silently *moves focus* and inserts, so a command
        // typed at the footer — or at a terminal whose PTY has exited — turns
        // into a chat draft, and the Enter that was meant to run it sends it to
        // the model instead. Refuse the hijack: keep the focus the UI is
        // showing and drop the key, rather than doing something plausible in
        // the wrong pane.
        //
        // Navigational blocks (Files/Search/Workspace/Sidebar) deliberately
        // keep type-to-chat: there the keystroke has no local meaning, so
        // starting a message is the only thing it could have meant.
        if matches!(
            self.focus.block(),
            FocusBlock::Footer | FocusBlock::BottomPanel
        ) {
            return Ok(false);
        }
        let Some(c) = Self::printable_chat_char(key) else {
            return Ok(false);
        };
        Box::pin(self.execute_semantic_command(SemanticCommand::FocusComposer)).await?;
        self.input.history_browse = false;
        self.input.insert(c);
        self.clamp_slash_suggest();
        Ok(true)
    }

    /// Moves the composer cursor one visual row up/down within the live
    /// draft. Returns `false` when already on the first/last visual row (or
    /// the composer's width isn't known yet), so the caller falls through
    /// to history recall.
    fn move_composer_cursor(&mut self, up: bool) -> bool {
        let Some(area) = self.composer_area else {
            return false;
        };
        let attachment_label = {
            let file = self.attachment.file().map(|a| a.label());
            let images = self.pending_image_label();
            match (file, images) {
                (Some(file), Some(images)) => Some(format!("{file} · {images}")),
                (Some(file), None) => Some(file),
                (None, Some(images)) => Some(images),
                (None, None) => None,
            }
        };
        let Some(width) = composer_text_area_width(&self.input, area, attachment_label.as_deref())
        else {
            return false;
        };
        if up {
            self.input.move_cursor_up(width as usize)
        } else {
            self.input.move_cursor_down(width as usize)
        }
    }

    async fn handle_chat_composer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let input_was_empty = self.input.text.is_empty();
        if let Some(command) = self.semantic_command_for_composer_key(key) {
            let consumed = Box::pin(self.execute_semantic_command(command)).await?;
            if input_was_empty && !self.input.text.is_empty() {
                self.conversation_view.splash_dismissed = true;
            }
            return Ok(consumed);
        }
        let consumed = match key.code {
            KeyCode::Tab => {
                if self.input.text.starts_with('/') && !self.slash_suggestions().is_empty() {
                    self.complete_slash_suggestion();
                    true
                } else {
                    false
                }
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    self.slash_suggestions.selected =
                        (self.slash_suggestions.selected + suggestions.len() - 1)
                            % suggestions.len();
                } else if !self.history.browsing() && self.move_composer_cursor(true) {
                    // Moved within a multi-line draft — history untouched.
                } else if let Some(text) = self.history.up(&self.input.text) {
                    self.apply_history_text(text);
                }
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    self.slash_suggestions.selected =
                        (self.slash_suggestions.selected + 1) % suggestions.len();
                } else if !self.history.browsing() && self.move_composer_cursor(false) {
                    // Moved within a multi-line draft — history untouched.
                } else if let Some(text) = self.history.down() {
                    self.apply_history_text(text);
                }
                true
            }
            KeyCode::Char(c) if key.modifiers.is_empty() && !c.is_control() => {
                self.input.history_browse = false;
                self.input.insert(c);
                self.clamp_slash_suggest();
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                if self.input.text.is_empty() && self.dismiss_last_image_chip() {
                    self.set_feedback(FeedbackSeverity::Info, "image dismissed");
                } else {
                    self.input.backspace();
                    self.clamp_slash_suggest();
                }
                true
            }
            // Standard readline "clear line" — previously unbound, so the
            // only way to clear a long/garbled composer was repeated
            // Backspace.
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.slash_suggestions.selected = 0;
                true
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                self.input.move_left();
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.input.move_right();
                true
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => true,
            KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => true,
            KeyCode::PageUp if key.modifiers.is_empty() => {
                let page = self.conversation_page_rows();
                self.scroll_conversation_up(page);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                let page = self.conversation_page_rows();
                self.scroll_conversation_down(page);
                true
            }
            _ => false,
        };
        if input_was_empty && !self.input.text.is_empty() {
            self.conversation_view.splash_dismissed = true;
        }
        Ok(consumed)
    }

    /// Navigate the footer's two controls (which-LLM, effort) while
    /// `FocusBlock::Footer` is active — an ordinary Tab stop now, not a
    /// separate `F3` side-channel. 0 = which-LLM, 1 = effort;
    /// `composer_chip_focus`'s lifecycle is managed by
    /// `focus.rs::normalize_focus`. Enter opens the relevant picker.
    async fn handle_footer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        const N: usize = 2;
        let idx = self.composer_chip_focus.unwrap_or(0).min(N - 1);
        match key.code {
            KeyCode::Left if key.modifiers.is_empty() => {
                self.composer_chip_focus = Some((idx + N - 1) % N);
                Ok(true)
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.composer_chip_focus = Some((idx + 1) % N);
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let focus = if idx == 0 {
                    FooterFocus::Llm
                } else {
                    FooterFocus::Effort
                };
                self.activate_composer_chip(focus).await?;
                Ok(true)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                Box::pin(self.execute_semantic_command(SemanticCommand::CancelCurrentInteraction))
                    .await
            }
            _ => Ok(false),
        }
    }

    async fn activate_composer_chip(&mut self, focus: FooterFocus) -> Result<(), TuiError> {
        match focus {
            // Which-LLM merges the old Connect+Model chips into one control:
            // opens the full connect flow when nothing's connected yet,
            // otherwise jumps straight to picking a model.
            FooterFocus::Llm => {
                if self.is_provider_connected() {
                    Box::pin(
                        self.execute_semantic_command(SemanticCommand::OpenModelControl(
                            ConnectModelColumn::Models,
                        )),
                    )
                    .await?;
                } else {
                    self.open_connect_picker();
                }
            }
            FooterFocus::Effort => {
                Box::pin(
                    self.execute_semantic_command(SemanticCommand::OpenModelControl(
                        ConnectModelColumn::Effort,
                    )),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) fn scroll_conversation_up(&mut self, amount: u16) {
        self.conversation_view.follow = false;
        self.conversation_view.scroll = self.conversation_view.scroll.saturating_add(amount);
    }

    pub(super) fn scroll_conversation_down(&mut self, amount: u16) {
        self.conversation_view.scroll = self.conversation_view.scroll.saturating_sub(amount);
        if self.conversation_view.scroll == 0 {
            self.conversation_view.follow = true;
        }
    }

    pub async fn handle_key(&mut self, key: event::KeyEvent) -> Result<(), TuiError> {
        // Allow arrow-key auto-repeat for overlays (and other selection UIs).
        if key.kind != KeyEventKind::Press {
            let allow_repeat = matches!(
                key.kind,
                KeyEventKind::Repeat
                    if matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                    )
            );
            if !allow_repeat {
                return Ok(());
            }
        }

        // A Vim command result remains visible until the next keypress; that
        // keypress also continues with its ordinary action.
        self.editor_message = None;

        if self.explorer_dialog.is_open() {
            self.handle_explorer_dialog_key(key);
            return Ok(());
        }

        if self.context_menu.is_some() {
            self.handle_context_menu_key(key);
            return Ok(());
        }

        if self.session.pending_question().is_some() && self.handle_question_menu_key(key).await? {
            return Ok(());
        }

        if self.session.pending_hitl().is_some() && self.handle_approval_menu_key(key).await? {
            return Ok(());
        }

        if matches!(self.overlay, Some(Overlay::StatusReport { .. })) {
            let ok = map_key(key);
            match ok {
                Key::Enter => {
                    self.overlay = None;
                    if self.input.text.trim().is_empty() {
                        return Ok(());
                    }
                }
                Key::Char(c) => {
                    self.overlay = None;
                    if !c.is_control() && !c.is_whitespace() {
                        self.input.history_browse = false;
                        self.input.insert(c);
                        self.clamp_slash_suggest();
                        return Ok(());
                    }
                }
                _ => {}
            }
        }

        if let Some(ref mut ov) = self.overlay {
            let ok = map_key(key);
            let action = handle_overlay_key(ov, ok);
            self.apply_overlay_action(action).await?;
            return Ok(());
        }

        self.normalize_focus();
        if self.editor_command.is_some() {
            self.handle_editor_command_key(key).await?;
            return Ok(());
        }
        match self.focus.mode() {
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                self.handle_search_key(key);
                return Ok(());
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                self.handle_jump_key(key);
                return Ok(());
            }
            FocusMode::Navigation if self.focus.block() == FocusBlock::Composer => {
                if self.handle_chat_composer_key(key).await? {
                    return Ok(());
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    Box::pin(self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    }))
                    .await?;
                    return Ok(());
                }
            }
            FocusMode::Navigation => {
                if self.focus.block() == FocusBlock::BottomPanel
                    && key.code == KeyCode::Tab
                    && key.modifiers.is_empty()
                    && self.handle_active_block_key(key).await?
                {
                    return Ok(());
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    Box::pin(self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    }))
                    .await?;
                    return Ok(());
                }
                if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
                    Box::pin(
                        self.execute_semantic_command(SemanticCommand::CycleFocus {
                            forward: false,
                        }),
                    )
                    .await?;
                    return Ok(());
                }
                if self.handle_active_block_key(key).await? {
                    self.source_viewer.clear_notice();
                    return Ok(());
                }
            }
        }

        if self.handle_global_key(key).await? {
            return Ok(());
        }
        let _ = self.type_to_compose(key).await?;
        Ok(())
    }
}

fn parse_editor_substitute(command: &str) -> Option<(bool, String, String, bool)> {
    let (all_lines, body) = command
        .strip_prefix("%s")
        .map(|body| (true, body))
        .or_else(|| command.strip_prefix('s').map(|body| (false, body)))?;
    let delimiter = body.chars().next()?;
    let body = &body[delimiter.len_utf8()..];
    let (pattern, body) = body.split_once(delimiter)?;
    let (replacement, flags) = body.split_once(delimiter)?;
    if flags.chars().any(|flag| flag != 'g') {
        return None;
    }
    Some((
        all_lines,
        pattern.to_owned(),
        replacement.to_owned(),
        flags == "g",
    ))
}

fn terminal_key_bytes(key: event::KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = key.code {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c as u8 - b'a' + 1]);
            }
        }
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c) = key.code {
            return Some(vec![0x1b, c as u8]);
        }
    }
    match key.code {
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            Some(c.to_string().into_bytes())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Esc => None,
        _ => None,
    }
}

fn map_key(key: event::KeyEvent) -> OverlayKey {
    match key.code {
        KeyCode::Esc => OverlayKey::Esc,
        KeyCode::Enter => OverlayKey::Enter,
        KeyCode::Tab => OverlayKey::Tab,
        KeyCode::BackTab => OverlayKey::BackTab,
        KeyCode::Up => OverlayKey::Up,
        KeyCode::Down => OverlayKey::Down,
        KeyCode::Left => OverlayKey::Left,
        KeyCode::Right => OverlayKey::Right,
        KeyCode::Backspace => OverlayKey::Backspace,
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::{AgentSession, LoopConfig};
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
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
                file_icons: forge_config::FileIconMode::Unicode,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );
        (dir, app)
    }

    /// Focus the composer in navigation mode, which is what `handle_key` needs
    /// before it will route a press to the chat composer.
    fn focus_composer(app: &mut TuiApp) {
        app.focus.set_navigation(app.focus.block());
        app.focus.set_navigation(FocusBlock::Composer);
    }

    fn press(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press_with(code: KeyCode, modifiers: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent::new(code, modifiers)
    }

    #[test]
    fn printable_chat_char_accepts_plain_and_shifted_characters() {
        assert_eq!(
            TuiApp::printable_chat_char(press(KeyCode::Char('a'))),
            Some('a')
        );
        // SHIFT is the one modifier that still yields a printable character.
        assert_eq!(
            TuiApp::printable_chat_char(press_with(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some('A')
        );
        assert_eq!(
            TuiApp::printable_chat_char(press(KeyCode::Char(' '))),
            Some(' ')
        );
        assert_eq!(
            TuiApp::printable_chat_char(press(KeyCode::Char('é'))),
            Some('é')
        );
    }

    #[test]
    fn printable_chat_char_rejects_control_combinations_and_non_chars() {
        // Any modifier other than SHIFT means the press is a chord, not text.
        for m in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            assert_eq!(
                TuiApp::printable_chat_char(press_with(KeyCode::Char('a'), m)),
                None,
                "modifier {m:?} should not produce text"
            );
        }
        // Control characters and non-character keys are never text.
        assert_eq!(
            TuiApp::printable_chat_char(press(KeyCode::Char('\u{1}'))),
            None
        );
        assert_eq!(TuiApp::printable_chat_char(press(KeyCode::Enter)), None);
        assert_eq!(TuiApp::printable_chat_char(press(KeyCode::Up)), None);
    }

    #[test]
    fn terminal_key_bytes_preserve_shell_editing_controls() {
        assert_eq!(
            terminal_key_bytes(press(KeyCode::Char('x'))),
            Some(b"x".to_vec())
        );
        assert_eq!(
            terminal_key_bytes(press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(terminal_key_bytes(press(KeyCode::Enter)), Some(vec![b'\r']));
        assert_eq!(
            terminal_key_bytes(press(KeyCode::Up)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            terminal_key_bytes(press(KeyCode::Backspace)),
            Some(vec![0x7f])
        );
    }

    #[tokio::test]
    async fn release_events_are_ignored_but_arrow_repeats_are_honoured() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);

        // A key release must not type anything.
        let mut release = press(KeyCode::Char('x'));
        release.kind = KeyEventKind::Release;
        app.handle_key(release).await.unwrap();
        assert_eq!(app.input.text, "");

        // Auto-repeat of a printable key is also dropped...
        let mut repeat_char = press(KeyCode::Char('x'));
        repeat_char.kind = KeyEventKind::Repeat;
        app.handle_key(repeat_char).await.unwrap();
        assert_eq!(app.input.text, "");

        // ...but arrow auto-repeat is allowed through, so held arrows still
        // scroll and move selections. It reaches the composer and is consumed
        // without inserting text.
        app.input.text = "ab".into();
        app.input.cursor = 2;
        let mut repeat_left = press(KeyCode::Left);
        repeat_left.kind = KeyEventKind::Repeat;
        app.handle_key(repeat_left).await.unwrap();
        assert_eq!(
            app.input.cursor, 1,
            "held Left should keep moving the cursor"
        );
    }

    #[tokio::test]
    async fn typing_into_the_composer_inserts_and_dismisses_the_splash() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        assert!(!app.conversation_view.splash_dismissed);

        for c in "hi".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }

        assert_eq!(app.input.text, "hi");
        assert!(
            app.conversation_view.splash_dismissed,
            "first typed character should dismiss the splash"
        );
    }

    #[tokio::test]
    async fn composer_editing_keys_move_and_delete() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        for c in "abc".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        assert_eq!(app.input.text, "abc");
        assert_eq!(app.input.cursor, 3);

        app.handle_key(press(KeyCode::Backspace)).await.unwrap();
        assert_eq!(app.input.text, "ab");

        app.handle_key(press(KeyCode::Left)).await.unwrap();
        assert_eq!(app.input.cursor, 1);
        app.handle_key(press(KeyCode::Right)).await.unwrap();
        assert_eq!(app.input.cursor, 2);
    }

    #[tokio::test]
    async fn composer_keeps_accepting_text_after_wrapping_to_a_second_row() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        // The sidebar composer is 30 columns wide at 80 terminal columns.
        // This phrase wraps before "fifth", then keeps typing through the
        // second visual row without an explicit newline.
        for c in "first second third fourth fifth sixth".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        for c in " seventh eighth ninth".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }

        assert_eq!(
            app.input.text,
            "first second third fourth fifth sixth seventh eighth ninth"
        );
        assert_eq!(app.focus.block(), FocusBlock::Composer);
    }

    #[tokio::test]
    async fn shifted_arrows_are_swallowed_without_moving_the_cursor() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.input.text = "abc".into();
        app.input.cursor = 2;

        // Shift+arrow is reserved for selection, which the composer does not
        // implement; it must be consumed rather than falling through to
        // focus cycling or global handling.
        app.handle_key(press_with(KeyCode::Left, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.input.cursor, 2);
        app.handle_key(press_with(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.input.cursor, 2);
        assert_eq!(app.input.text, "abc");
    }

    #[tokio::test]
    async fn status_report_overlay_closes_on_enter() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.overlay = Some(Overlay::StatusReport {
            title: "Status".into(),
            rows: vec![crate::overlays::StatusRow::field("Status", "all good")],
        });

        app.handle_key(press(KeyCode::Enter)).await.unwrap();

        assert!(app.overlay.is_none(), "Enter should dismiss the report");
        assert_eq!(app.input.text, "", "Enter must not type into the composer");
    }

    #[tokio::test]
    async fn status_report_overlay_closes_and_keeps_the_typed_character() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.overlay = Some(Overlay::StatusReport {
            title: "Status".into(),
            rows: vec![crate::overlays::StatusRow::field("Status", "all good")],
        });

        // Typing over the report dismisses it and keeps the keystroke, so the
        // character is not silently swallowed by the dismissal.
        app.handle_key(press(KeyCode::Char('h'))).await.unwrap();

        assert!(app.overlay.is_none());
        assert_eq!(app.input.text, "h");
    }

    #[tokio::test]
    async fn typing_with_a_non_composer_focus_still_reaches_the_composer() {
        let (_dir, mut app) = app().await;
        // Focus something other than the composer; a printable key should fall
        // through to `type_to_compose`, which refocuses and inserts.
        app.focus.set_navigation(FocusBlock::Workspace);

        app.handle_key(press(KeyCode::Char('z'))).await.unwrap();

        assert_eq!(app.input.text, "z");
        assert_eq!(app.focus.block(), FocusBlock::Composer);
    }

    #[tokio::test]
    async fn bang_submission_opens_terminal_and_records_history() {
        if !crate::interactive_terminal::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.input.set_text("!printf bang");

        app.submit_composer_message().await.unwrap();

        assert!(
            app.interactive_terminal.is_some(),
            "{}",
            app.status_state.message
        );
        assert!(app.bottom_panel.open);
        assert_eq!(app.input.text, "");
        assert_eq!(app.history.entries(), &["!printf bang".to_string()]);
    }

    #[tokio::test]
    async fn bang_submission_rejects_a_busy_embedded_terminal() {
        if !crate::interactive_terminal::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.input.set_text("!sleep 1");
        app.submit_composer_message().await.unwrap();

        app.input.set_text("!printf later");
        app.submit_composer_message().await.unwrap();

        assert_eq!(app.input.text, "!printf later");
        assert!(app.status_state.message.contains("terminal command failed"));
        assert_eq!(app.history.entries(), &["!sleep 1".to_string()]);
    }

    #[tokio::test]
    async fn lone_bang_opens_terminal_without_running_a_command() {
        if !crate::interactive_terminal::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.input.set_text("!");

        app.submit_composer_message().await.unwrap();

        assert!(app.bottom_panel.open);
        assert!(app.interactive_terminal.is_some());
        assert_eq!(app.input.text, "");
        assert_eq!(app.history.entries(), &["!".to_string()]);
    }

    #[tokio::test]
    async fn whitespace_before_bang_remains_a_chat_message() {
        let (_dir, mut app) = app().await;
        focus_composer(&mut app);
        app.input.set_text(" !printf hi");

        app.submit_composer_message().await.unwrap();

        assert!(app.interactive_terminal.is_none());
        assert_eq!(app.history.entries(), &["!printf hi".to_string()]);
    }
}
