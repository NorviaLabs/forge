//! Shared types for Forge (Phase 1 + 2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;

/// Authoritative task/attempt lifecycle. Owned by the runtime (`AgentSession`);
/// UI code must read this rather than deriving its own copy from busy/streaming flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskLifecycle {
    /// No active task attempt. Queued items may still exist.
    Ready,
    /// An attempt is actively processing (model inference, tool execution, evaluation).
    #[serde(alias = "running")]
    Working,
    /// Blocked on a specific external response (see `WaitReason`).
    #[serde(alias = "awaiting_hitl")]
    Waiting,
    Completed,
    Failed,
    /// Operator or system cancelled the foreground task.
    Cancelled,
    /// Persisted as active, but no recoverable runtime remains.
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SideEffectClass {
    Read,
    Write,
    Network,
    Exec,
    Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Model "thinking" / reasoning text. Re-sent as `reasoning_content` on OpenAI-compatible wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// How long the model spent thinking (seconds), for "Thought for Xs" UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_duration_secs: Option<f64>,
    /// Tool calls emitted by an assistant message, preserved for the next model step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON object arguments.
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// Process exit code, when the tool ran an external command. `None` for
    /// tools with no notion of an exit code, or when the process never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolValidationError {
    pub tool: String,
    pub path: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_hint: Option<String>,
}

impl std::fmt::Display for ToolValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tool `{}` validation failed at {}: {}",
            self.tool, self.path, self.message
        )
    }
}

impl std::error::Error for ToolValidationError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Provider-reported prompt tokens served from cache (read/hit).
    #[serde(default)]
    pub prompt_cache_read_tokens: u32,
    /// Provider-reported prompt tokens written to cache this request.
    #[serde(default)]
    pub prompt_cache_write_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelStreamEvent {
    TextDelta {
        text: String,
    },
    /// Reasoning / chain-of-thought token chunk (Grok, o-series, DeepSeek-R1, etc.).
    ThinkingDelta {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        arguments_delta: String,
    },
    ToolCallEnd {
        call: ToolCall,
    },
    Usage {
        usage: Usage,
    },
    MessageEnd,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Aggregated thinking / reasoning text for the turn (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum JournalEventType {
    SessionCreated,
    UserMessage,
    ModelRequest,
    ModelResponse,
    ToolIntent,
    ToolResult,
    ToolValidationFailed,
    StatePatch,
    /// Tag for a lifecycle-transition journal entry. Kept as `SessionStatus`
    /// (rather than renamed to `TaskLifecycle`) so its wire value
    /// (`"session_status"`) stays stable for existing persisted journals —
    /// this is an event-type tag, not the `TaskLifecycle` type itself.
    SessionStatus,
    /// Phase 2 — durable HITL
    HitlWait,
    HitlResume,
    /// Phase 2 — context handoff
    ContextReset,
    /// Unified task/queue lifecycle — future-task queue durability.
    QueueEnqueued,
    QueuePromoting,
    QueuePromoted,
    QueueRemoved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub seq: u64,
    pub session_id: SessionId,
    pub ts: DateTime<Utc>,
    pub event_type: JournalEventType,
    pub schema_version: u32,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub side_effect_class: SideEffectClass,
    pub idempotent: bool,
}

/// Progress handoff artifact (CTX-02). Default path: `.forge/progress.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgressDocument {
    pub version: u32,
    pub goal: String,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub in_progress: String,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub workspace_ref: String,
    pub session_id: String,
    pub updated_at: String,
}

impl ProgressDocument {
    pub fn new(session_id: SessionId, goal: impl Into<String>) -> Self {
        Self {
            version: 1,
            goal: goal.into(),
            completed: vec![],
            in_progress: String::new(),
            blockers: vec![],
            next_actions: vec![],
            workspace_ref: String::new(),
            session_id: session_id.to_string(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PolicyDecision {
    Allow,
    Deny,
    Hitl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Principal {
    pub id: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub surface: String,
}

impl Principal {
    pub fn local_dev() -> Self {
        Self {
            id: "local-dev".into(),
            roles: vec!["admin".into()],
            scopes: vec!["*".into()],
            surface: "tui".into(),
        }
    }

    pub fn restricted(surface: &str) -> Self {
        Self {
            id: format!("restricted-{surface}"),
            roles: vec!["restricted".into()],
            scopes: vec![],
            surface: surface.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HitlDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlPayload {
    pub call_id: String,
    pub tool: String,
    pub args_redacted: serde_json::Value,
    pub reason: String,
}

/// Identifies one user-requested unit of work within a session. A new task
/// (not a resumed one) is created either by direct dispatch or by queue
/// promotion — both paths go through the same counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskId(pub u64);

/// Identifies one execution episode of a `TaskId`. A terminal attempt never
/// resumes; continuing after a terminal outcome always starts a new task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptId(pub u64);

/// Why the active task is `TaskLifecycle::Waiting`. Every variant carries a
/// stable `request_id` so a response can be correlated to (and rejected if
/// stale against) the specific outstanding request.
///
/// Only `Approval` has a real producer in Forge today (the tool-call HITL
/// gate); the remaining variants are structurally complete but currently
/// unreachable — built ahead of need so adding a real clarification/selection
/// flow later is additive, not a migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WaitReason {
    Approval {
        request_id: String,
        payload: HitlPayload,
    },
    Clarification {
        request_id: String,
    },
    Selection {
        request_id: String,
    },
    MissingConfiguration {
        request_id: String,
        key: String,
    },
    ExternalAction {
        request_id: String,
        description: String,
    },
}

impl WaitReason {
    /// The correlation id every response must match to resume the attempt.
    pub fn request_id(&self) -> &str {
        match self {
            WaitReason::Approval { request_id, .. }
            | WaitReason::Clarification { request_id }
            | WaitReason::Selection { request_id }
            | WaitReason::MissingConfiguration { request_id, .. }
            | WaitReason::ExternalAction { request_id, .. } => request_id,
        }
    }
}

/// Stable identifier for one queued future-task instruction. Distinct from
/// `TaskId`: a queue item is never itself `Working`/`Completed`/etc. — only
/// its promoted task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueueItemId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemStatus {
    Queued,
    Promoting,
    Promoted,
    Removed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_roundtrip() {
        let cases = [
            (TaskLifecycle::Ready, "\"ready\""),
            (TaskLifecycle::Working, "\"working\""),
            (TaskLifecycle::Completed, "\"completed\""),
            (TaskLifecycle::Failed, "\"failed\""),
            (TaskLifecycle::Waiting, "\"waiting\""),
            (TaskLifecycle::Cancelled, "\"cancelled\""),
            (TaskLifecycle::Interrupted, "\"interrupted\""),
        ];
        for (status, wire) in cases {
            let j = serde_json::to_string(&status).unwrap();
            assert_eq!(j, wire);
            let back: TaskLifecycle = serde_json::from_str(&j).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn task_lifecycle_accepts_legacy_wire_aliases() {
        let legacy_running: TaskLifecycle = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(legacy_running, TaskLifecycle::Working);
        let legacy_awaiting_hitl: TaskLifecycle =
            serde_json::from_str("\"awaiting_hitl\"").unwrap();
        assert_eq!(legacy_awaiting_hitl, TaskLifecycle::Waiting);
    }

    #[test]
    fn wait_reason_request_id_extracts_for_every_variant() {
        let approval = WaitReason::Approval {
            request_id: "r1".into(),
            payload: HitlPayload {
                call_id: "c1".into(),
                tool: "bash".into(),
                args_redacted: serde_json::json!({}),
                reason: "policy requires human approval".into(),
            },
        };
        assert_eq!(approval.request_id(), "r1");

        let clarification = WaitReason::Clarification {
            request_id: "r2".into(),
        };
        assert_eq!(clarification.request_id(), "r2");

        let selection = WaitReason::Selection {
            request_id: "r3".into(),
        };
        assert_eq!(selection.request_id(), "r3");

        let missing_config = WaitReason::MissingConfiguration {
            request_id: "r4".into(),
            key: "api_key".into(),
        };
        assert_eq!(missing_config.request_id(), "r4");

        let external = WaitReason::ExternalAction {
            request_id: "r5".into(),
            description: "waiting on webhook".into(),
        };
        assert_eq!(external.request_id(), "r5");
    }

    #[test]
    fn message_new_initializes_empty_optional_fields() {
        let message = Message::new(MessageRole::Assistant, "hello");
        assert_eq!(message.role, MessageRole::Assistant);
        assert_eq!(message.content, "hello");
        assert!(message.tool_call_id.is_none());
        assert!(message.name.is_none());
        assert!(message.thinking.is_none());
        assert!(message.thinking_duration_secs.is_none());
        assert!(message.tool_calls.is_empty());
    }

    #[test]
    fn principal_helpers_set_expected_shapes() {
        let local = Principal::local_dev();
        assert_eq!(local.id, "local-dev");
        assert_eq!(local.roles, vec!["admin"]);
        assert_eq!(local.scopes, vec!["*"]);
        assert_eq!(local.surface, "tui");

        let restricted = Principal::restricted("cli");
        assert_eq!(restricted.id, "restricted-cli");
        assert_eq!(restricted.roles, vec!["restricted"]);
        assert!(restricted.scopes.is_empty());
        assert_eq!(restricted.surface, "cli");
    }

    #[test]
    fn tool_validation_error_displays_clear_message() {
        let err = ToolValidationError {
            tool: "read_file".into(),
            path: "/args/path".into(),
            message: "missing".into(),
            schema_hint: Some("string".into()),
        };
        assert_eq!(
            err.to_string(),
            "tool `read_file` validation failed at /args/path: missing"
        );
    }

    #[test]
    fn progress_document_new_initializes_session_and_goal() {
        let session_id = Uuid::new_v4();
        let doc = ProgressDocument::new(session_id, "ship coverage");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.goal, "ship coverage");
        assert!(doc.completed.is_empty());
        assert!(doc.in_progress.is_empty());
        assert!(doc.blockers.is_empty());
        assert!(doc.next_actions.is_empty());
        assert!(doc.workspace_ref.is_empty());
        assert_eq!(doc.session_id, session_id.to_string());
        assert!(!doc.updated_at.is_empty());
    }
}
