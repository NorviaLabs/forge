//! Turn lifecycle, streaming and the message queue for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `drain_pending_prompt` drives one agent turn:
//! it streams model output, redraws between deltas and settles the turn's
//! chrome. The queue methods decide which prompt runs next, and the thinking
//! timers record how long a turn spent reasoning.
//!
//! Human-in-the-loop approval is in `app/approvals.rs`. Methods are moved verbatim.

use super::*;

use super::shell::drain_events;

struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    fn is_finished(&self) -> bool {
        self.0.as_ref().is_some_and(|handle| handle.is_finished())
    }

    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        self.0.take().expect("tool task handle missing").await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

impl TuiApp {
    async fn execute_tool_application(
        &mut self,
        pending: PendingToolApplication,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<ModelResponseApplication, LoopError> {
        let execution = AbortOnDrop::new(tokio::spawn(pending.execute()));
        loop {
            if execution.is_finished() {
                let completed = execution
                    .join()
                    .await
                    .map_err(|error| LoopError::Other(format!("tool task join: {error}")))?;
                return self.session.finish_tool_application(completed).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            if terminal.is_some() {
                // A tool application owns the session while it is in flight. Do not
                // re-enter the general input dispatcher here: it can enqueue prompts,
                // open overlays, or otherwise mutate session-owned state mid-apply.
                // Pending input remains in Crossterm's queue for the outer event loop.
                self.poll_interactive_terminal();
                if self.cancellation.take_requested() || self.exit.is_requested() {
                    return Err(LoopError::Cancelled);
                }
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|frame| self.draw(frame))
                        .map_err(|error| LoopError::Other(error.to_string()))?;
                }
            }
        }
    }

    async fn apply_model_response_responsive(
        &mut self,
        response: forge_types::ModelResponse,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<ApplyOutcome, LoopError> {
        let mut application = self
            .session
            .begin_model_response_application(response)
            .await?;
        loop {
            application = match application {
                ModelResponseApplication::Finished(outcome) => return Ok(outcome),
                ModelResponseApplication::Execute(pending) => {
                    self.execute_tool_application(*pending, terminal.as_deref_mut())
                        .await?
                }
            };
        }
    }

    /// Close the thinking clock. Prefer wall time from first thinking token;
    /// if that is ~0 (same-batch non-stream dump), fall back to full turn elapsed.
    fn close_thinking_timer(&mut self) {
        if self.timing.thought_secs.is_some() {
            return;
        }
        if self.stream.thinking.is_empty() {
            return;
        }
        let from_think = self
            .timing
            .thinking_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let from_turn = self
            .timing
            .started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        // Same-batch dump of all thinking+answer → thinking_started ≈ now; use turn time.
        let secs = if from_think < 0.15 && from_turn > from_think {
            from_turn
        } else if from_think > 0.0 {
            from_think
        } else {
            from_turn
        };
        self.timing.thought_secs = Some(secs);
    }

    /// Apply one provider stream event to session state and live preview chrome.
    /// Returns `Some(error)` when the stream reports a terminal failure.
    fn handle_stream_event(
        &mut self,
        event: &ModelStreamEvent,
        acc: &mut ModelStepAccumulator,
    ) -> Option<String> {
        observe_stream_event(&mut self.session, event, None, acc);
        match event {
            ModelStreamEvent::TextDelta { text } => {
                self.close_thinking_timer();
                self.stream.preview.push_str(text);
            }
            ModelStreamEvent::ThinkingDelta { text } => {
                if self.timing.thinking_started.is_none() {
                    self.timing.thinking_started =
                        self.timing.started.or_else(|| Some(Instant::now()));
                }
                self.stream.thinking.push_str(text);
            }
            ModelStreamEvent::ToolCallStart { name, .. } => {
                self.busy_state
                    .set_phase(BusyPhase::Tool { name: name.clone() });
            }
            ModelStreamEvent::Error { message } => return Some(message.clone()),
            _ => {}
        }
        None
    }

    fn persist_turn_thinking_duration(&mut self, secs: f64) {
        if let Some(m) = self
            .session
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == forge_types::MessageRole::Assistant)
        {
            if m.thinking.is_some() {
                m.thinking_duration_secs = Some(secs);
            }
        }
    }

    fn record_interrupted_stream(&mut self, error: &str) {
        let text = self.stream.preview.trim_end().to_string();
        if !text.is_empty() {
            self.session.messages.push(Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: format!("{text}\n\n[Interrupted: {error}]"),
                tool_call_id: None,
                name: None,
                thinking: (!self.stream.thinking.trim().is_empty())
                    .then(|| self.stream.thinking.clone()),
                thinking_duration_secs: self.timing.thought_secs,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            });
        }
        self.set_feedback(
            FeedbackSeverity::Warn,
            "Response interrupted · Retry or Continue",
        );
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Warn,
            format!("response interrupted: {error}"),
        );
    }

    /// Enqueue while a message is processing (TUI Enter path only). Does not
    /// claim success (or clear the composer's caller-held text) until the
    /// queue store durably accepts the item.
    pub(super) async fn enqueue_user_message(&mut self, line: String) {
        match self.session.enqueue_task(&line).await {
            Ok(_item) => {
                let n = self.session.queue().len();
                self.task_selection.ensure_queue();
                self.push_toast(format!("queued #{n}"));
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!(
                        "queued #{n} · {n} waiting · Ctrl+Up/Down select · Ctrl+Backspace cancel"
                    ),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Info,
                    format!("queue enqueue #{n}"),
                );
            }
            Err(e) => {
                // Preserve the typed message rather than dropping it silently.
                self.input.set_text(line);
                self.report_error(&format!("Could not queue the instruction: {e}"));
            }
        }
    }

    /// Atomically promote the oldest queued item into a new task and start
    /// driving its turn. A thin wrapper over `AgentSession::promote_next_queued`
    /// — the queue store owns the atomic Queued->Promoting->Promoted pipeline;
    /// this only decides when to call it and how to kick off streaming.
    pub(super) async fn dequeue_and_send_next(&mut self) {
        if self.busy_state.is_active() || self.pending_turn.has_prompt() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "still processing — wait before sending the next queued message",
            );
            return;
        }
        if self.session.pending_hitl().is_some() {
            self.set_feedback(FeedbackSeverity::Warn, "resolve HITL before dequeuing");
            return;
        }
        if !self.is_provider_connected() {
            let msg = format!(
                "{} · cannot send queued message",
                self.disconnected_message()
            );
            self.report_error(&msg);
            return;
        }
        match self.session.promote_next_queued().await {
            Ok(Some(_task_id)) => {
                self.clamp_queue_selection();
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Info,
                    format!("queue dequeue · {} left", self.session.queue().len()),
                );
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!(
                        "sending dequeued · {} remaining",
                        self.session.queue().len()
                    ),
                );
                // Start the turn the same way as a normal Enter send (no dispatch
                // recursion). The user message was already appended by
                // `promote_next_queued`, so this continues the turn rather than
                // appending again — the same mechanism used to resume after the
                // turn-limit overlay.
                self.clear_error_chrome();
                if let Some(pid) = self.connect.profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                self.pending_turn.request_continue();
                self.busy_state.start(BusyPhase::Model);
                self.timing.started = Some(Instant::now());
                self.stream.preview.clear();
                self.stream.thinking.clear();
                self.push_activity(
                    ActivityKind::Model,
                    FeedbackSeverity::Info,
                    "model call started",
                );
            }
            Ok(None) => {
                self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            }
            Err(e) => {
                self.report_error(&format!("Could not start the next queued task: {e}"));
            }
        }
    }

    /// Cancel a queued message by 0-based visible-position index.
    async fn cancel_queued_at(&mut self, index: usize) {
        let one_based = index + 1;
        match self.session.cancel_queued_at(one_based).await {
            Ok(Some(item)) => {
                let preview: String = item.text.chars().take(48).collect();
                self.push_toast(format!("cancelled #{one_based}"));
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!(
                        "cancelled queued #{one_based} · {} left",
                        self.session.queue().len()
                    ),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Ok,
                    format!("queue cancel #{one_based}: {preview}"),
                );
                self.clamp_queue_selection();
            }
            Ok(None) => {
                self.set_feedback(FeedbackSeverity::Warn, "queue item gone");
            }
            Err(e) => {
                self.report_error(&format!("Could not cancel the queued item: {e}"));
            }
        }
    }

    fn clamp_queue_selection(&mut self) {
        self.task_selection.clamp_queue(self.session.queue().len());
    }

    pub(super) fn move_queue_selection(&mut self, delta: i32) {
        self.task_selection
            .move_queue(self.session.queue().len(), delta);
    }

    pub(super) async fn cancel_selected_queue(&mut self) {
        let Some(idx) = self.task_selection.queue() else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.cancel_queued_at(idx).await;
    }

    /// Non-blocking; call once per tick (mirrors `git_status.poll()`, just
    /// async since finishing a background task journals a result). Toasts
    /// each task that newly reached a terminal state this tick.
    pub(super) async fn poll_background_tasks(&mut self) -> Result<(), TuiError> {
        let running_before: std::collections::HashSet<_> = self
            .session
            .background()
            .list()
            .filter(|t| !t.status.is_terminal())
            .map(|t| t.id)
            .collect();
        self.session.poll_background_tasks().await?;
        for id in running_before {
            if let Some(task) = self.session.background().get(id) {
                if task.status.is_terminal() {
                    self.push_toast(format!(
                        "background task #{} finished: {}",
                        id.0, task.label
                    ));
                }
            }
        }
        Ok(())
    }

    fn clamp_tasks_selection(&mut self) {
        self.task_selection
            .clamp_tasks(self.session.background().list().count());
    }

    pub(super) fn move_tasks_selection(&mut self, delta: i32) {
        self.task_selection
            .move_tasks(self.session.background().list().count(), delta);
    }

    /// Cancel the background task at the currently selected row. Rows are
    /// sorted by id ascending, matching `tasks_lines`'s render order.
    pub(super) async fn cancel_selected_task(&mut self) {
        let Some(idx) = self.task_selection.task() else {
            self.set_feedback(FeedbackSeverity::Warn, "no background tasks");
            return;
        };
        let mut ids: Vec<_> = self.session.background().list().map(|t| t.id).collect();
        ids.sort_by_key(|id| id.0);
        let Some(id) = ids.get(idx).copied() else {
            self.clamp_tasks_selection();
            return;
        };
        if self.session.cancel_background_task(id) {
            self.set_feedback(FeedbackSeverity::Ok, format!("cancelling task #{}", id.0));
            self.push_activity(
                ActivityKind::System,
                FeedbackSeverity::Ok,
                format!("background task cancel #{}", id.0),
            );
        } else {
            self.set_feedback(FeedbackSeverity::Warn, "task already finished");
        }
        self.clamp_tasks_selection();
    }

    /// Approve/deny whatever the currently selected background task is
    /// waiting on. A no-op (with feedback) if nothing is selected or the
    /// selected task isn't actually waiting — e.g. it finished between the
    /// last redraw and this keypress, a race the selection index alone
    /// can't detect, which is exactly why `resolve_subagent_hitl` reports
    /// success/failure rather than being fire-and-forget.
    pub(super) fn resolve_selected_task_hitl(&mut self, decision: HitlDecision) {
        let Some(idx) = self.task_selection.task() else {
            self.set_feedback(FeedbackSeverity::Warn, "no background tasks");
            return;
        };
        let mut ids: Vec<_> = self.session.background().list().map(|t| t.id).collect();
        ids.sort_by_key(|id| id.0);
        let Some(id) = ids.get(idx).copied() else {
            return;
        };
        let verb = match decision {
            HitlDecision::Approve => "approve",
            HitlDecision::Deny => "deny",
            _ => "deny",
        };
        if self.session.resolve_subagent_hitl(id, decision) {
            self.set_feedback(
                FeedbackSeverity::Ok,
                format!("{verb} sent to task #{}", id.0),
            );
            self.push_activity(
                ActivityKind::System,
                FeedbackSeverity::Ok,
                format!("background task #{} {verb}d", id.0),
            );
        } else {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("task #{} isn't waiting for approval", id.0),
            );
        }
    }

    /// Run a queued user prompt with streaming + intermediate redraws.
    /// When `terminal` is `None` (unit tests), runs without intermediate draws.
    pub async fn drain_pending_prompt(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let (line, continuing, attachments) = self.pending_turn.take();
        if line.is_none() && !continuing {
            return Ok(());
        }

        // Refresh OAuth close to expiry and recycle the worker with the current token.
        if let Some(profile_id) = self.connect.profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }

        self.busy_state.start(BusyPhase::Model);
        self.stream.preview.clear();
        self.stream.thinking.clear();
        self.stream.live_lines = None;
        self.timing.started.get_or_insert_with(Instant::now);
        self.timing.thinking_started = None;
        self.timing.thought_secs = None;

        if let Some(ref line) = line {
            if let Err(e) = self
                .session
                .append_user_message_with_attachments(line, attachments)
                .await
            {
                self.busy_state.stop();
                self.report_error(&e.to_string());
                self.exit.set_code(ExitCode::Failed);
                return Ok(());
            }
        }

        // Paint YOU message immediately
        if let Some(term) = terminal.as_deref_mut() {
            term.draw(|f| self.draw(f))?;
        }

        self.sync_effort_to_session();

        let max_turns = self.session.max_turns();
        let mut outcome_err: Option<String> = None;
        let mut turn_cancelled = false;
        let mut turn_thought_secs = 0.0f64;
        let mut saw_thinking = false;

        'turns: for turn in 0..max_turns {
            let req = match self.session.prepare_model_step(turn).await {
                Ok(r) => r,
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            };

            let model = self.session.model_client();
            let (tx, rx) = std::sync::mpsc::channel::<ModelStreamEvent>();
            let handle =
                tokio::spawn(async move { model.complete_with_stream(req, Some(tx)).await });

            let mut step_acc = ModelStepAccumulator::default();
            // A provider can outpace terminal rendering. Bound normal-tick
            // processing so a large burst cannot starve input, cancellation,
            // or the next paint; the completion path below still drains every
            // remaining event before the final response is applied.
            const MAX_STREAM_EVENTS_PER_TICK: usize = 256;
            // Pump stream events + redraw until the model call finishes
            loop {
                if self.cancellation.is_requested() {
                    handle.abort();
                    self.cancellation.clear();
                    self.timing.started = None;
                    turn_cancelled = true;
                    outcome_err = Some("cancelled".into());
                    break 'turns;
                }
                for _ in 0..MAX_STREAM_EVENTS_PER_TICK {
                    let Ok(ev) = rx.try_recv() else {
                        break;
                    };
                    if let Some(message) = self.handle_stream_event(&ev, &mut step_acc) {
                        handle.abort();
                        outcome_err = Some(message);
                        break 'turns;
                    }
                }
                // Keep the terminal responsive while the current turn is streaming so
                // the operator can type the next message and enqueue it with Enter.
                //
                // This runs BEFORE the repaint below. Draining afterwards meant a
                // keystroke arriving during the 100ms sleep was not handled until the
                // next iteration, and so was not painted until the iteration after
                // that -- roughly 200ms plus two draws from keypress to glyph.
                if terminal.is_some() {
                    drain_events(self, terminal.as_deref_mut()).await?;
                    self.poll_interactive_terminal();
                    if self.exit.is_requested() {
                        handle.abort();
                        self.busy_state.stop();
                        self.stream.preview.clear();
                        self.stream.thinking.clear();
                        self.timing.started = None;
                        self.timing.thinking_started = None;
                        self.timing.thought_secs = None;
                        self.exit.set_code(ExitCode::Canceled);
                        let _ = self.session.mark_cancelled().await;
                        return Ok(());
                    }
                }

                // Redraw every tick so spinner and elapsed time stay current, and so
                // input drained above lands in this frame rather than the next one.
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }

                if handle.is_finished() {
                    // Drain remaining events
                    while let Ok(ev) = rx.try_recv() {
                        if let Some(message) = self.handle_stream_event(&ev, &mut step_acc) {
                            outcome_err = Some(message);
                            break 'turns;
                        }
                    }
                    // Thinking-only or late thinking dump: close the clock now
                    self.close_thinking_timer();
                    if let Some(term) = terminal.as_deref_mut() {
                        term.draw(|f| self.draw(f))?;
                    }
                    break;
                }

                // ~10 Hz keeps the timer + spinner smooth without burning CPU
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let mut last = match handle.await {
                Ok(Ok(r)) => merge_streamed_response(r, &step_acc),
                Ok(Err(e)) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
                Err(e) => {
                    outcome_err = Some(format!("model task join: {e}"));
                    break;
                }
            };

            // Provider may attach reasoning only on the final object (no stream deltas).
            if self.stream.thinking.is_empty() {
                if let Some(ref th) = last.thinking {
                    if !th.is_empty() {
                        if self.timing.thinking_started.is_none() {
                            self.timing.thinking_started = self.timing.started;
                        }
                        self.stream.thinking = th.clone();
                        self.close_thinking_timer();
                        // One paint so the user can see thinking before collapse
                        if let Some(term) = terminal.as_deref_mut() {
                            let _ = term.draw(|f| self.draw(f));
                        }
                    }
                }
            } else if last.thinking.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                // Prefer streamed thinking body on the final message
                last.thinking = Some(self.stream.thinking.clone());
            }
            self.close_thinking_timer();

            let thought = self.timing.thought_secs.take();
            self.stream.preview.clear();
            self.stream.thinking.clear();
            self.stream.live_lines = None;
            // Keep turn_started until full agent turn ends (multi-tool steps).
            if let Some(call) = last.tool_calls.first() {
                self.busy_state.set_phase(BusyPhase::Tool {
                    name: call.name.clone(),
                });
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Info,
                    format!("tool_intent {}", call.name),
                );
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }
            }
            match self
                .apply_model_response_responsive(last, terminal.as_deref_mut())
                .await
            {
                Ok(out) => {
                    if let Some(secs) = thought {
                        saw_thinking = true;
                        turn_thought_secs += secs;
                    }
                    // Reset per-model-step thinking timers for multi-tool loops.
                    self.timing.thinking_started = None;
                    self.timing.thought_secs = None;
                    match out {
                        ApplyOutcome::Done(_) | ApplyOutcome::Hitl(_) => {
                            outcome_err = None;
                            self.maybe_note_workspace_changed_from_recent_tools();
                            break 'turns;
                        }
                        ApplyOutcome::Continue => {
                            self.busy_state.set_phase(BusyPhase::Model);
                            if let Some(term) = terminal.as_deref_mut() {
                                term.draw(|f| self.draw(f))?;
                            }
                            continue;
                        }
                        // `ApplyOutcome` is `#[non_exhaustive]`. Treat an outcome this
                        // build does not recognise as terminal for the turn rather than
                        // looping: an unknown outcome must not drive another model call.
                        _ => {
                            outcome_err = None;
                            self.maybe_note_workspace_changed_from_recent_tools();
                            break 'turns;
                        }
                    }
                }
                Err(e) => {
                    turn_cancelled = matches!(e, LoopError::Cancelled);
                    outcome_err = Some(e.to_string());
                    break;
                }
            }
        }

        let turn_limit_reached = outcome_err.is_none()
            && self.session.active_task.lifecycle == forge_types::TaskLifecycle::Working;
        let interrupted_partial = outcome_err
            .as_ref()
            .filter(|_| !self.stream.preview.trim().is_empty())
            .cloned();

        self.busy_state.stop();

        if turn_limit_reached {
            self.stream.preview.clear();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            self.overlay = Some(Overlay::turn_limit(max_turns));
            self.exit.set_code(ExitCode::Success);
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("{max_turns} steps reached — continue?"),
            );
            self.push_activity(
                ActivityKind::Model,
                FeedbackSeverity::Warn,
                "turn limit reached",
            );
        } else if let Some(e) = outcome_err {
            let was_cancel = turn_cancelled;
            if let Some(interrupted) = interrupted_partial {
                self.record_interrupted_stream(&interrupted);
            } else if was_cancel {
                self.push_toast("cancelled");
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Info,
                    "turn cancelled",
                );
            } else {
                self.report_error(&e);
            }
            self.stream.preview.clear();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            if was_cancel {
                self.exit.set_code(ExitCode::Canceled);
                if let Err(err) = self.session.mark_cancelled().await {
                    self.report_error(&err.to_string());
                }
            } else {
                self.exit.set_code(ExitCode::Failed);
                // This error path means the turn ended without ever calling
                // `apply_model_response` successfully (a provider/HTTP error,
                // a join error, or `apply_model_response` itself returning
                // `Err` before its own transition logic ran) — nothing else
                // has moved the session lifecycle out of `Working`. Left
                // alone, the header sticks on "Working" and the message
                // queue's dispatch gate (which only checks lifecycle) never
                // reopens, even across a provider switch, until the process
                // is restarted.
                if let Err(err) = self.session.mark_model_call_failed(&e).await {
                    self.report_error(&err.to_string());
                }
            }
            // Leave queue intact so the operator can fix and continue.
        } else if self.session.pending_hitl().is_some() {
            self.stream.preview.clear();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            self.exit.set_code(ExitCode::AwaitingHitl);
            self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
            self.push_activity(ActivityKind::Hitl, FeedbackSeverity::Warn, "hitl waiting");
            // Do not auto-dequeue until HITL is resolved.
        } else {
            self.stream.preview.clear();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            if saw_thinking {
                self.persist_turn_thinking_duration(turn_thought_secs);
            }
            self.clear_error_chrome();
            self.tool_detail.collapse();
            if self.session.queue().is_empty() {
                self.feedback = FeedbackModel::default();
                self.status_state.message.clear();
            } else {
                self.push_toast(format!(
                    "{} queued · sending next",
                    self.session.queue().len()
                ));
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("{} in queue — sending next", self.session.queue().len()),
                );
            }
            self.push_activity(ActivityKind::Model, FeedbackSeverity::Ok, "model ok");
            if !self.session.queue().is_empty() {
                self.dequeue_and_send_next().await;
            }
        }
        Ok(())
    }
}
