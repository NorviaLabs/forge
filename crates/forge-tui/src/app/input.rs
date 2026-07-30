//! Keyboard input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `handle_key` is the entry point; the
//! `handle_*_key` family dispatches per focus target. Key-to-command mapping
//! lives in `app/commands.rs`; this module decides who handles a press.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
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
        };
        self.explorer_dialog = next;
        true
    }

    /// Insert bracketed-paste text into the current explicit text owner.
    pub(super) fn handle_paste(&mut self, data: &str) {
        if let Some(ExplorerDialog::Name { input, error, .. }) = self.explorer_dialog.as_mut() {
            for ch in data.chars().filter(|ch| !ch.is_control()) {
                input.push(ch);
            }
            *error = None;
            return;
        }
        if let Some(ref mut ov) = self.overlay {
            let _ = handle_overlay_key(ov, OverlayKey::Paste(data.to_string()));
            return;
        }
        self.normalize_focus();
        match self.focus.mode {
            FocusMode::Navigation if self.focus.block == FocusBlock::Composer => {
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

    pub(super) async fn submit_composer_message(&mut self) -> Result<(), TuiError> {
        let suggestions = self.slash_suggestions();
        if self.input.text.starts_with('/')
            && !suggestions.is_empty()
            && !self.input.text.contains(' ')
        {
            let idx = self.slash_suggest_idx.min(suggestions.len() - 1);
            let cmd = suggestions[idx].cmd.clone();
            let cur = self.input.text.trim();
            let line = if cur == cmd.as_str() || cur.starts_with(&(cmd.clone() + " ")) {
                self.input.take()
            } else {
                self.input.set_text(cmd);
                self.input.take()
            };
            if !line.is_empty() {
                self.history.push(&line);
                self.slash_suggest_idx = 0;
                self.notices.clear();
                self.input.history_browse = false;
                self.dispatch_line(&line).await?;
            }
            return Ok(());
        }

        let line = self.input.take();
        if line.trim().is_empty() {
            if !self.busy && !self.message_queue.is_empty() {
                self.dequeue_and_send_next();
            }
            return Ok(());
        }

        self.history.push(&line);
        self.slash_suggest_idx = 0;
        self.notices.clear();
        self.input.history_browse = false;
        if self.busy && !line.trim_start().starts_with('/') {
            self.enqueue_user_message(line);
        } else {
            self.dispatch_line(&line).await?;
        }
        Ok(())
    }

    fn handle_editor_key(&mut self, key: event::KeyEvent) -> bool {
        if !self.current_workspace_is_file() {
            return false;
        }

        let height = self.last_editor_height.saturating_sub(2) as usize;
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
            KeyCode::Left if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
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
            _ => false,
        }
    }

    async fn handle_bottom_panel_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if !self.bottom_panel.open {
            return Ok(false);
        }
        match key.code {
            KeyCode::Char(c)
                if self.bottom_panel.active == BottomPanelTab::Run
                    && self.run.editing
                    && key.modifiers.is_empty() =>
            {
                if self.run.editing_directory {
                    let mut text = self.run.draft.working_directory.display().to_string();
                    text.push(c);
                    self.run.draft.working_directory = PathBuf::from(text);
                } else {
                    self.run.draft.command_input.push(c);
                }
                Ok(true)
            }
            KeyCode::Backspace
                if self.bottom_panel.active == BottomPanelTab::Run && self.run.editing =>
            {
                if self.run.editing_directory {
                    let mut text = self.run.draft.working_directory.display().to_string();
                    text.pop();
                    self.run.draft.working_directory = PathBuf::from(text);
                } else {
                    self.run.draft.command_input.pop();
                }
                Ok(true)
            }
            _ => {
                if let Some(command) = self.semantic_command_for_bottom_panel_key(key) {
                    self.execute_semantic_command(command).await
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn handle_search_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_search();
                self.focus.mode = FocusMode::Navigation;
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
                self.focus.mode = FocusMode::Navigation;
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
        let Some(command) = self.semantic_command_for_file_key(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(command).await
    }

    async fn handle_workspace_navigation_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if let Some(command) = self.semantic_command_for_workspace_key(key) {
            return self.execute_semantic_command(command).await;
        }
        if self.current_workspace_is_file() {
            return Ok(self.handle_editor_key(key));
        }
        Ok(false)
    }

    async fn handle_active_block_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        match self.focus.block {
            FocusBlock::Files => self.handle_file_explorer_key(key).await,
            FocusBlock::Workspace => self.handle_workspace_navigation_key(key).await,
            FocusBlock::Composer => Ok(false),
            FocusBlock::Inspector => {
                if let Some(command) = self.semantic_command_for_inspector_key(key) {
                    self.execute_semantic_command(command).await
                } else {
                    Ok(false)
                }
            }
            FocusBlock::BottomPanel => self.handle_bottom_panel_key(key).await,
        }
    }

    async fn handle_global_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(command) = self.semantic_command_for_global_key(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(command).await
    }

    fn printable_chat_char(key: event::KeyEvent) -> Option<char> {
        let non_shift_modifiers = key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE);
        match key.code {
            KeyCode::Char(c) if non_shift_modifiers.is_empty() && !c.is_control() => Some(c),
            _ => None,
        }
    }

    async fn type_to_compose(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(c) = Self::printable_chat_char(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(SemanticCommand::FocusComposer)
            .await?;
        self.input.history_browse = false;
        self.input.insert(c);
        self.clamp_slash_suggest();
        Ok(true)
    }

    async fn handle_chat_composer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let input_was_empty = self.input.text.is_empty();
        if let Some(command) = self.semantic_command_for_composer_key(key) {
            let consumed = self.execute_semantic_command(command).await?;
            if input_was_empty && !self.input.text.is_empty() {
                self.splash_dismissed = true;
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
                    self.slash_suggest_idx =
                        (self.slash_suggest_idx + suggestions.len() - 1) % suggestions.len();
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
                    self.slash_suggest_idx = (self.slash_suggest_idx + 1) % suggestions.len();
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
                self.input.backspace();
                self.clamp_slash_suggest();
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
                self.scroll_conversation_up(5);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.scroll_conversation_down(5);
                true
            }
            _ => false,
        };
        if input_was_empty && !self.input.text.is_empty() {
            self.splash_dismissed = true;
        }
        Ok(consumed)
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

        if self.explorer_dialog.is_some() {
            self.handle_explorer_dialog_key(key);
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
        match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                self.handle_search_key(key);
                return Ok(());
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                self.handle_jump_key(key);
                return Ok(());
            }
            FocusMode::Navigation if self.focus.block == FocusBlock::Composer => {
                if self.handle_chat_composer_key(key).await? {
                    return Ok(());
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    })
                    .await?;
                    return Ok(());
                }
            }
            FocusMode::Navigation => {
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    })
                    .await?;
                    return Ok(());
                }
                if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus { forward: false })
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
