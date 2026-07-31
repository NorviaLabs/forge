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

impl TuiApp {
    /// Close the thinking clock. Prefer wall time from first thinking token;
    /// if that is ~0 (same-batch non-stream dump), fall back to full turn elapsed.
    fn close_thinking_timer(&mut self) {
        if self.thought_secs.is_some() {
            return;
        }
        if self.stream_thinking.is_empty() {
            return;
        }
        let from_think = self
            .thinking_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let from_turn = self
            .turn_started
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
        self.thought_secs = Some(secs);
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
        let text = self.stream_preview.trim_end().to_string();
        if !text.is_empty() {
            self.session.messages.push(Message {
                role: MessageRole::Assistant,
                content: format!("{text}\n\n[Interrupted: {error}]"),
                tool_call_id: None,
                name: None,
                thinking: (!self.stream_thinking.trim().is_empty())
                    .then(|| self.stream_thinking.clone()),
                thinking_duration_secs: self.thought_secs,
                tool_calls: Vec::new(),
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

    /// Enqueue while a message is processing (TUI Enter path only).
    pub(super) fn enqueue_user_message(&mut self, line: String) {
        let n = self.message_queue.enqueue(line);
        if self.queue_selected.is_none() {
            self.queue_selected = Some(0);
        }
        self.push_toast(format!("queued #{n}"));
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "queued #{n} · {} waiting · Ctrl+Up/Down select · Ctrl+Backspace cancel",
                self.message_queue.len()
            ),
        );
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue enqueue #{n}"),
        );
    }

    /// Take next queued message and start a model turn.
    pub(super) fn dequeue_and_send_next(&mut self) {
        if self.busy || self.pending_prompt.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "still processing — wait before sending the next queued message",
            );
            return;
        }
        if self.session.pending_hitl.is_some() {
            self.set_feedback(FeedbackSeverity::Warn, "resolve HITL before dequeuing");
            return;
        }
        let Some(next) = self.message_queue.dequeue() else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.clamp_queue_selection();
        if !self.is_provider_connected() {
            self.message_queue.push_front(next);
            self.report_error("Not connected — cannot send queued message. Run /connect.");
            return;
        }
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue dequeue · {} left", self.message_queue.len()),
        );
        self.set_feedback(
            FeedbackSeverity::Info,
            format!("sending dequeued · {} remaining", self.message_queue.len()),
        );
        // Start the turn the same way as a normal Enter send (no dispatch recursion).
        self.clear_error_chrome();
        if let Some(pid) = self.connect.profile.clone() {
            self.apply_connect_credentials(&pid);
        }
        self.pending_prompt = Some(next);
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.turn_started = Some(Instant::now());
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
    }

    /// Cancel a queued message by 0-based index.
    fn cancel_queued_at(&mut self, index: usize) {
        let one_based = index + 1;
        match self.message_queue.drop_at(one_based) {
            Some(t) => {
                let preview: String = t.chars().take(48).collect();
                self.push_toast(format!("cancelled #{one_based}"));
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!(
                        "cancelled queued #{one_based} · {} left",
                        self.message_queue.len()
                    ),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Ok,
                    format!("queue cancel #{one_based}: {preview}"),
                );
                self.clamp_queue_selection();
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "queue item gone");
            }
        }
    }

    fn clamp_queue_selection(&mut self) {
        let len = self.message_queue.len();
        self.queue_selected = match (len, self.queue_selected) {
            (0, _) => None,
            (_, Some(i)) if i < len => Some(i),
            (_, Some(_)) => Some(len - 1),
            (_, None) => Some(0),
        };
    }

    pub(super) fn move_queue_selection(&mut self, delta: i32) {
        let len = self.message_queue.len();
        if len == 0 {
            self.queue_selected = None;
            return;
        }
        let cur = self.queue_selected.unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.queue_selected = Some(next);
    }

    pub(super) fn cancel_selected_queue(&mut self) {
        let Some(idx) = self.queue_selected else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.cancel_queued_at(idx);
    }

    /// Run a queued user prompt with streaming + intermediate redraws.
    /// When `terminal` is `None` (unit tests), runs without intermediate draws.
    pub async fn drain_pending_prompt(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let continuing = std::mem::take(&mut self.pending_turn_continue);
        let line = self.pending_prompt.take();
        if line.is_none() && !continuing {
            return Ok(());
        }

        // Refresh OAuth close to expiry and recycle the worker with the current token.
        if let Some(profile_id) = self.connect.profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }

        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started.get_or_insert_with(Instant::now);
        self.thinking_started = None;
        self.thought_secs = None;

        if let Some(ref line) = line {
            if let Err(e) = self.session.append_user_message(line).await {
                self.busy = false;
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&e.to_string());
                self.last_exit = ExitCode::Failed;
                return Ok(());
            }
        }

        // Paint YOU message immediately
        if let Some(term) = terminal.as_deref_mut() {
            term.draw(|f| self.draw(f))?;
        }

        let max_turns = self.session.max_turns();
        let mut outcome_err: Option<String> = None;
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

            // Pump stream events + redraw until the model call finishes
            loop {
                if self.cancel_requested {
                    handle.abort();
                    self.cancel_requested = false;
                    self.turn_started = None;
                    outcome_err = Some("interrupted".into());
                    break 'turns;
                }
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        ModelStreamEvent::TextDelta { text } => {
                            // Thinking ends when answer tokens begin
                            self.close_thinking_timer();
                            self.stream_preview.push_str(&text);
                        }
                        ModelStreamEvent::ThinkingDelta { text } => {
                            if self.thinking_started.is_none() {
                                // Prefer turn start so duration covers full thinking wait if
                                // the provider dumps reasoning in one late chunk.
                                self.thinking_started =
                                    self.turn_started.or_else(|| Some(Instant::now()));
                            }
                            self.stream_thinking.push_str(&text);
                        }
                        ModelStreamEvent::Error { message } => {
                            handle.abort();
                            outcome_err = Some(message);
                            break 'turns;
                        }
                        _ => {}
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
                    drain_events(self).await?;
                    if self.should_quit {
                        handle.abort();
                        self.busy = false;
                        self.busy_phase = BusyPhase::Idle;
                        self.stream_preview.clear();
                        self.stream_thinking.clear();
                        self.turn_started = None;
                        self.thinking_started = None;
                        self.thought_secs = None;
                        self.last_exit = ExitCode::Canceled;
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
                        match ev {
                            ModelStreamEvent::TextDelta { text } => {
                                self.close_thinking_timer();
                                self.stream_preview.push_str(&text);
                            }
                            ModelStreamEvent::ThinkingDelta { text } => {
                                if self.thinking_started.is_none() {
                                    self.thinking_started =
                                        self.turn_started.or_else(|| Some(Instant::now()));
                                }
                                self.stream_thinking.push_str(&text);
                            }
                            ModelStreamEvent::Error { message } => {
                                handle.abort();
                                outcome_err = Some(message);
                                break 'turns;
                            }
                            _ => {}
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
                Ok(Ok(r)) => r,
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
            if self.stream_thinking.is_empty() {
                if let Some(ref th) = last.thinking {
                    if !th.is_empty() {
                        if self.thinking_started.is_none() {
                            self.thinking_started = self.turn_started;
                        }
                        self.stream_thinking = th.clone();
                        self.close_thinking_timer();
                        // One paint so the user can see thinking before collapse
                        if let Some(term) = terminal.as_deref_mut() {
                            let _ = term.draw(|f| self.draw(f));
                        }
                    }
                }
            } else if last.thinking.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                // Prefer streamed thinking body on the final message
                last.thinking = Some(self.stream_thinking.clone());
            }
            self.close_thinking_timer();

            let thought = self.thought_secs.take();
            self.stream_preview.clear();
            self.stream_thinking.clear();
            // Keep turn_started until full agent turn ends (multi-tool steps).
            if let Some(call) = last.tool_calls.first() {
                self.busy_phase = BusyPhase::Tool {
                    name: call.name.clone(),
                };
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Info,
                    format!("tool_intent {}", call.name),
                );
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }
            }
            match self.session.apply_model_response(last).await {
                Ok(out) => {
                    if let Some(secs) = thought {
                        saw_thinking = true;
                        turn_thought_secs += secs;
                    }
                    // Reset per-model-step thinking timers for multi-tool loops.
                    self.thinking_started = None;
                    self.thought_secs = None;
                    match out {
                        ApplyOutcome::Done(_) | ApplyOutcome::Hitl(_) => {
                            outcome_err = None;
                            self.maybe_note_workspace_changed_from_recent_tools();
                            break 'turns;
                        }
                        ApplyOutcome::Continue => {
                            self.busy_phase = BusyPhase::Model;
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
                    outcome_err = Some(e.to_string());
                    break;
                }
            }
        }

        let turn_limit_reached = outcome_err.is_none()
            && self.session.status != forge_types::SessionStatus::Completed
            && self.session.status != forge_types::SessionStatus::AwaitingHitl;
        let interrupted_partial = outcome_err
            .as_ref()
            .filter(|_| !self.stream_preview.trim().is_empty())
            .cloned();

        self.busy = false;
        self.busy_phase = BusyPhase::Idle;

        if turn_limit_reached {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            self.overlay = Some(Overlay::turn_limit(max_turns));
            self.last_exit = ExitCode::Success;
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
            let was_cancel = e == "interrupted";
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
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if was_cancel {
                self.last_exit = ExitCode::Canceled;
                if let Err(err) = self.session.mark_cancelled().await {
                    self.report_error(&err.to_string());
                }
            } else {
                self.last_exit = ExitCode::Failed;
            }
            // Leave queue intact so the operator can fix and continue.
        } else if self.session.pending_hitl.is_some() {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if let Some(ref p) = self.session.pending_hitl {
                self.open_hitl_overlay(p.clone());
            }
            self.last_exit = ExitCode::AwaitingHitl;
            self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
            self.push_activity(ActivityKind::Hitl, FeedbackSeverity::Warn, "hitl waiting");
            // Do not auto-dequeue until HITL is resolved.
        } else {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if saw_thinking {
                self.persist_turn_thinking_duration(turn_thought_secs);
            }
            self.clear_error_chrome();
            self.tool_expanded = false;
            if self.message_queue.is_empty() {
                self.feedback = FeedbackModel::default();
                self.status_message.clear();
            } else {
                self.push_toast(format!(
                    "{} queued · sending next",
                    self.message_queue.len()
                ));
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("{} in queue — sending next", self.message_queue.len()),
                );
            }
            self.push_activity(ActivityKind::Model, FeedbackSeverity::Ok, "model ok");
            if !self.message_queue.is_empty() {
                self.dequeue_and_send_next();
            }
        }
        Ok(())
    }
}
