//! Context reset and external-editor drains for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `/compact` context handoff and suspending the TUI
//! to open the active file in the user's configured editor. Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn queue_context_reset(&mut self) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_hitl_decision.is_some()
            || self.pending_context_reset
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /compact");
            return;
        }
        self.pending_context_reset = true;
        self.busy_phase = BusyPhase::Other("context reset".into());
        self.status_message = "resetting context…".into();
        self.set_feedback(FeedbackSeverity::Info, "resetting context…");
    }

    pub async fn drain_pending_context_reset(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_context_reset {
            return Ok(());
        }
        self.pending_context_reset = false;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let before_report = self.session.token_usage_report();
        let before = before_report.context_tokens_est;
        self.session.force_context_reset_async().await?;
        let after_report = self.session.token_usage_report();
        let after = after_report.context_tokens_est;
        self.context_reset_snapshot = Some((
            before as f64 / before_report.context_capacity.max(1) as f64 * 100.0,
            after as f64 / after_report.context_capacity.max(1) as f64 * 100.0,
        ));
        self.chat_message_start = self.session.messages.len();
        self.chat_event_start = self.session.events.len();
        self.push_toast("Continuing in a fresh context");
        let progress = fs::read_to_string(self.runtime.cwd.join(".forge/progress.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<ProgressDocument>(&text).ok());
        if let Some(progress) = progress {
            self.ui_banners.push(ChatItem::ContextHandoff {
                before_pct: self.context_reset_snapshot.unwrap().0,
                after_pct: self.context_reset_snapshot.unwrap().1,
                goal: progress.goal,
                completed: progress.completed,
                next_actions: progress.next_actions,
            });
        }
        self.push_activity(
            ActivityKind::Context,
            FeedbackSeverity::Ok,
            format!("fresh context prepared · {before} → {after} tokens"),
        );
        self.status_message = "Continuing in a fresh context".into();
        self.notices.clear();
        self.busy_phase = BusyPhase::Idle;
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
        if !self.pending_external_editor {
            return Ok(());
        }
        self.pending_external_editor = false;

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
        if matches!(self.busy_phase, BusyPhase::Tool { .. }) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "External editor unavailable while Forge is writing files.\n\n\
                 Wait for the current operation to finish, then try again.",
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
        let _ = crate::terminal::reinit_terminal(self.runtime.mouse_capture);
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
        self.note_workspace_changed();

        // Show a compact notice.
        let gs = &self.file_explorer.git_status;
        let changed = gs.status.len();
        let gs_text = if changed == 0 {
            "No repository changes detected".into()
        } else if changed == 1 {
            "1 file changed".into()
        } else {
            format!("{changed} files changed")
        };
        self.notices.clear();
        self.push_notice(vec!["Returned from external editor".into(), gs_text]);
    }
}
