//! Overlay actions, theme, and help rendering for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Applies palette overlay selections, routes
//! connect/model/HITL overlay actions, and renders the help overlay. Methods are
//! moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn render_help_overlay(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let r = centered_rect(64, 58, area);
        crate::theme::fill(r, buf, crate::theme::panel());
        Paragraph::new(self.help_text())
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::brand())
                    .style(theme::panel())
                    .title(Span::styled(" Help ", theme::brand())),
            )
            .render(r, buf);
    }

    pub(super) fn help_text(&self) -> String {
        let mode = match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch)
                if self.current_workspace_is_file() =>
            {
                "SEARCH"
            }
            FocusMode::Transient(TransientOwner::JumpToLine)
                if self.current_workspace_is_file() =>
            {
                "JUMP"
            }
            _ => match &self.workspace_navigation.current {
                None => "No file open",
                Some(WorkspaceView::File(_)) => "File",
            },
        };
        let mut text = String::from("Forge is an AI coding agent for your terminal.\n\n");
        text.push_str(&format!(
            "Active: {} · {}\n\n",
            self.focus.block.label(),
            mode
        ));
        text.push_str("Global\n");
        text.push_str("• Tab / Shift+Tab  Move between visible blocks\n");
        text.push_str("• Ctrl+E  Toggle Files (focuses explorer when opening)\n");
        text.push_str("• F4  Open model picker\n");
        text.push_str("• ?  Help\n");
        text.push_str("• Esc  Leave one interaction level\n\n");
        text.push_str("Active block\n");
        match self.focus.block {
            FocusBlock::Workspace => {
                text.push_str("• Alt+←  Back\n");
                text.push_str("• Type  Start chat in composer\n");
                text.push_str("• Vim Normal/Insert/Search modes  Edit text files\n");
                text.push_str("• :w / :q / :wq  Save / quit / save and quit\n");
                text.push_str("• :s/.../.../  Replace; :%s/.../.../  Replace all\n");
                text.push_str("• Alt+E  Open the external editor\n");
            }
            FocusBlock::Search => {
                text.push_str("• Type  Fuzzy-filter files by workspace path\n");
                text.push_str("• Ctrl+U  Clear file search\n");
                text.push_str("• ↑/↓  Move tree selection without leaving search\n");
                text.push_str("• Enter  Open the selected file\n");
                text.push_str("• Tab / Shift+Tab  Next / previous block\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Files => {
                text.push_str("• ↑/↓  Move selection\n");
                text.push_str("• ←/→  Collapse / expand directory\n");
                text.push_str("• Enter  Open file or expand directory\n");
                text.push_str("• n / N  New file / folder\n");
                text.push_str("• R  Rename · d  Delete\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Sidebar => {
                text.push_str("• Up/Down  Select a background task\n");
                text.push_str("• x / a / d  Cancel / approve / deny selected task\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Composer => {
                text.push_str("• Enter  Send\n");
                text.push_str("• ⇧Enter  Newline\n");
                text.push_str("• Tab  Next block (Footer, then Bottom Panel)\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Footer => {
                text.push_str("• ←/→  Select which-LLM or effort\n");
                text.push_str("• Enter  Open picker for the selected control\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Approval => {
                text.push_str("• ↑/↓  Select an approval option\n");
                text.push_str("• Enter  Confirm selection\n");
                text.push_str("• Esc  Cancel (deny) the pending call\n");
                text.push_str("• Tab  Navigate away (approval stays pending)\n");
            }
            FocusBlock::BottomPanel => {
                text.push_str("• Type / paste  Send input to the shell\n");
                text.push_str("• Ctrl+C / arrows / Tab  Shell controls\n");
                text.push_str("• Ctrl+`  Close the terminal panel\n");
            }
        }
        if matches!(self.focus.mode, FocusMode::Transient(_)) {
            text.push_str("\nTransient input\n• Esc  Close\n");
        }
        text
    }

    pub(super) fn toggle_bottom_panel(&mut self) {
        if self.bottom_panel.open {
            self.bottom_panel.open = false;
            self.restore_focus_after_closing(FocusBlock::BottomPanel);
            self.normalize_focus();
        } else {
            self.open_bottom_panel();
        }
    }

    pub(super) async fn apply_overlay_action(
        &mut self,
        action: OverlayAction,
    ) -> Result<(), TuiError> {
        match action {
            OverlayAction::None => {}
            OverlayAction::Close => {
                self.dismiss_overlay();
            }
            OverlayAction::BeginOnboarding => {
                self.open_connect_picker();
                self.set_feedback(FeedbackSeverity::Info, "Step 1 of 2 · choose a provider");
            }
            OverlayAction::ContinueTurns => {
                self.overlay = None;
                self.pending_turn.continue_turn = true;
                self.busy_state.active = true;
                self.push_toast("continuing");
            }
            OverlayAction::StopTurns => {
                self.overlay = None;
                self.status_state.message = "agent stopped at turn limit".into();
                self.set_feedback(FeedbackSeverity::Info, "stopped at turn limit");
            }
            OverlayAction::RunCommand(cmd) => {
                self.startup_resume.picker = false;
                self.overlay = None;
                self.execute_semantic_command(SemanticCommand::DispatchSlash {
                    origin: SlashCommandOrigin::GlobalPalette,
                    line: cmd,
                })
                .await?;
            }
            OverlayAction::SelectModel {
                provider,
                model,
                profile_id,
            } => {
                // Models is a standalone view (no chained Effort step) —
                // apply the pick, fall back to a safe effort default for
                // the new model if the previous one doesn't fit, and close.
                self.apply_model_selection(&provider, &model, profile_id.as_deref());
                self.resolve_effort_for_model(&model);
                self.overlay = None;
            }
            OverlayAction::ModelNotInCatalog(model) => {
                self.status_state.message =
                    format!("model `{model}` is not available for this account");
                self.push_notice(vec![
                    "Choose a model from the connected route's catalog.".into()
                ]);
            }
            OverlayAction::SwitchToRoute { profile_id } => {
                // A connected route is a credential/entitlement choice, not
                // an independent model selection. Browse its runnable models
                // without changing the active route-model combination until
                // the user confirms a row.
                self.open_model_picker_after_connect(&profile_id);
            }
            OverlayAction::SelectEffort(level) => {
                self.overlay = None;
                self.reasoning_effort.value = level;
                self.record_deliberate_selection();
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("reasoning effort: {}", level.label()),
                );
            }
            OverlayAction::PreviewTheme(theme_id) => {
                self.set_theme_active(&theme_id);
            }
            OverlayAction::SelectTheme(theme_id) => {
                self.apply_theme(theme_id, true);
            }
            OverlayAction::ConnectSubmitKey {
                profile_id,
                api_key,
            } => {
                // Keep overlay until connect succeeds so a bad key does not wipe paste.
                let key = api_key.trim().to_string();
                self.try_connect_api_key(&profile_id, Some(key));
            }
            OverlayAction::ConnectCompleteOauth { profile_id } => {
                // Enter: try one poll now; keep overlay if still pending.
                if self.connect.oauth_pending.is_some() {
                    self.connect.oauth_last_poll = None;
                    self.poll_oauth_tick();
                    if self.connect.oauth_pending.is_some() {
                        self.status_state.message =
                            format!("Still waiting for login… (code for {profile_id})");
                    }
                } else if std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok() {
                    self.overlay = None;
                    self.finish_connect(&profile_id, None, true);
                } else {
                    self.begin_oauth_flow(&profile_id);
                }
            }
            OverlayAction::ConnectUseEnv { profile_id } => {
                self.try_connect_api_key(&profile_id, None);
            }
            OverlayAction::ConnectPickProfile { profile_id } => {
                self.overlay = None;
                self.busy_state.phase = BusyPhase::Connect;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Info,
                    format!("connect {profile_id}"),
                );
                self.finish_connect(&profile_id, None, false);
                self.busy_state.phase = BusyPhase::Idle;
            }
            OverlayAction::FilePick { path, is_dir } => {
                if is_dir {
                    self.open_file_explorer(Some(&path), None);
                } else {
                    self.open_file_viewer(&path);
                }
            }
        }
        Ok(())
    }

    /// Dismiss the active overlay. Theme picker restores the theme that was
    /// active when `/theme` opened (preview is non-persistent until Enter).
    pub(super) fn dismiss_overlay(&mut self) {
        if self.startup_resume.picker {
            self.exit.requested = true;
            self.exit.code = ExitCode::Canceled;
        }
        if self.onboarding_connect
            && matches!(
                self.overlay,
                Some(
                    Overlay::ConnectModel { .. }
                        | Overlay::ConnectApiKey { .. }
                        | Overlay::ConnectOauth { .. }
                )
            )
        {
            self.exit.requested = true;
            self.exit.code = ExitCode::Canceled;
        }
        // An in-flight device-code OAuth poll must not be able to
        // complete a connection the user just cancelled — `poll_oauth_tick`
        // runs unconditionally every event-loop tick regardless of
        // which overlay (if any) is open.
        if matches!(self.overlay, Some(Overlay::ConnectOauth { .. })) {
            self.connect.oauth_pending = None;
            self.connect.oauth_last_poll = None;
        }
        if let Some(Overlay::Theme { current, .. }) = &self.overlay {
            let restore = current.clone();
            if crate::theme::active() != restore {
                self.set_theme_active(&restore);
            }
        }
        self.overlay = None;
    }

    pub(super) fn set_theme_active(&mut self, theme_id: &str) {
        crate::theme::set_active(theme_id);
        self.runtime.theme_id = theme_id.to_string();
        if let Some(editor) = self.editor_session.as_mut() {
            editor.set_syntax_theme(crate::theme::syntax_theme());
        }
        self.render_cache.conversation = None;
    }

    pub(super) fn apply_theme(&mut self, theme_id: String, persist: bool) {
        self.set_theme_active(&theme_id);
        self.overlay = None;
        if persist {
            self.save_ui_state();
            #[cfg(not(test))]
            let _ = forge_config::persist_committed_theme(&theme_id);
            let label = crate::theme::registry().display_name(&theme_id);
            self.set_feedback(FeedbackSeverity::Ok, format!("theme · {label}"));
        }
    }
}
