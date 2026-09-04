//! Subagents — independent nested agent loops that share the parent's
//! model client and tool registry but run in their own git worktree, with
//! their own `SessionId`/`Journal`/`ContextEngine`/message history.
//!
//! The spawn/poll/cancel machinery is entirely `background.rs`'s — this
//! module is only about constructing the child `AgentSession` and driving
//! its (unmodified) `run_user_message` loop. Reusing `AgentSession` here,
//! rather than inventing a second agent loop, is the entire point: a
//! subagent runs exactly the same turn/tool-call machinery the top-level
//! session already runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use forge_context::compaction::{CompactionPolicy, CompactionTelemetry, SessionContextState};
use forge_durable::Journal;
use forge_governance::AclPolicy;
use forge_storage::RuntimeStorage;
use forge_tools::ToolContext;
use forge_types::{BackgroundTaskId, HitlDecision, Message, MessageRole, SessionId, TaskLifecycle};
use tokio_util::sync::CancellationToken;

use crate::agent_coordinator::AgentCommand;
use crate::background::{BackgroundTaskKind, BackgroundTaskOutcome, BackgroundTaskStatus};
use crate::persistence::SessionPersistence;
use crate::turn_state::TurnState;
use crate::{
    assemble_system_prompt, ActiveTaskState, AgentSession, LoopError, SessionTokenUsage, TaskQueue,
};
use crate::{AgentCoordinator, AgentStatus};

/// What to hand a new subagent.
#[derive(Debug, Clone)]
pub struct SubagentSpec {
    /// Free-text role label (e.g. "test-fixer", "docs-updater", "reviewer")
    /// — shown in the Tasks tab and used to derive the worktree/branch name.
    pub role: String,
    /// The instruction handed to the subagent as its first user message.
    pub prompt: String,
    /// `None` inherits the parent's full tool registry and governance ACL.
    /// `Some(names)` replaces the child's ACL with an allow-list of exactly
    /// those tool names, narrowing (not widening) what it can call.
    pub tool_allowlist: Option<Vec<String>>,
    /// `None` inherits the parent's `max_turns`.
    pub max_turns: Option<u32>,
}

/// What a finished (or cancelled/failed) subagent reports back to its
/// parent. Delivered via `BackgroundTaskOutcome::Subagent` through the same
/// poll pattern `background.rs` uses for shell jobs.
#[derive(Debug, Clone)]
pub struct SubagentOutcome {
    pub child_session_id: SessionId,
    pub status: BackgroundTaskStatus,
    pub summary: String,
    pub token_usage: SessionTokenUsage,
}

impl AgentSession {
    /// Build a child session: fresh `SessionId`/`Journal` (same journal
    /// directory as the parent — journals are per-session `.db` files, so
    /// there's no collision), fresh `ContextEngine` rooted at `workspace`,
    /// but the parent's `Arc<ToolRegistry>` and `Arc<dyn ModelClient>`
    /// shared by cheap clone. `cancel_token` is threaded in (not generated
    /// here) so the caller's `BackgroundTaskHandle` and the child's own
    /// cancellation check share the exact same token.
    pub(crate) async fn create_child(
        &self,
        session_id: SessionId,
        workspace: PathBuf,
        cancel_token: CancellationToken,
        spec: &SubagentSpec,
    ) -> Result<AgentSession, LoopError> {
        let journal_dir = self.journal.directory().to_path_buf();
        let journal = Journal::open(&journal_dir, session_id).await?;
        journal.append_session_created(session_id).await?;

        let context = forge_context::ContextEngine::new(workspace.clone(), session_id);
        let agents = context.load_agents_md();
        let skills = context.load_skills();
        let session_tmp = forge_tools::SessionTempDir::create(session_id)?;
        let system = assemble_system_prompt(&agents, skills.as_slice(), session_tmp.path());

        // Same HITL/governance policy as the parent by default (per product
        // decision: a subagent can legitimately pause on an approval prompt,
        // same as the main session) — narrowed only if `tool_allowlist` asks
        // for it.
        let mut governance = self.governance.clone();
        if let Some(allowlist) = &spec.tool_allowlist {
            let mut acl = AclPolicy::new();
            for name in allowlist {
                acl.allow(name.clone());
            }
            governance.acl = acl;
        }

        Ok(AgentSession {
            session_id,
            coordinator: self.coordinator.clone(),
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
            tasks: crate::task_runtime::TaskRuntime::new(),
            active_model: self.active_model.clone(),
            active_route_id: self.active_route_id.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            thinking_enabled: self.thinking_enabled,
            journal: SessionPersistence::new(journal),
            tools: self.tools.clone(),
            model: self.model.clone(),
            // Shares the parent's egress rather than starting a second proxy:
            // one session, one allow-list, one place to revoke it. The parent
            // owns the proxy, so `egress` here is None — the child holds a
            // grant, not the listener.
            tool_ctx: {
                let mut ctx = ToolContext::new(workspace).with_session_tmp(session_tmp);
                ctx.egress = self.tool_ctx.egress.clone();
                ctx
            },
            egress: None,
            max_turns: spec.max_turns.unwrap_or(self.max_turns),
            governance,
            context,
            enable_context: self.enable_context,
            enable_gov: self.enable_gov,
            cancel_token: Some(cancel_token),
            turn_cancel_token: CancellationToken::new(),
            token_usage: SessionTokenUsage::default(),
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: HashMap::new(),
            ctx_tokens_cache: std::sync::Mutex::new(None),
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

    /// Reconstruct a subagent's `AgentSession` from its OWN existing journal
    /// (as opposed to `create_child`, which starts a brand new one) — used
    /// only by `resume_subagent_task` to auto-resume a subagent that was
    /// still running when the process crashed/restarted. Shares the
    /// parent's `Arc<ToolRegistry>`/`Arc<dyn ModelClient>`/governance, same
    /// as `create_child`. Note: the original `SubagentSpec.tool_allowlist`
    /// isn't itself journaled (only `role` is, via `SubagentSpawned`), so a
    /// resumed subagent always gets the parent's un-narrowed governance —
    /// a documented limitation, not a silent one.
    async fn resume_child(
        &self,
        session_id: SessionId,
        workspace: PathBuf,
        cancel_token: CancellationToken,
    ) -> Result<AgentSession, LoopError> {
        let journal_dir = self.journal.directory().to_path_buf();
        let journal = Journal::open(&journal_dir, session_id).await?;
        let state = journal.replay(session_id).await?;

        let context = forge_context::ContextEngine::new(workspace.clone(), session_id);
        let session_tmp = forge_tools::SessionTempDir::create(session_id)?;
        let system = assemble_system_prompt(
            &context.load_agents_md(),
            context.load_skills().as_slice(),
            session_tmp.path(),
        );
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
            .filter(|m| m.role == MessageRole::System)
        {
            *first = system_message;
        } else {
            messages.insert(0, system_message);
        }

        let incomplete = state.incomplete_intents.clone();
        let wait_reason = crate::restored_wait_reason(&state.pending_hitl);
        let queue =
            TaskQueue::from_restored(crate::restored_queue_items(session_id, state.queue_items));

        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let mut child = AgentSession {
            session_id,
            coordinator: self.coordinator.clone(),
            messages: messages.into(),
            events: vec![crate::TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            active_task: ActiveTaskState::from_restored(session_id, state.status, wait_reason),
            tasks: crate::task_runtime::TaskRuntime::with_queue(queue),
            active_model: self.active_model.clone(),
            active_route_id: self.active_route_id.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            thinking_enabled: self.thinking_enabled,
            journal: SessionPersistence::new(journal),
            tools: self.tools.clone(),
            model: self.model.clone(),
            // Shares the parent's egress rather than starting a second proxy:
            // one session, one allow-list, one place to revoke it. The parent
            // owns the proxy, so `egress` here is None — the child holds a
            // grant, not the listener.
            tool_ctx: {
                let mut ctx = ToolContext::new(workspace).with_session_tmp(session_tmp);
                ctx.egress = self.tool_ctx.egress.clone();
                ctx
            },
            egress: None,
            max_turns: self.max_turns,
            governance: self.governance.clone(),
            context,
            enable_context: self.enable_context,
            enable_gov: self.enable_gov,
            cancel_token: Some(cancel_token),
            turn_cancel_token: CancellationToken::new(),
            token_usage,
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: state.tool_results.clone(),
            ctx_tokens_cache: std::sync::Mutex::new(None),
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
        child.reconcile_incomplete_intents(&incomplete).await?;
        // Deliberately no `mark_interrupted_if_stale()` here — that method
        // exists to convert a stale `Working`/`Waiting` top-level session
        // into `Interrupted` on resume, on the assumption a human will
        // re-issue instructions if they want to continue. A subagent has no
        // human to do that: the entire point of auto-resume is to continue
        // a `Working` state via `run_agent_turns` (see
        // `resume_subagent_task`), which calling this first would defeat by
        // marking it terminal before that ever gets a chance to run.
        Ok(child)
    }

    /// Auto-resume one orphaned subagent found by
    /// `reconcile_orphaned_background_tasks`: reopen its journal, reconcile
    /// its own incomplete tool intents (it gets this for free — it's a
    /// normal session on its own terms), and re-launch its agent loop
    /// exactly as if `spawn_subagent` had just been called. Errors resuming
    /// one subagent are reported on that task (`Failed`) rather than
    /// propagated — one bad subagent must not block the parent session
    /// itself from finishing resume.
    pub(crate) async fn resume_subagent_task(
        &mut self,
        task: &forge_durable::RestoredBackgroundTask,
        workspace: PathBuf,
    ) -> Result<(), LoopError> {
        let Some(child_session_id) = task.child_session_id else {
            return Ok(());
        };
        let cancel = CancellationToken::new();
        if !self.coordinator.contains(child_session_id) {
            self.coordinator
                .register_child_with_cancel(
                    self.session_id,
                    child_session_id,
                    task.label.clone(),
                    cancel.clone(),
                )
                .map_err(|error| LoopError::Other(error.to_string()))?;
        }
        // Reuses `task.id` (not a fresh id) — see `resume_slot`'s doc
        // comment for why that's what lets the eventual
        // `append_background_task_finished` (from either the error path
        // below or the normal `drive_subagent` -> `finish_background_task`
        // path once this subagent actually finishes) close the SAME pair
        // `BackgroundTaskStarted` opened under.
        let id = self.tasks.background.resume_slot(
            task.id,
            BackgroundTaskKind::Subagent {
                role: task.label.clone(),
                prompt: String::new(),
            },
            task.label.clone(),
            self.active_task.task_id,
            cancel.clone(),
            Some(child_session_id),
        );

        let child = match self.resume_child(child_session_id, workspace, cancel).await {
            Ok(child) => child,
            Err(e) => {
                let error = format!("could not resume subagent: {e}");
                let _ = self.coordinator.update(
                    child_session_id,
                    AgentStatus::Failed,
                    Some(error.clone()),
                );
                self.tasks.background.set_status(
                    id,
                    BackgroundTaskStatus::Failed {
                        error: error.clone(),
                    },
                );
                self.journal
                    .append_background_task_finished(
                        self.session_id,
                        id,
                        BackgroundTaskStatus::Failed {
                            error: error.clone(),
                        }
                        .tag(),
                        &error,
                    )
                    .await?;
                return Ok(());
            }
        };
        self.tasks.background.mark_running(id);

        // Branch name follows `create_worktree`'s naming convention
        // (`forge/subagent/<dir-basename>`) — reconstructing it from the
        // path avoids duplicating `forge_storage::worktree`'s private
        // sanitize/slug logic just to redisplay something derivable.
        let branch = child
            .workspace_root()
            .file_name()
            .map(|name| format!("forge/subagent/{}", name.to_string_lossy()));
        if let Some(branch) = branch {
            self.tasks
                .background
                .set_worktree(id, child.workspace_root().to_path_buf(), branch);
        }
        let latest_message = self
            .tasks
            .background
            .latest_message_cell(id)
            .unwrap_or_default();

        let (tx, rx) = std::sync::mpsc::channel();
        let result_sink = Arc::new(std::sync::Mutex::new(Some(tx)));
        let (hitl_tx, hitl_rx) = tokio::sync::mpsc::unbounded_channel::<HitlDecision>();
        self.tasks.subagent_hitl_senders.insert(id, hitl_tx);
        let coordinator = self.coordinator.clone();
        let initial_resume = child.active_task.lifecycle == TaskLifecycle::Working;
        spawn_subagent_actor(
            child,
            None,
            initial_resume,
            child_session_id,
            result_sink,
            hitl_rx,
            latest_message,
            coordinator,
        )?;
        self.tasks.receivers.insert(id, std::sync::Mutex::new(rx));
        Ok(())
    }

    pub(crate) async fn followup_retained_subagent(
        &mut self,
        target: SessionId,
        message: String,
    ) -> Result<Option<BackgroundTaskId>, LoopError> {
        let Some(retained) = self.tasks.retained_subagents.get(&target) else {
            return Ok(None);
        };
        let snapshot = self
            .coordinator
            .descendant(self.session_id, target)
            .map_err(|error| LoopError::Other(error.to_string()))?;
        if !snapshot.status.is_terminal() {
            return Ok(None);
        }
        let label = retained.label.clone();
        let workspace = retained.workspace.clone();
        let result_sink = retained.result_sink.clone();
        let hitl_sender = retained.hitl_sender.clone();
        let latest_message = retained.latest_message.clone();
        let cancel = CancellationToken::new();
        let task_id = self.tasks.background.spawn_slot(
            BackgroundTaskKind::Subagent {
                role: label.clone(),
                prompt: message.clone(),
            },
            label.clone(),
            self.active_task.task_id,
            cancel.clone(),
            Some(target),
        );
        self.journal
            .append_background_task_started_with_parent(
                self.session_id,
                task_id,
                "subagent",
                &label,
                Some(target),
                Some(self.session_id),
            )
            .await?;
        self.journal
            .append_subagent_spawned(self.session_id, task_id, target, &label, &workspace)
            .await?;
        let branch = workspace
            .file_name()
            .map(|name| format!("forge/subagent/{}", name.to_string_lossy()))
            .unwrap_or_else(|| format!("forge/subagent/{label}"));
        self.tasks
            .background
            .set_worktree(task_id, workspace, branch);
        let (tx, rx) = std::sync::mpsc::channel();
        *result_sink.lock().unwrap() = Some(tx);
        self.tasks
            .background
            .set_latest_message_cell(task_id, latest_message);
        self.tasks
            .subagent_hitl_senders
            .insert(task_id, hitl_sender);
        self.tasks
            .receivers
            .insert(task_id, std::sync::Mutex::new(rx));
        if let Err(error) =
            self.coordinator
                .followup_with_cancel(self.session_id, target, message, cancel)
        {
            let error = error.to_string();
            self.tasks.background.set_status(
                task_id,
                BackgroundTaskStatus::Failed {
                    error: error.clone(),
                },
            );
            self.tasks.receivers.remove(&task_id);
            self.tasks.subagent_hitl_senders.remove(&task_id);
            self.journal
                .append_background_task_finished(
                    self.session_id,
                    task_id,
                    BackgroundTaskStatus::Failed {
                        error: error.clone(),
                    }
                    .tag(),
                    &error,
                )
                .await?;
            return Err(LoopError::Other(error));
        }
        self.tasks.background.mark_running(task_id);
        Ok(Some(task_id))
    }

    pub(crate) async fn restore_finished_subagent_task(
        &mut self,
        task: &forge_durable::RestoredBackgroundTask,
        workspace: PathBuf,
    ) -> Result<(), LoopError> {
        let Some(child_session_id) = task.child_session_id else {
            return Ok(());
        };
        let cancel = CancellationToken::new();
        let child = self
            .resume_child(child_session_id, workspace, cancel)
            .await?;
        let result_sink = Arc::new(std::sync::Mutex::new(None));
        let latest_message = Arc::new(std::sync::Mutex::new(None));
        let (hitl_tx, hitl_rx) = tokio::sync::mpsc::unbounded_channel::<HitlDecision>();
        self.tasks.retained_subagents.insert(
            child_session_id,
            crate::task_runtime::RetainedSubagent {
                label: task.label.clone(),
                workspace: child.workspace_root().to_path_buf(),
                result_sink: result_sink.clone(),
                hitl_sender: hitl_tx,
                latest_message: latest_message.clone(),
            },
        );
        spawn_subagent_actor(
            child,
            None,
            false,
            child_session_id,
            result_sink,
            hitl_rx,
            latest_message,
            self.coordinator.clone(),
        )?;
        Ok(())
    }

    /// Create a worktree, spin up a child session in it, and drive the
    /// child's agent loop to completion in the background. Returns as soon
    /// as the child is spawned — does not wait for it to finish.
    pub async fn spawn_subagent(
        &mut self,
        spec: SubagentSpec,
    ) -> Result<BackgroundTaskId, LoopError> {
        let repo_root = forge_storage::detect_repo_info(&self.tool_ctx.workspace_root)
            .worktree_root
            .ok_or_else(|| {
                LoopError::Other(
                    "subagents require the workspace to be inside a git repository".into(),
                )
            })?;
        let storage = forge_storage::LocalRuntimeStorage::new(&self.tool_ctx.workspace_root);
        let base_dir = storage
            .path_for(forge_storage::RuntimeDataKind::Worktree)
            .map_err(|e| LoopError::Other(format!("could not prepare worktree storage: {e}")))?;

        let child_session_id = forge_durable::new_session_id();
        let cancel = CancellationToken::new();
        self.coordinator
            .register_child_with_cancel(
                self.session_id,
                child_session_id,
                spec.role.clone(),
                cancel.clone(),
            )
            .map_err(|error| LoopError::Other(error.to_string()))?;
        self.events.push(crate::TurnEvent {
            kind: "subagent_spawned".into(),
            detail: serde_json::json!({
                "agent_id": child_session_id,
                "parent_id": self.session_id,
                "task_name": spec.role.clone(),
            })
            .to_string(),
        });
        let task_id = self.tasks.background.spawn_slot(
            BackgroundTaskKind::Subagent {
                role: spec.role.clone(),
                prompt: spec.prompt.clone(),
            },
            spec.role.clone(),
            self.active_task.task_id,
            cancel.clone(),
            Some(child_session_id),
        );
        self.journal
            .append_background_task_started_with_parent(
                self.session_id,
                task_id,
                "subagent",
                &spec.role,
                Some(child_session_id),
                Some(self.session_id),
            )
            .await?;

        let worktree =
            match forge_storage::create_worktree(&repo_root, &base_dir, task_id.0, &spec.role) {
                Ok(wt) => wt,
                Err(e) => {
                    let _ = self.coordinator.update(
                        child_session_id,
                        AgentStatus::Failed,
                        Some(e.to_string()),
                    );
                    return self
                        .fail_subagent_spawn(task_id, format!("could not create worktree: {e}"))
                        .await;
                }
            };
        // Only journaled once the worktree actually exists — `workspace` is
        // how a restart finds this same checkout again (see
        // `reconcile_orphaned_background_tasks`).
        self.journal
            .append_subagent_spawned(
                self.session_id,
                task_id,
                child_session_id,
                &spec.role,
                &worktree.path,
            )
            .await?;

        let child = match self
            .create_child(child_session_id, worktree.path.clone(), cancel, &spec)
            .await
        {
            Ok(child) => child,
            Err(e) => {
                let _ = self.coordinator.update(
                    child_session_id,
                    AgentStatus::Failed,
                    Some(e.to_string()),
                );
                let _ = forge_storage::remove_worktree(&repo_root, &worktree.path);
                return self
                    .fail_subagent_spawn(task_id, format!("could not start subagent: {e}"))
                    .await;
            }
        };

        self.tasks.background.mark_running(task_id);
        self.tasks
            .background
            .set_worktree(task_id, worktree.path.clone(), worktree.branch.clone());
        let latest_message = self
            .tasks
            .background
            .latest_message_cell(task_id)
            .unwrap_or_default();

        let (tx, rx) = std::sync::mpsc::channel();
        let result_sink = Arc::new(std::sync::Mutex::new(Some(tx)));
        let (hitl_tx, hitl_rx) = tokio::sync::mpsc::unbounded_channel::<HitlDecision>();
        self.tasks.subagent_hitl_senders.insert(task_id, hitl_tx);
        let prompt = spec.prompt.clone();
        spawn_subagent_actor(
            child,
            Some(prompt),
            false,
            child_session_id,
            result_sink,
            hitl_rx,
            latest_message,
            self.coordinator.clone(),
        )?;
        self.tasks
            .receivers
            .insert(task_id, std::sync::Mutex::new(rx));

        Ok(task_id)
    }

    /// Roll back a `spawn_subagent` call that failed before the child's
    /// agent loop ever started — marks the already-registered task `Failed`
    /// (rather than leaving it phantom-`Queued` forever) and journals both
    /// halves of the lifecycle so replay sees a closed pair.
    async fn fail_subagent_spawn(
        &mut self,
        task_id: BackgroundTaskId,
        error: String,
    ) -> Result<BackgroundTaskId, LoopError> {
        self.tasks.background.set_status(
            task_id,
            BackgroundTaskStatus::Failed {
                error: error.clone(),
            },
        );
        self.journal
            .append_background_task_finished(
                self.session_id,
                task_id,
                BackgroundTaskStatus::Failed {
                    error: error.clone(),
                }
                .tag(),
                &error,
            )
            .await?;
        Err(LoopError::Other(error))
    }
}

/// Drive an already-started (or about-to-continue) subagent to a terminal
/// state, looping through any number of HITL waits along the way, then send
/// the final outcome. Shared by `spawn_subagent` (`result` is a fresh
/// `run_user_message`'s result) and `reconcile_orphaned_background_tasks`
/// (`result` is a continued `run_agent_turns`, or a synthetic `Ok` if the
/// resumed child was already `Waiting`/terminal when its journal replayed —
/// the loop and status derivation below handle either starting point
/// identically).
fn spawn_subagent_actor(
    child: AgentSession,
    initial_prompt: Option<String>,
    resume_turn: bool,
    child_session_id: SessionId,
    result_sink: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<BackgroundTaskOutcome>>>>,
    mut hitl_rx: tokio::sync::mpsc::UnboundedReceiver<HitlDecision>,
    latest_message: Arc<std::sync::Mutex<Option<String>>>,
    coordinator: AgentCoordinator,
) -> Result<(), LoopError> {
    let (_, mut command_rx) = coordinator
        .actor_channel(child_session_id)
        .map_err(|error| LoopError::Other(error.to_string()))?;
    tokio::spawn(async move {
        let mut child = child;
        let result = if let Some(prompt) = initial_prompt {
            Some(child.run_user_message(&prompt).await)
        } else if resume_turn {
            Some(child.run_agent_turns(None).await)
        } else {
            None
        };
        if let Some(result) = result {
            child = drive_subagent(
                child,
                result,
                child_session_id,
                result_sink.clone(),
                &mut hitl_rx,
                latest_message.clone(),
                coordinator.clone(),
            )
            .await;
        }

        while let Some(AgentCommand::Wake { cancel }) = command_rx.recv().await {
            child.cancel_token = Some(cancel);
            let messages = coordinator
                .take_mailbox(child_session_id)
                .unwrap_or_default();
            if messages.is_empty() {
                continue;
            }
            let result = child.run_user_message(&messages.join("\n\n")).await;
            child = drive_subagent(
                child,
                result,
                child_session_id,
                result_sink.clone(),
                &mut hitl_rx,
                latest_message.clone(),
                coordinator.clone(),
            )
            .await;
        }
    });
    Ok(())
}

async fn drive_subagent(
    mut child: AgentSession,
    mut result: Result<forge_types::ModelResponse, LoopError>,
    child_session_id: SessionId,
    result_sink: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<BackgroundTaskOutcome>>>>,
    hitl_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HitlDecision>,
    latest_message: Arc<std::sync::Mutex<Option<String>>>,
    coordinator: AgentCoordinator,
) -> AgentSession {
    let cancel_token = child
        .cancel_token
        .clone()
        .expect("subagent sessions always carry a cancel_token (set in create_child/resume_child)");
    let snapshot = |child: &AgentSession| {
        let text = last_assistant_text(&child.messages);
        if !text.is_empty() {
            *latest_message.lock().unwrap() = Some(text);
        }
    };
    snapshot(&child);

    // A subagent can hit any number of HITL gates across its turns. Each
    // time, park here (not consuming the tokio task's slot wastefully —
    // `recv`/`cancelled` both suspend) until the parent supplies a decision
    // via `resolve_subagent_hitl`, then resume the same turn loop
    // `run_agent_turns` would have continued.
    while child.active_task.lifecycle == TaskLifecycle::Waiting {
        let Some(payload) = child.pending_hitl().cloned() else {
            break;
        };
        let _ = coordinator.update(child_session_id, AgentStatus::Waiting, None);
        if let Some(tx) = result_sink.lock().unwrap().clone() {
            let _ = tx.send(BackgroundTaskOutcome::WaitingForApproval(payload));
        }

        tokio::select! {
            decision = hitl_rx.recv() => match decision {
                Some(decision) => {
                    if let Err(e) = child.resolve_hitl(decision, "subagent-approval").await {
                        result = Err(e);
                        break;
                    }
                    result = child.run_agent_turns(None).await;
                    snapshot(&child);
                    if child.active_task.lifecycle == TaskLifecycle::Working {
                        let _ = coordinator.update(child_session_id, AgentStatus::Running, None);
                    }
                }
                // Sender dropped (parent session gone) — fall through to the
                // reconciliation below, which cancels any non-terminal lifecycle.
                None => break,
            },
            _ = cancel_token.cancelled() => break,
        }
    }

    // Any lifecycle that isn't already terminal here means the loop above
    // exited without a natural Completed/Failed outcome (cancelled — via
    // token or a dropped decision channel — while Working or Waiting).
    // Reconcile explicitly rather than reporting a stale in-progress status.
    if matches!(
        child.active_task.lifecycle,
        TaskLifecycle::Working | TaskLifecycle::Waiting
    ) {
        let _ = child.mark_cancelled().await;
    }
    snapshot(&child);

    let status = match child.active_task.lifecycle {
        TaskLifecycle::Completed => BackgroundTaskStatus::Succeeded {
            summary: last_assistant_text(&child.messages),
        },
        TaskLifecycle::Cancelled => BackgroundTaskStatus::Cancelled,
        // A `Failed` lifecycle is normally reached via a plain `Ok` return
        // (the completion evaluator inside `apply_model_response` decided
        // the turn failed and called `finalize_turn_failure` itself — no
        // `LoopError` propagates for that path), so `result.err()` is
        // usually `None` here. `last_completion`'s evidence summary is the
        // real reason; only fall back to `result`/a generic message for the
        // narrower case of a `LoopError` (e.g. journal I/O) surfacing after
        // the lifecycle was already terminal.
        TaskLifecycle::Failed => BackgroundTaskStatus::Failed {
            error: child
                .last_completion
                .as_ref()
                .map(|d| d.evidence_summary.detail.clone())
                .or_else(|| result.as_ref().err().map(|e| e.to_string()))
                .unwrap_or_else(|| "subagent failed".into()),
        },
        _ => BackgroundTaskStatus::Failed {
            error: result
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "subagent ended without a terminal status".into()),
        },
    };
    let summary = match &status {
        BackgroundTaskStatus::Succeeded { summary } => summary.clone(),
        BackgroundTaskStatus::Failed { error } => error.clone(),
        _ => "cancelled".to_string(),
    };
    let outcome = SubagentOutcome {
        child_session_id,
        status,
        summary,
        token_usage: child.token_usage.clone(),
    };
    let coordinator_status = match &outcome.status {
        BackgroundTaskStatus::Succeeded { .. } => AgentStatus::Completed,
        BackgroundTaskStatus::Failed { .. } => AgentStatus::Failed,
        BackgroundTaskStatus::Cancelled => AgentStatus::Cancelled,
        BackgroundTaskStatus::Queued
        | BackgroundTaskStatus::Running
        | BackgroundTaskStatus::WaitingForApproval { .. } => AgentStatus::Failed,
    };
    let _ = coordinator.update(
        child_session_id,
        coordinator_status,
        Some(outcome.summary.clone()),
    );
    if let Some(tx) = result_sink.lock().unwrap().clone() {
        let _ = tx.send(BackgroundTaskOutcome::Subagent(outcome));
    }
    child
}

fn last_assistant_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant && !m.content.trim().is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use tempfile::TempDir;

    use super::*;
    use crate::LoopConfig;

    async fn git(dir: &std::path::Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    async fn init_repo(dir: &std::path::Path) {
        git(dir, &["init", "-q", "--initial-branch=main"]).await;
        git(dir, &["config", "user.email", "forge@example.com"]).await;
        git(dir, &["config", "user.name", "Forge Test"]).await;
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]).await;
        git(dir, &["commit", "-q", "-m", "init"]).await;
    }

    fn cfg(dir: &std::path::Path) -> LoopConfig {
        LoopConfig {
            max_turns: 5,
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        }
    }

    async fn session_with_script(
        dir: &std::path::Path,
        responses: Vec<ModelResponse>,
    ) -> AgentSession {
        let model = Arc::new(MockModelClient::script(responses));
        AgentSession::create(cfg(dir), model, ToolRegistry::new())
            .await
            .unwrap()
    }

    fn text_response(text: &str) -> ModelResponse {
        ModelResponse {
            text: text.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    async fn wait_terminal(
        session: &mut AgentSession,
        id: BackgroundTaskId,
    ) -> BackgroundTaskStatus {
        for _ in 0..300 {
            session.poll_background_tasks().await.unwrap();
            if let Some(task) = session.tasks.background.get(id) {
                if task.status.is_terminal() {
                    return task.status.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("subagent task {id:?} did not finish within timeout");
    }

    async fn wait_for_waiting(
        session: &mut AgentSession,
        id: BackgroundTaskId,
    ) -> forge_types::HitlPayload {
        for _ in 0..300 {
            session.poll_background_tasks().await.unwrap();
            if let Some(task) = session.tasks.background.get(id) {
                if let BackgroundTaskStatus::WaitingForApproval { payload } = &task.status {
                    return payload.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("subagent task {id:?} never reached WaitingForApproval");
    }

    #[tokio::test]
    async fn subagent_hitl_wait_surfaces_to_the_parent_and_resumes_on_approval() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(
            dir.path(),
            vec![
                ModelResponse {
                    text: "".into(),
                    tool_calls: vec![forge_types::ToolCall {
                        id: "1".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo risky"}),
                    }],
                    usage: None,
                    thinking: None,
                },
                text_response("finished after approval"),
            ],
        )
        .await;
        s.set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "risky-runner".into(),
                prompt: "run the risky command".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();

        let payload = wait_for_waiting(&mut s, id).await;
        assert_eq!(payload.tool, "bash");
        assert_eq!(
            s.background().get(id).unwrap().status,
            BackgroundTaskStatus::WaitingForApproval {
                payload: payload.clone()
            }
        );

        assert!(s.resolve_subagent_hitl(id, forge_types::HitlDecision::Approve));
        let status = wait_terminal(&mut s, id).await;
        match status {
            BackgroundTaskStatus::Succeeded { summary } => {
                assert_eq!(summary, "finished after approval");
            }
            other => panic!("expected Succeeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_subagent_hitl_on_an_unknown_id_returns_false() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(dir.path(), vec![]).await;
        assert!(!s.resolve_subagent_hitl(BackgroundTaskId(999), forge_types::HitlDecision::Approve));
    }

    /// A denied tool call is evidence of a failed turn — same evaluator
    /// behavior the single-session path already has — so a denial finishes
    /// the subagent as `Failed`, not `Succeeded`. What this test actually
    /// guards: (a) denial doesn't hang the subagent forever waiting for a
    /// user that will never show up, and (b) the reported failure carries
    /// the real completion-evaluator reason, not a generic placeholder —
    /// see the `TaskLifecycle::Failed` arm in `spawn_subagent`'s status
    /// derivation.
    #[tokio::test]
    async fn denying_a_subagent_hitl_request_finishes_it_as_failed_with_the_real_reason() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(
            dir.path(),
            vec![
                ModelResponse {
                    text: "".into(),
                    tool_calls: vec![forge_types::ToolCall {
                        id: "1".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo risky"}),
                    }],
                    usage: None,
                    thinking: None,
                },
                text_response("acknowledged the denial"),
            ],
        )
        .await;
        s.set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "denied-runner".into(),
                prompt: "run the risky command".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();

        wait_for_waiting(&mut s, id).await;
        assert!(s.resolve_subagent_hitl(id, forge_types::HitlDecision::Deny));

        let status = wait_terminal(&mut s, id).await;
        match status {
            BackgroundTaskStatus::Failed { error } => {
                assert!(
                    !error.contains("ended without a terminal status"),
                    "expected the real completion-evaluator reason, got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// A model client whose call never resolves — used to hold a subagent
    /// genuinely suspended mid-turn (its journal shows `UserMessage` but no
    /// completing `ModelResponse`), so a "crash" can be simulated by simply
    /// building a second `AgentSession` against the same journal while the
    /// first is still (forever) in flight. A `MockModelClient` that errors
    /// or completes synchronously doesn't work for this: `drive_subagent`'s
    /// own reconciliation step would race ahead and mark the task
    /// `Cancelled` before the "crash" ever happens, since nothing else in
    /// the same process is left to interrupt it.
    struct HangingModelClient;

    #[async_trait::async_trait]
    impl forge_model::ModelClient for HangingModelClient {
        async fn complete(
            &self,
            _req: forge_model::ModelRequest,
        ) -> Result<ModelResponse, forge_model::ModelError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn resuming_the_parent_auto_resumes_a_still_working_subagent() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let cfg = cfg(dir.path());

        // The child's turn starts (`UserMessage` journaled) but its model
        // call never returns — simulating a crash mid-turn: durably
        // `Working`, never reaching a `ModelResponse`/`SessionStatus`. The
        // parent's journal likewise never gets a `BackgroundTaskFinished`.
        let hanging_model = Arc::new(HangingModelClient);
        let parent_session_id = {
            let mut s = AgentSession::create(cfg.clone(), hanging_model, ToolRegistry::new())
                .await
                .unwrap();
            s.spawn_subagent(SubagentSpec {
                role: "interrupted".into(),
                prompt: "do the thing".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();
            // Give the spawned task a moment to actually start running (and
            // journal its `UserMessage`) before "the process dies" — it
            // will still be there hanging on the model call indefinitely.
            tokio::time::sleep(Duration::from_millis(100)).await;
            s.session_id
        };

        // Resume the parent — with a model that will actually complete this
        // time — and confirm the subagent comes back `Running`, not
        // orphaned/`Cancelled`, then finishes normally.
        let resumed_model = Arc::new(MockModelClient::script(vec![text_response(
            "finished after auto-resume",
        )]));
        let mut resumed = AgentSession::resume(
            cfg.clone(),
            resumed_model,
            ToolRegistry::new(),
            parent_session_id,
        )
        .await
        .unwrap();

        assert_eq!(resumed.background().list().count(), 1);
        let task = resumed.background().list().next().unwrap();
        assert!(matches!(task.kind, BackgroundTaskKind::Subagent { .. }));
        assert_ne!(
            task.status,
            BackgroundTaskStatus::Cancelled,
            "a still-Working subagent must be auto-resumed, not marked cancelled"
        );
        let id = task.id;

        let status = wait_terminal(&mut resumed, id).await;
        match status {
            BackgroundTaskStatus::Succeeded { summary } => {
                assert_eq!(summary, "finished after auto-resume");
            }
            other => panic!("expected Succeeded, got {other:?}"),
        }

        // Regression check for `resume_slot` reusing the ORIGINAL journaled
        // id: once this resumed-and-now-finished subagent's
        // `BackgroundTaskFinished` is drained into the parent's own journal
        // (via `poll_background_tasks`), a SECOND resume of the parent must
        // see the pair as closed — not re-detect it as orphaned and attempt
        // to auto-resume an already-completed subagent all over again.
        resumed.poll_background_tasks().await.unwrap();
        let resumed_again_model = Arc::new(MockModelClient::script(vec![text_response("unused")]));
        let resumed_again = AgentSession::resume(
            cfg,
            resumed_again_model,
            ToolRegistry::new(),
            parent_session_id,
        )
        .await
        .unwrap();
        assert_eq!(
            resumed_again.background().list().count(),
            0,
            "the already-finished subagent must not be re-resumed"
        );
    }

    #[tokio::test]
    async fn spawn_subagent_outside_a_git_repo_fails_without_registering_a_phantom_task() {
        let dir = TempDir::new().unwrap();
        let mut s = session_with_script(dir.path(), vec![]).await;
        let err = s
            .spawn_subagent(SubagentSpec {
                role: "test-fixer".into(),
                prompt: "fix the tests".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("git repository"));
        assert_eq!(s.background().list().count(), 0);
    }

    #[tokio::test]
    async fn spawn_subagent_creates_a_worktree_and_reports_success_back_to_the_parent() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(dir.path(), vec![text_response("all fixed")]).await;

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "test-fixer".into(),
                prompt: "fix the failing tests".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();

        assert_eq!(s.background().list().count(), 1);
        let handle = s.background().get(id).unwrap();
        assert!(matches!(handle.kind, BackgroundTaskKind::Subagent { .. }));
        assert!(handle.child_session_id.is_some());

        let status = wait_terminal(&mut s, id).await;
        match status {
            BackgroundTaskStatus::Succeeded { summary } => {
                assert_eq!(summary, "all fixed");
            }
            other => panic!("expected Succeeded, got {other:?}"),
        }
        // Subagent results are retrieved explicitly through the orchestration
        // tools rather than being injected into the parent's prompt queue.
        assert_eq!(s.queue().len(), 0);
    }

    #[tokio::test]
    async fn spawn_subagent_gives_the_child_its_own_worktree_workspace() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(dir.path(), vec![text_response("done")]).await;

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "isolated".into(),
                prompt: "go".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();
        wait_terminal(&mut s, id).await;

        let worktree_dir = dir.path().join(".forge/local/worktrees");
        let entries: Vec<_> = std::fs::read_dir(&worktree_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one worktree dir");
    }

    #[tokio::test]
    async fn spawn_subagent_records_worktree_info_and_a_live_message_snapshot() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s =
            session_with_script(dir.path(), vec![text_response("here's what I found")]).await;

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "explorer".into(),
                prompt: "look around".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();

        let task = s.background().get(id).unwrap();
        let branch = task.worktree_branch.clone().unwrap();
        assert!(branch.starts_with("forge/subagent/"));
        let worktree_path = task.worktree_path.clone().unwrap();
        assert!(
            worktree_path.ends_with(format!("subagent-{}-explorer", id.0)),
            "unexpected worktree path: {worktree_path:?}"
        );

        wait_terminal(&mut s, id).await;
        let task = s.background().get(id).unwrap();
        assert_eq!(
            task.latest_message.lock().unwrap().as_deref(),
            Some("here's what I found")
        );
    }

    #[tokio::test]
    async fn cancelling_a_subagent_reports_cancelled_status() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        // No scripted response — the child's model call would hang/never
        // resolve on a real client, but `MockModelClient` with an empty
        // script errors immediately on the first call, which is fine here:
        // we cancel before that even matters, since `cancel()` races the
        // job itself in `spawn_background_shell`'s sibling pattern.
        let mut s = session_with_script(dir.path(), vec![text_response("too late")]).await;

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "cancel-me".into(),
                prompt: "go".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();
        assert!(s.cancel_background_task(id));

        let status = wait_terminal(&mut s, id).await;
        assert_eq!(status, BackgroundTaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn mark_cancelled_on_the_parent_propagates_to_a_running_subagent() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let mut s = session_with_script(dir.path(), vec![text_response("done")]).await;
        s.append_user_message("parent task").await.unwrap();

        let id = s
            .spawn_subagent(SubagentSpec {
                role: "child".into(),
                prompt: "go".into(),
                tool_allowlist: None,
                max_turns: None,
            })
            .await
            .unwrap();

        s.mark_cancelled().await.unwrap();
        assert!(s.background().get(id).unwrap().cancel.is_cancelled());
    }

    #[tokio::test]
    async fn tool_allowlist_replaces_the_childs_acl() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path()).await;
        let s = session_with_script(dir.path(), vec![]).await;

        let cancel = CancellationToken::new();
        let child = s
            .create_child(
                forge_durable::new_session_id(),
                dir.path().to_path_buf(),
                cancel,
                &SubagentSpec {
                    role: "scoped".into(),
                    prompt: "go".into(),
                    tool_allowlist: Some(vec!["read_file".into()]),
                    max_turns: None,
                },
            )
            .await
            .unwrap();

        let principal = forge_types::Principal::local_dev();
        assert!(child.governance.acl.is_allowed(
            &principal,
            "read_file",
            forge_types::SideEffectClass::Meta
        ));
        assert!(!child.governance.acl.is_allowed(
            &principal,
            "bash",
            forge_types::SideEffectClass::Exec
        ));
    }
}
