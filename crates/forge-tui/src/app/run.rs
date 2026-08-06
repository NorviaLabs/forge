//! Command runs and validation for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Covers the Run panel's lifecycle — drafting,
//! launching, polling and cancelling a command, and capturing its output — and
//! the post-run validation drain. Methods are moved verbatim.

use super::*;

/// Live-run execution observation: output/exit polling and cancellation for
/// whichever Run-panel process is currently executing, plus whether its
/// post-run validation is queued to drain on the event loop.
///
/// `pending_validation` and `rx` are read from `app/shell.rs` and set up by
/// test fixtures outside this module, so they're `pub(super)`. `abort` has no
/// caller outside `app/run.rs` itself, so it stays fully private -- the one
/// field in this cluster where the module boundary is real, not just a name.
#[derive(Default)]
pub(super) struct RunExecution {
    pub(super) pending_validation: bool,
    pub(super) rx: Option<std::sync::mpsc::Receiver<RunEvent>>,
    abort: Option<tokio::task::JoinHandle<()>>,
}

impl TuiApp {
    pub(super) fn normalize_restored_run(&mut self) {
        if let Some(record) = self.run.current.as_mut() {
            if matches!(record.state, RunState::Running | RunState::Queued) {
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
            }
        }
        self.run_execution.execution.pending_validation = false;
        self.run_execution.execution.rx = None;
        self.run_execution.execution.abort = None;
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
        self.run_execution.execution.pending_validation = true;
        self.busy_state.phase = BusyPhase::Tool { name: "run".into() };
        self.status_state.message = format!("run: {}", invocation.summary());
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
                if let Some(handle) = self.run_execution.execution.abort.take() {
                    handle.abort();
                }
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
                record.duration = record.started_at.and_then(|start| {
                    record
                        .finished_at
                        .and_then(|end| end.duration_since(start).ok())
                });
                self.run_execution.execution.rx = None;
                self.run_execution.execution.pending_validation = false;
                self.busy_state.phase = BusyPhase::Idle;
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
        if !self.run_execution.execution.pending_validation
            || !self
                .run
                .current
                .as_ref()
                .is_some_and(|record| record.state == RunState::Running)
        {
            return Ok(());
        }
        self.run_execution.execution.pending_validation = false;
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
        self.run_execution.execution.rx = Some(rx);
        self.run_execution.execution.abort = Some(tokio::spawn(async move {
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
        let Some(rx) = self.run_execution.execution.rx.take() else {
            return;
        };
        const MAX_EVENTS_PER_POLL: usize = 64;
        let mut output = Vec::new();
        let mut event = Err(std::sync::mpsc::TryRecvError::Empty);
        for _ in 0..MAX_EVENTS_PER_POLL {
            match rx.try_recv() {
                Ok(RunEvent::Output(chunk)) => output.extend(chunk),
                other => {
                    event = other;
                    break;
                }
            }
        }
        if !output.is_empty() {
            self.append_terminal_output(&output);
        }
        if matches!(event, Err(std::sync::mpsc::TryRecvError::Empty)) {
            self.run_execution.execution.rx = Some(rx);
            return;
        }
        match event {
            Ok(RunEvent::Output(_)) | Err(std::sync::mpsc::TryRecvError::Empty) => unreachable!(),
            Ok(RunEvent::Finished { exit_code, success }) => {
                self.run_execution.execution.abort = None;
                self.run_execution.execution.pending_validation = false;
                self.busy_state.phase = BusyPhase::Idle;
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
                self.run_execution.execution.abort = None;
                self.run_execution.execution.pending_validation = false;
                self.busy_state.phase = BusyPhase::Idle;
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
                self.run_execution.execution.abort = None;
                self.run_execution.execution.pending_validation = false;
                self.busy_state.phase = BusyPhase::Idle;
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
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.run_execution.execution.abort = None;
                self.run_execution.execution.pending_validation = false;
                self.busy_state.phase = BusyPhase::Idle;
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
