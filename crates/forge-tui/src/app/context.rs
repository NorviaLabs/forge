//! Context compaction and external-editor drains for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `/compact` runs the same compaction
//! pipeline the automatic context-pressure trigger uses; the UI here is
//! deliberately a one-line result, not a view of the checkpoint.

use super::*;

impl TuiApp {
    pub(super) async fn execute_context_compaction_responsive<B: ratatui::backend::Backend>(
        &mut self,
        pending: forge_core::PendingContextCompaction,
        mut terminal: Option<&mut Terminal<B>>,
    ) -> Result<Option<forge_core::CompletedContextCompaction>, TuiError> {
        let mut execution = IsolatedTask::spawn(pending.execute());
        let mut ui_tick = tokio::time::interval(Duration::from_millis(100));
        ui_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if execution.is_finished() {
                return execution
                    .join()
                    .await
                    .map_err(|error| TuiError::Other(format!("compaction task join: {error}")));
            }
            super::shell::tick_foreground_frame(self, terminal.as_deref_mut(), &mut ui_tick)
                .await?;
            if self.cancellation.take_requested() || self.exit.is_requested() {
                execution.abort();
                return Ok(None);
            }
        }
    }

    pub(super) fn queue_context_reset(&mut self) {
        if self.busy_state.is_active()
            || self.pending_turn.has_prompt()
            || self.pending_interaction.has_hitl_decision()
            || self.pending_interaction.context_reset_pending()
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /compact");
            return;
        }
        self.pending_interaction.request_context_reset();
        self.busy_state
            .start(BusyPhase::Other("compacting context".into()));
        self.status_state.message = "compacting context…".into();
        self.set_feedback(FeedbackSeverity::Info, "compacting context…");
    }

    pub async fn drain_pending_context_reset(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_interaction.take_context_reset() {
            return Ok(());
        }
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        // A failed compaction is not a failed session: the previous context is
        // still valid and still installed, so report and carry on.
        let pending = self
            .session
            .begin_context_compaction(forge_core::CompactionTrigger::Manual);
        let Some(completed) =
            Box::pin(self.execute_context_compaction_responsive(pending, terminal.as_deref_mut()))
                .await?
        else {
            self.busy_state.stop();
            self.status_state.message = "compaction cancelled".into();
            self.set_feedback(FeedbackSeverity::Warn, "compaction cancelled");
            return Ok(());
        };
        let record = match self.session.finish_context_compaction(completed).await {
            Ok(record) => record,
            Err(error) => {
                self.busy_state.stop();
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    format!("compaction failed · context unchanged · {error}"),
                );
                // After `set_feedback`, which writes the status line itself:
                // the banner carries the detail, the status line stays short.
                self.status_state.message = "context unchanged".into();
                self.push_activity(
                    ActivityKind::Context,
                    FeedbackSeverity::Warn,
                    format!("compaction failed · {error}"),
                );
                if let Some(term) = terminal {
                    let _ = term.draw(|f| self.draw(f));
                }
                return Ok(());
            }
        };

        let before_pct = record.utilization_before * 100.0;
        let after_pct = record.utilization_after * 100.0;
        self.conversation_view.context_reset_snapshot = Some((before_pct, after_pct));
        self.conversation_view.message_start = self.session.messages.len();
        self.conversation_view.event_start = self.session.events.len();

        let summary = format!(
            "Context compacted · {} → {}",
            forge_core::compact_tokens(record.tokens_before),
            forge_core::compact_tokens(record.tokens_after)
        );
        self.push_toast(summary.clone());
        // One line of the checkpoint — the current objective — so the operator
        // can see what state survived. The checkpoint itself stays out of the
        // transcript.
        if let Some(checkpoint) = self.session.context_state().checkpoint.as_ref() {
            self.banner_state.items.push(ChatItem::ContextHandoff {
                before_pct,
                after_pct,
                goal: checkpoint.section("objective").unwrap_or_default().into(),
                completed: Vec::new(),
                next_actions: checkpoint
                    .section("next_action")
                    .map(|next| vec![next.to_string()])
                    .unwrap_or_default(),
            });
        }
        self.push_activity(ActivityKind::Context, FeedbackSeverity::Ok, summary.clone());
        self.status_state.message = summary;
        self.busy_state.stop();
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    /// Open the active file in the user's configured external editor.
    ///
    /// Suspends the TUI terminal, spawns the editor, waits for it to
    /// complete, restores the TUI, and refreshes the source viewer and
    /// Git status.
    pub async fn drain_pending_external_editor(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.external_editor.requested {
            return Ok(());
        }
        self.external_editor.requested = false;

        // 1. Guard: must have a valid text file open.
        let file_path = match &self.source_viewer.path {
            Some(p) if self.source_viewer.status.is_openable() => p.clone(),
            Some(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "Cannot open binary files in an external editor",
                );
                return Ok(());
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "No file open in the source viewer");
                return Ok(());
            }
        };

        // 2. Guard: no unsafe write-active tool.
        if matches!(self.busy_state.phase(), BusyPhase::Tool { .. }) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "External editor unavailable while Forge is writing files.\n\n\
                 Wait for the current operation to finish, then try again.",
            );
            return Ok(());
        }

        if self
            .editor_session
            .as_ref()
            .is_some_and(|editor| editor.is_dirty())
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "Embedded editor has unsaved changes · save or discard them before opening the external editor",
            );
            return Ok(());
        }

        // 3. Resolve editor.
        let (editor_cmd, _editor_args) = match crate::editor::resolve_editor() {
            Some(r) => r,
            None => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    EditorError::NotConfigured.to_string(),
                );
                return Ok(());
            }
        };

        // 4. Flush pending redraw.
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // Stop the async Crossterm reader before handing stdin to the external
        // editor. It is recreated after Forge retakes the terminal.
        let resume_terminal_events = if let Some(events) = self.terminal_events.take() {
            events.shutdown().await;
            true
        } else {
            false
        };

        // 5. Suspend the TUI terminal (restore normal terminal state).
        crate::terminal::restore_terminal();

        // 6. Spawn the editor and wait.
        let mut cmd = std::process::Command::new(&editor_cmd);
        for arg in &_editor_args {
            cmd.arg(arg);
        }
        cmd.arg(&file_path);

        let status = match cmd.status() {
            Ok(s) => s,
            Err(e) => {
                let _ = self.resume_after_external_editor(terminal.as_deref_mut());
                if resume_terminal_events {
                    self.terminal_events = Some(super::shell::TerminalEventSource::spawn());
                }
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    EditorError::SpawnFailed(e).to_string(),
                );
                return Ok(());
            }
        };

        // 8. Report non-zero exit.
        if let Some(code) = status.code() {
            if code != 0 {
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Warn,
                    format!("external editor exited with status {code}"),
                );
            }
        }

        let _ = self.resume_after_external_editor(terminal);
        if resume_terminal_events {
            self.terminal_events = Some(super::shell::TerminalEventSource::spawn());
        }

        // 9. Refresh the active file and Git status.
        self.refresh_post_editor();
        Ok(())
    }

    pub(super) fn resume_after_external_editor(
        &mut self,
        terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Best effort: terminal restoration must not fail the UI in test or headless
        // contexts where a real terminal may not be attached.
        let _ = crate::terminal::reinit_terminal();
        let _ = crate::terminal::clear_terminal();
        if let Some(term) = terminal {
            term.autoresize()?;
            term.clear()?;
            term.draw(|f| self.draw(f))?;
        }
        Ok(())
    }

    /// Called after the external editor exits. Reloads the file, refreshes
    /// syntax highlighting, search state, and Git markers.
    pub(super) fn refresh_post_editor(&mut self) {
        self.refresh_active_source_viewer();
        if let (Some(editor), Some(text)) = (
            self.editor_session.as_mut(),
            self.source_viewer.document_text.as_deref(),
        ) {
            if self.source_viewer.status == crate::source_viewer::ViewerStatus::Ok {
                editor.replace_text(text);
                self.status_state.message = "Editing file · NORMAL mode".into();
            }
        }
        self.note_workspace_changed();

        // Show a compact notice.
        let gs = &self.workspace_files.explorer.git_status;
        let changed = gs.status.len();
        let gs_text = if changed == 0 {
            "No repository changes detected".into()
        } else if changed == 1 {
            "1 file changed".into()
        } else {
            format!("{changed} files changed")
        };
        self.banner_state.items.push(ChatItem::Banner {
            text: format!("Returned from external editor\n{gs_text}"),
            kind: BannerKind::Info,
        });
    }
}
