//! Building an `AgentSession`: fresh, resumed, and its governance setup.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use std::collections::HashSet;

use crate::*;

impl AgentSession {
    /// Replace the active conversation by replaying another session journal.
    pub async fn resume_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<ResumeReport, LoopError> {
        if session_id == self.session_id {
            // No in-memory record of local-only composer lines exists (only
            // the journal has it) — re-open and replay this session's own
            // journal rather than track a parallel in-memory copy. This is
            // the trivial "resume into the session you're already in"
            // branch, not the common path, so the extra read is cheap and
            // acceptable. Best-effort: a read glitch here degrades to no
            // recalled history rather than failing the whole no-op resume.
            let composer_lines = match Journal::open(self.journal.directory(), session_id).await {
                Ok(journal) => journal
                    .replay(session_id)
                    .await
                    .map(|state| state.composer_lines)
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            return Ok(ResumeReport {
                last_seq: self.events.len() as u64,
                model_steps: self.token_usage.model_steps as usize,
                tool_results: self
                    .messages
                    .iter()
                    .filter(|message| message.role == MessageRole::Tool)
                    .count(),
                incomplete_intents: 0,
                composer_lines,
            });
        }

        let journal = Journal::open(self.journal.directory(), session_id).await?;
        let state = journal.replay(session_id).await?;
        let mut context = ContextEngine::new(self.context.workspace.clone(), session_id);
        context.config = self.context.config.clone();
        let session_tmp = forge_tools::SessionTempDir::create(session_id)?;
        let mut messages = state.messages;
        restore_system_message(
            &mut messages,
            assemble_system_prompt(
                &context.load_agents_md(),
                context.load_skills().as_slice(),
                session_tmp.path(),
            ),
        );
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume");
        }

        let incomplete = state.incomplete_intents.clone();
        let user_messages = state.user_messages.clone();
        let context_state = state.context_state.clone();
        let journaled_tool_results = state.tool_results.clone();
        let active_root = context.workspace.clone();
        let wait_reason = restored_wait_reason(&state.pending_hitl);
        let queue = TaskQueue::from_restored(restored_queue_items(session_id, state.queue_items));
        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let report = ResumeReport {
            last_seq: state.last_seq,
            model_steps: state.model_responses.len(),
            tool_results: state.tool_results.len(),
            incomplete_intents: state.incomplete_intents.len(),
            composer_lines: state.composer_lines,
        };
        self.session_id = session_id;
        self.coordinator = AgentCoordinator::with_config(session_id, self.coordinator.config());
        self.active_task = ActiveTaskState::from_restored(session_id, state.status, wait_reason);
        self.tasks = TaskRuntime::with_queue(queue);
        self.messages = messages.into();
        self.events = vec![TurnEvent {
            kind: "resume".into(),
            detail: format!("seq={}", state.last_seq),
        }];
        self.journal = SessionPersistence::new(journal);
        self.tool_ctx = ToolContext::new(active_root).with_session_tmp(session_tmp);
        self.tool_ctx.egress = self.egress.as_ref().map(|runtime| runtime.grant());
        self.context = context;
        self.token_usage = token_usage;
        self.journaled_tool_results = journaled_tool_results;
        self.last_prompt_wire = None;
        self.last_prompt_hash = state.last_prompt_hash.clone();
        self.cache_epoch = state.cache_epoch;
        self.last_cache_transport = state.last_cache_transport.clone();
        self.context_state = SessionContextState::default();
        self.compaction = CompactionTelemetry::default();
        self.restore_protected_facts(&user_messages);
        self.canonical_user_messages = user_messages;
        self.restore_context_state(context_state.as_ref());
        self.reconcile_incomplete_intents(&incomplete).await?;
        let mut restored_children = HashSet::new();
        for task in state.background_tasks.iter().rev() {
            let Some(child_session_id) = task.child_session_id else {
                continue;
            };
            if !restored_children.insert(child_session_id) {
                continue;
            }
            if !task.finished {
                continue;
            }
            let status = match task.status.as_str() {
                "succeeded" => AgentStatus::Completed,
                "failed" => AgentStatus::Failed,
                "cancelled" => AgentStatus::Cancelled,
                _ => continue,
            };
            let _ = self.coordinator.restore_child(
                task.parent_session_id.unwrap_or(session_id),
                child_session_id,
                task.label.clone(),
                status,
                task.summary.clone(),
            );
        }
        restored_children.clear();
        for task in state
            .background_tasks
            .iter()
            .rev()
            .filter(|task| task.finished && task.kind == "subagent")
        {
            let Some(child_session_id) = task.child_session_id else {
                continue;
            };
            if !restored_children.insert(child_session_id) {
                continue;
            }
            if let Some(workspace) = state.subagent_workspaces.get(&task.id.0) {
                self.restore_finished_subagent_task(task, workspace.clone())
                    .await?;
            }
        }
        // Stale Working without a live executor is Interrupted, not eternal Working.
        self.mark_interrupted_if_stale().await?;
        Ok(report)
    }

    pub async fn create(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
    ) -> Result<Self, LoopError> {
        tools.install_default_builtins(&loop_cfg.web_search, &loop_cfg.workspace);
        let session_id = new_session_id();
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        journal.append_session_created(session_id).await?;

        let active_root = loop_cfg.workspace.clone();
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let agents = context.load_agents_md();
        let skills = context.load_skills();
        let session_tmp = forge_tools::SessionTempDir::create(session_id)?;
        let system = assemble_system_prompt(&agents, skills.as_slice(), session_tmp.path());

        // Start the session's egress proxy. `None` leaves the network off,
        // which is the safe direction: a command that needs it then fails with
        // the sandbox's own explanation rather than reaching the network.
        // Hosts start denied; `open_session` overlays personal `host(...)`
        // allows after the permission files are loaded.
        let egress =
            crate::permission::start_egress(session_id, forge_tools::egress::EgressPolicy::new())
                .await;
        let mut tool_ctx = ToolContext::new(active_root).with_session_tmp(session_tmp);
        tool_ctx.egress = egress.as_ref().map(|runtime| runtime.grant());

        Ok(Self {
            session_id,
            coordinator: AgentCoordinator::with_config(
                session_id,
                AgentCoordinatorConfig::from(&loop_cfg.agents),
            ),
            messages: vec![Message {
                outcome: Default::default(),
                role: MessageRole::System,
                content: system,
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            }]
            .into(),
            events: vec![],
            active_task: ActiveTaskState::new(session_id),
            tasks: TaskRuntime::new(),
            active_model: String::new(),
            active_route_id: String::new(),
            reasoning_effort: None,
            thinking_enabled: true,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx,
            egress,
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            turn_cancel_token: CancellationToken::new(),
            token_usage: SessionTokenUsage::default(),
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: HashMap::new(),
            ctx_tokens_cache: Mutex::new(None),
            last_prompt_wire: None,
            last_prompt_hash: None,
            cache_epoch: 0,
            last_cache_transport: None,
            compaction_policy: CompactionPolicy::default(),
            context_state: SessionContextState::default(),
            compaction: CompactionTelemetry::default(),
            canonical_user_turns: 0,
            canonical_user_messages: Vec::new(),
        })
    }

    pub async fn resume(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
        session_id: SessionId,
    ) -> Result<Self, LoopError> {
        tools.install_default_builtins(&loop_cfg.web_search, &loop_cfg.workspace);
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        let state = journal.replay(session_id).await?;
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let session_tmp = forge_tools::SessionTempDir::create(session_id)?;
        let mut messages = state.messages.clone();
        restore_system_message(
            &mut messages,
            assemble_system_prompt(
                &context.load_agents_md(),
                context.load_skills().as_slice(),
                session_tmp.path(),
            ),
        );
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume");
        }

        let incomplete = state.incomplete_intents.clone();
        let active_root = loop_cfg.workspace.clone();

        let wait_reason = restored_wait_reason(&state.pending_hitl);
        let queue = TaskQueue::from_restored(restored_queue_items(session_id, state.queue_items));

        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let egress =
            crate::permission::start_egress(session_id, forge_tools::egress::EgressPolicy::new())
                .await;
        let mut tool_ctx = ToolContext::new(active_root).with_session_tmp(session_tmp);
        tool_ctx.egress = egress.as_ref().map(|runtime| runtime.grant());

        let mut session = Self {
            session_id,
            coordinator: AgentCoordinator::with_config(
                session_id,
                AgentCoordinatorConfig::from(&loop_cfg.agents),
            ),
            messages: messages.into(),
            events: vec![TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            active_task: ActiveTaskState::from_restored(session_id, state.status, wait_reason),
            tasks: TaskRuntime::with_queue(queue),
            active_model: String::new(),
            active_route_id: String::new(),
            reasoning_effort: None,
            thinking_enabled: true,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx,
            egress,
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            turn_cancel_token: CancellationToken::new(),
            token_usage,
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: state.tool_results.clone(),
            ctx_tokens_cache: Mutex::new(None),
            last_prompt_wire: None,
            last_prompt_hash: state.last_prompt_hash.clone(),
            cache_epoch: state.cache_epoch,
            last_cache_transport: state.last_cache_transport.clone(),
            compaction_policy: CompactionPolicy::default(),
            context_state: SessionContextState::default(),
            compaction: CompactionTelemetry::default(),
            canonical_user_turns: 0,
            canonical_user_messages: state.user_messages.clone(),
        };
        session.restore_protected_facts(&state.user_messages);
        session.restore_context_state(state.context_state.as_ref());
        session.reconcile_incomplete_intents(&incomplete).await?;
        session
            .reconcile_orphaned_background_tasks(
                &state.background_tasks,
                &state.subagent_workspaces,
            )
            .await?;
        let mut restored_children = HashSet::new();
        for task in state.background_tasks.iter().rev() {
            let Some(child_session_id) = task.child_session_id else {
                continue;
            };
            if !restored_children.insert(child_session_id) {
                continue;
            }
            if !task.finished {
                continue;
            }
            let status = match task.status.as_str() {
                "succeeded" => AgentStatus::Completed,
                "failed" => AgentStatus::Failed,
                "cancelled" => AgentStatus::Cancelled,
                _ => continue,
            };
            let _ = session.coordinator.restore_child(
                task.parent_session_id.unwrap_or(session_id),
                child_session_id,
                task.label.clone(),
                status,
                task.summary.clone(),
            );
        }
        restored_children.clear();
        for task in state
            .background_tasks
            .iter()
            .rev()
            .filter(|task| task.finished && task.kind == "subagent")
        {
            let Some(child_session_id) = task.child_session_id else {
                continue;
            };
            if !restored_children.insert(child_session_id) {
                continue;
            }
            if let Some(workspace) = state.subagent_workspaces.get(&task.id.0) {
                session
                    .restore_finished_subagent_task(task, workspace.clone())
                    .await?;
            }
        }
        // Legacy fallback: Running with no runtime becomes Interrupted (not guessed from text).
        session.mark_interrupted_if_stale().await?;
        Ok(session)
    }

    /// Create a new session seeded with this session's current model-visible
    /// context. The source journal is untouched; the fork is ready for a new
    /// task with a fresh lifecycle and runtime scratch space.
    pub async fn fork(&self) -> Result<Self, LoopError> {
        let loop_cfg = LoopConfig {
            max_turns: self.max_turns,
            workspace: self.workspace_root().to_path_buf(),
            journal_dir: self.journal.directory().to_path_buf(),
            enable_context_lifecycle: self.enable_context,
            enable_governance: self.enable_gov,
            web_search: WebSearchConfig::default(),
            agents: self.coordinator.config().into(),
        };
        let mut fork = Self::create(loop_cfg, self.model.clone(), (*self.tools).clone()).await?;
        fork.active_model = self.active_model.clone();
        fork.active_route_id = self.active_route_id.clone();
        fork.reasoning_effort = self.reasoning_effort.clone();
        fork.thinking_enabled = self.thinking_enabled;
        fork.governance = self.governance.clone();
        fork.context.config = self.context.config.clone();
        fork.context.goal = self.context.goal.clone();
        fork.compaction_policy = self.compaction_policy;
        fork.tool_ctx.image_input = self.tool_ctx.image_input;
        fork.tool_ctx.active_model = self.tool_ctx.active_model.clone();

        let mut messages: Vec<Message> = self.messages.iter().cloned().collect();
        let session_tmp = fork
            .tool_ctx
            .session_tmp
            .as_ref()
            .map(|tmp| tmp.path().to_path_buf());
        let Some(session_tmp) = session_tmp else {
            return Err(LoopError::Other(
                "fork session scratch space is missing".into(),
            ));
        };
        let system = assemble_system_prompt(
            &fork.context.load_agents_md(),
            fork.context.load_skills().as_slice(),
            &session_tmp,
        );
        restore_system_message(&mut messages, system.clone());
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            first.content = system;
        }
        let context_state = serde_json::to_value(&self.context_state).map_err(|error| {
            LoopError::Other(format!("could not serialize fork context: {error}"))
        })?;
        let tool_results = serde_json::to_value(&self.journaled_tool_results).map_err(|error| {
            LoopError::Other(format!("could not serialize fork tool results: {error}"))
        })?;
        fork.journal
            .append_context_reset(
                fork.session_id,
                json!({
                    "messages": messages,
                    "user_messages": self.canonical_user_messages.clone(),
                    "context_state": context_state,
                    "tool_results": tool_results,
                }),
            )
            .await?;
        fork.journal
            .append_status(fork.session_id, TaskLifecycle::Ready)
            .await?;
        fork.messages = messages.into();
        fork.journaled_tool_results = self.journaled_tool_results.clone();
        fork.canonical_user_messages = self.canonical_user_messages.clone();
        let fork_user_messages = fork.canonical_user_messages.clone();
        fork.restore_protected_facts(&fork_user_messages);
        fork.context_state = self.context_state.clone();
        fork.apply_egress_policy(crate::egress_policy_for_workspace(fork.workspace_root()))
            .await;
        fork.last_prompt_wire = None;
        fork.last_prompt_hash = None;
        fork.cache_epoch = 0;
        fork.last_cache_transport = None;
        Ok(fork)
    }

    /// Whether this session can reach the network at all.
    ///
    /// False when no egress proxy could be started, in which case confined
    /// commands have no route out. Worth surfacing: a failed `cargo build`
    /// looks like a broken build rather than a missing proxy, and this is how
    /// a caller can tell the difference.
    pub fn has_network_egress(&self) -> bool {
        self.egress.is_some()
    }

    /// The hosts this session may reach, for display.
    pub fn egress_grant(&self) -> Option<std::sync::Arc<forge_tools::sandbox::EgressGrant>> {
        self.egress.as_ref().map(|runtime| runtime.grant())
    }

    /// Replace the policy (ACL and pattern-rule files) while keeping the
    /// grants the operator made at a prompt this session.
    ///
    /// Those grants live in the governance object but did not come from a
    /// file, so a reload that simply overwrote it would silently revoke every
    /// "allow for this session" the operator had given — and the next matching
    /// call would prompt again with no indication why.
    /// `retain_session_patterns_from` existed for this and was never wired to
    /// anything; this is the call site its doc comment describes.
    pub fn set_governance(&mut self, mut g: Governance) {
        g.retain_session_patterns_from(&self.governance);
        self.governance = g;
    }

    /// Replace the session proxy with one that enforces `policy`.
    ///
    /// The previous runtime is dropped first so the session-id socket is
    /// free before the new listener binds. Called from session open after
    /// personal `host(...)` rules are loaded.
    pub async fn apply_egress_policy(&mut self, policy: forge_tools::egress::EgressPolicy) {
        self.egress = None;
        self.tool_ctx.egress = None;
        let runtime = crate::permission::start_egress(self.session_id, policy).await;
        self.tool_ctx.egress = runtime.as_ref().map(|r| r.grant());
        self.egress = runtime;
    }

    /// Permit `pattern` on the live proxy without replacing the socket.
    pub fn grant_egress_host(&self, pattern: &str) {
        if let Some(runtime) = &self.egress {
            runtime.grant_host(pattern);
        }
    }

    /// Remember the suggested pattern for `call` for the rest of this session.
    /// `None` when the call has no pattern that could ever match it again, in
    /// which case nothing was remembered.
    pub fn allow_suggested_pattern_for_session(
        &mut self,
        call: &forge_types::ToolCall,
    ) -> Option<String> {
        self.governance.allow_suggested_pattern_for_session(call)
    }

    pub fn clear_session_pattern_allows(&mut self) {
        self.governance.clear_session_pattern_allows();
    }

    pub fn session_pattern_allow_count(&self) -> usize {
        self.governance.session_pattern_allow_count()
    }

    pub fn session_pattern_allows(&self, call: &forge_types::ToolCall) -> bool {
        self.governance.session_pattern_allows(call)
    }

    /// Whether the operator has already consented to this call's shape and
    /// has not carved it back out with a `deny` rule. This is the question
    /// every gate outside `authorize` should ask;
    /// [`Self::session_pattern_allows`] sees only this session's grants and
    /// will miss an "always allow" rule.
    pub fn grant_covers(&self, call: &forge_types::ToolCall) -> bool {
        self.governance.grant_covers(call)
    }
}
