//! Agent loop — Phase 1 base + Phase 2 hooks (context, HITL, governance).

mod background;
mod completion;
mod lifecycle;
mod persistence;
mod queue;
mod resume;
mod stream;
mod subagent;
mod task_runtime;
mod turn;
pub(crate) mod turn_state;

pub use stream::{
    accumulate_stream_event, merge_streamed_response, observe_stream_event, stream_turn_event,
    ModelStepAccumulator,
};

pub use background::{
    BackgroundTaskHandle, BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus,
};
pub use completion::{
    CompletionDecision, CompletionEvaluator, CompletionReason, DefaultCompletionEvaluator,
    EvidenceEntry, EvidenceSummary, ExecutionEvent, ExecutionEvidence, FileEffectExpectation,
    FileEffectKind, GitEffectExpectation, GitEffectKind, TaskExpectation, ToolExpectation,
};
pub use lifecycle::{ActiveTaskState, TransitionError, TransitionReason};
pub use queue::{QueuedTask, TaskQueue};
pub use session::tools::{
    CompletedToolApplication, ModelResponseApplication, PendingToolApplication,
};
pub use subagent::{SubagentOutcome, SubagentSpec};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use forge_config::WebSearchConfig;
use forge_context::{estimate_messages_tokens, estimate_tokens, ContextEngine};
use forge_durable::{new_session_id, Journal};
use forge_governance::{AuditEvent, Governance};
use forge_model::{ModelClient, ModelRequest, SharedMessages, StreamEventTx};
use forge_tools::{
    default_builtins_with_web_search, ToolContext, ToolError, ToolRegistry, ValidationBudget,
};
use forge_types::{
    BackgroundTaskId, ExecutionOutcome, HitlDecision, HitlPayload, Message, MessageRole,
    ModelResponse, PolicyDecision, SessionId, SideEffectClass, TaskId, TaskLifecycle, ToolCall,
    ToolOutput, Usage, WaitReason,
};
use serde_json::json;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::persistence::SessionPersistence;
use crate::task_runtime::TaskRuntime;
use crate::turn::TurnCoordinator;
use crate::turn_state::TurnState;

mod helpers;
mod session;
mod types;

pub use helpers::{session_title_hint, TURN_FAILED_MARKER};
pub use types::*;

pub(crate) use helpers::*;

pub struct AgentSession {
    pub session_id: SessionId,
    pub messages: SharedMessages,
    pub events: Vec<TurnEvent>,
    /// Authoritative task/attempt lifecycle. The single source of truth for
    /// "what task is active, is Forge working/waiting, and why" — UI code
    /// must read this rather than deriving its own copy from busy/streaming
    /// flags. All mutation goes through `AgentSession::transition`/
    /// `enter_waiting`/`start_new_task`, never a direct field write.
    pub active_task: ActiveTaskState,
    /// Queue and background-task runtime stores. Public access is provided by
    /// read-only accessors so their ownership stays inside the session.
    tasks: TaskRuntime,
    /// Provider/model id for the next completion (empty → client default).
    pub active_model: String,
    /// Stable offering identity for `active_model`.
    pub active_route_id: String,
    /// Wire-level reasoning-effort value for the next completion, or `None`
    /// to omit the field entirely (model doesn't support it, or effort is
    /// Auto). Set via `set_reasoning_effort`, read by `build_model_request`.
    reasoning_effort: Option<String>,
    journal: SessionPersistence,
    /// Shared across a parent session and every subagent spawned from it —
    /// `register` only ever runs during `create`/`resume` setup, so sharing
    /// via `Arc` after that point is a type change, not a behavior change.
    tools: Arc<ToolRegistry>,
    model: Arc<dyn ModelClient>,
    tool_ctx: ToolContext,
    max_turns: u32,
    governance: Governance,
    context: ContextEngine,
    enable_context: bool,
    enable_gov: bool,
    /// `Some` only for a subagent session — flipped by the parent's
    /// `BackgroundTaskRegistry::cancel`, checked in
    /// `run_model_step_with_stream`'s streaming poll loop. `None` for the
    /// top-level/foreground session, which is cancelled via the TUI's own
    /// `cancel_requested` bool instead.
    cancel_token: Option<CancellationToken>,
    /// Cumulative provider token usage for this session.
    pub token_usage: SessionTokenUsage,
    /// Runtime bookkeeping scoped to the current user turn. It is reset when
    /// a new user message starts and is intentionally not persisted as its
    /// own journal state.
    turn: TurnState,
    /// The `CompletionEvaluator` decision for the most recently finished
    /// turn, for callers/tests that want the machine-readable reason behind
    /// `status` without re-deriving it from messages.
    pub last_completion: Option<CompletionDecision>,
    /// Journaled tool results indexed by call id — used to avoid re-execution on resume.
    journaled_tool_results: HashMap<String, forge_durable::ToolResultPayload>,
    /// Memoized `estimate_messages_tokens` total. When the same transcript
    /// allocation only grows at the tail, the cache adds the new messages
    /// instead of recounting the complete history.
    ctx_tokens_cache: Mutex<Option<CtxTokensCache>>,
}

#[derive(Debug)]
struct CtxTokensCache {
    storage_id: u64,
    fingerprint: CtxTokensFingerprint,
    total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CtxTokensFingerprint {
    messages: usize,
    last_content: usize,
    last_thinking: usize,
}

impl CtxTokensFingerprint {
    fn of(messages: &[Message]) -> Self {
        Self {
            messages: messages.len(),
            last_content: messages.last().map_or(0, |m| m.content.len()),
            last_thinking: messages
                .last()
                .and_then(|m| m.thinking.as_ref())
                .map_or(0, String::len),
        }
    }
}

#[cfg(test)]
mod tests;
