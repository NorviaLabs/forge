//! Building an `AgentSession`: fresh, resumed, and its governance setup.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

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
        let system_message = Message {
            outcome: Default::default(),
            role: MessageRole::System,
            content: assemble_system_prompt(
                &context.load_agents_md(),
                context.load_skills().as_slice(),
            ),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
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
            warn!(call_id = %incomplete, "incomplete tool intent on resume");
        }

        let incomplete = state.incomplete_intents.clone();
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
        self.active_task = ActiveTaskState::from_restored(session_id, state.status, wait_reason);
        self.tasks = TaskRuntime::with_queue(queue);
        self.messages = messages;
        self.events = vec![TurnEvent {
            kind: "resume".into(),
            detail: format!("seq={}", state.last_seq),
        }];
        self.journal = SessionPersistence::new(journal);
        self.tool_ctx = ToolContext::new(active_root);
        self.context = context;
        self.token_usage = token_usage;
        self.journaled_tool_results = journaled_tool_results;
        self.reconcile_incomplete_intents(&incomplete).await?;
        // Stale Working without a live executor is Interrupted, not eternal Working.
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
        let system = assemble_system_prompt(&agents, skills.as_slice());

        Ok(Self {
            session_id,
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
            }],
            events: vec![],
            active_task: ActiveTaskState::new(session_id),
            tasks: TaskRuntime::new(),
            active_model: String::new(),
            active_route_id: String::new(),
            reasoning_effort: None,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            token_usage: SessionTokenUsage::default(),
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: HashMap::new(),
            ctx_tokens_cache: Mutex::new(None),
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
        let system =
            assemble_system_prompt(&context.load_agents_md(), context.load_skills().as_slice());
        let system_message = Message {
            outcome: Default::default(),
            role: MessageRole::System,
            content: system,
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
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

        let mut session = Self {
            session_id,
            messages,
            events: vec![TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            active_task: ActiveTaskState::from_restored(session_id, state.status, wait_reason),
            tasks: TaskRuntime::with_queue(queue),
            active_model: String::new(),
            active_route_id: String::new(),
            reasoning_effort: None,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            token_usage,
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: state.tool_results.clone(),
            ctx_tokens_cache: Mutex::new(None),
        };
        session.reconcile_incomplete_intents(&incomplete).await?;
        session
            .reconcile_orphaned_background_tasks(
                &state.background_tasks,
                &state.subagent_workspaces,
            )
            .await?;
        // Legacy fallback: Running with no runtime becomes Interrupted (not guessed from text).
        session.mark_interrupted_if_stale().await?;
        Ok(session)
    }

    pub fn set_governance(&mut self, g: Governance) {
        self.governance = g;
    }

    /// Apply a named permission mode in place — preserves the ACL and any
    /// loaded pattern rules, only pre-seeding `hitl_classes` (see
    /// `Governance::apply_mode`). Unlike `set_governance`, this can't
    /// accidentally drop rules a `permissions.toml` load already put in
    /// place.
    pub fn apply_permission_mode(&mut self, mode: forge_governance::PermissionMode) {
        self.governance.apply_mode(mode);
    }
}
