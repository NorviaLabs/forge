//! Turn lifecycle, streaming and the message queue for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `drain_pending_prompt` drives one agent turn:
//! it streams model output, redraws between deltas and settles the turn's
//! chrome. The queue methods decide which prompt runs next, and the thinking
//! timers record how long a turn spent reasoning.
//!
//! Human-in-the-loop approval is in `app/approvals.rs`. Methods are moved verbatim.

use super::*;

use forge_model::ModelError;

use super::shell::{
    next_foreground_wake, paint_foreground_frame, render_foreground_wake, tick_foreground_frame,
};

impl TuiApp {
    async fn execute_tool_application<B: ratatui::backend::Backend>(
        &mut self,
        pending: PendingToolApplication,
        mut terminal: Option<&mut Terminal<B>>,
    ) -> Result<ModelResponseApplication, LoopError> {
        let execution = IsolatedTask::spawn(pending.execute());
        let mut ui_tick = tokio::time::interval(Duration::from_millis(100));
        ui_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if execution.is_finished() {
                let completed = execution
                    .join()
                    .await
                    .map_err(|error| LoopError::Other(format!("tool task join: {error}")))?;
                let Some(completed) = completed else {
                    return Err(LoopError::Cancelled);
                };
                return self.session.finish_tool_application(completed).await;
            }
            // Foreground execution owns only its detached tool data. The full TUI
            // application keeps ticking around it: input, file watching, background
            // tasks, approvals, connection polling and transient chrome all advance.
            tick_foreground_frame(self, terminal.as_deref_mut(), &mut ui_tick)
                .await
                .map_err(|error| LoopError::Other(error.to_string()))?;
            if self.cancellation.take_requested() || self.exit.is_requested() {
                return Err(LoopError::Cancelled);
            }
        }
    }

    async fn apply_model_response_responsive<B: ratatui::backend::Backend>(
        &mut self,
        response: forge_types::ModelResponse,
        mut terminal: Option<&mut Terminal<B>>,
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
                self.timing.chars += text.chars().count();
                self.stream.preview.push_str(text);
            }
            ModelStreamEvent::ThinkingDelta { text } => {
                if self.timing.thinking_started.is_none() {
                    self.timing.thinking_started =
                        self.timing.started.or_else(|| Some(Instant::now()));
                }
                self.timing.chars += text.chars().count();
                self.stream.thinking.push_str(text);
            }
            ModelStreamEvent::ToolCallStart { name, .. } => {
                self.timing.tools += 1;
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

    /// Close a finished turn with a line saying how long it took and what it
    /// cost. Without one the transcript simply stopped: nothing marked the
    /// bottom edge of a turn, and nothing recorded its duration.
    ///
    /// Only one summary is ever on screen — the previous turn's is replaced,
    /// so the transcript doesn't fill with receipts.
    pub(super) fn record_turn_summary(&mut self) {
        let Some(started) = self.timing.turn_started else {
            return;
        };
        self.banner_state
            .items
            .retain(|item| !matches!(item, ChatItem::TurnSummary { .. }));
        // The provider's own count for this turn: cumulative session usage
        // less what it stood at when the turn began. `None` when the provider
        // reported no usage, so the summary omits a rate rather than printing
        // a confident zero.
        let produced = self
            .session
            .token_usage_report()
            .api
            .completion_tokens
            .saturating_sub(self.timing.completion_tokens_at_start);
        let secs = started.elapsed().as_secs_f64();
        let chars = self.timing.chars;
        let tools = self.timing.tools;
        let output_tokens = (produced > 0).then_some(produced);
        self.banner_state.items.push(ChatItem::TurnSummary {
            secs,
            chars,
            tools,
            output_tokens,
        });
        // Archive by turn ordinal (not list position — a cancelled or still-
        // paused turn never reaches this function, so position would desync
        // against `turn_boundaries()`). The ordinal is exactly "how many
        // real turns are visible right now," since this turn's own User
        // message is already in the transcript by the time it completes.
        let all_messages = self.transcript_view.messages();
        let visible_messages =
            &all_messages[self.conversation_view.message_start.min(all_messages.len())..];
        let turn_count = visible_messages
            .iter()
            .filter(|m| {
                m.role == forge_types::MessageRole::User && !m.content.starts_with("[REPAIR TASK")
            })
            .count();
        if let Some(ordinal) = turn_count.checked_sub(1) {
            self.turn_stats.insert(
                ordinal,
                forge_transcript::TurnStats {
                    secs,
                    chars,
                    tools,
                    output_tokens,
                },
            );
        }
    }

    /// How many times one model step may be re-issued after a transient
    /// failure. Small on purpose: this covers a blip, not an outage.
    const MAX_MODEL_RETRIES: usize = 2;

    /// A step may be retried only when the provider failed in a way that
    /// looks transient *and* nothing of the answer has been shown yet —
    /// re-issuing after partial output would duplicate text the reader has
    /// already seen.
    fn can_retry_model_call(&self, error: &ModelError, attempts: usize) -> bool {
        error.is_retryable()
            && attempts < Self::MAX_MODEL_RETRIES
            && self.stream.preview.is_empty()
            && self.stream.thinking.is_empty()
            && !self.cancellation.is_requested()
            && !self.exit.is_requested()
    }

    /// Wait out the backoff before re-issuing a step, counting down in the
    /// live turn line. Returns false if the operator interrupted the wait.
    ///
    /// Silence here is indistinguishable from a hang, which is the whole
    /// reason this is on screen rather than in the activity log.
    async fn await_model_retry<B: ratatui::backend::Backend>(
        &mut self,
        attempt: usize,
        error: &ModelError,
        mut terminal: Option<&mut Terminal<B>>,
    ) -> Result<bool, TuiError> {
        let wait = Duration::from_secs(1 << (attempt - 1));
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Warn,
            format!(
                "model call failed ({error}) — retry {attempt} in {}s",
                wait.as_secs()
            ),
        );
        let until = Instant::now() + wait;
        let mut ui_tick = tokio::time::interval(Duration::from_millis(100));
        ui_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while Instant::now() < until {
            if self.cancellation.take_requested() || self.exit.is_requested() {
                self.busy_state.set_phase(BusyPhase::Model);
                return Ok(false);
            }
            let left = until.saturating_duration_since(Instant::now()).as_secs() + 1;
            self.busy_state.set_phase(BusyPhase::Other(format!(
                "retrying in {left}s · attempt {} of {}",
                attempt + 1,
                Self::MAX_MODEL_RETRIES + 1
            )));
            tick_foreground_frame(self, terminal.as_deref_mut(), &mut ui_tick).await?;
        }
        self.busy_state.set_phase(BusyPhase::Model);
        Ok(true)
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
        if self.session.pending_question().is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "answer the question before dequeuing",
            );
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
                self.timing.turn_started.get_or_insert_with(Instant::now);
                self.stream.clear_preview();
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
        terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        self.drain_pending_prompt_with_terminal(terminal).await
    }

    async fn drain_pending_prompt_with_terminal<B: ratatui::backend::Backend>(
        &mut self,
        mut terminal: Option<&mut Terminal<B>>,
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
        self.stream.clear_preview();
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
            term.draw(|f| self.draw(f))
                .map_err(|error| TuiError::Other(error.to_string()))?;
        }

        self.sync_effort_to_session();

        let max_turns = self.session.max_turns();
        let mut outcome_err: Option<String> = None;
        let mut turn_cancelled = false;
        let mut turn_thought_secs = 0.0f64;
        let mut saw_thinking = false;

        // Transient provider failures used to end the turn outright: forge
        // classified errors as retryable and then never retried one, so a blip
        // cost the whole turn. Counted per step and reset on success.
        let mut model_retries = 0usize;
        'turns: for turn in 0..max_turns {
            if let Some(pending) = self.session.begin_auto_context_compaction() {
                let completed = self
                    .execute_context_compaction_responsive(pending, terminal.as_deref_mut())
                    .await?;
                let Some(completed) = completed else {
                    turn_cancelled = true;
                    outcome_err = Some("cancelled".into());
                    break;
                };
                // Automatic compaction is opportunistic. A failed checkpoint
                // leaves the old context installed and the model turn remains
                // valid, matching AgentSession's non-interactive path.
                let _ = self.session.finish_context_compaction(completed).await;
            }
            let req = match self.session.prepare_model_step_after_compaction(turn).await {
                Ok(r) => r,
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            };

            let model = self.session.model_client();
            let (tx, provider_rx) = std::sync::mpsc::channel::<ModelStreamEvent>();
            let (stream_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let relay = tokio::task::spawn_blocking(move || {
                while let Ok(event) = provider_rx.recv() {
                    if stream_tx.send(event).is_err() {
                        break;
                    }
                }
            });
            let mut handle =
                IsolatedTask::spawn(async move { model.complete_with_stream(req, Some(tx)).await });
            let mut ui_tick = tokio::time::interval(Duration::from_millis(100));
            ui_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut step_acc = ModelStepAccumulator::default();
            // A provider can outpace terminal rendering. Bound normal-tick
            // processing so a large burst cannot starve input, cancellation,
            // or the next paint; the completion path below still drains every
            // remaining event before the final response is applied.
            const MAX_STREAM_EVENTS_PER_TICK: usize = 256;
            let mut stream_open = true;
            // Pump stream events + redraw until the model call finishes
            loop {
                if self.cancellation.is_requested() {
                    handle.abort();
                    self.cancellation.clear();
                    self.timing.started = None;
                    self.timing.turn_started = None;
                    turn_cancelled = true;
                    outcome_err = Some("cancelled".into());
                    break 'turns;
                }
                tokio::select! {
                    event = rx.recv(), if stream_open => {
                        if let Some(event) = event {
                            if let Some(message) = self.handle_stream_event(&event, &mut step_acc) {
                                handle.abort();
                                outcome_err = Some(message);
                                break 'turns;
                            }
                            for _ in 1..MAX_STREAM_EVENTS_PER_TICK {
                                let Ok(event) = rx.try_recv() else {
                                    break;
                                };
                                if let Some(message) = self.handle_stream_event(&event, &mut step_acc) {
                                    handle.abort();
                                    outcome_err = Some(message);
                                    break 'turns;
                                }
                            }
                            // Stream arrival is itself the wake source: paint the new
                            // prefix now instead of waiting for an arbitrary poll delay.
                            paint_foreground_frame(self, terminal.as_deref_mut(), false).await?;
                        } else {
                            stream_open = false;
                        }
                    }
                    wake = next_foreground_wake(self, &mut ui_tick) => {
                        render_foreground_wake(self, terminal.as_deref_mut(), wake?).await?;
                    }
                }
                if self.exit.is_requested() {
                    handle.abort();
                    self.busy_state.stop();
                    self.stream.clear_preview();
                    self.stream.thinking.clear();
                    self.timing.started = None;
                    self.timing.turn_started = None;
                    self.timing.thinking_started = None;
                    self.timing.thought_secs = None;
                    self.exit.set_code(ExitCode::Canceled);
                    let _ = self.session.mark_cancelled().await;
                    return Ok(());
                }

                if handle.is_finished() {
                    relay
                        .await
                        .map_err(|error| TuiError::Other(format!("stream relay join: {error}")))?;
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
                        term.draw(|f| self.draw(f))
                            .map_err(|error| TuiError::Other(error.to_string()))?;
                    }
                    break;
                }
            }

            let mut last = match handle.join().await {
                Ok(Some(Ok(r))) => {
                    model_retries = 0;
                    merge_streamed_response(r, &step_acc)
                }
                Ok(Some(Err(e))) if self.can_retry_model_call(&e, model_retries) => {
                    model_retries += 1;
                    self.stream.clear_preview();
                    self.stream.thinking.clear();
                    if self
                        .await_model_retry(model_retries, &e, terminal.as_deref_mut())
                        .await?
                    {
                        continue 'turns;
                    }
                    turn_cancelled = true;
                    outcome_err = Some("cancelled".into());
                    break;
                }
                Ok(Some(Err(e))) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
                Ok(None) => {
                    turn_cancelled = true;
                    outcome_err = Some("cancelled".into());
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
            self.stream.clear_preview();
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
                    term.draw(|f| self.draw(f))
                        .map_err(|error| TuiError::Other(error.to_string()))?;
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
                                term.draw(|f| self.draw(f))
                                    .map_err(|error| TuiError::Other(error.to_string()))?;
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
            self.stream.clear_preview();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.turn_started = None;
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
            self.stream.clear_preview();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.turn_started = None;
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
            self.stream.clear_preview();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.turn_started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            self.exit.set_code(ExitCode::AwaitingHitl);
            self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
            self.push_activity(ActivityKind::Hitl, FeedbackSeverity::Warn, "hitl waiting");
            // Do not auto-dequeue until HITL is resolved.
        } else if self.session.pending_question().is_some() {
            self.stream.clear_preview();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.turn_started = None;
            self.timing.thinking_started = None;
            self.timing.thought_secs = None;
            self.exit.set_code(ExitCode::AwaitingHitl);
            self.set_feedback(FeedbackSeverity::Warn, "awaiting your answer");
            self.push_activity(
                ActivityKind::Hitl,
                FeedbackSeverity::Warn,
                "question waiting",
            );
        } else {
            self.record_turn_summary();
            self.stream.clear_preview();
            self.stream.thinking.clear();
            self.timing.started = None;
            self.timing.turn_started = None;
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

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    use async_trait::async_trait;
    use crossterm::event::{KeyEvent, KeyEventState};
    use forge_core::LoopConfig;
    use forge_model::{MockModelClient, ModelClient, ModelError, ModelRequest};
    use forge_tools::{BashTool, Tool, ToolError, ToolRegistry};
    use forge_types::{ModelResponse, SideEffectClass, ToolCall, ToolOutput};
    use ratatui::backend::TestBackend;
    use serde_json::{json, Value};

    struct BlockingTool;
    struct BlockingModel;

    #[tokio::test(flavor = "current_thread")]
    async fn queued_key_burst_is_not_rate_limited_by_the_animation_tick() {
        crate::app::tests::helpers::isolate_global_skills();
        let workspace = tempfile::tempdir().unwrap();
        let session = AgentSession::create(
            LoopConfig {
                workspace: workspace.path().to_path_buf(),
                journal_dir: workspace.path().join("j"),
                ..Default::default()
            },
            Arc::new(MockModelClient::script(Vec::new())),
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let mut app = TuiApp::new(session, crate::app::tests::helpers::test_runtime_config());
        app.enter_chat_composer();
        for _ in 0..160 {
            app.test_events.push_back(Event::Key(KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }));
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut ui_tick = tokio::time::interval(Duration::from_secs(1));
        ui_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let started = Instant::now();
        while !app.test_events.is_empty() {
            tick_foreground_frame(&mut app, Some(&mut terminal), &mut ui_tick)
                .await
                .unwrap();
        }

        assert_eq!(app.input.text.len(), 160);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "ready key events waited for the animation tick: {:?}",
            started.elapsed()
        );
    }

    #[async_trait]
    impl ModelClient for BlockingModel {
        async fn complete(&self, _req: ModelRequest) -> Result<ModelResponse, ModelError> {
            std::thread::sleep(Duration::from_secs(1));
            Ok(ModelResponse {
                text: "not reached before cancellation".into(),
                tool_calls: Vec::new(),
                usage: None,
                thinking: None,
            })
        }
    }

    #[async_trait]
    impl Tool for BlockingTool {
        fn name(&self) -> &str {
            "blocking_test"
        }

        fn description(&self) -> &str {
            "blocks its executor thread to reproduce an uncooperative tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn side_effect_class(&self) -> SideEffectClass {
            SideEffectClass::Exec
        }

        async fn call(
            &self,
            _ctx: &forge_tools::ToolContext,
            _args: Value,
        ) -> Result<ToolOutput, ToolError> {
            std::thread::sleep(Duration::from_secs(1));
            Ok(ToolOutput::success("finished blocking"))
        }
    }

    #[tokio::test]
    async fn tui_runtime_keeps_processing_while_a_tool_runs() {
        crate::app::tests::helpers::isolate_global_skills();
        let workspace = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(BashTool));
        let mut session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.path().to_path_buf(),
                journal_dir: workspace.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: false,
                ..Default::default()
            },
            Arc::new(MockModelClient::script(Vec::new())),
            tools,
        )
        .await
        .unwrap();
        session
            .append_user_message("run a slow command")
            .await
            .unwrap();

        let application = session
            .begin_model_response_application(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "slow-command".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "sleep 0.3"}),
                }],
                usage: None,
                thinking: None,
            })
            .await
            .unwrap();
        let ModelResponseApplication::Execute(pending) = application else {
            panic!("bash should be returned as a pending tool application");
        };

        let mut app = TuiApp::new(session, crate::app::tests::helpers::test_runtime_config());
        app.enter_chat_composer();
        app.busy_state.start(BusyPhase::Tool {
            name: "bash".into(),
        });
        let external_file = workspace.path().join("created-while-running.txt");
        std::fs::write(&external_file, "visible before the tool completes\n").unwrap();
        app.file_watch.inject_change(external_file.clone());
        app.test_events.push_back(Event::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        let result = app
            .execute_tool_application(*pending, Some(&mut terminal))
            .await
            .unwrap();

        assert_eq!(app.input.text, "x");
        assert!(
            app.workspace_files
                .explorer
                .visible_nodes()
                .iter()
                .any(|node| node.path.file_name() == external_file.file_name()),
            "the foreground wait must keep the Files pane's watcher alive; visible: {:?}",
            app.workspace_files
                .explorer
                .visible_nodes()
                .iter()
                .map(|node| node.path.clone())
                .collect::<Vec<_>>()
        );
        assert!(matches!(
            result,
            ModelResponseApplication::Finished(ApplyOutcome::Continue)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_tool_cannot_starve_tui_cancellation() {
        crate::app::tests::helpers::isolate_global_skills();
        let workspace = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(BlockingTool));
        let mut session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.path().to_path_buf(),
                journal_dir: workspace.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: false,
                ..Default::default()
            },
            Arc::new(MockModelClient::script(Vec::new())),
            tools,
        )
        .await
        .unwrap();
        session
            .append_user_message("run blocking work")
            .await
            .unwrap();
        let application = session
            .begin_model_response_application(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "blocking-command".into(),
                    name: "blocking_test".into(),
                    arguments: json!({}),
                }],
                usage: None,
                thinking: None,
            })
            .await
            .unwrap();
        let ModelResponseApplication::Execute(pending) = application else {
            panic!("blocking tool should be returned as pending work");
        };

        let mut app = TuiApp::new(session, crate::app::tests::helpers::test_runtime_config());
        app.enter_chat_composer();
        app.busy_state.start(BusyPhase::Tool {
            name: "blocking_test".into(),
        });
        app.test_events.push_back(Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        let started = Instant::now();
        let result = app
            .execute_tool_application(*pending, Some(&mut terminal))
            .await;

        assert!(matches!(result, Err(LoopError::Cancelled)));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "blocking work starved the TUI for {:?}",
            started.elapsed()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_compaction_model_cannot_starve_tui_cancellation() {
        crate::app::tests::helpers::isolate_global_skills();
        let workspace = tempfile::tempdir().unwrap();
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.path().to_path_buf(),
                journal_dir: workspace.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: false,
                ..Default::default()
            },
            Arc::new(BlockingModel),
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let pending = session.begin_context_compaction(forge_core::CompactionTrigger::Manual);
        let mut app = TuiApp::new(session, crate::app::tests::helpers::test_runtime_config());
        app.enter_chat_composer();
        app.busy_state
            .start(BusyPhase::Other("compacting context".into()));
        app.test_events.push_back(Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        let started = Instant::now();
        let completed = app
            .execute_context_compaction_responsive(pending, Some(&mut terminal))
            .await
            .unwrap();

        assert!(completed.is_none());
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "blocking compaction starved the TUI for {:?}",
            started.elapsed()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_model_call_cannot_starve_tui_cancellation() {
        crate::app::tests::helpers::isolate_global_skills();
        let workspace = tempfile::tempdir().unwrap();
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.path().to_path_buf(),
                journal_dir: workspace.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: false,
                ..Default::default()
            },
            Arc::new(BlockingModel),
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let mut app = TuiApp::new(session, crate::app::tests::helpers::test_runtime_config());
        app.pending_turn
            .queue("wait for the model".into(), Vec::new());
        app.test_events.push_back(Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        let started = Instant::now();
        app.drain_pending_prompt_with_terminal(Some(&mut terminal))
            .await
            .unwrap();

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "blocking model call starved the TUI for {:?}",
            started.elapsed()
        );
        assert!(!app.busy_state.is_active());
    }
}
