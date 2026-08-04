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
                Some(WorkspaceView::Diff(_)) => "Review changes",
                Some(WorkspaceView::Run(_)) => "Run",
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
        text.push_str("• Ctrl+E  Toggle Files\n");
        text.push_str(&format!(
            "• Alt+P  Cycle permission mode (now: {})\n",
            self.permission_mode.label()
        ));
        text.push_str(
            "• Hold Shift (⌥ on iTerm2) while dragging to select/copy in your terminal\n",
        );
        text.push_str("• ?  Help\n");
        text.push_str("• Esc  Leave one interaction level\n\n");
        text.push_str("Active block\n");
        match self.focus.block {
            FocusBlock::Workspace => {
                text.push_str("• Alt+←  Back\n");
                text.push_str("• Alt+→  Review changes\n");
                text.push_str("• Type  Start chat in composer\n");
                text.push_str("• G / r  Editor navigation and refresh\n");
                text.push_str("• Ctrl+F / Ctrl+G  Search or jump\n");
            }
            FocusBlock::Sidebar => {
                text.push_str("• Up/Down  Select a background task\n");
                text.push_str("• x / a / d  Cancel / approve / deny selected task\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Composer => {
                text.push_str("• Enter  Send\n");
                text.push_str("• ⇧Enter  Newline\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::BottomPanel => {
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Files => {
                text.push_str("• Enter  Open or expand\n");
                text.push_str("• n / N  New file / folder\n");
                text.push_str("• R  Rename selected entry\n");
                text.push_str("• d  Delete selected entry\n");
                text.push_str("• r  Refresh selected directory\n");
                text.push_str("• Esc  Return to previous block\n");
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
            OverlayAction::HitlApprove => {
                self.resolve_hitl_overlay(HitlDecision::Approve, false)
                    .await?;
            }
            OverlayAction::HitlApproveSession => {
                self.resolve_hitl_overlay(HitlDecision::Approve, true)
                    .await?;
            }
            OverlayAction::HitlDeny => {
                self.resolve_hitl_overlay(HitlDecision::Deny, false).await?;
            }
            OverlayAction::HitlApprovePattern { pattern } => {
                self.resolve_hitl_overlay_with_pattern(pattern).await?;
            }
            OverlayAction::HitlDenyWithFeedback { feedback } => {
                self.resolve_hitl_overlay_with_feedback(feedback).await?;
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
            OverlayAction::SwitchToRoute { profile_id } => {
                self.apply_default_model_for_profile(&profile_id, "active");
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
            OverlayAction::QuickOpenFile { path } => {
                self.overlay = None;
                let abs = self.session.workspace_root().join(&path);
                self.open_file_in_editor(&abs);
                self.set_feedback(FeedbackSeverity::Ok, format!("opened · {path}"));
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
        self.render_cache.conversation = None;
        self.invalidate_hit_regions();
    }

    pub(super) fn apply_theme(&mut self, theme_id: String, persist: bool) {
        self.set_theme_active(&theme_id);
        self.overlay = None;
        if persist {
            self.save_ui_state();
            let label = crate::theme::registry().display_name(&theme_id);
            self.set_feedback(FeedbackSeverity::Ok, format!("theme · {label}"));
        }
    }
}
