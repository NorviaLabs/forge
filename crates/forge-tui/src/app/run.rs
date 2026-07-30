//! Command runs, validation and the `/sync` flow for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Covers the Run panel's lifecycle — drafting,
//! launching, polling and cancelling a command, and capturing its output — the
//! post-run validation drain, and the `/sync` path that stages, commits and
//! pushes with a generated message. Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn normalize_restored_run(&mut self) {
        if let Some(record) = self.run.current.as_mut() {
            if matches!(record.state, RunState::Running | RunState::Queued) {
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
            }
        }
        self.pending_validation = false;
        self.run_rx = None;
        self.run_abort = None;
    }

    /// Run the built-in `git` tool in the session workspace. Never touches the model.
    pub(super) async fn run_git_tool(
        &self,
        subcommand: &str,
        args: Vec<String>,
    ) -> Result<forge_types::ToolOutput, forge_tools::ToolError> {
        let ctx = ToolContext::new(self.session.workspace_root().to_path_buf());
        GitTool
            .call(
                &ctx,
                json!({
                    "subcommand": subcommand,
                    "args": args,
                }),
            )
            .await
    }

    /// `/sync` — stage all changes, invent a commit message from the changeset, commit, push.
    pub(super) fn queue_sync(&mut self) {
        if self.busy || self.pending_prompt.is_some() || self.pending_sync {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /sync");
            return;
        }
        self.pending_sync = true;
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.push_toast("syncing…");
        self.status_message = "syncing…".into();
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync queued",
        );
    }

    pub(super) fn run_current_draft(&mut self) {
        if self
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Running)
        {
            self.run.error = Some("a run is already active; cancel it first".into());
            return;
        }
        let invocation = match self.run.draft.invocation() {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run.error = Some(error.to_string());
                return;
            }
        };
        if !invocation.working_directory.is_dir() {
            self.run.error = Some(format!(
                "working directory is not accessible: {}",
                invocation.working_directory.display()
            ));
            return;
        }
        let mut record = self.run.record(
            invocation.clone(),
            crate::RunProvenance::Manual,
            Some(self.session.session_id.to_string()),
        );
        record.state = RunState::Running;
        record.started_at = Some(std::time::SystemTime::now());
        self.run.current = Some(record);
        self.run.error = None;
        self.pending_validation = true;
        self.busy_phase = BusyPhase::Tool { name: "run".into() };
        self.status_message = format!("run: {}", invocation.summary());
        self.push_activity(
            ActivityKind::Run,
            FeedbackSeverity::Info,
            format!("run started: {}", invocation.summary()),
        );
    }

    pub(super) fn rerun_current(&mut self) {
        let Some(record) = self
            .run
            .current
            .clone()
            .or_else(|| self.run.recent.front().cloned())
        else {
            self.run.error = Some("no previous run".into());
            return;
        };
        let draft = &mut self.run.draft;
        draft.command_input = record.invocation.summary();
        draft.working_directory = record.invocation.working_directory;
        draft.environment_delta = record.invocation.environment_delta;
        draft.execution_mode = record.invocation.execution_mode;
        draft.source_record_id = Some(record.id);
        self.run_current_draft();
    }

    pub(super) fn edit_and_rerun_current(&mut self) {
        let Some(record) = self
            .run
            .current
            .clone()
            .or_else(|| self.run.recent.front().cloned())
        else {
            self.run.error = Some("no previous run".into());
            return;
        };
        self.run.draft.command_input = record.invocation.summary();
        self.run.draft.working_directory = record.invocation.working_directory;
        self.run.draft.environment_delta = record.invocation.environment_delta;
        self.run.draft.execution_mode = record.invocation.execution_mode;
        self.run.draft.source_record_id = Some(record.id);
        self.run.editing = true;
    }

    pub(super) fn cancel_run(&mut self) {
        let mut cancelled = None;
        if let Some(record) = self.run.current.as_mut() {
            if record.state == RunState::Running {
                if let Some(handle) = self.run_abort.take() {
                    handle.abort();
                }
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
                record.duration = record.started_at.and_then(|start| {
                    record
                        .finished_at
                        .and_then(|end| end.duration_since(start).ok())
                });
                self.run_rx = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                cancelled = Some(record.clone());
            }
        }
        if let Some(record) = cancelled {
            self.push_activity(
                ActivityKind::Run,
                FeedbackSeverity::Warn,
                format!("run cancelled: {}", record.invocation.summary()),
            );
            self.run.remember(record);
            self.save_run_history();
        }
    }

    pub async fn drain_pending_validation(
        &mut self,
        terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_validation
            || !self
                .run
                .current
                .as_ref()
                .is_some_and(|record| record.state == RunState::Running)
        {
            return Ok(());
        }
        self.pending_validation = false;
        let Some(record) = self.run.current.as_ref() else {
            return Ok(());
        };
        let invocation = record.invocation.clone();
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        self.terminal_capture.title = Some(format!("run · {}", invocation.summary()));
        self.terminal_capture.content.clear();
        self.terminal_capture.truncated = false;
        let (tx, rx) = std::sync::mpsc::channel();
        self.run_rx = Some(rx);
        self.run_abort = Some(tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut cmd = tokio::process::Command::new(&invocation.executable);
            cmd.args(&invocation.arguments)
                .current_dir(&invocation.working_directory)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for change in invocation.environment_delta {
                match change {
                    crate::RunEnvironmentChange::Set { name, value } => {
                        cmd.env(name, value);
                    }
                    crate::RunEnvironmentChange::Remove { name } => {
                        cmd.env_remove(name);
                    }
                }
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    let mut stdout = child.stdout.take();
                    let mut stderr = child.stderr.take();
                    let tx_out = tx.clone();
                    let stdout_task = tokio::spawn(async move {
                        if let Some(mut stream) = stdout.take() {
                            let mut buf = [0u8; 1024];
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let _ = tx_out.send(RunEvent::Output(buf[..n].to_vec()));
                                    }
                                    Err(error) => {
                                        let _ = tx_out.send(RunEvent::CaptureFailed(format!(
                                            "output capture failed: {error}"
                                        )));
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    let tx_err = tx.clone();
                    let stderr_task = tokio::spawn(async move {
                        if let Some(mut stream) = stderr.take() {
                            let mut buf = [0u8; 1024];
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let _ = tx_err.send(RunEvent::Output(buf[..n].to_vec()));
                                    }
                                    Err(error) => {
                                        let _ = tx_err.send(RunEvent::CaptureFailed(format!(
                                            "output capture failed: {error}"
                                        )));
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    let status = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    match status {
                        Ok(status) => {
                            let _ = tx.send(RunEvent::Finished {
                                exit_code: status.code(),
                                success: status.success(),
                            });
                        }
                        Err(error) => {
                            let _ = tx.send(RunEvent::SpawnFailed(error.to_string()));
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(RunEvent::SpawnFailed(error.to_string()));
                }
            }
        }));
        Ok(())
    }

    pub(super) fn poll_run(&mut self) {
        let Some(rx) = self.run_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(RunEvent::Output(chunk)) => {
                self.append_terminal_output(&chunk);
                self.run_rx = Some(rx);
            }
            Ok(RunEvent::Finished { exit_code, success }) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = if success {
                        RunState::Succeeded
                    } else {
                        RunState::Failed
                    };
                    record.exit_status = exit_code;
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        if success {
                            FeedbackSeverity::Ok
                        } else {
                            FeedbackSeverity::Error
                        },
                        if success {
                            format!("run succeeded: {}", record.invocation.summary())
                        } else {
                            format!("run failed: {}", record.invocation.summary())
                        },
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
            }
            Ok(RunEvent::SpawnFailed(error)) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = RunState::StartFailed;
                    record.spawn_error = Some(error.clone());
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run failed to start: {}", record.invocation.summary()),
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
                self.terminal_capture.content = error.clone();
                self.report_error(&format!("run launch failed: {error}"));
            }
            Ok(RunEvent::CaptureFailed(error)) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = RunState::CaptureFailed;
                    record.spawn_error = Some(error.clone());
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run capture failed: {}", record.invocation.summary()),
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
                self.terminal_capture.content = error.clone();
                self.report_error(&format!("run output capture failed: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.run_rx = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(record) = self.run.current.as_mut() {
                    record.state = RunState::CaptureFailed;
                    record.finished_at = Some(std::time::SystemTime::now());
                    let summary = record.invocation.summary();
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run capture failed: {summary}"),
                    );
                }
            }
        }
    }

    pub(super) fn append_terminal_output(&mut self, chunk: &[u8]) {
        const MAX_CAPTURE: usize = 16_000;
        self.terminal_capture
            .content
            .push_str(&String::from_utf8_lossy(chunk));
        if self.terminal_capture.content.len() > MAX_CAPTURE {
            self.terminal_capture.content.truncate(MAX_CAPTURE);
            self.terminal_capture.truncated = true;
        }
    }

    /// Drive `/sync` work with a terminal handle available for intermediate redraws.
    pub async fn drain_pending_sync(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_sync {
            return Ok(());
        }
        self.pending_sync = false;
        self.slash_sync_inner(&mut terminal).await;
        Ok(())
    }

    async fn slash_sync_inner(
        &mut self,
        terminal: &mut Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) {
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync (stage · message · commit · push)",
        );
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.set_feedback(FeedbackSeverity::Info, "syncing… inspecting changes");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // Anything unstaged or untracked?
        let status = match self
            .run_git_tool("status", vec!["--porcelain".into()])
            .await
        {
            Ok(o) if !o.is_error => o.content,
            Ok(o) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {e}"));
                return;
            }
        };
        if status.trim().is_empty() {
            self.busy_phase = BusyPhase::Idle;
            self.set_feedback(FeedbackSeverity::Info, "nothing to sync (clean tree)");
            self.notices.clear();
            self.push_toast("working tree clean");
            self.push_activity(
                ActivityKind::Tool,
                FeedbackSeverity::Info,
                "git sync skipped · clean tree",
            );
            if let Some(term) = terminal.as_deref_mut() {
                let _ = term.draw(|f| self.draw(f));
            }
        }

        // Stage everything.
        match self.run_git_tool("add", vec!["-A".into()]).await {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {e}"));
                return;
            }
            Ok(_) => {}
        }

        // Build a message from the staged changeset (stat + name-status + optional LLM).
        let name_status = self
            .run_git_tool("diff", vec!["--cached".into(), "--name-status".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let stat = self
            .run_git_tool("diff", vec!["--cached".into(), "--stat".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let patch_snip = self
            .run_git_tool("diff", vec!["--cached".into()])
            .await
            .map(|o| o.content.chars().take(6_000).collect::<String>())
            .unwrap_or_default();

        self.set_feedback(FeedbackSeverity::Info, "syncing… writing commit message");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let message = self
            .commit_message_from_changeset(&name_status, &stat, &patch_snip)
            .await;

        self.set_feedback(
            FeedbackSeverity::Info,
            format!("syncing… commit: {}", truncate_one_line(&message, 48)),
        );
        let commit = self
            .run_git_tool("commit", vec!["-m".into(), message.clone()])
            .await;
        match commit {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {}", o.content.trim()));
                self.push_notice(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(16)
                        .collect(),
                );
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {e}"));
                return;
            }
            Ok(o) => {
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Ok,
                    format!("git commit · {message}"),
                );
                let _ = o;
            }
        }

        self.set_feedback(FeedbackSeverity::Info, "syncing… push");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let push = self.run_git_tool("push", vec![]).await;
        self.busy_phase = BusyPhase::Idle;
        match push {
            Ok(o) if o.is_error => {
                // Commit succeeded; push failed — surface both.
                self.report_error(&format!("committed but push failed: {}", o.content.trim()));
                let mut lines = vec![format!("Committed: {message}"), "Push failed:".into()];
                lines.extend(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(12),
                );
                self.push_notice(lines);
            }
            Err(e) => {
                self.report_error(&format!("committed but push failed: {e}"));
                self.push_notice(vec![
                    format!("Committed: {message}"),
                    format!("Push error: {e}"),
                ]);
            }
            Ok(o) => {
                self.push_toast("synced");
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("synced · {}", truncate_one_line(&message, 40)),
                );
                let mut lines = vec![format!("Commit: {message}"), "Push: ok".into()];
                if !stat.trim().is_empty() {
                    lines.push(String::new());
                    lines.push("Changeset:".into());
                    for l in stat.lines().take(12) {
                        lines.push(l.to_string());
                    }
                }
                if !o.content.trim().is_empty() {
                    lines.push(String::new());
                    for l in o.content.lines().take(8) {
                        lines.push(l.to_string());
                    }
                }
                self.notices.clear();
                self.notices_until = None;
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, lines.join(" · "));
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, "git push");
            }
        }
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
    }

    /// Prefer a short model-written summary of the staged diff; fall back to a file-list heuristic.
    async fn commit_message_from_changeset(
        &self,
        name_status: &str,
        stat: &str,
        patch_snip: &str,
    ) -> String {
        let fallback = heuristic_commit_message(name_status);
        if !self.is_provider_connected() {
            return fallback;
        }
        let model_id = if self.session.active_model.is_empty() {
            self.runtime.model_label.clone()
        } else {
            self.session.active_model.clone()
        };
        let user = format!(
            "Write a single-line git commit message (max ~72 chars) for this staged change.\n\
Rules: imperative mood, no quotes, no trailing period, no conventional-commit prefix unless clearly needed.\n\
Reply with ONLY the commit message line.\n\n\
## name-status\n{name_status}\n\n\
## stat\n{stat}\n\n\
## patch (truncated)\n{patch_snip}"
        );
        let req = forge_model::ModelRequest {
            messages: vec![
                forge_types::Message::new(
                    forge_types::MessageRole::System,
                    "You write concise git commit messages from diffs.",
                ),
                forge_types::Message::new(forge_types::MessageRole::User, user),
            ],
            tools: vec![],
            model: model_id,
            reasoning_effort: Some(self.reasoning_effort.to_string())
                .filter(|value| value != "auto"),
            prompt_cache: true,
        };
        match self.session.model_client().complete(req).await {
            Ok(resp) => {
                let line = resp
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                    .trim_end_matches('.')
                    .to_string();
                if line.is_empty() || line.len() > 200 {
                    fallback
                } else {
                    line
                }
            }
            Err(_) => fallback,
        }
    }

    pub(super) fn current_run_id(&self) -> Option<String> {
        self.run.current.as_ref().map(|record| record.id.clone())
    }

    pub(super) fn run_exists(&self, id: &str) -> bool {
        self.run
            .current
            .as_ref()
            .is_some_and(|record| record.id == id)
            || self.run.recent.iter().any(|record| record.id == id)
    }
}
