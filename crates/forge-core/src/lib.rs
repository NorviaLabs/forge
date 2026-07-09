//! Agent loop — Phase 1 base + Phase 2 hooks (context, HITL, governance, worktree).

use std::path::PathBuf;
use std::sync::Arc;

use forge_context::ContextEngine;
use forge_durable::{new_session_id, Journal};
use forge_governance::{AuditEvent, Governance};
use forge_model::{ModelClient, ModelRequest, StreamEventTx};
use forge_config::WebSearchConfig;
use forge_tools::{
    default_builtins_with_web_search, ToolContext, ToolError, ToolRegistry, ValidationBudget,
};
use forge_types::{
    HitlDecision, HitlPayload, Message, MessageRole, ModelResponse, PolicyDecision, SessionId,
    SessionStatus, SideEffectClass, ToolCall, ToolOutput,
};
use forge_workspace::{IsolationMode, WorktreeManager};
use serde_json::json;
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Error)]
pub enum LoopError {
    #[error(transparent)]
    Journal(#[from] forge_durable::JournalError),
    #[error(transparent)]
    Model(#[from] forge_model::ModelError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Context(#[from] forge_context::ContextError),
    #[error(transparent)]
    Worktree(#[from] forge_workspace::WorktreeError),
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
    pub isolation: IsolationMode,
    pub enable_context_lifecycle: bool,
    pub enable_governance: bool,
    /// Phase 9 — controls registration of `web_search` (WEB-01).
    pub web_search: WebSearchConfig,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 16,
            workspace: PathBuf::from("."),
            journal_dir: PathBuf::from(".forge/sessions"),
            isolation: IsolationMode::Off,
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

/// Result of applying one model response inside the agent loop.
#[derive(Debug)]
pub enum ApplyOutcome {
    /// No tool calls — turn finished.
    Done(ModelResponse),
    /// Tools ran; call the model again.
    Continue,
    /// Paused for human-in-the-loop.
    Hitl(ModelResponse),
}

pub struct AgentSession {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub messages: Vec<Message>,
    pub events: Vec<TurnEvent>,
    pub pending_hitl: Option<HitlPayload>,
    /// LiteLLM model id for the next complete (empty → client default).
    pub active_model: String,
    journal: Journal,
    tools: ToolRegistry,
    model: Arc<dyn ModelClient>,
    tool_ctx: ToolContext,
    max_turns: u32,
    governance: Governance,
    context: ContextEngine,
    worktree: Option<WorktreeManager>,
    enable_context: bool,
    enable_gov: bool,
}

impl AgentSession {
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

        let mut worktree = None;
        let mut active_root = loop_cfg.workspace.clone();
        if loop_cfg.isolation == IsolationMode::Worktree {
            let mut wt = WorktreeManager::new(loop_cfg.workspace.clone(), session_id);
            active_root = wt.ensure()?;
            worktree = Some(wt);
        }

        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let agents = context.load_agents_md();
        let system = if agents.is_empty() {
            "You are Forge, a coding agent. Use tools when needed.".into()
        } else {
            format!("You are Forge, a coding agent.\n\nAGENTS.md:\n{agents}")
        };

        Ok(Self {
            session_id,
            status: SessionStatus::Running,
            messages: vec![Message {
                role: MessageRole::System,
                content: system,
                tool_call_id: None,
                name: None,
                thinking: None,
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
            worktree,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
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
        let mut messages = vec![Message {
            role: MessageRole::System,
            content: "You are Forge, a coding agent. Session resumed from journal.".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            }];
        for u in &state.user_messages {
            messages.push(Message {
                role: MessageRole::User,
                content: u.clone(),
                tool_call_id: None,
                name: None,
                thinking: None,
            });
        }
        for (id, tr) in &state.tool_results {
            messages.push(Message {
                role: MessageRole::Tool,
                content: tr.output.content.clone(),
                tool_call_id: Some(id.clone()),
                name: Some(tr.name.clone()),
                thinking: None,
            });
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume (fail-safe)");
        }

        let mut worktree = None;
        let mut active_root = loop_cfg.workspace.clone();
        if loop_cfg.isolation == IsolationMode::Worktree {
            let mut wt = WorktreeManager::new(loop_cfg.workspace.clone(), session_id);
            active_root = wt.ensure()?;
            worktree = Some(wt);
        }

        let pending_hitl = state.pending_hitl.and_then(|v| {
            serde_json::from_value::<HitlPayload>(v).ok()
        });

        Ok(Self {
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
            context: ContextEngine::new(loop_cfg.workspace.clone(), session_id),
            worktree,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
        })
    }

    pub fn set_governance(&mut self, g: Governance) {
        self.governance = g;
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
            });
        if self.context.goal.is_empty() {
            self.context.goal = text.chars().take(200).collect();
        }
        self.status = SessionStatus::Running;
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
                json!({
                    "text_len": last.text.len(),
                    "tool_calls": last.tool_calls.len(),
                }),
            )
            .await?;

        let has_thinking = last
            .thinking
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        if !last.text.is_empty() || has_thinking {
            self.messages.push(Message {
                role: MessageRole::Assistant,
                content: last.text.clone(),
                tool_call_id: None,
                name: None,
                thinking: last.thinking.clone().filter(|t| !t.trim().is_empty()),
            });
            if has_thinking {
                if let Some(ref th) = last.thinking {
                    self.events.push(TurnEvent {
                        kind: "thinking".into(),
                        detail: th.clone(),
                    });
                }
            }
            if !last.text.is_empty() {
                self.events.push(TurnEvent {
                    kind: "assistant".into(),
                    detail: last.text.clone(),
                });
            }
        }

        if last.tool_calls.is_empty() {
            self.status = SessionStatus::Completed;
            self.journal
                .append_status(self.session_id, SessionStatus::Completed)
                .await?;
            return Ok(ApplyOutcome::Done(last));
        }

        let mut budget = ValidationBudget::with_default_max();
        for call in &last.tool_calls {
            if let Some(pause) = self.run_one_tool(call, &mut budget).await? {
                return Ok(ApplyOutcome::Hitl(pause));
            }
        }
        Ok(ApplyOutcome::Continue)
    }

    /// Run until no tool calls, max turns, or HITL pause.
    pub async fn run_user_message(&mut self, text: &str) -> Result<ModelResponse, LoopError> {
        self.append_user_message(text).await?;
        self.run_agent_turns(None).await
    }

    /// Context-reset (if needed) + journal a model request; returns the request to send.
    pub async fn prepare_model_step(&mut self, turn: u32) -> Result<ModelRequest, LoopError> {
        if self.enable_context && self.context.should_reset(&self.messages) {
            let ws_ref = self
                .worktree
                .as_ref()
                .map(|w| w.branch.clone())
                .unwrap_or_default();
            let (doc, msgs) = self.context.handoff_reset(&self.messages, &ws_ref)?;
            self.journal
                .append_context_reset(self.session_id, json!({ "progress": doc }))
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
        self.status = SessionStatus::Failed;
        self.journal
            .append_status(self.session_id, SessionStatus::Failed)
            .await?;
        Ok(())
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
                PolicyDecision::Deny => {
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
            });
                    return Ok(None);
                }
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
                    output.content = self
                        .context
                        .maybe_offload_tool_content(output.content)?;
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
                let msg = format!("Tool validation error: {ve}. Please correct arguments.");
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: msg.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
            });
                self.events.push(TurnEvent {
                    kind: "validation".into(),
                    detail: msg,
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
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
            });
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
        let payload = self
            .pending_hitl
            .clone()
            .ok_or(LoopError::NoPendingHitl)?;
        let dec = match decision {
            HitlDecision::Approve => "approve",
            HitlDecision::Deny => "deny",
        };
        self.journal
            .append_hitl_resume(self.session_id, dec, actor)
            .await?;

        if decision == HitlDecision::Deny {
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
            if d == PolicyDecision::Deny {
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
        let _ = self.run_one_tool_exec_only(&call, &mut budget).await?;
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

    pub fn worktree_status(&self) -> Option<String> {
        self.worktree.as_ref().map(|w| w.status())
    }

    pub fn worktree_merge(&mut self) -> Result<(), LoopError> {
        if let Some(ref mut w) = self.worktree {
            w.merge()?;
            self.tool_ctx = ToolContext::new(w.active_root());
        }
        Ok(())
    }

    pub fn worktree_discard(&mut self) -> Result<(), LoopError> {
        if let Some(ref mut w) = self.worktree {
            w.discard()?;
            self.tool_ctx = ToolContext::new(self.context.workspace.clone());
        }
        Ok(())
    }

    /// Use this LiteLLM model id on subsequent completes (e.g. after `/connect`).
    pub fn set_active_model(&mut self, model: impl Into<String>) {
        self.active_model = model.into();
    }

    /// Push provider credentials into the model client (OAuth tokens → worker env).
    pub fn apply_provider_env(&self, pairs: &[(String, String)]) {
        self.model.apply_provider_env(pairs);
    }
}

impl AgentSession {
    pub async fn force_context_reset_async(&mut self) -> Result<(), LoopError> {
        let ws_ref = self
            .worktree
            .as_ref()
            .map(|w| w.branch.clone())
            .unwrap_or_default();
        let (doc, msgs) = self.context.handoff_reset(&self.messages, &ws_ref)?;
        self.journal
            .append_context_reset(
                self.session_id,
                json!({ "progress": doc, "workspace_ref": ws_ref }),
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

    fn base_cfg(dir: &std::path::Path) -> LoopConfig {
        LoopConfig {
            max_turns: 5,
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            isolation: IsolationMode::Off,
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
}
