//! Agent loop — Phase 1 base + Phase 2 hooks (context, HITL, governance).

use std::path::PathBuf;
use std::sync::Arc;

use forge_config::WebSearchConfig;
use forge_context::{estimate_messages_tokens, estimate_tokens, ContextEngine};
use forge_durable::{new_session_id, Journal};
use forge_governance::{AuditEvent, Governance};
use forge_model::{ModelClient, ModelRequest, StreamEventTx};
use forge_tools::{
    default_builtins_with_web_search, ToolContext, ToolError, ToolRegistry, ValidationBudget,
};
use forge_types::{
    HitlDecision, HitlPayload, Message, MessageRole, ModelResponse, PolicyDecision, SessionId,
    SessionStatus, SideEffectClass, ToolCall, ToolOutput, Usage,
};
use serde_json::json;
use thiserror::Error;
use tracing::warn;

const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

fn assemble_system_prompt(agents_md: &str, skills: &[(String, String)]) -> String {
    let mut prompt = SYSTEM_PROMPT.trim_end().to_owned();

    if !agents_md.trim().is_empty() {
        prompt.push_str("\n\n# Project Instructions\n\nAGENTS.md:\n");
        prompt.push_str(agents_md);
    }

    if !skills.is_empty() {
        prompt.push_str("\n\n# Skills");
        for (name, content) in skills {
            prompt.push_str("\n\n## ");
            prompt.push_str(name);
            prompt.push_str("\n\n");
            prompt.push_str(content.trim());
        }
    }

    prompt
}

/// Durable marker for a terminal turn failure summary in session messages.
/// Presentation maps this to TurnFailure; it is never a user-facing answer.
pub const TURN_FAILED_MARKER: &str = "[forge.turn_failed]";

/// Remove structural protocol control markers from final-answer text before
/// persistence. Not phrase filtering — only known control envelopes.
fn strip_protocol_markers(text: &str) -> String {
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
    #[error("session awaiting HITL; call resolve_hitl first")]
    AwaitingHitl,
    #[error("no pending HITL")]
    NoPendingHitl,
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
}

pub struct AgentSession {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub messages: Vec<Message>,
    pub events: Vec<TurnEvent>,
    pub pending_hitl: Option<HitlPayload>,
    /// Provider/model id for the next completion (empty → client default).
    pub active_model: String,
    journal: Journal,
    tools: ToolRegistry,
    model: Arc<dyn ModelClient>,
    tool_ctx: ToolContext,
    max_turns: u32,
    governance: Governance,
    context: ContextEngine,
    enable_context: bool,
    enable_gov: bool,
    /// Cumulative provider token usage for this session.
    pub token_usage: SessionTokenUsage,
    /// Validation failures within the current user turn (not persisted).
    validation_budget: ValidationBudget,
}

impl AgentSession {
    /// Replace the active conversation by replaying another session journal.
    pub async fn resume_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<ResumeReport, LoopError> {
        if session_id == self.session_id {
            return Ok(ResumeReport {
                last_seq: self.events.len() as u64,
                model_steps: self.token_usage.model_steps as usize,
                tool_results: self
                    .messages
                    .iter()
                    .filter(|message| message.role == MessageRole::Tool)
                    .count(),
                incomplete_intents: 0,
            });
        }

        let journal = Journal::open(self.journal.directory(), session_id).await?;
        let state = journal.replay(session_id).await?;
        let mut context = ContextEngine::new(self.context.workspace.clone(), session_id);
        context.config = self.context.config.clone();
        let system_message = Message {
            role: MessageRole::System,
            content: assemble_system_prompt(&context.load_agents_md(), &context.load_skills()),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        };
        let mut messages = state.messages;
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            *first = system_message;
        } else {
            messages.insert(0, system_message);
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume (fail-safe)");
        }

        let active_root = context.workspace.clone();
        let pending_hitl = state
            .pending_hitl
            .and_then(|value| serde_json::from_value::<HitlPayload>(value).ok());
        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let report = ResumeReport {
            last_seq: state.last_seq,
            model_steps: state.model_responses.len(),
            tool_results: state.tool_results.len(),
            incomplete_intents: state.incomplete_intents.len(),
        };
        self.session_id = session_id;
        self.status = state.status;
        self.messages = messages;
        self.events = vec![TurnEvent {
            kind: "resume".into(),
            detail: format!("seq={}", state.last_seq),
        }];
        self.pending_hitl = pending_hitl;
        self.journal = journal;
        self.tool_ctx = ToolContext::new(active_root);
        self.context = context;
        self.token_usage = token_usage;
        // Stale Running without a live executor is Interrupted, not eternal Working.
        self.mark_interrupted_if_stale().await?;
        Ok(report)
    }

    pub async fn create(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
    ) -> Result<Self, LoopError> {
        for t in default_builtins_with_web_search(&loop_cfg.web_search) {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let session_id = new_session_id();
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        journal.append_session_created(session_id).await?;

        let active_root = loop_cfg.workspace.clone();
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let agents = context.load_agents_md();
        let skills = context.load_skills();
        let system = assemble_system_prompt(&agents, &skills);

        Ok(Self {
            session_id,
            status: SessionStatus::Running,
            messages: vec![Message {
                role: MessageRole::System,
                content: system,
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            }],
            events: vec![],
            pending_hitl: None,
            active_model: String::new(),
            journal,
            tools,
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            token_usage: SessionTokenUsage::default(),
            validation_budget: ValidationBudget::with_default_max(),
        })
    }

    pub async fn resume(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
        session_id: SessionId,
    ) -> Result<Self, LoopError> {
        for t in default_builtins_with_web_search(&loop_cfg.web_search) {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        let state = journal.replay(session_id).await?;
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let system = assemble_system_prompt(&context.load_agents_md(), &context.load_skills());
        let system_message = Message {
            role: MessageRole::System,
            content: system,
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        };
        let mut messages = state.messages.clone();
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            *first = system_message;
        } else {
            messages.insert(0, system_message);
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume (fail-safe)");
        }

        let active_root = loop_cfg.workspace.clone();

        let pending_hitl = state
            .pending_hitl
            .and_then(|v| serde_json::from_value::<HitlPayload>(v).ok());

        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let mut session = Self {
            session_id,
            status: state.status,
            messages,
            events: vec![TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            pending_hitl,
            active_model: String::new(),
            journal,
            tools,
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            token_usage,
            validation_budget: ValidationBudget::with_default_max(),
        };
        // Legacy fallback: Running with no runtime becomes Interrupted (not guessed from text).
        session.mark_interrupted_if_stale().await?;
        Ok(session)
    }

    pub fn set_governance(&mut self, g: Governance) {
        self.governance = g;
    }

    pub fn journal_dir(&self) -> &std::path::Path {
        self.journal.directory()
    }

    /// Number of project/global skills available to the current session.
    /// This is intentionally a count only: skill contents remain model context.
    pub fn loaded_skills_count(&self) -> usize {
        self.context.load_skills().len()
    }

    /// Names of project/global skills available to the current session.
    pub fn loaded_skill_names(&self) -> Vec<String> {
        self.context
            .load_skills()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    pub fn list_tools(&self) -> Vec<String> {
        let desc = self.tools.list_descriptors();
        if self.enable_gov {
            self.governance
                .filter_tools(desc)
                .into_iter()
                .map(|t| t.name)
                .collect()
        } else {
            self.tools.names()
        }
    }

    pub fn context_usage_ratio(&self) -> f64 {
        self.context.usage_ratio(&self.messages)
    }

    pub fn context_reset_ratio(&self) -> f64 {
        self.context.config.reset_usage_ratio
    }

    pub async fn journal_cursor(&self) -> Result<u64, LoopError> {
        Ok(self.journal.last_seq().await?)
    }

    /// Full token-usage report for status UIs (API totals + in-context estimates). No $.
    pub fn token_usage_report(&self) -> TokenUsageReport {
        let mut system_tokens_est = 0usize;
        let mut user_tokens_est = 0usize;
        let mut assistant_tokens_est = 0usize;
        let mut tool_tokens_est = 0usize;
        let mut thinking_in_context_est = 0usize;
        let mut tool_message_count = 0usize;
        for m in &self.messages {
            let n = estimate_tokens(&m.content);
            match m.role {
                MessageRole::System => system_tokens_est = system_tokens_est.saturating_add(n),
                MessageRole::User => user_tokens_est = user_tokens_est.saturating_add(n),
                MessageRole::Assistant => {
                    assistant_tokens_est = assistant_tokens_est.saturating_add(n);
                    if let Some(ref th) = m.thinking {
                        thinking_in_context_est =
                            thinking_in_context_est.saturating_add(estimate_tokens(th));
                    }
                }
                MessageRole::Tool => {
                    tool_tokens_est = tool_tokens_est.saturating_add(n);
                    tool_message_count = tool_message_count.saturating_add(1);
                }
                // `MessageRole` is `#[non_exhaustive]`. Count an unrecognised future role
                // toward the user bucket so the context budget total stays accurate instead
                // of silently under-counting the window.
                _ => user_tokens_est = user_tokens_est.saturating_add(n),
            }
        }
        let context_tokens_est =
            estimate_messages_tokens(&self.messages).saturating_add(thinking_in_context_est);
        let context_capacity = self.context.config.capacity_tokens.max(1);
        let context_pct = (context_tokens_est as f64 / context_capacity as f64) * 100.0;
        TokenUsageReport {
            api: self.token_usage.clone(),
            context_tokens_est,
            context_capacity,
            context_pct,
            system_tokens_est,
            user_tokens_est,
            assistant_tokens_est,
            tool_tokens_est,
            thinking_in_context_est,
            message_count: self.messages.len(),
            tool_message_count,
        }
    }

    pub fn token_usage_lines(&self) -> Vec<String> {
        let r = self.token_usage_report();
        let api = &r.api;
        let mut lines = vec![
            "Session token usage (not $)".to_string(),
            String::new(),
            "API-reported (cumulative)".to_string(),
            format!("  prompt/input tokens:      {}", api.prompt_tokens),
            format!("  completion/output tokens: {}", api.completion_tokens),
            format!("  total API tokens:         {}", api.total_api_tokens()),
            format!(
                "  model steps:              {} ({} with usage metadata)",
                api.model_steps, api.model_calls_with_usage
            ),
            format!("  thinking tokens (est.):   {}", api.thinking_tokens_est),
            String::new(),
            "In-context estimate (~4 chars/token)".to_string(),
            format!(
                "  total: {} / {}  ({:.1}% of capacity)",
                r.context_tokens_est, r.context_capacity, r.context_pct
            ),
            format!("  system:    {}", r.system_tokens_est),
            format!("  user:      {}", r.user_tokens_est),
            format!("  assistant: {}", r.assistant_tokens_est),
            format!(
                "  tool:      {} ({} tool msgs)",
                r.tool_tokens_est, r.tool_message_count
            ),
            format!("  thinking:  {}", r.thinking_in_context_est),
            format!("  messages:  {}", r.message_count),
        ];
        if api.model_steps > 0 && api.model_calls_with_usage == 0 {
            lines.push(String::new());
            lines.push("Note: provider did not return usage; API totals may stay 0.".into());
        }
        lines
    }

    /// Append a user message to the session (journal + transcript) without calling the model.
    /// Used by the TUI so the YOU bubble can paint before the model run starts.
    pub async fn append_user_message(&mut self, text: &str) -> Result<(), LoopError> {
        if self.status == SessionStatus::AwaitingHitl {
            return Err(LoopError::AwaitingHitl);
        }
        self.journal
            .append_user_message(self.session_id, text)
            .await?;
        self.messages.push(Message {
            role: MessageRole::User,
            content: text.into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        if self.context.goal.is_empty() {
            self.context.goal = text.chars().take(200).collect();
        }
        self.status = SessionStatus::Running;
        // Fresh validation budget for each user turn.
        self.validation_budget = ValidationBudget::with_default_max();
        Ok(())
    }

    /// Shared model client handle (for streaming from the TUI without holding `&mut self`).
    pub fn model_client(&self) -> Arc<dyn ModelClient> {
        self.model.clone()
    }

    /// Build the next model request from current transcript + tools.
    pub fn build_model_request(&self) -> ModelRequest {
        let mut tools = self.tools.list_descriptors();
        if self.enable_gov {
            tools = self.governance.filter_tools(tools);
        }
        ModelRequest {
            messages: self.messages.clone(),
            tools,
            model: self.active_model.clone(),
            reasoning_effort: None,
            prompt_cache: true,
        }
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
                role: MessageRole::Assistant,
                content: final_text.clone(),
                tool_call_id: None,
                name: None,
                thinking: last.thinking.clone().filter(|t| !t.trim().is_empty()),
                thinking_duration_secs: None,
                tool_calls: last.tool_calls.clone(),
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
            if final_text.is_empty() {
                // No durable final answer. If the turn already did tool/validation
                // work, this is a failed terminal state — not silent success.
                if self.current_turn_has_tool_activity() {
                    self.finalize_turn_failure(
                        "Forge couldn't complete this turn.",
                        "no_final_answer",
                    )
                    .await?;
                    return Ok(ApplyOutcome::Done(last));
                }
                // Idle / no-op response with no tools: still complete cleanly.
            }
            self.status = SessionStatus::Completed;
            self.journal
                .append_status(self.session_id, SessionStatus::Completed)
                .await?;
            return Ok(ApplyOutcome::Done(last));
        }

        // Budget spans the whole user turn so repeated invalid calls across
        // model steps still exhaust instead of looping forever.
        let mut budget = std::mem::take(&mut self.validation_budget);
        let tool_result = async {
            for call in &last.tool_calls {
                if let Some(pause) = self.run_one_tool(call, &mut budget).await? {
                    return Ok(ApplyOutcome::Hitl(pause));
                }
                // Retry exhaustion is a terminal failure — do not Continue the loop.
                if self.status == SessionStatus::Failed {
                    return Ok(ApplyOutcome::Done(last.clone()));
                }
            }
            Ok(ApplyOutcome::Continue)
        }
        .await;
        self.validation_budget = budget;
        tool_result
    }

    /// True when the open user turn already has tool or validation activity.
    fn current_turn_has_tool_activity(&self) -> bool {
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
        if self.status == SessionStatus::Failed {
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
            role: MessageRole::Assistant,
            content: content.clone(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        self.events.push(TurnEvent {
            kind: "turn_failed".into(),
            detail: format!("{category}: {summary}"),
        });
        self.status = SessionStatus::Failed;
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
        self.journal
            .append_status(self.session_id, SessionStatus::Failed)
            .await?;
        Ok(())
    }

    /// Persist operator/system cancellation of the foreground task.
    pub async fn mark_cancelled(&mut self) -> Result<(), LoopError> {
        if self.status == SessionStatus::Cancelled {
            return Ok(());
        }
        self.status = SessionStatus::Cancelled;
        self.pending_hitl = None;
        self.journal
            .append_status(self.session_id, SessionStatus::Cancelled)
            .await?;
        self.events.push(TurnEvent {
            kind: "cancelled".into(),
            detail: "foreground task cancelled".into(),
        });
        Ok(())
    }

    /// On resume/reload: a durable Running/AwaitingHitl task with no live runtime
    /// cannot safely continue as Working. HITL remains Waiting; bare Running becomes
    /// Interrupted. Legacy sessions with no terminal metadata stay Interrupted rather
    /// than eternal Working. Completed/Failed/Cancelled are left untouched.
    pub async fn mark_interrupted_if_stale(&mut self) -> Result<(), LoopError> {
        match self.status {
            SessionStatus::Running => {
                // No active runtime after reload/resume.
                self.status = SessionStatus::Interrupted;
                self.journal
                    .append_status(self.session_id, SessionStatus::Interrupted)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "interrupted".into(),
                    detail: "stale running task has no recoverable runtime".into(),
                });
                Ok(())
            }
            // AwaitingHitl is still a recoverable waiting state (operator can decide).
            SessionStatus::AwaitingHitl
            | SessionStatus::Completed
            | SessionStatus::Failed
            | SessionStatus::Cancelled
            | SessionStatus::Interrupted => Ok(()),
            // `SessionStatus` is `#[non_exhaustive]`. Leave an unrecognised status untouched
            // rather than forcing it to Interrupted.
            _ => Ok(()),
        }
    }

    /// Run until no tool calls, max turns, or HITL pause.
    pub async fn run_user_message(&mut self, text: &str) -> Result<ModelResponse, LoopError> {
        self.append_user_message(text).await?;
        self.run_agent_turns(None).await
    }

    /// Context-reset (if needed) + journal a model request; returns the request to send.
    pub async fn prepare_model_step(&mut self, turn: u32) -> Result<ModelRequest, LoopError> {
        if self.enable_context && self.context.should_reset(&self.messages) {
            let ws_ref = String::new();
            let system =
                assemble_system_prompt(&self.context.load_agents_md(), &self.context.load_skills());
            let (doc, msgs) = self
                .context
                .handoff_reset(&self.messages, &ws_ref, &system)?;
            self.journal
                .append_context_reset(
                    self.session_id,
                    json!({ "progress": doc, "messages": msgs }),
                )
                .await?;
            self.messages = msgs;
            self.events.push(TurnEvent {
                kind: "context_reset".into(),
                detail: "threshold".into(),
            });
        }

        tracing::debug!(turn, "model step");
        self.journal
            .append_model_request(
                self.session_id,
                json!({ "turn": turn, "messages": self.messages.len() }),
            )
            .await?;
        Ok(self.build_model_request())
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
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
        if self.status == SessionStatus::AwaitingHitl {
            return Err(LoopError::AwaitingHitl);
        }

        for turn in 0..self.max_turns {
            let req = self.prepare_model_step(turn).await?;
            let last = self
                .model
                .complete_with_stream(req, stream_tx.clone())
                .await?;

            match self.apply_model_response(last).await? {
                ApplyOutcome::Done(resp) => return Ok(resp),
                ApplyOutcome::Hitl(resp) => return Ok(resp),
                ApplyOutcome::Continue => continue,
            }
        }

        self.fail_max_turns().await?;
        Err(LoopError::Other("max_turns exceeded".into()))
    }

    /// Returns Some(response) if paused for HITL.
    async fn run_one_tool(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<Option<ModelResponse>, LoopError> {
        let class = self
            .tools
            .get(&call.name)
            .map(|t| t.side_effect_class())
            .unwrap_or(SideEffectClass::Meta);

        if self.enable_gov {
            let decision = self.governance.authorize(call, class);
            let redacted = self.governance.redact_args(&call.arguments);
            self.governance.record_audit(AuditEvent {
                session_id: self.session_id.to_string(),
                principal: self.governance.principal.id.clone(),
                tool: call.name.clone(),
                args_redacted: redacted.clone(),
                decision,
                policy_id: "default".into(),
                result: format!("{decision:?}"),
                duration_ms: 0,
                trace_id: None,
            });
            match decision {
                PolicyDecision::Hitl => {
                    let payload = HitlPayload {
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        args_redacted: redacted,
                        reason: "policy requires human approval".into(),
                    };
                    self.journal
                        .append_hitl_wait(self.session_id, &serde_json::to_value(&payload).unwrap())
                        .await?;
                    self.journal
                        .append_status(self.session_id, SessionStatus::AwaitingHitl)
                        .await?;
                    self.pending_hitl = Some(payload.clone());
                    self.status = SessionStatus::AwaitingHitl;
                    self.events.push(TurnEvent {
                        kind: "hitl_wait".into(),
                        detail: payload.tool.clone(),
                    });
                    return Ok(Some(ModelResponse {
                        text: format!("Awaiting HITL approval for tool {}", call.name),
                        tool_calls: vec![call.clone()],
                        usage: None,
                        thinking: None,
                    }));
                }
                PolicyDecision::Allow => {}
                // `PolicyDecision` is `#[non_exhaustive]`, so the denial path is the
                // wildcard rather than a named `Deny`: this gate must fail CLOSED. An
                // explicit deny and a decision this build does not recognise are both
                // refused here, so neither can fall through to the execution below.
                _ => {
                    let output = ToolOutput {
                        content: format!("denied by ACL: {}", call.name),
                        is_error: true,
                    };
                    self.journal
                        .append_tool_intent(self.session_id, call)
                        .await?;
                    self.journal
                        .append_tool_result(self.session_id, call, &output)
                        .await?;
                    self.messages.push(Message {
                        role: MessageRole::Tool,
                        content: output.content,
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        thinking: None,
                        thinking_duration_secs: None,
                        tool_calls: vec![],
                    });
                    return Ok(None);
                }
            }
        }

        self.journal
            .append_tool_intent(self.session_id, call)
            .await?;

        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(mut output) => {
                if self.enable_context {
                    output.content = self.context.maybe_offload_tool_content(output.content)?;
                }
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                self.events.push(TurnEvent {
                    kind: "tool".into(),
                    detail: format!("{} -> {} chars", call.name, output.content.len()),
                });
            }
            Err(ToolError::Validation(ve)) => {
                self.journal
                    .append_validation_failed(self.session_id, &call.name, &ve.to_string())
                    .await?;
                // Actionable, schema-derived feedback — no guessed corrected values.
                let msg = format!(
                    "Tool validation error: {ve}. \
                     Do not concatenate fields. Use separate JSON properties with native types \
                     (for example offset: 1, limit: 100 as integers)."
                );
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: msg.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                self.events.push(TurnEvent {
                    kind: "validation".into(),
                    detail: msg,
                });
            }
            Err(e) => {
                let content = e.to_string();
                let is_budget = content.contains("validation retry budget exceeded");
                let output = ToolOutput {
                    content: if is_budget {
                        format!(
                            "{content}. Stop retrying this tool with the same invalid argument shape. \
                             Either call it with valid structured JSON types or finish with a final answer."
                        )
                    } else {
                        content
                    },
                    is_error: true,
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                if is_budget {
                    self.events.push(TurnEvent {
                        kind: "validation_exhausted".into(),
                        detail: output.content.clone(),
                    });
                    // Terminal failure: stop the turn instead of activity-only hang.
                    self.finalize_turn_failure(
                        "Forge couldn't complete this turn after repeated invalid tool calls.",
                        "validation_exhausted",
                    )
                    .await?;
                }
            }
        }
        Ok(None)
    }

    /// DUR-03: resolve pending HITL then optionally execute the tool.
    pub async fn resolve_hitl(
        &mut self,
        decision: HitlDecision,
        actor: &str,
    ) -> Result<(), LoopError> {
        let payload = self.pending_hitl.clone().ok_or(LoopError::NoPendingHitl)?;
        // `HitlDecision` is `#[non_exhaustive]`. Derive approval explicitly rather than
        // testing `== Deny` below: a decision this build does not recognise must never
        // be read as approval and reach execution. Fail closed.
        let (dec, approved) = match decision {
            HitlDecision::Approve => ("approve", true),
            HitlDecision::Deny => ("deny", false),
            _ => ("deny", false),
        };
        self.journal
            .append_hitl_resume(self.session_id, dec, actor)
            .await?;

        if !approved {
            let output = ToolOutput {
                content: format!("HITL denied by {actor}"),
                is_error: true,
            };
            let call = ToolCall {
                id: payload.call_id.clone(),
                name: payload.tool.clone(),
                arguments: payload.args_redacted.clone(),
            };
            self.journal
                .append_tool_intent(self.session_id, &call)
                .await?;
            self.journal
                .append_tool_result(self.session_id, &call, &output)
                .await?;
            self.messages.push(Message {
                role: MessageRole::Tool,
                content: output.content,
                tool_call_id: Some(payload.call_id),
                name: Some(payload.tool),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            });
            self.pending_hitl = None;
            self.status = SessionStatus::Running;
            self.journal
                .append_status(self.session_id, SessionStatus::Running)
                .await?;
            return Ok(());
        }

        // Re-authorize
        let call = ToolCall {
            id: payload.call_id.clone(),
            name: payload.tool.clone(),
            arguments: payload.args_redacted.clone(),
        };
        let class = self
            .tools
            .get(&call.name)
            .map(|t| t.side_effect_class())
            .unwrap_or(SideEffectClass::Meta);
        if self.enable_gov {
            let d = self.governance.authorize(&call, class);
            // `PolicyDecision` is `#[non_exhaustive]`. Testing `== Deny` let every other
            // verdict through, so an unrecognised one would execute. Decide explicitly:
            // `Hitl` still proceeds because the operator already approved this call and
            // re-requiring approval here would stall the turn; anything unrecognised is
            // refused. Behaviour is unchanged for Allow, Hitl and Deny.
            let refuse = !matches!(d, PolicyDecision::Allow | PolicyDecision::Hitl);
            if refuse {
                self.pending_hitl = None;
                self.status = SessionStatus::Running;
                return Err(LoopError::Other(
                    "policy denies tool after HITL approve".into(),
                ));
            }
        }

        // Restore args from pending — we only have redacted; for tests use redacted as args
        let mut budget = ValidationBudget::with_default_max();
        self.pending_hitl = None;
        self.status = SessionStatus::Running;
        self.journal
            .append_status(self.session_id, SessionStatus::Running)
            .await?;
        // Execute with stored args (may be redacted in production; Phase 2 keeps full call in journal intent before wait ideally)
        // Re-fetch from last HitlWait — for approve path re-execute with redacted args is weak;
        // store original args in pending for this implementation:
        self.run_one_tool_exec_only(&call, &mut budget).await?;
        Ok(())
    }

    async fn run_one_tool_exec_only(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<(), LoopError> {
        self.journal
            .append_tool_intent(self.session_id, call)
            .await?;
        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(output) => {
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
            }
            Err(e) => {
                let output = ToolOutput {
                    content: e.to_string(),
                    is_error: true,
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
            }
        }
        Ok(())
    }

    /// Active workspace root.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.tool_ctx.workspace_root
    }

    /// Use this provider/model id on subsequent completions (e.g. after `/connect`).
    pub fn set_active_model(&mut self, model: impl Into<String>) {
        self.active_model = model.into();
    }

    /// Push provider credentials into the model client (OAuth tokens → worker env).
    pub fn apply_provider_env(&self, pairs: &[(String, String)]) {
        self.model.apply_provider_env(pairs);
    }

    /// Clear provider credentials from the model client and recycle the worker.
    pub fn clear_provider_env(&self) {
        self.model.clear_provider_env();
    }
}

impl AgentSession {
    pub async fn force_context_reset_async(&mut self) -> Result<(), LoopError> {
        let ws_ref = String::new();
        let system =
            assemble_system_prompt(&self.context.load_agents_md(), &self.context.load_skills());
        let (doc, msgs) = self
            .context
            .handoff_reset(&self.messages, &ws_ref, &system)?;
        self.journal
            .append_context_reset(
                self.session_id,
                json!({ "progress": doc, "workspace_ref": ws_ref, "messages": msgs }),
            )
            .await?;
        self.messages = msgs;
        self.events.push(TurnEvent {
            kind: "context_reset".into(),
            detail: "handoff written".into(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_governance::AclPolicy;
    use forge_model::MockModelClient;
    use forge_types::ToolCall;
    use tempfile::tempdir;

    #[test]
    fn system_prompt_uses_forge_policy() {
        let prompt = assemble_system_prompt("", &[]);
        assert!(prompt.starts_with("You are a coding agent running in the Forge"));
        assert!(prompt.contains("Forge is an open source project led by NorviaLabs."));
        assert!(!prompt.contains("# Project Instructions"));
    }

    #[test]
    fn system_prompt_appends_project_instructions() {
        let prompt = assemble_system_prompt("Run cargo test", &[]);
        assert!(prompt.starts_with("You are a coding agent running in the Forge"));
        assert!(prompt.ends_with("AGENTS.md:\nRun cargo test"));
    }

    #[test]
    fn system_prompt_appends_skills() {
        let skills = vec![(
            "ponytail".to_string(),
            "# Ponytail\nUse less code.".to_string(),
        )];
        let prompt = assemble_system_prompt("", &skills);
        assert!(prompt.contains("# Skills\n\n## ponytail"));
        assert!(prompt.ends_with("# Ponytail\nUse less code."));
    }

    fn base_cfg(dir: &std::path::Path) -> LoopConfig {
        LoopConfig {
            max_turns: 5,
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn session_registers_web_search_with_default_config() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let names = s.list_tools();
        assert!(
            names.iter().any(|n| n == "web_search"),
            "expected web_search in {names:?}"
        );
    }

    #[tokio::test]
    async fn session_omits_web_search_when_disabled() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut cfg = base_cfg(dir.path());
        cfg.web_search.enabled = false;
        let s = AgentSession::create(cfg, model, ToolRegistry::new())
            .await
            .unwrap();
        assert!(!s.list_tools().iter().any(|n| n == "web_search"));
    }

    #[tokio::test]
    async fn loop_text_only_completes() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "all done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("hello").await.unwrap();
        assert_eq!(r.text, "all done");
        assert_eq!(s.status, SessionStatus::Completed);
    }

    #[tokio::test]
    async fn loop_runs_tool_then_finishes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "read ok".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("read it").await.unwrap();
        assert_eq!(r.text, "read ok");
        let assistant = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
            .unwrap();
        assert_eq!(assistant.tool_calls[0].id, "1");
    }

    #[tokio::test]
    async fn malformed_read_file_offset_is_rejected_and_does_not_execute() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello\nworld\n").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "bad".into(),
                    name: "read_file".into(),
                    // Exact observed failure class — composite string must not be salvaged.
                    arguments: json!({"path": "README.md", "offset": "1arglimit\">100"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "ok".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "README.md", "offset": 1, "limit": 100}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "Forge is a Rust workspace.".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("Summarize this codebase").await.unwrap();
        assert_eq!(r.text, "Forge is a Rust workspace.");
        let tool_msgs: Vec<_> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .collect();
        assert!(
            tool_msgs.iter().any(|m| m.content.contains("validation")),
            "expected validation rejection: {tool_msgs:?}"
        );
        assert!(
            tool_msgs.iter().any(|m| m.content.contains("hello")),
            "valid retry should execute and return file content: {tool_msgs:?}"
        );
        // Validation feedback may quote the bad value; execution must not have
        // salvaged it into a successful read of the wrong slice.
        assert!(
            tool_msgs
                .iter()
                .filter(|m| m.content.contains("hello"))
                .all(|m| !m.content.contains("validation")),
            "successful tool result must not be a validation message"
        );
    }

    #[tokio::test]
    async fn repeated_malformed_read_file_exhausts_validation_budget() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "x\n").unwrap();
        let bad = || ToolCall {
            id: "bad".into(),
            name: "read_file".into(),
            arguments: json!({"path": "README.md", "offset": "1arglimit\">50"}),
        };
        // Four invalid attempts across model steps → budget exhausts → terminal failure.
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let _ = s.run_user_message("read it").await.unwrap();
        assert_eq!(s.status, SessionStatus::Failed);
        assert!(
            s.messages
                .iter()
                .any(|m| m.content.starts_with(TURN_FAILED_MARKER)),
            "expected durable failure summary: {:?}",
            s.messages
        );
        assert!(
            s.events.iter().any(|e| e.kind == "turn_failed"),
            "expected turn_failed event"
        );
        assert!(
            s.messages.iter().any(|m| {
                m.role == MessageRole::Tool
                    && m.content.contains("validation retry budget exceeded")
            }) || s.events.iter().any(|e| e.kind == "validation_exhausted"),
            "expected budget exhaustion signal"
        );
    }

    #[tokio::test]
    async fn empty_final_after_tools_is_terminal_failure_not_success() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            // Model ends with no answer after tools.
            ModelResponse {
                text: "".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let _ = s.run_user_message("read it").await.unwrap();
        assert_eq!(s.status, SessionStatus::Failed);
        assert!(s
            .messages
            .iter()
            .any(|m| m.content.starts_with(TURN_FAILED_MARKER)));
    }

    #[tokio::test]
    async fn resume_restores_conversation_context_and_usage() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                }),
                thinking: Some("inspect".into()),
            },
            ModelResponse {
                text: "read ok".into(),
                tool_calls: vec![],
                usage: Some(Usage {
                    prompt_tokens: 20,
                    completion_tokens: 4,
                }),
                thinking: None,
            },
        ]));
        let cfg = base_cfg(dir.path());
        let mut session = AgentSession::create(cfg.clone(), model, ToolRegistry::new())
            .await
            .unwrap();
        session.run_user_message("read it").await.unwrap();
        let session_id = session.session_id;
        drop(session);

        let resumed = AgentSession::resume(
            cfg,
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            session_id,
        )
        .await
        .unwrap();
        let roles = resumed
            .messages
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![
                MessageRole::System,
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::Assistant,
            ]
        );
        assert_eq!(resumed.messages[2].tool_calls[0].id, "1");
        assert_eq!(resumed.messages[4].content, "read ok");
        assert_eq!(resumed.token_usage.prompt_tokens, 30);
        assert_eq!(resumed.token_usage.completion_tokens, 6);
        assert_eq!(resumed.token_usage.model_steps, 2);
    }

    #[tokio::test]
    async fn hitl_pauses_on_git_push() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("push").await.unwrap();
        assert_eq!(s.status, SessionStatus::AwaitingHitl);
        assert!(s.pending_hitl.is_some());
        assert!(r.text.contains("HITL"));
        s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
        assert_eq!(s.status, SessionStatus::Running);
        assert!(s.pending_hitl.is_none());
    }

    #[tokio::test]
    async fn acl_hides_denied_tools() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let mut acl = AclPolicy::allow_all();
        acl.deny("bash".into());
        s.set_governance(Governance::default().with_acl(acl));
        let names = s.list_tools();
        assert!(!names.iter().any(|n| n == "bash"));
        assert!(names.iter().any(|n| n == "read_file"));
    }

    /// The ACL denial arm in `run_one_tool` is the trailing wildcard, so this pins the
    /// behaviour: a denied tool is refused at execution time, not merely hidden from the
    /// catalogue. `acl_hides_denied_tools` covers listing; this covers execution.
    #[tokio::test]
    async fn acl_denied_tool_call_is_refused_at_execution_time() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "SENTINEL-CONTENT").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "secret.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let mut acl = AclPolicy::allow_all();
        acl.deny("read_file".into());
        s.set_governance(Governance::default().with_acl(acl));
        s.run_user_message("read it").await.unwrap();

        let tool_contents: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            tool_contents.iter().any(|c| c.contains("denied by ACL")),
            "expected an ACL denial result, got {tool_contents:?}"
        );
        assert!(
            !tool_contents.iter().any(|c| c.contains("SENTINEL-CONTENT")),
            "a denied tool must never execute"
        );
    }

    /// `resolve_hitl` derives approval explicitly rather than testing `== Deny`, so this
    /// pins that an approval does **not** take the denial path. Together with
    /// `hitl_pauses_on_git_push` (which covers deny) both branches are now exercised.
    #[tokio::test]
    async fn hitl_approve_does_not_take_the_denial_path() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    // `bash` is in the default `hitl_tools`, and since #26 that requires
                    // approval for *every* command, so a benign one is enough to reach
                    // the gate. No need to shell out to git.
                    arguments: json!({"command": "echo ok"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.status, SessionStatus::AwaitingHitl);

        s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
        assert_eq!(s.status, SessionStatus::Running);
        assert!(s.pending_hitl.is_none());

        let tool_contents: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            !tool_contents.iter().any(|c| c.contains("HITL denied")),
            "approval must not be routed through the denial path, got {tool_contents:?}"
        );
        assert!(
            !tool_contents.is_empty(),
            "approval should reach execution and record a tool result"
        );
    }

    #[tokio::test]
    async fn offload_large_tool_output() {
        let dir = tempdir().unwrap();
        let big = "z".repeat(25_000);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "big.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("read big").await.unwrap();
        let tool_msg = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert!(tool_msg.content.contains("offloaded tool output"));
    }

    #[tokio::test]
    async fn accumulates_api_token_usage_for_cost() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "one".into(),
            tool_calls: vec![],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 3,
            }),
            thinking: Some("hmm".into()),
        }]));
        // Need two responses if we call twice — first call only.
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("hi").await.unwrap();
        assert_eq!(s.token_usage.prompt_tokens, 10);
        assert_eq!(s.token_usage.completion_tokens, 3);
        assert_eq!(s.token_usage.model_steps, 1);
        assert_eq!(s.token_usage.model_calls_with_usage, 1);
        assert!(s.token_usage.thinking_tokens_est >= 1);
        let lines = s.token_usage_lines();
        assert!(lines.iter().any(|l| l.contains("prompt/input")));
        assert!(lines.iter().any(|l| l.contains("completion/output")));
        assert!(lines.iter().any(|l| l.contains("In-context estimate")));
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("$0") || l.contains("USD") || l.contains("price")),
            "should not report dollar cost: {lines:?}"
        );
        let report = s.token_usage_report();
        assert!(report.user_tokens_est >= 1);
        assert!(report.system_tokens_est >= 1);
    }

    #[tokio::test]
    async fn mark_interrupted_if_stale_converts_running() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hi".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        assert_eq!(s.status, SessionStatus::Running);
        s.mark_interrupted_if_stale().await.unwrap();
        assert_eq!(s.status, SessionStatus::Interrupted);
        // Idempotent on terminal interrupted.
        s.mark_interrupted_if_stale().await.unwrap();
        assert_eq!(s.status, SessionStatus::Interrupted);
    }

    #[tokio::test]
    async fn mark_cancelled_persists_terminal_state() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hi".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.mark_cancelled().await.unwrap();
        assert_eq!(s.status, SessionStatus::Cancelled);
        let s2 = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();
        assert_eq!(s2.status, SessionStatus::Cancelled);
    }

    #[tokio::test]
    async fn resume_running_session_becomes_interrupted() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "partial".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        // Fresh session journal is Running with no completion event.
        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();
        assert_eq!(resumed.status, SessionStatus::Interrupted);
    }

    /// A session with no scripted model turns; enough to exercise state helpers
    /// that never reach the provider.
    async fn idle_session(dir: &std::path::Path) -> AgentSession {
        AgentSession::create(
            base_cfg(dir),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
        )
        .await
        .unwrap()
    }

    fn assistant_with_tool_call(name: &str) -> Message {
        let mut m = Message::new(MessageRole::Assistant, "calling");
        m.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: json!({}),
        }];
        m
    }

    #[test]
    fn strip_protocol_markers_removes_confidence_annotations() {
        assert_eq!(strip_protocol_markers("done \\confidence{0.9}"), "done");
        assert_eq!(
            strip_protocol_markers("a \\confidence{0.1}b \\confidence{0.2}c"),
            "a b c"
        );
        assert_eq!(strip_protocol_markers("\\confidence{0.5}only"), "only");
        assert_eq!(strip_protocol_markers("  plain  "), "plain");
    }

    /// Regression: an unterminated marker used to duplicate the text before it,
    /// because `rest` was not rewound before breaking out of the scan.
    #[test]
    fn unterminated_confidence_marker_is_kept_verbatim_once() {
        assert_eq!(
            strip_protocol_markers("keep \\confidence{oops"),
            "keep \\confidence{oops"
        );
        assert_eq!(
            strip_protocol_markers("a \\confidence{0.1}b \\confidence{trunc"),
            "a b \\confidence{trunc"
        );
    }

    #[tokio::test]
    async fn current_turn_has_tool_activity_stops_at_the_user_boundary() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        // No messages at all: nothing to scan.
        s.messages.clear();
        assert!(!s.current_turn_has_tool_activity());

        // Only plain assistant text since the last user message.
        s.messages = vec![
            Message::new(MessageRole::User, "hi"),
            Message::new(MessageRole::Assistant, "hello"),
        ];
        assert!(!s.current_turn_has_tool_activity());

        // An assistant turn carrying tool calls counts as activity.
        s.messages = vec![
            Message::new(MessageRole::User, "hi"),
            assistant_with_tool_call("read_file"),
        ];
        assert!(s.current_turn_has_tool_activity());

        // Tool activity from a *previous* turn must not leak into this one:
        // the scan walks backwards and stops at the newer user message.
        s.messages = vec![
            Message::new(MessageRole::User, "first"),
            assistant_with_tool_call("read_file"),
            Message::new(MessageRole::Tool, "contents"),
            Message::new(MessageRole::User, "second"),
            Message::new(MessageRole::Assistant, "plain reply"),
        ];
        assert!(!s.current_turn_has_tool_activity());
    }

    #[tokio::test]
    async fn finalize_turn_failure_keeps_the_first_summary() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        s.finalize_turn_failure("first failure", "cat_a")
            .await
            .unwrap();
        assert_eq!(s.status, SessionStatus::Failed);
        let after_first = s.messages.len();

        // Idempotent: a second failure must not append another marker message
        // or overwrite the original summary.
        s.finalize_turn_failure("second failure", "cat_b")
            .await
            .unwrap();
        assert_eq!(s.messages.len(), after_first);
        let markers: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(markers.len(), 1);
        assert!(markers[0].contains("first failure"));
        assert!(!markers[0].contains("second failure"));
    }

    #[tokio::test]
    async fn fail_max_turns_records_a_step_limit_failure() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        s.fail_max_turns().await.unwrap();

        assert_eq!(s.status, SessionStatus::Failed);
        let marker = s
            .messages
            .iter()
            .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .expect("a turn_failed marker should be recorded");
        assert!(marker.content.contains("step limit"));
        assert!(s
            .events
            .iter()
            .any(|e| e.kind == "turn_failed" && e.detail.starts_with("max_turns:")));
    }

    #[tokio::test]
    async fn prepare_model_step_resets_context_when_over_threshold() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        // Shrink the window so the messages below cross the reset ratio.
        s.context.config.capacity_tokens = 32;
        s.messages = vec![
            Message::new(MessageRole::User, "x".repeat(400)),
            Message::new(MessageRole::Assistant, "y".repeat(400)),
        ];
        assert!(s.context.should_reset(&s.messages));

        let request = s.prepare_model_step(3).await.unwrap();

        assert!(s
            .events
            .iter()
            .any(|e| e.kind == "context_reset" && e.detail == "threshold"));
        // The handoff replaces the whole conversation with a two-message
        // restart: a system prompt carrying the progress document, then a
        // user turn telling the model to continue.
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, MessageRole::System);
        assert!(s.messages[0].content.contains("# Context Handoff"));
        assert_eq!(s.messages[1].role, MessageRole::User);
        assert!(s.messages[1].content.starts_with("Continue the task."));
        // The padded turns are gone as standalone messages.
        assert!(!s
            .messages
            .iter()
            .any(|m| m.content == "x".repeat(400) || m.content == "y".repeat(400)));
        assert!(!request.messages.is_empty());
    }

    #[tokio::test]
    async fn prepare_model_step_leaves_a_small_context_alone() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.messages = vec![Message::new(MessageRole::User, "short")];

        s.prepare_model_step(1).await.unwrap();

        assert!(!s.events.iter().any(|e| e.kind == "context_reset"));
        assert_eq!(s.messages.len(), 1);
    }

    #[tokio::test]
    async fn context_reset_ratio_and_journal_cursor_are_exposed() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        s.context.config.reset_usage_ratio = 0.75;
        assert!((s.context_reset_ratio() - 0.75).abs() < f64::EPSILON);

        let before = s.journal_cursor().await.unwrap();
        s.append_user_message("advance the journal").await.unwrap();
        let after = s.journal_cursor().await.unwrap();
        assert!(
            after > before,
            "cursor should advance after a journalled append ({before} -> {after})"
        );
    }

    #[tokio::test]
    async fn token_usage_report_buckets_tool_messages_separately() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        let mut thinking_turn = Message::new(MessageRole::Assistant, "answer");
        thinking_turn.thinking = Some("pondering at some length".into());

        s.messages = vec![
            Message::new(MessageRole::System, "system preamble"),
            Message::new(MessageRole::User, "a question"),
            thinking_turn,
            Message::new(MessageRole::Tool, "tool output one"),
            Message::new(MessageRole::Tool, "tool output two"),
        ];

        let report = s.token_usage_report();

        assert_eq!(report.tool_message_count, 2);
        assert!(report.tool_tokens_est > 0);
        assert!(report.system_tokens_est > 0);
        assert!(report.user_tokens_est > 0);
        assert!(report.assistant_tokens_est > 0);
        assert!(
            report.thinking_in_context_est > 0,
            "assistant thinking should be counted in the context estimate"
        );
    }

    #[tokio::test]
    async fn exec_only_tool_failure_is_journalled_without_a_tool_message() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        let before = s.messages.len();
        let mut budget = ValidationBudget::default();

        let call = ToolCall {
            id: "c1".into(),
            name: "no_such_tool".into(),
            arguments: json!({}),
        };
        // A failing call must not surface as tool output in the conversation,
        // but it still has to be journalled for replay.
        s.run_one_tool_exec_only(&call, &mut budget).await.unwrap();

        assert_eq!(s.messages.len(), before);
        assert!(s.journal_cursor().await.unwrap() > 0);
    }
}
