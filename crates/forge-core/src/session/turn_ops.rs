//! Driving one user turn: appending the message, running model steps,
//! and the terminal outcomes (failure, cancellation, interruption).
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use super::tools::PendingToolApplications;
use crate::*;

impl AgentSession {
    /// Append a user message to the session (journal + transcript) without calling the model.
    /// Used by the TUI so the YOU bubble can paint before the model run starts.
    pub async fn append_user_message(&mut self, text: &str) -> Result<(), LoopError> {
        self.append_user_message_with_attachments(text, Vec::new())
            .await
    }

    pub async fn append_user_message_with_attachments(
        &mut self,
        text: &str,
        attachments: Vec<forge_types::ImageRef>,
    ) -> Result<(), LoopError> {
        if self.active_task.lifecycle == TaskLifecycle::Waiting {
            return Err(LoopError::AwaitingHitl);
        }
        let mut content = text.to_string();
        let attachments = self.freeze_attachments(&mut content, attachments);
        self.journal
            .append_user_message_with_attachments(self.session_id, &content, &attachments)
            .await?;
        self.canonical_user_messages.push(content.clone());
        self.record_protected_fact(&content);
        self.messages
            .push(Message::new(MessageRole::User, content).with_attachments(attachments));
        if self.context.goal.is_empty() {
            self.context.goal = text.chars().take(200).collect();
        }
        let next_id = TaskId(self.active_task.task_id.0 + 1);
        self.transition_to_new_task(next_id).await?;
        // Fresh turn-local bookkeeping — a prior turn's tool calls/evidence
        // must never leak into this one's decision.
        self.turn.reset();
        self.last_completion = None;
        Ok(())
    }

    /// Lightweight, side-effect-free record of a composer submission — slash
    /// command, plain chat, or any future submission type — independent of
    /// whether it becomes a model-directed `UserMessage` via
    /// `append_user_message`. No lifecycle transition, no message-list
    /// mutation, callable regardless of session state (unlike
    /// `append_user_message`, which is gated on not being `Waiting`).
    /// Feeds `ResumeReport::composer_lines`, restoring the TUI's Up/Down
    /// arrow-key history on resume for lines that never reach the model.
    pub async fn record_composer_line(&self, text: &str) -> Result<(), LoopError> {
        self.journal
            .append_composer_line(self.session_id, text)
            .await?;
        Ok(())
    }

    /// Build the next model request from current transcript + tools.
    pub fn build_model_request(&self) -> ModelRequest {
        let tools = self.tools_for_model();
        // Requests share the transcript in the usual case. Image availability is
        // checked at request time, so only requests with missing attachments pay
        // the copy-on-write cost needed to add the model-visible fallback note.
        let messages = self.messages.shared();
        ModelRequest {
            messages,
            tools,
            model: self.active_model.clone(),
            workspace_root: self.tool_ctx.workspace_root.clone(),
            route_id: (!self.active_route_id.is_empty()).then(|| self.active_route_id.clone()),
            reasoning_effort: self.reasoning_effort.clone(),
            thinking_enabled: self.thinking_enabled,
            prompt_cache: true,
        }
    }

    /// Tool list the model sees. Cached on the registry; filtered when
    /// governance changes the set and again when MCP tool schemas outgrow
    /// their budget. `view_image` stays listed even when the active model
    /// has no image input — execution denies the call.
    pub(crate) fn tools_for_model(&self) -> Vec<forge_types::ToolDescriptor> {
        let descriptors = self.tools.list_descriptors();
        let visible = if self.enable_gov {
            self.governance.filter_tools((*descriptors).clone())
        } else {
            (*descriptors).clone()
        };
        self.defer_mcp_tool_schemas(visible)
    }

    /// Batteries-included token optimization, no configuration required:
    /// once the MCP tools in `visible` would cost more than
    /// `CompactionPolicy::tool_schema_budget` tokens on every single
    /// request, stop sending the full schema for every MCP tool the model
    /// hasn't used yet. `search_tools` (registered by `forge-mcp` — see
    /// `install_search_tools` — whenever any MCP tool exists) is the
    /// model's way to find one by name; once found, its first call either
    /// succeeds or fails with a schema-validation error that tells it the
    /// right shape, and from then on `called` below keeps its full schema
    /// declared, because a provider requires every tool named in the
    /// transcript's tool calls to stay declared in `tools`.
    ///
    /// Below budget, `search_tools` itself is hidden: with nothing deferred,
    /// it has nothing to find.
    fn defer_mcp_tool_schemas(
        &self,
        visible: Vec<forge_types::ToolDescriptor>,
    ) -> Vec<forge_types::ToolDescriptor> {
        let mcp_tokens: usize = visible
            .iter()
            .filter(|t| t.name.starts_with(forge_types::MCP_TOOL_NAME_PREFIX))
            .map(descriptor_tokens)
            .sum();
        if mcp_tokens <= self.compaction_policy.tool_schema_budget() {
            return visible
                .into_iter()
                .filter(|t| t.name != forge_types::SEARCH_TOOLS_TOOL_NAME)
                .collect();
        }
        let called: std::collections::HashSet<&str> = self
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|c| c.name.as_str()))
            .collect();
        visible
            .into_iter()
            .filter(|t| {
                !t.name.starts_with(forge_types::MCP_TOOL_NAME_PREFIX)
                    || called.contains(t.name.as_str())
            })
            .collect()
    }

    /// Apply a model response: journal, assistant message, then run tools.
    /// Returns `Ok(None)` when the turn is finished (no more tool calls).
    /// Returns `Ok(Some(resp))` when paused for HITL.
    /// Returns `Ok(Some(resp))` with empty tool path... actually:
    /// - finished cleanly → Ok(ApplyOutcome::Done(resp))
    /// - need another model step after tools → Ok(ApplyOutcome::Continue)
    /// - HITL → Ok(ApplyOutcome::Hitl(resp))
    pub async fn apply_model_response(
        &mut self,
        last: ModelResponse,
    ) -> Result<ApplyOutcome, LoopError> {
        let mut application = self.begin_model_response_application(last).await?;
        loop {
            application = match application {
                ModelResponseApplication::Finished(outcome) => return Ok(outcome),
                ModelResponseApplication::Execute(pending) => {
                    let completed = IsolatedTask::spawn((*pending).execute())
                        .join()
                        .await
                        .map_err(|error| LoopError::Other(format!("tool task join: {error}")))?
                        .ok_or(LoopError::Cancelled)?;
                    self.finish_tool_application(completed).await?
                }
            };
        }
    }

    pub async fn begin_model_response_application(
        &mut self,
        last: ModelResponse,
    ) -> Result<ModelResponseApplication, LoopError> {
        self.journal
            .append_model_response(
                self.session_id,
                serde_json::to_value(&last).map_err(|error| LoopError::Other(error.to_string()))?,
            )
            .await?;

        self.token_usage
            .record_response(last.usage.as_ref(), last.thinking.as_deref());

        let has_thinking = last
            .thinking
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        // Final-answer channel is `text` only. Thinking stays internal/progress.
        let final_text = strip_protocol_markers(&last.text);
        if !final_text.is_empty() || has_thinking || !last.tool_calls.is_empty() {
            self.messages.push(Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: final_text.clone(),
                tool_call_id: None,
                name: None,
                thinking: last.thinking.clone().filter(|t| !t.trim().is_empty()),
                thinking_duration_secs: None,
                tool_calls: last.tool_calls.clone(),
                attachments: Vec::new(),
            });
            if has_thinking {
                if let Some(ref th) = last.thinking {
                    self.events.push(TurnEvent {
                        kind: "progress".into(),
                        detail: th.clone(),
                    });
                }
            }
            // Durable assistant event only for primary final text without tool calls.
            if !final_text.is_empty() && last.tool_calls.is_empty() {
                self.events.push(TurnEvent {
                    kind: "assistant".into(),
                    detail: final_text.clone(),
                });
            }
        }

        if last.tool_calls.is_empty() {
            // Once a turn has reached a terminal state, a stray extra model
            // step (e.g. a caller re-driving `apply_model_response` outside
            // the normal `run_agent_turns` loop) must never resurrect or
            // overwrite it — a new attempt only starts via a new user
            // message (`append_user_message`), which starts a new task itself.
            if matches!(
                self.active_task.lifecycle,
                TaskLifecycle::Completed
                    | TaskLifecycle::Failed
                    | TaskLifecycle::Cancelled
                    | TaskLifecycle::Interrupted
            ) {
                return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(last)));
            }
            // No durable final answer *and* the turn already did tool/validation
            // work: a failed terminal state, not silent success. An idle / no-op
            // response with no prior activity still counts as a valid (empty)
            // answer below — unchanged from before this evaluator existed.
            if final_text.is_empty() && self.current_turn_has_tool_activity() {
                self.finalize_turn_failure("Forge couldn't complete this turn.", "no_final_answer")
                    .await?;
                return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(last)));
            }
            // The model issued zero real tool calls this turn (in this step
            // or any earlier one), but its final text looks like an attempt
            // to invoke one anyway (e.g. a JSON-ish blob naming a real tool).
            // Left unchecked, this is indistinguishable from a legitimate
            // no-op chat answer and falls through to `TaskExpectation::ReadOnly`,
            // which completes on any non-empty text — reporting success while
            // nothing actually happened. Fail explicitly instead.
            if self.turn.calls().is_empty() {
                let tool_names: Vec<String> = self
                    .tools
                    .list_descriptors()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect();
                if looks_like_dangling_tool_call(&final_text, &tool_names) {
                    self.finalize_turn_failure(
                        "The model attempted to call a tool but didn't format the call correctly, so no changes were made.",
                        CompletionReason::DanglingToolCallText.as_category(),
                    )
                    .await?;
                    return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(last)));
                }
            }
            self.turn.push_evidence(EvidenceEntry::new(
                ExecutionEvent::AssistantResponseProduced,
            ));

            // The model's own words never decide this — only the expectation
            // derived from tool calls actually issued this turn, and the
            // evidence those calls produced.
            let expectation = classify_turn(self.turn.calls());
            let mut decision =
                DefaultCompletionEvaluator.evaluate(&expectation, self.turn.evidence());
            // `classify_turn` picks exactly one `TaskExpectation` category per
            // turn (git > file-edit > tool-execution > search > read-only), so
            // a turn that e.g. both writes a file and runs a failing
            // validation command evaluates the file-edit evidence only — the
            // failing bash evidence never gets consulted.
            //
            // Errored evidence still has to surface, but *how* depends on whose
            // failure it is:
            //
            // - An operation the expectation required failed → the requested
            //   work did not happen. Fail the turn, loudly.
            // - Any other operation failed → the requested work *did* happen and
            //   some extra step the model chose to run did not. Reporting that
            //   as a failed turn is the false-failure bug: it erases a verified
            //   edit because an unrelated command was missing. Stay `Completed`
            //   and carry the unfinished steps in `incomplete` instead.
            if decision.state == TaskLifecycle::Completed {
                let required = expectation.required_operation_ids();
                let is_required = |entry: &EvidenceEntry| {
                    entry
                        .operation_id
                        .as_deref()
                        .is_some_and(|id| required.contains(&id))
                };
                // A step "didn't finish" if the tool itself errored *or* the
                // command it ran exited non-zero. `is_error` alone misses the
                // common case: `bash` dispatches fine and the command inside it
                // fails (a missing `pytest` exits 127 with `is_error` unset),
                // which is exactly the step worth reporting.
                let errored: Vec<&EvidenceEntry> = self
                    .turn
                    .evidence()
                    .0
                    .iter()
                    .filter(|e| e.error.is_some() || e.exit_code.is_some_and(|code| code != 0))
                    .collect();

                if let Some(bad) = errored.iter().copied().find(|e| is_required(e)) {
                    let tool = bad.tool_name.clone().unwrap_or_else(|| "a step".into());
                    decision = CompletionDecision {
                        state: TaskLifecycle::Failed,
                        reason: CompletionReason::PartialFailure,
                        evidence_summary: EvidenceSummary {
                            succeeded: decision.evidence_summary.succeeded,
                            failed: vec![tool.clone()],
                            incomplete: Vec::new(),
                            detail: format!(
                                "{tool} did not finish successfully, so this turn is not complete."
                            ),
                        },
                    };
                } else if !errored.is_empty() {
                    let mut incomplete: Vec<String> = Vec::new();
                    for entry in errored {
                        let tool = entry.tool_name.clone().unwrap_or_else(|| "a step".into());
                        if !incomplete.contains(&tool) {
                            incomplete.push(tool);
                        }
                    }
                    let detail = format!(
                        "{} {} didn't finish.",
                        incomplete.join(", "),
                        if incomplete.len() == 1 {
                            "check"
                        } else {
                            "checks"
                        }
                    );
                    decision = CompletionDecision {
                        state: TaskLifecycle::Completed,
                        reason: CompletionReason::CompletedWithIncompleteChecks,
                        evidence_summary: EvidenceSummary {
                            succeeded: decision.evidence_summary.succeeded,
                            failed: decision.evidence_summary.failed,
                            incomplete,
                            detail,
                        },
                    };
                }
            }
            tracing::debug!(
                expectation = ?expectation,
                evidence_count = self.turn.evidence().0.len(),
                reason = decision.reason.as_category(),
                state = ?decision.state,
                "turn completion decision"
            );
            match decision.state {
                TaskLifecycle::Completed => {
                    // Unfinished ancillary steps travel as their own event so
                    // the UI can report them next to a completed turn without
                    // borrowing failure styling.
                    if !decision.evidence_summary.incomplete.is_empty() {
                        self.events.push(TurnEvent {
                            kind: "turn_incomplete_checks".into(),
                            detail: decision.evidence_summary.incomplete.join(", "),
                        });
                    }
                    self.transition(
                        TaskLifecycle::Completed,
                        TransitionReason::Completion(decision.reason),
                    )
                    .await?;
                }
                TaskLifecycle::Failed => {
                    self.finalize_turn_failure(
                        &decision.evidence_summary.detail,
                        decision.reason.as_category(),
                    )
                    .await?;
                }
                TaskLifecycle::Waiting | TaskLifecycle::Cancelled | TaskLifecycle::Interrupted => {
                    // The evaluator can, in principle, observe evidence that maps
                    // to one of these states (e.g. a `WaitingForUser`/`UserCancelled`
                    // entry left over from earlier in the same turn) — but only the
                    // runtime's own coordinators may actually author these
                    // transitions (the HITL gate in `run_one_tool`, `mark_cancelled`).
                    // A completion decision alone must never force or re-enter one of
                    // them; this used to fall through to `finalize_turn_failure` and
                    // wrongly mark the turn Failed.
                    tracing::debug!(
                        state = ?decision.state,
                        reason = decision.reason.as_category(),
                        "completion decision observed a non-authoritative state; lifecycle left unchanged"
                    );
                }
                // `TaskLifecycle` is `#[non_exhaustive]`; an unrecognised decision
                // state fails safe rather than silently completing.
                _ => {
                    self.finalize_turn_failure(
                        &decision.evidence_summary.detail,
                        decision.reason.as_category(),
                    )
                    .await?;
                }
            }
            self.last_completion = Some(decision);
            return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(last)));
        }

        // Budget spans the whole user turn so repeated invalid calls across
        // model steps still exhaust instead of looping forever.
        let pending = PendingToolApplications {
            calls: last.tool_calls.clone().into_iter(),
            response: last,
            budget: self.turn.take_validation_budget(),
        };
        self.next_tool_application(pending).await
    }

    /// True when the open user turn already has tool or validation activity.
    pub(crate) fn current_turn_has_tool_activity(&self) -> bool {
        for m in self.messages.iter().rev() {
            match m.role {
                MessageRole::User => return false,
                MessageRole::Tool => return true,
                MessageRole::Assistant if !m.tool_calls.is_empty() => return true,
                _ => {}
            }
        }
        false
    }

    /// Persist a concise terminal failure and mark the session failed.
    pub async fn finalize_turn_failure(
        &mut self,
        summary: &str,
        category: &str,
    ) -> Result<(), LoopError> {
        if self.active_task.lifecycle == TaskLifecycle::Failed {
            // Idempotent: keep the first failure summary.
            if self
                .messages
                .iter()
                .any(|m| m.content.starts_with(TURN_FAILED_MARKER))
            {
                return Ok(());
            }
        }
        let summary = summary.trim();
        let content = format!("{TURN_FAILED_MARKER}{summary}");
        self.messages.push(Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: content.clone(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        });
        self.events.push(TurnEvent {
            kind: "turn_failed".into(),
            detail: format!("{category}: {summary}"),
        });
        self.transition(TaskLifecycle::Failed, TransitionReason::TurnFailure)
            .await?;
        let response = ModelResponse {
            text: content,
            tool_calls: vec![],
            usage: None,
            thinking: None,
        };
        self.journal
            .append_model_response(
                self.session_id,
                serde_json::to_value(&response)
                    .map_err(|error| LoopError::Other(error.to_string()))?,
            )
            .await?;
        Ok(())
    }

    /// Persist operator/system cancellation of the foreground task. A no-op
    /// (not an error) when no attempt is actually active — cancelling a
    /// task that already reached a terminal state, or was never started,
    /// must never overwrite that terminal outcome.
    pub async fn mark_cancelled(&mut self) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working | TaskLifecycle::Waiting => {
                self.transition(TaskLifecycle::Cancelled, TransitionReason::UserCancel)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "cancelled".into(),
                    detail: "foreground task cancelled".into(),
                });
                // Cancelling the foreground task must not leave its
                // subagents/background jobs running unsupervised — flip
                // every still-in-flight child's `CancellationToken` too.
                let child_ids: Vec<_> = self
                    .background()
                    .children_of(self.active_task.task_id)
                    .map(|t| t.id)
                    .collect();
                for id in child_ids {
                    self.tasks.background.cancel(id);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A model/provider request failed before ever producing a
    /// `ModelResponse` (e.g. an HTTP error from `ModelClient::complete_with_stream`,
    /// or a `LoopError` surfacing from `apply_model_response` itself before it
    /// reached its own transition logic). There is no assistant turn for
    /// `apply_model_response`'s evaluator to judge in that case, so nothing
    /// else moves the lifecycle out of `Working` — and because the message
    /// queue's dispatch gate and `start_new_task` both refuse to act while
    /// `Working`, an unhandled error here previously left the session stuck
    /// forever (every later message queuing, never sending) until the whole
    /// process was killed and restarted, even after switching to a healthy
    /// provider. Mirrors `mark_cancelled`'s shape: a lifecycle-only
    /// transition, no synthetic assistant message — the caller (the TUI)
    /// already shows the error to the operator through its own error-banner
    /// mechanism, so duplicating it into the transcript here would just be
    /// noise.
    pub async fn mark_model_call_failed(&mut self, detail: &str) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working | TaskLifecycle::Waiting => {
                self.transition(TaskLifecycle::Failed, TransitionReason::TurnFailure)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "turn_failed".into(),
                    detail: format!("model_call_failed: {detail}"),
                });
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// On resume/reload: a durable Running/AwaitingHitl task with no live runtime
    /// cannot safely continue as Working. HITL remains Waiting; bare Running becomes
    /// Interrupted. Legacy sessions with no terminal metadata stay Interrupted rather
    /// than eternal Working. Completed/Failed/Cancelled are left untouched.
    pub async fn mark_interrupted_if_stale(&mut self) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working => {
                // No active runtime after reload/resume.
                self.transition(TaskLifecycle::Interrupted, TransitionReason::StaleOnResume)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "interrupted".into(),
                    detail: "stale running task has no recoverable runtime".into(),
                });
                Ok(())
            }
            // Waiting is still a recoverable state (operator can decide).
            TaskLifecycle::Ready
            | TaskLifecycle::Waiting
            | TaskLifecycle::Completed
            | TaskLifecycle::Failed
            | TaskLifecycle::Cancelled
            | TaskLifecycle::Interrupted => Ok(()),
            // `TaskLifecycle` is `#[non_exhaustive]`. Leave an unrecognised status untouched
            // rather than forcing it to Interrupted.
            _ => Ok(()),
        }
    }

    /// Run until no tool calls, max turns, or HITL pause.
    pub async fn run_user_message(&mut self, text: &str) -> Result<ModelResponse, LoopError> {
        self.reset_turn_cancel();
        self.append_user_message(text).await?;
        self.run_agent_turns(None).await
    }

    /// Context-reset (if needed) + journal a model request; returns the request to send.
    pub async fn prepare_model_step(&mut self, turn: u32) -> Result<ModelRequest, LoopError> {
        // The single projection point: context pressure is resolved here,
        // before the request is built, so there is only ever one path from
        // canonical history to a model request. A failed compaction is not
        // fatal — the pre-compaction context is still valid, so the step
        // proceeds on it (see `maybe_auto_compact`).
        //
        // Compaction opens its own cache epoch, so `epoch_reason` stays
        // `None` here: bumping it again in `record_prompt_snapshot` would
        // double-count the epoch for a single boundary.
        let _ = self.maybe_auto_compact().await;

        self.prepare_model_step_after_compaction(turn).await
    }

    /// Journal and build one model request after a frontend has responsively
    /// handled any pending automatic context compaction.
    pub async fn prepare_model_step_after_compaction(
        &mut self,
        turn: u32,
    ) -> Result<ModelRequest, LoopError> {
        let request = self.build_model_request();
        self.record_prompt_snapshot(&request, None);
        tracing::debug!(turn, "model step");
        self.journal
            .append_model_request(
                self.session_id,
                json!({
                    "turn": turn,
                    "messages": self.messages.len(),
                    "prompt_wire_sha256": self.last_prompt_hash,
                    "prompt_wire_bytes": self.last_prompt_wire.as_ref().map(|bytes| bytes.len()),
                    "cache_epoch": self.cache_epoch,
                    "cache_transport": self.last_cache_transport,
                }),
            )
            .await?;
        Ok(request)
    }

    fn record_prompt_snapshot(&mut self, request: &ModelRequest, epoch_reason: Option<&str>) {
        let transport = self.model.prompt_transport_key(request).to_string();
        if let Some(reason) = epoch_reason {
            self.begin_cache_epoch(reason);
        } else if self
            .last_cache_transport
            .as_deref()
            .is_some_and(|previous| previous != transport)
        {
            self.begin_cache_epoch("transport");
        }

        // Prefix snapshots are debug diagnostics. Building one projects the
        // complete provider body before the provider builds that same body for
        // the request, so keep it off the production hot path unless its logs
        // can actually be emitted. Tests retain snapshots to cover prefix
        // stability and compaction boundaries.
        if !cfg!(test) && !tracing::enabled!(tracing::Level::DEBUG) {
            self.last_prompt_wire = None;
            self.last_prompt_hash = None;
            self.last_cache_transport = Some(transport);
            return;
        }

        let wire = self.model.prompt_wire(request);
        let snapshot = forge_model::snapshot_prompt(&wire);
        if let Some(previous) = self.last_prompt_wire.as_deref() {
            let common = forge_model::common_prefix_len(previous, &snapshot.bytes);
            if common < previous.len() {
                // `previous` is the append-only `tools\nsystem\nmsg0\n…`
                // encoding — one JSON document per part, not a single document
                // — so parsing it back into one value always failed and left
                // the diff comparing `{}` against the current prompt, which
                // never named a real mutation site. `common` already locates
                // the divergence, so name the part it lands in instead. Only
                // the `debug!` below wants it, so it does not run when DEBUG is
                // filtered out.
                let first = if tracing::enabled!(tracing::Level::DEBUG) {
                    forge_model::part_pointer_at(&wire, previous, common)
                } else {
                    String::new()
                };
                let pct = (common as f64 / previous.len() as f64) * 100.0;
                tracing::debug!(
                    previous_bytes = previous.len(),
                    reusable_prefix = common,
                    prefix_reuse = format!("{pct:.1}%"),
                    first_difference = %first,
                    previous_hash = self.last_prompt_hash.as_deref().unwrap_or(""),
                    current_hash = %snapshot.sha256,
                    "CACHE PREFIX INVALIDATED"
                );
            } else {
                let ratio = self.token_usage.prompt_tokens.max(1);
                let cache_read_ratio = self.token_usage.prompt_cache_hits as f64 / ratio as f64;
                tracing::debug!(
                    serialized_request_bytes = snapshot.bytes.len(),
                    common_prefix_bytes = common,
                    common_prefix_percentage = (common as f64 / snapshot.bytes.len().max(1) as f64)
                        * 100.0,
                    stable_prefix_hash = %snapshot.sha256,
                    cache_read_ratio,
                    "prompt prefix reused"
                );
            }
        } else if let Some(expected) = self.last_prompt_hash.as_deref() {
            if expected != snapshot.sha256 {
                tracing::debug!(
                    previous_hash = expected,
                    current_hash = %snapshot.sha256,
                    current_bytes = snapshot.bytes.len(),
                    "CACHE PREFIX INVALIDATED"
                );
            }
        }

        self.last_prompt_wire = Some(snapshot.bytes);
        self.last_prompt_hash = Some(snapshot.sha256);
        self.last_cache_transport = Some(transport);
    }

    pub(crate) fn begin_cache_epoch(&mut self, reason: &str) {
        self.cache_epoch = self.cache_epoch.saturating_add(1);
        self.last_prompt_wire = None;
        tracing::debug!(cache_epoch = self.cache_epoch, reason, "cache epoch reset");
    }

    /// Mark the session failed after exhausting turns.
    pub async fn fail_max_turns(&mut self) -> Result<(), LoopError> {
        self.finalize_turn_failure(
            "Forge couldn't complete this turn within the step limit.",
            "max_turns",
        )
        .await
    }

    /// Drive the agent loop after the user message is already appended.
    /// Optional `stream_tx` receives token deltas during each model complete.
    pub async fn run_agent_turns(
        &mut self,
        stream_tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, LoopError> {
        self.reset_turn_cancel();
        TurnCoordinator::run(self, stream_tx).await
    }

    /// `/compact`: run the compaction pipeline on demand.
    ///
    /// Identical to the automatic path in every respect but the trigger
    /// label, and permitted below the automatic threshold — the operator may
    /// know the next stretch of work needs headroom before the policy does.
    pub async fn force_context_reset_async(&mut self) -> Result<CompactionRecord, LoopError> {
        self.compact_context(CompactionTrigger::Manual).await
    }
}
