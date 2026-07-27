//! Shared types for Forge (Phase 1 + 2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Completed,
    Failed,
    /// Phase 2 HITL — reserved so journal can round-trip later.
    AwaitingHitl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    Read,
    Write,
    Network,
    Exec,
    Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
pub enum JournalEventType {
    SessionCreated,
    UserMessage,
    ModelRequest,
    ModelResponse,
    ToolIntent,
    ToolResult,
    ToolValidationFailed,
    StatePatch,
    SessionStatus,
    /// Phase 2 — durable HITL
    HitlWait,
    HitlResume,
    /// Phase 2 — context handoff
    ContextReset,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_roundtrip() {
        let s = SessionStatus::Running;
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(j, "\"running\"");
        let back: SessionStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(back, SessionStatus::Running);
    }
}
