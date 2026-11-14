//! What happened in a turn, and the one record each finished turn writes.
//!
//! A turn starts when a new task attempt begins
//! (`AgentSession::transition_to_new_task`) and ends at its terminal
//! lifecycle transition (completed, failed, cancelled, interrupted). Along
//! the way the session records [`TurnSignal`]s — approvals asked and
//! answered, verification commands and their results, cancellation, tier
//! switches, escalation. At the end it writes one
//! [`forge_durable::TurnRecord`] into its own journal: model, effort, tier,
//! how and why the turn ended, whether it escalated, tokens, duration, and
//! those signals. Top-level sessions and subagents alike.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use forge_types::TaskLifecycle;
use serde::{Deserialize, Serialize};

use crate::tiers::{EscalationPlan, TierOverride};
use crate::{AgentSession, LoopError, SessionTokenUsage, TransitionReason};

/// Something observed during a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSignalKind {
    /// A tool call paused for the user's approval.
    ApprovalRequested,
    /// The paused call asked to leave the sandbox.
    SandboxEscalationRequested,
    ApprovalGranted,
    ApprovalDenied,
    /// The agent asked the user a question.
    QuestionAsked,
    QuestionAnswered,
    QuestionDismissed,
    /// A test, check, lint or build command exited zero.
    VerificationPassed,
    /// A verification command exited non-zero or could not run.
    VerificationFailed,
    /// A verification command was still running when its result came back.
    VerificationUnfinished,
    /// A tool call reported an error.
    ToolFailed,
    /// The provider request failed.
    ModelCallFailed,
    /// The turn stopped on a runtime error other than the provider's (journal
    /// I/O, an invalid transition, …).
    RuntimeError,
    /// The conversation was compacted to fit the context window.
    ContextCompacted,
    /// The turn switched to a model tier.
    TierApplied,
    /// A tier was asked for but nothing could be changed.
    TierUnavailable,
    /// This failed subagent turn will be re-run on the strong tier.
    Escalated,
    /// This failed subagent turn qualified, but was not re-run.
    EscalationSkipped,
    /// The user stopped the turn.
    UserCancelled,
    /// The parent agent interrupted this subagent's turn.
    InterruptedByParent,
    /// Forge restarted while the turn was running.
    InterruptedByRestart,
    /// The user sent a message while the turn was running (or waiting on
    /// them). It is queued, and runs after this turn.
    UserMessageQueued,
}

/// One observation, with when it happened relative to the turn's start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSignal {
    pub kind: TurnSignalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Milliseconds since the turn started; absent when the start is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

/// Signals the session's owner records for the turn in progress without
/// holding the session, which a running turn keeps locked — the user queuing
/// a message while it runs. Cheap to clone; every clone feeds the same
/// session. Merged into the turn's record when the turn ends.
#[derive(Debug, Clone, Default)]
pub struct TurnSignalInbox(Arc<Mutex<InboxState>>);

#[derive(Debug, Default)]
struct InboxState {
    /// A turn is in progress: between its start and its record.
    open: bool,
    pending: Vec<(TurnSignalKind, Option<String>, Instant)>,
}

impl TurnSignalInbox {
    fn state(&self) -> std::sync::MutexGuard<'_, InboxState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record `kind` for the turn in progress. Returns `false`, recording
    /// nothing, when no turn is in progress.
    pub fn record(&self, kind: TurnSignalKind, detail: Option<String>) -> bool {
        let mut state = self.state();
        if state.open {
            state.pending.push((kind, detail, Instant::now()));
        }
        state.open
    }

    fn open(&self) {
        let mut state = self.state();
        state.open = true;
        state.pending.clear();
    }

    fn close(&self) -> Vec<(TurnSignalKind, Option<String>, Instant)> {
        let mut state = self.state();
        state.open = false;
        std::mem::take(&mut state.pending)
    }
}

/// Per-turn measurements. Reset when a turn starts.
#[derive(Debug, Default)]
pub(crate) struct TurnTelemetry {
    started: Option<(Instant, DateTime<Utc>)>,
    usage_at_start: Option<SessionTokenUsage>,
    waiting_since: Option<Instant>,
    waiting: Duration,
    signals: Vec<TurnSignal>,
    /// When each of `signals` happened, exactly, to order signals recorded
    /// elsewhere (the inbox) among them.
    signal_times: Vec<Instant>,
    /// Set just before a `TurnFailure` transition: the failure's category.
    failure_category: Option<String>,
    /// Set just before a `UserCancel` transition: who stopped the turn.
    cancel_reason: Option<TurnSignalKind>,
    pub(crate) escalated: bool,
    pub(crate) escalation_retry: bool,
}

impl TurnTelemetry {
    fn elapsed_ms(&self) -> Option<u64> {
        self.started
            .map(|(start, _)| start.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
    }
}

/// Everything the session tracks for turn records, tiers and escalation.
#[derive(Debug, Default)]
pub(crate) struct TurnTracking {
    pub(crate) telemetry: TurnTelemetry,
    pub(crate) tier_override: Option<TierOverride>,
    pub(crate) pending_escalation: Option<EscalationPlan>,
    pub(crate) next_turn_is_escalation_retry: bool,
    pub(crate) inbox: TurnSignalInbox,
}

fn outcome_name(lifecycle: TaskLifecycle) -> &'static str {
    match lifecycle {
        TaskLifecycle::Completed => "completed",
        TaskLifecycle::Failed => "failed",
        TaskLifecycle::Cancelled => "cancelled",
        TaskLifecycle::Interrupted => "interrupted",
        _ => "unknown",
    }
}

fn signal_name(kind: TurnSignalKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{kind:?}"))
}

impl AgentSession {
    /// Record a signal for the current turn.
    pub(crate) fn record_signal(&mut self, kind: TurnSignalKind, detail: Option<String>) {
        self.push_signal(kind, detail, Instant::now());
    }

    fn push_signal(&mut self, kind: TurnSignalKind, detail: Option<String>, at: Instant) {
        let telemetry = &mut self.tracking.telemetry;
        let at_ms = telemetry.started.map(|(start, _)| {
            at.saturating_duration_since(start)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX)
        });
        telemetry.signals.push(TurnSignal {
            kind,
            detail,
            at_ms,
        });
        telemetry.signal_times.push(at);
    }

    /// Record what a finished tool call says about the turn: a verification
    /// command's result, or a tool failure. A sandbox refusal is not
    /// recorded here (it becomes an approval, or a note to the model), nor is
    /// a call the user refused.
    pub(crate) fn note_tool_result(
        &mut self,
        call: &forge_types::ToolCall,
        result: &Result<forge_types::ToolOutput, forge_tools::ToolError>,
    ) {
        use forge_types::ExecutionOutcome;
        match result {
            Err(forge_tools::ToolError::SandboxDenied { .. }) => return,
            Ok(output) if matches!(output.outcome, Some(ExecutionOutcome::Denied { .. })) => return,
            _ => {}
        }
        let command = match call.name.as_str() {
            "bash" => call.arguments.get("command"),
            "exec_command" => call.arguments.get("cmd"),
            _ => None,
        }
        .and_then(serde_json::Value::as_str);
        if let Some(command) = command.filter(|command| {
            crate::verification::is_verification_command(command, &self.verification_commands)
        }) {
            let label = crate::truncate(command.trim().lines().next().unwrap_or_default(), 80);
            let kind = match result {
                Ok(output) if output.is_error => TurnSignalKind::VerificationFailed,
                Ok(output) => match output.exit_code {
                    Some(0) => TurnSignalKind::VerificationPassed,
                    Some(_) => TurnSignalKind::VerificationFailed,
                    // An interactive session handed back before it exited.
                    None if call.name == "exec_command" => TurnSignalKind::VerificationUnfinished,
                    None => TurnSignalKind::VerificationPassed,
                },
                Err(_) => TurnSignalKind::VerificationFailed,
            };
            self.record_signal(kind, Some(label));
            return;
        }
        let failed = match result {
            Ok(output) => output.is_error,
            Err(_) => true,
        };
        if failed {
            self.record_signal(TurnSignalKind::ToolFailed, Some(call.name.clone()));
        }
    }

    /// Set the project's configured validation command lines, which count as
    /// verification commands in turn signals.
    pub fn set_verification_commands(&mut self, commands: Vec<String>) {
        self.verification_commands = std::sync::Arc::new(
            commands
                .into_iter()
                .map(|command| command.trim().to_string())
                .filter(|command| !command.is_empty())
                .collect(),
        );
    }

    /// A handle the session's owner uses to record signals for the turn in
    /// progress while the turn holds the session.
    pub fn turn_signal_inbox(&self) -> TurnSignalInbox {
        self.tracking.inbox.clone()
    }

    /// The signals recorded so far in the current (or just-finished) turn.
    pub fn turn_signals(&self) -> &[TurnSignal] {
        &self.tracking.telemetry.signals
    }

    /// Every turn record this session has written, oldest first.
    pub async fn turn_records(&self) -> Result<Vec<forge_durable::TurnRecordRow>, LoopError> {
        Ok(self.journal.turn_records(self.session_id).await?)
    }

    /// A new turn started: reset the measurements.
    pub(crate) fn begin_turn_telemetry(&mut self) {
        let escalation_retry = std::mem::take(&mut self.tracking.next_turn_is_escalation_retry);
        self.tracking.pending_escalation = None;
        self.tracking.telemetry = TurnTelemetry {
            started: Some((Instant::now(), Utc::now())),
            usage_at_start: Some(self.token_usage.clone()),
            escalation_retry,
            ..TurnTelemetry::default()
        };
        self.tracking.inbox.open();
    }

    /// Take what the owner recorded during the turn, in time order with the
    /// session's own signals.
    fn merge_inbox(&mut self) {
        let received = self.tracking.inbox.close();
        if received.is_empty() {
            return;
        }
        for (kind, detail, at) in received {
            self.push_signal(kind, detail, at);
        }
        let telemetry = &mut self.tracking.telemetry;
        let mut timed: Vec<(Instant, TurnSignal)> = telemetry
            .signal_times
            .drain(..)
            .zip(telemetry.signals.drain(..))
            .collect();
        // Stable: signals recorded at the same instant keep their order.
        timed.sort_by_key(|(at, _)| *at);
        for (at, signal) in timed {
            telemetry.signal_times.push(at);
            telemetry.signals.push(signal);
        }
    }

    pub(crate) fn note_waiting_started(&mut self) {
        self.tracking
            .telemetry
            .waiting_since
            .get_or_insert_with(Instant::now);
    }

    pub(crate) fn note_waiting_ended(&mut self) {
        if let Some(since) = self.tracking.telemetry.waiting_since.take() {
            self.tracking.telemetry.waiting += since.elapsed();
        }
    }

    /// The category of the failure about to be recorded.
    pub(crate) fn note_failure_category(&mut self, category: &str) {
        self.tracking.telemetry.failure_category = Some(category.to_string());
    }

    /// Who or what is stopping the turn, recorded as a signal and as the
    /// reason. The first reason noted wins: a turn an error ended is not
    /// also recorded as cancelled by the user when it is then wound down.
    pub(crate) fn note_cancellation(&mut self, kind: TurnSignalKind, detail: Option<String>) {
        if self.tracking.telemetry.cancel_reason.is_some() {
            return;
        }
        self.tracking.telemetry.cancel_reason = Some(kind);
        self.record_signal(kind, detail);
    }

    fn completion_reason(&mut self, to: TaskLifecycle, reason: &TransitionReason) -> String {
        let telemetry = &mut self.tracking.telemetry;
        match reason {
            TransitionReason::Completion(reason) => reason.as_category().to_string(),
            TransitionReason::QueueYield => "queue_yield".into(),
            TransitionReason::TurnFailure => telemetry
                .failure_category
                .take()
                .unwrap_or_else(|| "turn_failure".into()),
            TransitionReason::UserCancel => signal_name(
                telemetry
                    .cancel_reason
                    .take()
                    .unwrap_or(TurnSignalKind::UserCancelled),
            ),
            TransitionReason::StaleOnResume => signal_name(TurnSignalKind::InterruptedByRestart),
            _ => outcome_name(to).to_string(),
        }
    }

    /// Write the record for the turn that just reached `to`. Never fails the
    /// turn: a record that cannot be written is logged and skipped.
    pub(crate) async fn record_finished_turn(
        &mut self,
        to: TaskLifecycle,
        reason: &TransitionReason,
    ) {
        self.note_waiting_ended();
        self.merge_inbox();
        if matches!(reason, TransitionReason::StaleOnResume) {
            self.record_signal(TurnSignalKind::InterruptedByRestart, None);
        }
        let completion_reason = self.completion_reason(to, reason);
        let selection = self.model_selection();
        let tier = self.active_model_tier();
        let parent_session_id = self.coordinator.parent_of(self.session_id);
        let telemetry = &self.tracking.telemetry;
        let usage = telemetry.usage_at_start.as_ref().map(|start| {
            let now = &self.token_usage;
            (
                now.prompt_tokens.saturating_sub(start.prompt_tokens),
                now.completion_tokens
                    .saturating_sub(start.completion_tokens),
                now.prompt_cache_hits
                    .saturating_sub(start.prompt_cache_hits),
                now.prompt_cache_writes
                    .saturating_sub(start.prompt_cache_writes),
                now.model_steps.saturating_sub(start.model_steps),
            )
        });
        let record = forge_durable::TurnRecord {
            session_id: self.session_id,
            parent_session_id,
            task_id: self.active_task.task_id.0,
            started_at: telemetry.started.map(|(_, at)| at),
            ended_at: Utc::now(),
            model: selection.model,
            profile_id: selection.profile_id,
            reasoning_effort: selection.reasoning_effort,
            tier: tier.map(|tier| tier.as_str().to_string()),
            outcome: outcome_name(to).to_string(),
            completion_reason,
            escalated: telemetry.escalated,
            escalation_retry: telemetry.escalation_retry,
            prompt_tokens: usage.map(|usage| usage.0),
            completion_tokens: usage.map(|usage| usage.1),
            cache_read_tokens: usage.map(|usage| usage.2),
            cache_write_tokens: usage.map(|usage| usage.3),
            model_calls: usage.map(|usage| usage.4),
            duration_ms: telemetry.elapsed_ms(),
            waiting_ms: telemetry
                .started
                .map(|_| telemetry.waiting.as_millis().try_into().unwrap_or(u64::MAX)),
            signals: serde_json::to_value(&telemetry.signals).unwrap_or_default(),
        };
        if let Err(error) = self.journal.append_turn_record(&record).await {
            tracing::warn!(%error, session = %self.session_id, "could not write the turn record");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_serialize_as_snake_case_kinds() {
        let signal = TurnSignal {
            kind: TurnSignalKind::VerificationFailed,
            detail: Some("cargo test".into()),
            at_ms: Some(12),
        };
        assert_eq!(
            serde_json::to_value(&signal).unwrap(),
            serde_json::json!({"kind": "verification_failed", "detail": "cargo test", "at_ms": 12})
        );
        let bare = TurnSignal {
            kind: TurnSignalKind::UserCancelled,
            detail: None,
            at_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"kind": "user_cancelled"})
        );
        assert_eq!(
            signal_name(TurnSignalKind::InterruptedByParent),
            "interrupted_by_parent"
        );
    }
}
