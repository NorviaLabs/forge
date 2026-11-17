//! Turn records for tests (feature `turn-records`): a builder that starts
//! from a plain completed turn, so a test names only what matters to it.

use chrono::{DateTime, Utc};
use forge_durable::TurnRecord;
use forge_types::{SessionId, TaskKind};
use serde_json::{json, Value};

/// Builds a [`TurnRecord`].
#[derive(Debug, Clone)]
#[must_use]
pub struct TurnRecordBuilder(TurnRecord);

impl TurnRecordBuilder {
    /// A completed turn of `kind` by `model` (`<provider>/<model>`, on the
    /// provider its prefix names), in a session of its own, that ended a
    /// day ago: task 1, no start time, default effort, no tier, 1,000 prompt
    /// and 100 completion tokens, one model call, 10 s with no waiting, no
    /// cache figures, no signals, `edit_verified`.
    pub fn new(kind: TaskKind, model: &str) -> Self {
        Self(TurnRecord {
            session_id: forge_durable::new_session_id(),
            parent_session_id: None,
            task_id: 1,
            started_at: None,
            ended_at: Utc::now() - chrono::Duration::days(1),
            model: model.to_string(),
            profile_id: model.split('/').next().unwrap_or_default().to_string(),
            reasoning_effort: None,
            tier: None,
            task_kind: kind,
            outcome: "completed".into(),
            completion_reason: "edit_verified".into(),
            escalated: false,
            escalation_retry: false,
            prompt_tokens: Some(1_000),
            completion_tokens: Some(100),
            cache_read_tokens: None,
            cache_write_tokens: None,
            model_calls: Some(1),
            duration_ms: Some(10_000),
            waiting_ms: Some(0),
            signals: json!([]),
        })
    }

    pub fn session_id(mut self, session_id: SessionId) -> Self {
        self.0.session_id = session_id;
        self
    }

    pub fn parent(mut self, parent: Option<SessionId>) -> Self {
        self.0.parent_session_id = parent;
        self
    }

    pub fn task_id(mut self, task_id: u64) -> Self {
        self.0.task_id = task_id;
        self
    }

    pub fn started_at(mut self, started_at: Option<DateTime<Utc>>) -> Self {
        self.0.started_at = started_at;
        self
    }

    pub fn ended_at(mut self, ended_at: DateTime<Utc>) -> Self {
        self.0.ended_at = ended_at;
        self
    }

    /// The provider profile, when it is not the model's prefix.
    pub fn provider(mut self, profile_id: &str) -> Self {
        self.0.profile_id = profile_id.to_string();
        self
    }

    pub fn effort(mut self, effort: Option<&str>) -> Self {
        self.0.reasoning_effort = effort.map(str::to_string);
        self
    }

    pub fn tier(mut self, tier: Option<&str>) -> Self {
        self.0.tier = tier.map(str::to_string);
        self
    }

    pub fn outcome(mut self, outcome: &str) -> Self {
        self.0.outcome = outcome.to_string();
        self
    }

    /// `completed`, or else `failed`.
    pub fn completed(self, completed: bool) -> Self {
        self.outcome(if completed { "completed" } else { "failed" })
    }

    pub fn completion_reason(mut self, reason: &str) -> Self {
        self.0.completion_reason = reason.to_string();
        self
    }

    pub fn escalated(mut self, escalated: bool) -> Self {
        self.0.escalated = escalated;
        self
    }

    pub fn escalation_retry(mut self, retry: bool) -> Self {
        self.0.escalation_retry = retry;
        self
    }

    /// Prompt and completion tokens.
    pub fn tokens(mut self, prompt: Option<u64>, completion: Option<u64>) -> Self {
        self.0.prompt_tokens = prompt;
        self.0.completion_tokens = completion;
        self
    }

    /// Cache-read and cache-write tokens.
    pub fn cache_tokens(mut self, read: Option<u64>, write: Option<u64>) -> Self {
        self.0.cache_read_tokens = read;
        self.0.cache_write_tokens = write;
        self
    }

    pub fn model_calls(mut self, calls: Option<u32>) -> Self {
        self.0.model_calls = calls;
        self
    }

    pub fn duration_ms(mut self, duration: Option<u64>) -> Self {
        self.0.duration_ms = duration;
        self
    }

    pub fn waiting_ms(mut self, waiting: Option<u64>) -> Self {
        self.0.waiting_ms = waiting;
        self
    }

    /// The stored signals array.
    pub fn signals(mut self, signals: Value) -> Self {
        self.0.signals = signals;
        self
    }

    pub fn build(self) -> TurnRecord {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_a_plain_completed_turn_until_told_otherwise() {
        let before = Utc::now();
        let record = TurnRecordBuilder::new(TaskKind::Main, "anthropic/claude-opus-4-1").build();
        assert_eq!(record.task_kind, TaskKind::Main);
        assert_eq!(record.model, "anthropic/claude-opus-4-1");
        assert_eq!(record.profile_id, "anthropic", "the model's provider");
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.task_id, 1);
        assert_eq!(record.started_at, None);
        let day_ago = before - chrono::Duration::days(1);
        assert!((record.ended_at - day_ago).num_seconds().abs() < 60);
        assert_eq!(record.reasoning_effort, None);
        assert_eq!(record.tier, None);
        assert_eq!(record.outcome, "completed");
        assert_eq!(record.completion_reason, "edit_verified");
        assert!(!record.escalated && !record.escalation_retry);
        assert_eq!(
            (record.prompt_tokens, record.completion_tokens),
            (Some(1_000), Some(100))
        );
        assert_eq!(
            (record.cache_read_tokens, record.cache_write_tokens),
            (None, None)
        );
        assert_eq!(record.model_calls, Some(1));
        assert_eq!(
            (record.duration_ms, record.waiting_ms),
            (Some(10_000), Some(0))
        );
        assert_eq!(record.signals, json!([]));
        assert_ne!(
            record.session_id,
            TurnRecordBuilder::new(TaskKind::Main, "a/b")
                .build()
                .session_id,
            "each record its own session"
        );
    }

    #[test]
    fn every_field_can_be_set() {
        let session = forge_durable::new_session_id();
        let parent = forge_durable::new_session_id();
        let ended = Utc::now();
        let record = TurnRecordBuilder::new(TaskKind::Subagent, "a/b")
            .session_id(session)
            .parent(Some(parent))
            .task_id(7)
            .started_at(Some(ended - chrono::Duration::seconds(3)))
            .ended_at(ended)
            .provider("c")
            .effort(Some("high"))
            .tier(Some("strong"))
            .completed(false)
            .completion_reason("dangling_tool_call_text")
            .escalated(true)
            .escalation_retry(true)
            .tokens(Some(5), None)
            .cache_tokens(Some(1), Some(2))
            .model_calls(None)
            .duration_ms(Some(3_000))
            .waiting_ms(None)
            .signals(json!([{"kind": "approval_denied"}]))
            .build();
        assert_eq!(record.session_id, session);
        assert_eq!(record.parent_session_id, Some(parent));
        assert_eq!(record.task_id, 7);
        assert_eq!(
            record.started_at,
            Some(ended - chrono::Duration::seconds(3))
        );
        assert_eq!(record.ended_at, ended);
        assert_eq!(record.profile_id, "c");
        assert_eq!(record.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(record.tier.as_deref(), Some("strong"));
        assert_eq!(record.outcome, "failed");
        assert_eq!(record.completion_reason, "dangling_tool_call_text");
        assert!(record.escalated && record.escalation_retry);
        assert_eq!(
            (record.prompt_tokens, record.completion_tokens),
            (Some(5), None)
        );
        assert_eq!(
            (record.cache_read_tokens, record.cache_write_tokens),
            (Some(1), Some(2))
        );
        assert_eq!(record.model_calls, None);
        assert_eq!((record.duration_ms, record.waiting_ms), (Some(3_000), None));
        assert_eq!(record.signals, json!([{"kind": "approval_denied"}]));
        let cancelled = TurnRecordBuilder::new(TaskKind::Main, "a/b")
            .outcome("cancelled")
            .build();
        assert_eq!(cancelled.outcome, "cancelled");
    }
}
