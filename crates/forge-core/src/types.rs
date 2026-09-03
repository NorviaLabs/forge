//! Public types describing a session's configuration, errors, and the
//! reports it produces.
//!
//! Split out of `lib.rs`; moved verbatim.

use crate::*;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoopError {
    #[error(transparent)]
    Journal(#[from] forge_durable::JournalError),
    #[error(transparent)]
    Model(#[from] forge_model::ModelError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Context(#[from] forge_context::ContextError),
    /// Compaction could not produce an installable context. The active
    /// context is unchanged; see `forge_context::compaction::CompactionError`.
    #[error(transparent)]
    Compaction(#[from] forge_context::compaction::CompactionError),
    #[error("session awaiting HITL; call resolve_hitl first")]
    AwaitingHitl,
    #[error("no pending HITL")]
    NoPendingHitl,
    #[error("no pending question")]
    NoPendingQuestion,
    #[error(transparent)]
    Transition(#[from] TransitionError),
    /// A `cancel_token` (subagent cancellation) fired mid-step — distinct
    /// from `Other` so callers can react without string-matching an error
    /// message.
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_turns: u32,
    pub workspace: PathBuf,
    pub journal_dir: PathBuf,
    pub enable_context_lifecycle: bool,
    pub enable_governance: bool,
    /// Phase 9 — controls registration of `web_search` (WEB-01).
    pub web_search: WebSearchConfig,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 128,
            workspace: PathBuf::from("."),
            journal_dir: PathBuf::from(".forge/sessions"),
            enable_context_lifecycle: true,
            enable_governance: true,
            web_search: WebSearchConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnEvent {
    pub kind: String,
    pub detail: String,
}

/// Cumulative API-reported token usage for a session (not $ cost).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTokenUsage {
    /// Sum of provider-reported prompt/input tokens across model calls.
    pub prompt_tokens: u64,
    /// Sum of provider-reported completion/output tokens across model calls.
    pub completion_tokens: u64,
    /// Number of model complete/stream calls that reported usage.
    pub model_calls_with_usage: u32,
    /// Model steps applied (with or without usage metadata).
    pub model_steps: u32,
    /// Estimated thinking/reasoning tokens (from thinking text, ~4 chars/token).
    pub thinking_tokens_est: u64,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
}

impl SessionTokenUsage {
    pub fn total_api_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }

    pub fn record_response(&mut self, usage: Option<&Usage>, thinking: Option<&str>) {
        self.model_steps = self.model_steps.saturating_add(1);
        if let Some(u) = usage {
            self.prompt_tokens = self.prompt_tokens.saturating_add(u.prompt_tokens as u64);
            self.completion_tokens = self
                .completion_tokens
                .saturating_add(u.completion_tokens as u64);
            self.prompt_cache_hits = self
                .prompt_cache_hits
                .saturating_add(u.prompt_cache_read_tokens as u64);
            self.prompt_cache_writes = self
                .prompt_cache_writes
                .saturating_add(u.prompt_cache_write_tokens as u64);
            self.model_calls_with_usage = self.model_calls_with_usage.saturating_add(1);
        }
        if let Some(th) = thinking.filter(|t| !t.trim().is_empty()) {
            self.thinking_tokens_est = self
                .thinking_tokens_est
                .saturating_add(estimate_tokens(th) as u64);
        }
    }
}

/// Snapshot of session token metrics for status UIs.
#[derive(Debug, Clone)]
pub struct TokenUsageReport {
    pub api: SessionTokenUsage,
    pub context_tokens_est: usize,
    pub context_capacity: usize,
    pub context_pct: f64,
    pub system_tokens_est: usize,
    pub user_tokens_est: usize,
    pub assistant_tokens_est: usize,
    pub tool_tokens_est: usize,
    pub thinking_in_context_est: usize,
    /// Estimated wire cost of the tool schemas sent with every request
    /// (name + description + JSON input schema, post-governance filtering).
    /// Not part of `context_tokens_est`: it isn't a transcript message, it's
    /// the `tools` field resent on every completion.
    pub tool_schema_tokens_est: usize,
    pub message_count: usize,
    pub tool_message_count: usize,
}

/// Result of applying one model response inside the agent loop.
#[derive(Debug)]
#[non_exhaustive]
pub enum ApplyOutcome {
    /// No tool calls — turn finished.
    Done(ModelResponse),
    /// Tools ran; call the model again.
    Continue,
    /// Paused for human-in-the-loop.
    Hitl(ModelResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    pub last_seq: u64,
    pub model_steps: usize,
    pub tool_results: usize,
    pub incomplete_intents: usize,
    /// Legacy session composer lines retained for journal compatibility. TUI
    /// recall is now loaded from its user-level workspace history store.
    pub composer_lines: Vec<String>,
}
