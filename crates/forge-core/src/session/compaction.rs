//! Transactional context compaction.
//!
//! The pure parts — pressure policy, checkpoint schema, tail selection,
//! candidate validation — live in `forge_context::compaction`. This module
//! owns the two things a session has and that crate must not: the model
//! client and the durable journal.
//!
//! The transaction (§15/§18):
//!
//! ```text
//! BEGIN  snapshot nothing — the current context is simply never written to
//!   │
//!   ├─ send [current model-visible context] + [appended instruction]
//!   ├─ parse + validate the checkpoint
//!   ├─ select the recent raw tail
//!   ├─ construct and validate the candidate context
//!   │
//!   ├─ COMMIT   install, journal, open a new cache epoch
//!   └─ ROLLBACK return the error; nothing above was mutated
//! ```

use std::time::Instant;

use forge_context::compaction::{
    checkpoint_message, compaction_message, is_checkpoint_message, messages_tokens,
    plan_compaction, protected_fact, Checkpoint, CompactionError, CompactionPlan, CompactionPolicy,
    CompactionRecord, CompactionTelemetry, CompactionTrigger, SessionContextState,
};
use forge_model::{ModelClient, ModelRequest};
use forge_types::{strip_protocol_markers, ModelResponse};
use serde_json::json;
use std::sync::Arc;

use crate::{AgentSession, LoopError, TurnEvent};

/// Provider work for one context compaction, detached from the mutable session.
///
/// Frontends may execute this on an isolated worker while continuing to service
/// input and rendering, then hand the result back to [`AgentSession`].
pub struct PendingContextCompaction {
    trigger: CompactionTrigger,
    started: Instant,
    epoch_before: u64,
    tokens_before: usize,
    request: ModelRequest,
    model: Arc<dyn ModelClient>,
}

/// The provider result plus the transaction metadata needed to finish a
/// context compaction against the session that created it.
pub struct CompletedContextCompaction {
    trigger: CompactionTrigger,
    started: Instant,
    epoch_before: u64,
    tokens_before: usize,
    response: Result<ModelResponse, CompactionError>,
}

impl PendingContextCompaction {
    /// Perform only the provider call. No session state is mutated here.
    pub async fn execute(self) -> CompletedContextCompaction {
        let response = self
            .model
            .complete(self.request)
            .await
            .map_err(|error| CompactionError::Provider(error.to_string()));
        CompletedContextCompaction {
            trigger: self.trigger,
            started: self.started,
            epoch_before: self.epoch_before,
            tokens_before: self.tokens_before,
            response,
        }
    }
}

impl AgentSession {
    /// Window arithmetic used for the automatic trigger and tail sizing.
    pub fn compaction_policy(&self) -> &CompactionPolicy {
        &self.compaction_policy
    }

    /// Point compaction at the active model's real limits.
    ///
    /// Keeps `ContextEngine::capacity_tokens` in step so the status bar's
    /// percentage and the compaction trigger describe the same window.
    pub fn set_context_window(&mut self, context_window: usize, max_output: Option<usize>) {
        if context_window == 0 {
            return;
        }
        self.compaction_policy = CompactionPolicy::for_window(context_window, max_output);
        self.context.config.capacity_tokens = context_window;
    }

    /// Cumulative compaction counters plus the most recent attempt.
    pub fn compaction_telemetry(&self) -> &CompactionTelemetry {
        &self.compaction
    }

    /// Persisted projection state: epoch, installed checkpoint, tail boundary,
    /// and the protected facts collected from canonical history.
    pub fn context_state(&self) -> &SessionContextState {
        &self.context_state
    }

    /// Estimated tokens in the current model-visible context. Counts the same
    /// way the compaction planner does, so the trigger and the plan agree.
    pub fn context_tokens(&self) -> usize {
        messages_tokens(&self.messages)
    }

    /// True when the next turn would cross the context-pressure boundary.
    pub fn context_pressure_reached(&self) -> bool {
        self.compaction_policy.should_compact(self.context_tokens())
    }

    /// Record a user turn as a protected fact. Called once per canonical user
    /// message, so the set survives any number of compactions.
    pub(crate) fn record_protected_fact(&mut self, text: &str) {
        let index = self.canonical_user_turns;
        self.canonical_user_turns = self.canonical_user_turns.saturating_add(1);
        if let Some(fact) = protected_fact(index, text) {
            self.context_state.protected_facts.push(fact);
        }
    }

    /// Rebuild protected facts from replayed canonical user turns.
    pub(crate) fn restore_protected_facts(&mut self, user_turns: &[String]) {
        self.canonical_user_turns = user_turns.len();
        self.context_state.protected_facts =
            forge_context::compaction::collect_protected_facts(user_turns);
    }

    /// Compact the model-visible context. The same pipeline serves `/compact`
    /// and the automatic trigger; only [`CompactionTrigger`] differs.
    ///
    /// On success the context is replaced and a new cache epoch is open. On
    /// any failure the active context, the cache epoch, and canonical history
    /// are all exactly as they were.
    pub async fn compact_context(
        &mut self,
        trigger: CompactionTrigger,
    ) -> Result<CompactionRecord, LoopError> {
        let completed = self.begin_context_compaction(trigger).execute().await;
        self.finish_context_compaction(completed).await
    }

    /// Build a compaction request without borrowing the session while the
    /// provider is in flight.
    pub fn begin_context_compaction(&self, trigger: CompactionTrigger) -> PendingContextCompaction {
        let mut request = self.build_model_request();
        request.messages.push(compaction_message(
            self.context_state.checkpoint.as_ref(),
            &self.context_state.protected_facts,
        ));
        PendingContextCompaction {
            trigger,
            started: Instant::now(),
            epoch_before: self.cache_epoch,
            tokens_before: self.context_tokens(),
            request,
            model: self.model.clone(),
        }
    }

    /// Commit a completed provider response, or record a failed compaction
    /// while preserving the currently installed context.
    pub async fn finish_context_compaction(
        &mut self,
        completed: CompletedContextCompaction,
    ) -> Result<CompactionRecord, LoopError> {
        let CompletedContextCompaction {
            trigger,
            started,
            epoch_before,
            tokens_before,
            response,
        } = completed;
        match self
            .finish_context_compaction_inner(
                trigger,
                started,
                epoch_before,
                tokens_before,
                response,
            )
            .await
        {
            Ok(record) => Ok(record),
            Err(error) => {
                let record = CompactionRecord {
                    trigger,
                    tokens_before,
                    tokens_after: tokens_before,
                    checkpoint_tokens: 0,
                    retained_tail_tokens: 0,
                    retained_messages: 0,
                    context_window: self.compaction_policy.context_window,
                    utilization_before: self.compaction_policy.utilization(tokens_before),
                    utilization_after: self.compaction_policy.utilization(tokens_before),
                    epoch_before,
                    epoch_after: self.cache_epoch,
                    duration_ms: started.elapsed().as_millis() as u64,
                    failure_reason: Some(error.category().to_string()),
                };
                self.compaction.record(record);
                self.events.push(TurnEvent {
                    kind: "compaction_failed".into(),
                    detail: format!("{}: {error}", error.category()),
                });
                tracing::warn!(
                    trigger = trigger.as_str(),
                    reason = error.category(),
                    %error,
                    "context compaction failed; keeping the current context"
                );
                Err(LoopError::Compaction(error))
            }
        }
    }

    /// Begin automatic compaction only when the pressure policy requires it.
    pub fn begin_auto_context_compaction(&self) -> Option<PendingContextCompaction> {
        if !self.enable_context || !self.context_pressure_reached() {
            return None;
        }
        Some(self.begin_context_compaction(CompactionTrigger::Automatic))
    }

    /// Automatic compaction: runs only under pressure, and a failure is never
    /// allowed to fail the user's turn — the old context is still valid, so
    /// the step proceeds on it.
    pub(crate) async fn maybe_auto_compact(&mut self) -> Option<CompactionRecord> {
        let pending = self.begin_auto_context_compaction()?;
        let completed = pending.execute().await;
        self.finish_context_compaction(completed).await.ok()
    }

    /// The fallible half, so `compact_context` can record one failure record
    /// for every way this can go wrong.
    async fn finish_context_compaction_inner(
        &mut self,
        trigger: CompactionTrigger,
        started: Instant,
        epoch_before: u64,
        tokens_before: usize,
        response: Result<ModelResponse, CompactionError>,
    ) -> Result<CompactionRecord, CompactionError> {
        let response = response?;
        // A real provider call: its tokens belong in the session totals.
        self.token_usage
            .record_response(response.usage.as_ref(), response.thinking.as_deref());
        let checkpoint = Checkpoint::parse(&strip_protocol_markers(&response.text))?;
        let plan = plan_compaction(
            &self.messages,
            &checkpoint,
            &self.compaction_policy,
            &self.context_state.protected_facts,
        )?;
        let record = CompactionRecord {
            trigger,
            tokens_before: plan.tokens_before,
            tokens_after: plan.tokens_after,
            checkpoint_tokens: plan.checkpoint_tokens,
            retained_tail_tokens: plan.tail_tokens,
            retained_messages: plan.retained_messages,
            context_window: self.compaction_policy.context_window,
            utilization_before: self.compaction_policy.utilization(plan.tokens_before),
            utilization_after: self.compaction_policy.utilization(plan.tokens_after),
            epoch_before,
            epoch_after: epoch_before.saturating_add(1),
            duration_ms: started.elapsed().as_millis() as u64,
            failure_reason: None,
        };
        debug_assert_eq!(tokens_before, plan.tokens_before);

        // Commit point. Everything above this line is read-only.
        self.install_compaction(plan, &record).await?;
        Ok(record)
    }

    /// Install a validated plan: replace the projection, journal it, and open
    /// the new cache epoch. Fails only on journal I/O, which happens before
    /// the in-memory swap so a write failure cannot leave a projection that
    /// resume would not reproduce.
    async fn install_compaction(
        &mut self,
        plan: CompactionPlan,
        record: &CompactionRecord,
    ) -> Result<(), CompactionError> {
        let mut context_state = SessionContextState {
            epoch: self.cache_epoch.saturating_add(1),
            checkpoint: Some(plan.checkpoint.clone()),
            tail_start_message_index: Some(plan.tail_start),
            protected_facts: self.context_state.protected_facts.clone(),
        };
        self.journal
            .append_context_compacted(
                self.session_id,
                json!({
                    "messages": plan.messages,
                    "context_state": context_state,
                    "metrics": record,
                }),
            )
            .await
            .map_err(|error| CompactionError::Journal(error.to_string()))?;

        self.messages = plan.messages.into();
        self.begin_cache_epoch("compaction");
        context_state.epoch = self.cache_epoch;
        self.context_state = context_state;
        self.compaction.record(record.clone());
        self.events.push(TurnEvent {
            kind: "context_compacted".into(),
            detail: format!(
                "{} → {} tokens",
                compact_tokens(record.tokens_before),
                compact_tokens(record.tokens_after)
            ),
        });
        tracing::debug!(
            trigger = record.trigger.as_str(),
            tokens_before = record.tokens_before,
            tokens_after = record.tokens_after,
            checkpoint_tokens = record.checkpoint_tokens,
            retained_tail_tokens = record.retained_tail_tokens,
            context_window = record.context_window,
            utilization_before = record.utilization_before,
            utilization_after = record.utilization_after,
            epoch_before = record.epoch_before,
            epoch_after = self.cache_epoch,
            duration_ms = record.duration_ms,
            "context compacted"
        );
        Ok(())
    }

    /// Restore projection state after journal replay. The replayed messages
    /// are already the compacted projection — this only recovers the
    /// checkpoint and tail boundary that produced it.
    pub(crate) fn restore_context_state(&mut self, persisted: Option<&serde_json::Value>) {
        let Some(value) = persisted else {
            self.context_state.checkpoint = None;
            self.context_state.tail_start_message_index = None;
            return;
        };
        match serde_json::from_value::<SessionContextState>(value.clone()) {
            Ok(state) => {
                self.context_state.epoch = state.epoch;
                self.context_state.checkpoint = state.checkpoint;
                self.context_state.tail_start_message_index = state.tail_start_message_index;
            }
            Err(error) => {
                tracing::warn!(%error, "unreadable persisted context state; continuing uncompacted");
            }
        }
    }

    /// Number of checkpoint messages in the live context. Exactly one after a
    /// compaction, and never more however many times compaction has run.
    pub fn installed_checkpoint_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| is_checkpoint_message(message))
            .count()
    }

    /// The checkpoint as it currently appears in model-visible context.
    pub fn checkpoint_context_message(&self) -> Option<forge_types::Message> {
        self.context_state
            .checkpoint
            .as_ref()
            .map(checkpoint_message)
    }
}

/// `109300` → `109K`, for a one-line status result.
pub fn compact_tokens(tokens: usize) -> String {
    if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}
