//! Shared types for Forge (Phase 1 + 2).

mod git;
mod image;

pub use git::is_readonly_git_subcommand;
pub use image::{
    inspect_image, sample_png_bytes, sniff_allowed_image, ImageInspectError, ImageMeta, ImageRef,
    MAX_IMAGE_BYTES,
};

/// Remove structural protocol control markers from final-answer text before
/// persistence. Not phrase filtering — only known control envelopes.
pub fn strip_protocol_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\\confidence{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "\\confidence{".len()..];
        if let Some(end) = after.find('}') {
            rest = &after[end + 1..];
        } else {
            // Unterminated marker, e.g. model output truncated mid-annotation.
            // Rewind to the marker so the tail is emitted exactly once: the
            // prefix was already pushed above, so leaving `rest` untouched
            // would duplicate it.
            rest = &rest[start..];
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

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
    /// Real execution result for `MessageRole::Tool` messages, carried
    /// end-to-end from `ToolOutput::effective_outcome()` rather than
    /// re-derived later by pattern-matching rendered text. `Success` for
    /// every non-tool role (no execution to report).
    #[serde(default)]
    pub outcome: ExecutionOutcome,
    /// Path-refs for images on user messages (clipboard paste) and tool
    /// results (`view_image`). Never holds bytes; transports re-read the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImageRef>,
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
            outcome: ExecutionOutcome::Success,
            attachments: Vec::new(),
        }
    }

    pub fn with_attachments(mut self, attachments: Vec<ImageRef>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn from_tool_output(call: &ToolCall, output: &ToolOutput) -> Self {
        Self {
            outcome: output.effective_outcome(),
            role: MessageRole::Tool,
            content: output.content.clone(),
            tool_call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: output.attachments.clone(),
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

/// The single, structured source of truth for how a tool call or process
/// finished. Constructed at the point where the fact is known (ACL/HITL
/// gate, tool dispatch, process exit) — never reconstructed later by
/// pattern-matching rendered text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionOutcome {
    #[default]
    Success,
    /// A process started and exited non-zero.
    Failed { exit_code: Option<i32> },
    /// The process/tool could not be started at all (e.g. the shell binary
    /// itself failed to spawn, or a shell-reported "command not found",
    /// conventionally exit code 127).
    SpawnFailed { reason: String },
    /// Terminal and distinct from "blocked/pending": the call never ran
    /// because governance/HITL refused it. Resolved negatively, not
    /// awaiting resolution.
    Denied { reason: String },
    /// Operator/system cancelled before or during execution.
    Cancelled,
    /// Stub only: no real deadline enforcement exists anywhere yet. Exists
    /// so the type, icon, copy, and tests are in place ahead of enforcement.
    TimedOut,
}

impl ExecutionOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, ExecutionOutcome::Success)
    }
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
    /// Structured outcome. `None` only for JSON persisted before this field
    /// existed; `effective_outcome()` derives a safe fallback from
    /// `is_error`/`exit_code` for those legacy records (never upgrades a
    /// legacy failure to `Success`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExecutionOutcome>,
    /// Path-refs for images this tool result should send on the next request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImageRef>,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            exit_code: None,
            outcome: Some(ExecutionOutcome::Success),
            attachments: Vec::new(),
        }
    }

    pub fn failed_exit(content: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            exit_code,
            outcome: Some(ExecutionOutcome::Failed { exit_code }),
            attachments: Vec::new(),
        }
    }

    pub fn spawn_failed(content: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            exit_code: None,
            outcome: Some(ExecutionOutcome::SpawnFailed {
                reason: reason.into(),
            }),
            attachments: Vec::new(),
        }
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            content: reason.clone(),
            is_error: true,
            exit_code: None,
            outcome: Some(ExecutionOutcome::Denied { reason }),
            attachments: Vec::new(),
        }
    }

    pub fn cancelled(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            exit_code: None,
            outcome: Some(ExecutionOutcome::Cancelled),
            attachments: Vec::new(),
        }
    }

    /// Never trust a missing `outcome` as success: old journaled records
    /// (no `outcome` field) fall back to a coarse classification from
    /// `is_error`/`exit_code` alone — this can't recover Denied/SpawnFailed/
    /// Cancelled distinctions for pre-existing data, but it can never turn
    /// a recorded failure into a false Success.
    pub fn effective_outcome(&self) -> ExecutionOutcome {
        if let Some(o) = &self.outcome {
            return o.clone();
        }
        if self.is_error {
            ExecutionOutcome::Failed {
                exit_code: self.exit_code,
            }
        } else {
            ExecutionOutcome::Success
        }
    }
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
    /// Total input tokens for the request, cached portion included.
    ///
    /// Normalised across providers: Anthropic reports `input_tokens` as the
    /// uncached remainder, so its cache read/write counts are added back
    /// before this is stored. Without that, `prompt_cache_read_tokens /
    /// prompt_tokens` is a fraction of the prompt on one provider and a
    /// multiple of the uncached part on another.
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
    /// Context compaction installed a new model-visible projection: a
    /// structured checkpoint plus a recent raw tail. Canonical history is
    /// untouched — every earlier event remains in the journal, and this event
    /// only records how the projection was rebuilt.
    ContextCompacted,
    /// Unified task/queue lifecycle — future-task queue durability.
    QueueEnqueued,
    QueuePromoting,
    QueuePromoted,
    QueueRemoved,
    /// Background task lifecycle (shell jobs and subagents alike) — see
    /// `forge-core`'s `BackgroundTaskRegistry`.
    BackgroundTaskStarted,
    BackgroundTaskFinished,
    /// Subagent spawn/completion, appended to the *parent's* journal only —
    /// the child records its own full turn-by-turn history in its own
    /// journal under its own `SessionId`.
    SubagentSpawned,
    SubagentFinished,
    /// A line was submitted in the composer — slash command, plain chat, or
    /// any future submission type (e.g. a planned `!shell` direct-execute
    /// prefix) — independent of whether it became a model-directed
    /// `UserMessage`. Feeds the TUI's Up/Down arrow-key history on resume;
    /// unlike `UserMessage`, this fires for every submission, not just ones
    /// that reach the model.
    ComposerLineSubmitted,
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

/// Status of one step in an `update_plan` checklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

impl PlanStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// One checklist row for the `update_plan` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub step: String,
    pub status: PlanStepStatus,
}

/// Arguments for the model-callable `update_plan` TODO/checklist tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlanArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub plan: Vec<PlanItem>,
}

/// One selectable answer offered by `ask_user_question`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskUserQuestionOption {
    /// Short user-facing label (1–5 words).
    pub label: String,
    /// One sentence explaining the tradeoff. Optional so models that omit it
    /// still validate.
    #[serde(default)]
    pub description: String,
}

/// One question in an `ask_user_question` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskUserQuestionItem {
    /// Stable id echoed in the answer. Empty ids are filled as `q1`, `q2`, …
    #[serde(default)]
    pub id: String,
    pub question: String,
    /// Short heading shown as a chip (max 30 characters after normalize).
    #[serde(default)]
    pub header: String,
    /// 2–4 choices, or empty for a free-text-only question. Do not include
    /// "Other" — the host adds free-text input automatically.
    #[serde(default)]
    pub options: Vec<AskUserQuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

/// Arguments for the model-callable `ask_user_question` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskUserQuestionArgs {
    pub questions: Vec<AskUserQuestionItem>,
}

/// Answer to one question in an `ask_user_question` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserQuestionAnswerItem {
    pub id: String,
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

/// Canonical tool result for `ask_user_question`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserQuestionResult {
    pub answers: Vec<AskUserQuestionAnswerItem>,
}

/// Durable payload for [`WaitReason::Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPayload {
    pub call_id: String,
    pub tool: String,
    pub questions: Vec<AskUserQuestionItem>,
}

impl AskUserQuestionArgs {
    pub const MIN_QUESTIONS: usize = 1;
    pub const MAX_QUESTIONS: usize = 4;
    pub const MIN_OPTIONS: usize = 2;
    pub const MAX_OPTIONS: usize = 4;
    pub const MAX_HEADER_CHARS: usize = 30;

    /// Fill missing ids/headers and reject shapes the TUI cannot present.
    pub fn normalize(mut self) -> Result<Self, String> {
        if self.questions.len() < Self::MIN_QUESTIONS || self.questions.len() > Self::MAX_QUESTIONS
        {
            return Err(format!(
                "ask_user_question requires {}–{} questions, got {}",
                Self::MIN_QUESTIONS,
                Self::MAX_QUESTIONS,
                self.questions.len()
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, question) in self.questions.iter_mut().enumerate() {
            if question.question.trim().is_empty() {
                return Err(format!("question {} is missing text", index + 1));
            }
            if question.id.trim().is_empty() {
                question.id = format!("q{}", index + 1);
            }
            if !seen.insert(question.id.clone()) {
                return Err(format!("duplicate question id `{}`", question.id));
            }
            if question.header.trim().is_empty() {
                question.header = question
                    .question
                    .chars()
                    .take(Self::MAX_HEADER_CHARS)
                    .collect();
            } else if question.header.chars().count() > Self::MAX_HEADER_CHARS {
                question.header = question
                    .header
                    .chars()
                    .take(Self::MAX_HEADER_CHARS)
                    .collect();
            }
            let option_count = question.options.len();
            if option_count == 1 || option_count > Self::MAX_OPTIONS {
                return Err(format!(
                    "question `{}` must have 0 or {}–{} options, got {option_count}",
                    question.id,
                    Self::MIN_OPTIONS,
                    Self::MAX_OPTIONS
                ));
            }
            for (opt_index, option) in question.options.iter_mut().enumerate() {
                option.label = option.label.trim().to_string();
                if option.label.is_empty() {
                    return Err(format!(
                        "question `{}` option {} is missing a label",
                        question.id,
                        opt_index + 1
                    ));
                }
            }
        }
        Ok(self)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitlPayload {
    pub call_id: String,
    pub tool: String,
    pub args_redacted: serde_json::Value,
    pub reason: String,
    /// What the sandbox actually reported, when this prompt follows a real
    /// refusal. `reason` explains the *category* of denial and is identical
    /// for every command in that category; this is the evidence for this one.
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub sandbox_escalation: bool,
    /// Host the egress proxy refused, when this prompt is a network grant
    /// rather than an unconfined retry. `None` for MCP and filesystem HITL.
    #[serde(default)]
    pub denied_host: Option<String>,
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
/// `Approval` is the tool-call HITL gate. `Question` is the
/// `ask_user_question` producer. The remaining variants are structurally
/// complete but currently unused.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WaitReason {
    Approval {
        request_id: String,
        payload: HitlPayload,
    },
    Question {
        request_id: String,
        payload: QuestionPayload,
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
            | WaitReason::Question { request_id, .. }
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

/// Stable identifier for one background task (shell job or subagent),
/// distinct from `TaskId` — a background task is not the single foreground
/// attempt tracked by `ActiveTaskState`, it's one entry in
/// `forge-core`'s `BackgroundTaskRegistry`. Scoped to one session, like
/// `TaskId`/`QueueItemId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackgroundTaskId(pub u64);

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
                failure: None,
                sandbox_escalation: false,
                denied_host: None,
            },
        };
        assert_eq!(approval.request_id(), "r1");

        let question = WaitReason::Question {
            request_id: "r2".into(),
            payload: QuestionPayload {
                call_id: "r2".into(),
                tool: "ask_user_question".into(),
                questions: vec![AskUserQuestionItem {
                    id: "q1".into(),
                    question: "Which auth?".into(),
                    header: "Auth".into(),
                    options: vec![],
                    multi_select: false,
                }],
            },
        };
        assert_eq!(question.request_id(), "r2");

        let clarification = WaitReason::Clarification {
            request_id: "r2b".into(),
        };
        assert_eq!(clarification.request_id(), "r2b");

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
        assert_eq!(message.outcome, ExecutionOutcome::Success);
    }

    #[test]
    fn execution_outcome_roundtrips_through_serde() {
        let cases = [
            ExecutionOutcome::Success,
            ExecutionOutcome::Failed { exit_code: Some(7) },
            ExecutionOutcome::Failed { exit_code: None },
            ExecutionOutcome::SpawnFailed {
                reason: "command not found".into(),
            },
            ExecutionOutcome::Denied {
                reason: "denied by ACL".into(),
            },
            ExecutionOutcome::Cancelled,
            ExecutionOutcome::TimedOut,
        ];
        for outcome in cases {
            let j = serde_json::to_string(&outcome).unwrap();
            let back: ExecutionOutcome = serde_json::from_str(&j).unwrap();
            assert_eq!(back, outcome);
        }
    }

    #[test]
    fn tool_output_effective_outcome_falls_back_for_legacy_json() {
        let legacy = r#"{"content":"boom","is_error":true,"exit_code":2}"#;
        let output: ToolOutput = serde_json::from_str(legacy).unwrap();
        assert!(output.outcome.is_none());
        assert_eq!(
            output.effective_outcome(),
            ExecutionOutcome::Failed { exit_code: Some(2) }
        );

        let legacy_success = r#"{"content":"ok","is_error":false}"#;
        let output: ToolOutput = serde_json::from_str(legacy_success).unwrap();
        assert_eq!(output.effective_outcome(), ExecutionOutcome::Success);
    }

    #[test]
    fn tool_output_constructors_keep_fields_in_sync() {
        let denied = ToolOutput::denied("denied by ACL: bash");
        assert!(denied.is_error);
        assert_eq!(
            denied.outcome,
            Some(ExecutionOutcome::Denied {
                reason: "denied by ACL: bash".into()
            })
        );

        let spawn_failed = ToolOutput::spawn_failed("boom", "command not found");
        assert!(spawn_failed.is_error);
        assert_eq!(
            spawn_failed.outcome,
            Some(ExecutionOutcome::SpawnFailed {
                reason: "command not found".into()
            })
        );
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

    #[test]
    fn update_plan_args_round_trip_snake_case_status() {
        let raw = serde_json::json!({
            "explanation": "starting",
            "plan": [
                {"step": "one", "status": "completed"},
                {"step": "two", "status": "in_progress"},
                {"step": "three", "status": "pending"}
            ]
        });
        let args: UpdatePlanArgs = serde_json::from_value(raw).unwrap();
        assert_eq!(args.explanation.as_deref(), Some("starting"));
        assert_eq!(args.plan.len(), 3);
        assert_eq!(args.plan[1].status, PlanStepStatus::InProgress);
        assert_eq!(args.plan[1].status.as_str(), "in_progress");
    }

    #[test]
    fn ask_user_question_args_normalize_fills_ids_and_rejects_bad_counts() {
        let args = AskUserQuestionArgs {
            questions: vec![AskUserQuestionItem {
                id: String::new(),
                question: "Which database?".into(),
                header: String::new(),
                options: vec![
                    AskUserQuestionOption {
                        label: "Postgres (Recommended)".into(),
                        description: "Relational default.".into(),
                    },
                    AskUserQuestionOption {
                        label: "SQLite".into(),
                        description: "Local file.".into(),
                    },
                ],
                multi_select: false,
            }],
        };
        let normalized = args.normalize().unwrap();
        assert_eq!(normalized.questions[0].id, "q1");
        assert_eq!(normalized.questions[0].header, "Which database?");

        let empty = AskUserQuestionArgs { questions: vec![] };
        assert!(empty.normalize().unwrap_err().contains("1–4"));

        let one_option = AskUserQuestionArgs {
            questions: vec![AskUserQuestionItem {
                id: "db".into(),
                question: "Which database?".into(),
                header: "DB".into(),
                options: vec![AskUserQuestionOption {
                    label: "Postgres".into(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
        };
        assert!(one_option.normalize().unwrap_err().contains("0 or 2–4"));
    }
}
