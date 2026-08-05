//! Keyboard input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `handle_key` is the entry point; the
//! `handle_*_key` family dispatches per focus target. Key-to-command mapping
//! lives in `app/commands.rs`; this module decides who handles a press.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    fn handle_explorer_dialog_key(&mut self, key: event::KeyEvent) -> bool {
        let Some(dialog) = self.explorer_dialog.current.take() else {
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
        self.explorer_dialog.current = next;
        true
    }

    /// Insert bracketed-paste text into the current explicit text owner.
    pub(super) fn handle_paste(&mut self, data: &str) {
        if let Some(ExplorerDialog::Name { input, error, .. }) =
            self.explorer_dialog.current.as_mut()
        {
            for ch in data.chars().filter(|ch| !ch.is_control()) {
                input.push(ch);
            }
            *error = None;
            return;
        }
        if self.focus.block == FocusBlock::BottomPanel {
            if let Some(terminal) = self.interactive_terminal.as_mut() {
                if let Err(error) = terminal.write(data.as_bytes()) {
                    self.set_feedback(
                        FeedbackSeverity::Error,
                        format!("terminal paste failed: {error}"),
                    );
                }
                return;
            }
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
                self.history.push(&line);
                self.slash_suggestions.selected = 0;
                self.notice_state.items.clear();
                self.input.history_browse = false;
                self.dispatch_line(&line).await?;
            }
            return Ok(());
        }

        let line = self.input.take();
        if line.trim().is_empty() {
            if !self.busy_state.active && !self.session.queue().is_empty() {
                self.dequeue_and_send_next().await;
            }
            return Ok(());
        }

        // Slash commands always dispatch immediately regardless of lifecycle.
        if line.trim_start().starts_with('/') {
            self.history.push(&line);
            self.slash_suggestions.selected = 0;
            self.notice_state.items.clear();
            self.input.history_browse = false;
            self.dispatch_line(&line).await?;
            return Ok(());
        }

        let route =
            input_route::classify_input(&self.session.active_task, self.overlay.is_some(), &line);
        let consumed = !matches!(route, input_route::InputRoute::RejectStaleResponse);
        if consumed {
            self.history.push(&line);
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
            input_route::InputRoute::ResolveApproval(action) => {
                self.resolve_approval_line(action).await?;
            }
            input_route::InputRoute::AnswerClarification
            | input_route::InputRoute::ResolveSelection => {
                // No runtime producer exists yet for these wait reasons —
                // structurally routable, but nothing sets them today.
                self.set_feedback(FeedbackSeverity::Warn, "nothing pending to answer");
            }
            input_route::InputRoute::RejectStaleResponse => {
                // Keep the operator's text so they can edit it into a valid
                // approval answer instead of retyping from scratch.
                self.input.set_text(line);
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "resolve the pending approval first — ↑↓ select · Enter confirm · Esc deny (or type yes/no/…)",
                );
            }
        }
        Ok(())
    }

    fn handle_editor_key(&mut self, key: event::KeyEvent) -> bool {
        if !self.current_workspace_is_file() {
            return false;
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
            _ => false,
        }
    }

    async fn handle_sidebar_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if let Some(command) = self.semantic_command_for_sidebar_key(key) {
            self.execute_semantic_command(command).await
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
                if let Err(error) = terminal.write(&bytes) {
                    self.set_feedback(
                        FeedbackSeverity::Error,
                        format!("terminal input failed: {error}"),
                    );
                }
                return Ok(true);
            }
        }
        if let Some(command) = self.semantic_command_for_bottom_panel_key(key) {
            self.execute_semantic_command(command).await
        } else {
            Ok(false)
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
            FocusBlock::Sidebar => self.handle_sidebar_key(key).await,
            FocusBlock::Composer => Ok(false),
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
            self.conversation_view.splash_dismissed = true;
        }
        Ok(consumed)
    }

    /// Navigate composer chips when `composer_chip_focus` is set (`Alt+.`).
    pub(super) async fn handle_composer_chip_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        let Some(idx) = self.composer_chip_focus else {
            return Ok(false);
        };
        let chips = self.current_composer_chips();
        if chips.is_empty() {
            self.composer_chip_focus = None;
            return Ok(false);
        }
        let n = chips.len();
        let idx = idx.min(n - 1);
        self.composer_chip_focus = Some(idx);
        match key.code {
            KeyCode::Left if key.modifiers.is_empty() => {
                self.composer_chip_focus = Some((idx + n - 1) % n);
                Ok(true)
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.composer_chip_focus = Some((idx + 1) % n);
                Ok(true)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.composer_chip_focus = None;
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let kind = chips[idx].kind;
                self.composer_chip_focus = None;
                self.activate_composer_chip(kind).await?;
                Ok(true)
            }
            _ => {
                // Global chords (Alt/Ctrl) and typing leave chip focus.
                if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
                    return Ok(false);
                }
                if Self::printable_chat_char(key).is_some()
                    || matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
                {
                    self.composer_chip_focus = None;
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    fn current_composer_chips(&self) -> Vec<ComposerChip> {
        let connected = self.is_provider_connected();
        let vendor = self
            .connect
            .profile
            .as_deref()
            .and_then(|id| self.vendor_route_labels(id).0);
        let effort = self
            .reasoning_effort
            .value
            .display_label(&self.runtime.model_label)
            .to_string();
        let raw = composer_chips(
            self.permission_mode.label(),
            connected,
            vendor.as_deref(),
            &self.runtime.model_label,
            &effort,
        );
        // Width unknown here; navigation uses full set (fit only for paint).
        raw
    }

    async fn activate_composer_chip(&mut self, kind: ComposerChipKind) -> Result<(), TuiError> {
        match kind {
            ComposerChipKind::Mode => {
                self.execute_semantic_command(SemanticCommand::CyclePermissionMode)
                    .await?;
            }
            ComposerChipKind::Connect => {
                if self.is_provider_connected() {
                    self.open_connect_picker_compact(ConnectModelColumn::Providers);
                } else {
                    self.open_connect_picker();
                }
            }
            ComposerChipKind::Model => {
                self.execute_semantic_command(SemanticCommand::OpenModelControl(
                    ConnectModelColumn::Models,
                ))
                .await?;
            }
            ComposerChipKind::Effort => {
                self.execute_semantic_command(SemanticCommand::OpenModelControl(
                    ConnectModelColumn::Effort,
                ))
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

        if self.explorer_dialog.current.is_some() {
            self.handle_explorer_dialog_key(key);
            return Ok(());
        }

        if self.hitl_session.pattern_nudge.is_some() && self.handle_pattern_nudge_key(key) {
            return Ok(());
        }

        if self.session.pending_hitl().is_some() && self.handle_approval_menu_key(key).await? {
            return Ok(());
        }

        if self.composer_chip_focus.is_some() && self.handle_composer_chip_key(key).await? {
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
            let refresh_quick_open = matches!(self.overlay, Some(Overlay::QuickOpen { .. }));
            self.apply_overlay_action(action).await?;
            if refresh_quick_open {
                self.refresh_quick_open_results();
            }
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
                validation_command: None,
                file_icons: forge_config::FileIconMode::Unicode,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );
        (dir, app)
    }

    /// Focus the composer in navigation mode, which is what `handle_key` needs
    /// before it will route a press to the chat composer.
    fn focus_composer(app: &mut TuiApp) {
        app.focus.mode = FocusMode::Navigation;
        app.focus.block = FocusBlock::Composer;
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
        assert_eq!(app.focus.block, FocusBlock::Composer);
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
            lines: vec!["all good".into()],
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
            lines: vec!["all good".into()],
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
        app.focus.mode = FocusMode::Navigation;
        app.focus.block = FocusBlock::Workspace;

        app.handle_key(press(KeyCode::Char('z'))).await.unwrap();

        assert_eq!(app.input.text, "z");
        assert_eq!(app.focus.block, FocusBlock::Composer);
    }
}
