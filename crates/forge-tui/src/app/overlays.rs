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
        let mode = match self.focus.mode() {
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
            _ => match self.workspace_navigation.current() {
                None => "No file open",
                Some(WorkspaceView::File(_)) => "File",
                Some(WorkspaceView::Diff) => "DIFF",
            },
        };
        let mut text = String::from("Forge is an AI coding agent for your terminal.\n\n");
        text.push_str(&format!(
            "Active: {} · {}\n\n",
            self.focus.block().label(),
            mode
        ));
        text.push_str("Global\n");
        text.push_str("• Tab / Shift+Tab  Move between visible blocks\n");
        text.push_str("• Ctrl+E  Toggle Files (focuses explorer when opening)\n");
        text.push_str("• Ctrl+`  Toggle terminal panel\n");
        text.push_str("• F4  Open model picker\n");
        text.push_str("• F1  Help\n");
        text.push_str("• Esc  Leave one interaction level\n\n");
        text.push_str("Active block\n");
        match self.focus.block() {
            FocusBlock::TaskStrip => {
                text.push_str("• ←/→  Select task slot\n");
                text.push_str("• Enter  Switch task\n");
                // Ctrl+T stays last-turn expansion; the switcher takes the
                // shifted form so an existing binding is not repurposed.
                text.push_str("• Ctrl+Shift+T or /tasks  Open task switcher\n");
                text.push_str("• s / c  Stop / continue the selected task\n");
                text.push_str("• p  Pin  ·  x  Archive\n");
                text.push_str("• Esc  Return to previous block\n");
            }
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
                text.push_str("• !command  Run in the embedded terminal\n");
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
                if self.session.pending_question().is_some() {
                    text.push_str("• ↑/↓  Select an answer\n");
                    text.push_str("• ←/→  Switch questions\n");
                    text.push_str("• Space  Toggle a multi-select option\n");
                    text.push_str("• Enter  Confirm (or type Other in the composer)\n");
                    text.push_str("• Esc  Skip the questions\n");
                } else {
                    text.push_str("• ↑/↓  Select an approval option\n");
                    text.push_str("• Enter  Confirm selection\n");
                    text.push_str("• Esc  Cancel (deny) the pending call\n");
                    text.push_str("• Tab  Navigate away (approval stays pending)\n");
                }
            }
            FocusBlock::BottomPanel => {
                text.push_str("• Type / paste  Send input to the shell\n");
                text.push_str("• Ctrl+C / arrows / Tab  Shell controls\n");
                text.push_str("• Esc / Ctrl+` / exit  Close the terminal panel\n");
            }
        }
        if matches!(self.focus.mode(), FocusMode::Transient(_)) {
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
            OverlayAction::SelectTask(id) => {
                let Ok(session_id) = id.parse::<uuid::Uuid>() else {
                    self.set_feedback(FeedbackSeverity::Error, "invalid task session id");
                    return Ok(());
                };
                let Some(index) = self
                    .task_chrome
                    .iter()
                    .position(|task| task.session_id == session_id)
                else {
                    self.set_feedback(FeedbackSeverity::Warn, "task is no longer available");
                    return Ok(());
                };
                self.task_strip_selection = index;
                self.overlay = None;
                self.focus_block(FocusBlock::TaskStrip);
                self.handle_task_strip_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::NONE,
                ))
                .await?;
            }
            OverlayAction::Toast(message) => {
                self.set_feedback(FeedbackSeverity::Warn, message);
            }
            OverlayAction::OpenTaskInput(mode) => {
                self.overlay = Some(Overlay::task_input(mode));
            }
            OverlayAction::OpenTaskRename { session_id, label } => {
                self.overlay = Some(Overlay::TaskRename {
                    session_id,
                    label,
                    error: None,
                });
            }
            OverlayAction::OpenTaskConfirm {
                kind,
                session_id,
                label,
                detail,
            } => {
                self.overlay = Some(Overlay::TaskConfirm {
                    kind,
                    session_id,
                    label,
                    detail,
                });
            }
            OverlayAction::RenameTask { session_id, label } => {
                let Some(session_id) = parse_task_session_id(&session_id) else {
                    self.set_feedback(FeedbackSeverity::Error, "invalid task session id");
                    return Ok(());
                };
                if self
                    .send_task_command(forge_session::SupervisorCommand::RenameTask {
                        session_id,
                        label: label.clone(),
                    })
                    .await
                {
                    self.set_feedback(FeedbackSeverity::Ok, format!("renamed to `{label}`"));
                }
                self.overlay = None;
            }
            OverlayAction::ArchiveTask { session_id } => {
                let Some(session_id) = parse_task_session_id(&session_id) else {
                    self.set_feedback(FeedbackSeverity::Error, "invalid task session id");
                    return Ok(());
                };
                if self
                    .send_task_command(forge_session::SupervisorCommand::ArchiveTask { session_id })
                    .await
                {
                    self.set_feedback(FeedbackSeverity::Ok, "task archived");
                }
                self.overlay = None;
            }
            OverlayAction::CleanupTaskWorktree { session_id } => {
                let Some(session_id) = parse_task_session_id(&session_id) else {
                    self.set_feedback(FeedbackSeverity::Error, "invalid task session id");
                    return Ok(());
                };
                if self
                    .send_task_command(forge_session::SupervisorCommand::RemoveManagedWorktree {
                        session_id,
                    })
                    .await
                {
                    self.set_feedback(FeedbackSeverity::Ok, "worktree removed · branch kept");
                }
                self.overlay = None;
            }
            OverlayAction::CreateTask {
                label,
                first_prompt,
            } => {
                if self
                    .send_task_command(forge_session::SupervisorCommand::CreateTask {
                        label,
                        first_prompt,
                    })
                    .await
                {
                    self.overlay = None;
                    self.set_feedback(FeedbackSeverity::Info, "creating task worktree…");
                }
            }
            OverlayAction::AttachTask {
                workspace,
                label,
                branch,
            } => {
                // The overlay validates shape; the path is resolved here,
                // where the launch directory is known, and its membership in
                // the repository is settled by the supervisor.
                let workspace = Overlay::normalize_workspace_path(&workspace, &self.runtime.cwd);
                if self
                    .send_task_command(forge_session::SupervisorCommand::AttachWorktree {
                        workspace,
                        label,
                        branch,
                    })
                    .await
                {
                    self.overlay = None;
                    self.set_feedback(FeedbackSeverity::Info, "attaching worktree…");
                }
            }
            OverlayAction::FinalizeTaskCreation { operation_id } => {
                if self
                    .send_task_command(forge_session::SupervisorCommand::FinalizeCreation {
                        operation_id,
                    })
                    .await
                {
                    self.set_feedback(FeedbackSeverity::Ok, "task worktree trusted");
                }
                // The overlay closes either way: on failure the supervisor has
                // already rolled the creation back, so there is nothing left
                // to trust.
                self.overlay = None;
            }
            OverlayAction::CancelTaskCreation { operation_id } => {
                self.send_task_command(forge_session::SupervisorCommand::CancelCreation {
                    operation_id,
                })
                .await;
                self.overlay = None;
                self.set_feedback(FeedbackSeverity::Info, "task creation cancelled");
            }
            OverlayAction::BeginOnboarding => {
                self.open_connect_picker();
                self.set_feedback(FeedbackSeverity::Info, "Step 1 of 2 · choose a provider");
            }
            OverlayAction::ContinueTurns => {
                self.overlay = None;
                self.pending_turn.request_continue();
                self.busy_state.activate();
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
                Box::pin(
                    self.execute_semantic_command(SemanticCommand::DispatchSlash {
                        origin: SlashCommandOrigin::GlobalPalette,
                        line: cmd,
                    }),
                )
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
                //
                // Model choice is per-task, so a pick made while a sibling is
                // selected must reach that actor, not the primary session.
                // Provider authentication stays global and is unaffected.
                if let SelectedRuntime::Sibling(session_id) = self.selected_runtime() {
                    let route_id = profile_id
                        .as_deref()
                        .map(super::connect::route_id_for_profile)
                        .unwrap_or_default();
                    if self
                        .send_task_command(forge_session::SupervisorCommand::SetModel {
                            session_id,
                            model_id: model.clone(),
                            route_id,
                            reasoning_effort: Some(self.reasoning_effort.value.to_string()),
                        })
                        .await
                    {
                        self.set_feedback(
                            FeedbackSeverity::Ok,
                            format!("{} · model {model}", self.selected_task_label()),
                        );
                    }
                    self.overlay = None;
                    return Ok(());
                }
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
                self.busy_state.set_phase(BusyPhase::Connect);
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Info,
                    format!("connect {profile_id}"),
                );
                // The row the user pressed Enter on was labelled `reuse …`,
                // so adopting that session *is* what they asked for. A failed
                // import falls through to the ordinary flow rather than
                // stranding them.
                if !self.try_reuse_login(&profile_id) {
                    self.finish_connect(&profile_id, None, false);
                }
                self.busy_state.set_phase(BusyPhase::Idle);
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
            self.exit.request_with_code(ExitCode::Canceled);
        }
        // Esc pops exactly one interaction level (FORGE-DESIGN §8.1). The
        // API-key screen is one level *below* the provider list, so Esc
        // returns to that list — matching its own footer's "Esc back" —
        // rather than jumping out two levels at once. Only the provider list
        // itself (the top of the picker) is the intentional quit point during
        // first-run onboarding.
        if matches!(self.overlay, Some(Overlay::ConnectApiKey { .. })) {
            self.open_connect_picker();
            return;
        }
        if self.onboarding_connect && matches!(self.overlay, Some(Overlay::ConnectModel { .. })) {
            self.exit.request_with_code(ExitCode::Canceled);
        }
        // An in-flight device-code OAuth poll must not be able to complete a
        // connection the user just cancelled — `poll_oauth_tick` runs
        // unconditionally every event-loop tick regardless of which overlay
        // (if any) is open.
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

/// Parse a switcher row's session id. The overlay carries it as a string so
/// the overlay module stays free of a `uuid` dependency.
fn parse_task_session_id(value: &str) -> Option<uuid::Uuid> {
    value.parse::<uuid::Uuid>().ok()
}
