//! Executing a tool call and recording the evidence it produced.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

pub(crate) struct PendingToolExecution {
    call: ToolCall,
    tools: Arc<ToolRegistry>,
    tool_ctx: ToolContext,
    budget: ValidationBudget,
}

pub(crate) struct CompletedToolExecution {
    call: ToolCall,
    budget: ValidationBudget,
    pre_edit: Option<Vec<(String, Option<u64>)>>,
    pre_git: Option<GitPre>,
    result: Result<ToolOutput, ToolError>,
}

pub(crate) enum ToolExecutionStart {
    Finished(Option<ModelResponse>),
    Execute(PendingToolExecution),
}

pub(crate) struct PendingToolApplications {
    pub(crate) response: ModelResponse,
    pub(crate) calls: std::vec::IntoIter<ToolCall>,
    pub(crate) budget: ValidationBudget,
}

pub enum ModelResponseApplication {
    Finished(ApplyOutcome),
    Execute(Box<PendingToolApplication>),
}

pub struct PendingToolApplication {
    execution: PendingToolExecution,
    remaining: PendingToolApplications,
}

pub struct CompletedToolApplication {
    execution: CompletedToolExecution,
    remaining: PendingToolApplications,
}

pub struct PendingHitlExecution {
    execution: PendingToolExecution,
}

pub struct CompletedHitlExecution {
    execution: CompletedToolExecution,
}

impl PendingHitlExecution {
    pub fn tool_name(&self) -> &str {
        &self.execution.call.name
    }

    pub async fn execute(self) -> CompletedHitlExecution {
        CompletedHitlExecution {
            execution: self.execution.execute().await,
        }
    }
}

impl PendingToolExecution {
    pub async fn execute(self) -> CompletedToolExecution {
        let Self {
            call,
            tools,
            tool_ctx,
            mut budget,
        } = self;
        let pre_edit = pre_edit_snapshot(&tool_ctx, &call).await;
        let pre_git = git_pre_state(&tool_ctx, &call).await;
        let result = tools
            .call(&tool_ctx, &call.name, call.arguments.clone(), &mut budget)
            .await;
        CompletedToolExecution {
            call,
            budget,
            pre_edit,
            pre_git,
            result,
        }
    }
}

impl PendingToolApplication {
    pub async fn execute(self) -> CompletedToolApplication {
        CompletedToolApplication {
            execution: self.execution.execute().await,
            remaining: self.remaining,
        }
    }
}

async fn hash_workspace_path(tool_ctx: &ToolContext, relative: &str) -> Option<u64> {
    hash_file(&tool_ctx.workspace_root.join(relative)).await
}

async fn run_git_readonly(tool_ctx: &ToolContext, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(&tool_ctx.workspace_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn pre_edit_snapshot(
    tool_ctx: &ToolContext,
    call: &ToolCall,
) -> Option<Vec<(String, Option<u64>)>> {
    match call.name.as_str() {
        "write_file" | "edit" => {
            let path = call
                .arguments
                .get("path")
                .or_else(|| call.arguments.get("file_path"))?
                .as_str()?
                .to_string();
            let hash = hash_workspace_path(tool_ctx, &path).await;
            Some(vec![(path, hash)])
        }
        "apply_patch" => {
            let patch = call.arguments.get("patch")?.as_str()?;
            let mut snapshots = Vec::new();
            for (path, _kind) in parse_patch_paths(patch) {
                let hash = hash_workspace_path(tool_ctx, &path).await;
                snapshots.push((path, hash));
            }
            Some(snapshots)
        }
        _ => None,
    }
}

async fn git_pre_state(tool_ctx: &ToolContext, call: &ToolCall) -> Option<GitPre> {
    if call.name != "git" {
        return None;
    }
    let args: GitCallArgsLite = serde_json::from_value(call.arguments.clone()).ok()?;
    let subcommand = args.subcommand.trim().to_ascii_lowercase();
    Some(match subcommand.as_str() {
        "commit" => GitPre::Head(run_git_readonly(tool_ctx, &["rev-parse", "HEAD"]).await),
        "checkout" | "switch" => {
            GitPre::Branch(run_git_readonly(tool_ctx, &["rev-parse", "--abbrev-ref", "HEAD"]).await)
        }
        "restore" => GitPre::RestorePath(
            args.args
                .iter()
                .rev()
                .find(|argument| !argument.starts_with('-'))
                .cloned(),
        ),
        "add" => GitPre::NotVerified,
        _ => GitPre::NotVerified,
    })
}

impl AgentSession {
    pub(crate) async fn next_tool_application(
        &mut self,
        mut pending: PendingToolApplications,
    ) -> Result<ModelResponseApplication, LoopError> {
        while let Some(call) = pending.calls.next() {
            match self.start_tool_call(&call, &mut pending.budget).await? {
                ToolExecutionStart::Finished(Some(pause)) => {
                    self.turn.restore_validation_budget(pending.budget);
                    return Ok(ModelResponseApplication::Finished(ApplyOutcome::Hitl(
                        pause,
                    )));
                }
                ToolExecutionStart::Finished(None) => {
                    if self.active_task.lifecycle == TaskLifecycle::Failed {
                        self.turn.restore_validation_budget(pending.budget);
                        return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(
                            pending.response,
                        )));
                    }
                }
                ToolExecutionStart::Execute(execution) => {
                    return Ok(ModelResponseApplication::Execute(Box::new(
                        PendingToolApplication {
                            execution,
                            remaining: pending,
                        },
                    )));
                }
            }
        }
        self.turn.restore_validation_budget(pending.budget);
        Ok(ModelResponseApplication::Finished(ApplyOutcome::Continue))
    }

    pub async fn finish_tool_application(
        &mut self,
        completed: CompletedToolApplication,
    ) -> Result<ModelResponseApplication, LoopError> {
        let mut pending = completed.remaining;
        if let Err(ToolError::SandboxDenied {
            reason,
            denied_host,
            ..
        }) = &completed.execution.result
        {
            let call = completed.execution.call.clone();
            pending.budget = completed.execution.budget;
            self.turn.restore_validation_budget(pending.budget);
            let denied_host = denied_host
                .clone()
                .or_else(|| self.take_denied_egress_host());
            return self
                .enter_sandbox_hitl(call, reason.clone(), denied_host)
                .await
                .map(ModelResponseApplication::Finished);
        }
        let (budget, result) = self.finish_tool_call(completed.execution).await;
        pending.budget = budget;
        if let Err(error) = result {
            self.turn.restore_validation_budget(pending.budget);
            return Err(error);
        }
        if self.active_task.lifecycle == TaskLifecycle::Failed {
            self.turn.restore_validation_budget(pending.budget);
            return Ok(ModelResponseApplication::Finished(ApplyOutcome::Done(
                pending.response,
            )));
        }
        self.next_tool_application(pending).await
    }

    async fn hash_workspace_path(&self, relative: &str) -> Option<u64> {
        hash_workspace_path(&self.tool_ctx, relative).await
    }

    async fn run_git_readonly(&self, args: &[&str]) -> Option<String> {
        run_git_readonly(&self.tool_ctx, args).await
    }

    pub(crate) async fn pre_tool_state(
        &self,
        call: &ToolCall,
    ) -> (Option<Vec<(String, Option<u64>)>>, Option<GitPre>) {
        (
            pre_edit_snapshot(&self.tool_ctx, call).await,
            git_pre_state(&self.tool_ctx, call).await,
        )
    }

    /// Build evidence for a `write_file`/`apply_patch` call from its
    /// pre-call content hashes and the tool's own success/failure report.
    /// Post-call state is re-read from the filesystem — never trusted from
    /// the tool's text output alone.
    async fn push_file_edit_evidence(
        &mut self,
        call: &ToolCall,
        pre: Vec<(String, Option<u64>)>,
        output: &ToolOutput,
    ) {
        match call.name.as_str() {
            // `edit` must be handled here alongside `write_file`: both are
            // classified as `FileEffectKind::Modified` expectations by
            // `classify_turn`, so an `edit` that pushed no evidence would leave
            // its own expectation permanently unverifiable and report
            // "No file modifications were successfully applied" for an edit
            // that actually landed on disk.
            "write_file" | "edit" => {
                let Some((path, pre_hash)) = pre.into_iter().next() else {
                    return;
                };
                let post_hash = self.hash_workspace_path(&path).await;
                let event = if output.is_error {
                    ExecutionEvent::ToolFailed
                } else if pre_hash.is_none() {
                    ExecutionEvent::FileCreated
                } else {
                    ExecutionEvent::FileWritten
                };
                let mut entry = EvidenceEntry::new(event)
                    .operation_id(call.id.clone())
                    .tool_name(call.name.clone())
                    .path(path)
                    .checksum_before(pre_hash)
                    .checksum_after(post_hash);
                if output.is_error {
                    entry = entry.error(truncate(&output.content, 200));
                }
                self.turn.push_evidence(entry);
            }
            "apply_patch" => {
                for (path, pre_hash) in pre {
                    let post_hash = self.hash_workspace_path(&path).await;
                    let event = if output.is_error {
                        ExecutionEvent::PatchRejected
                    } else {
                        ExecutionEvent::PatchApplied
                    };
                    let mut entry = EvidenceEntry::new(event)
                        .operation_id(call.id.clone())
                        .tool_name("apply_patch")
                        .path(path)
                        .checksum_before(pre_hash)
                        .checksum_after(post_hash);
                    if output.is_error {
                        entry = entry.error(truncate(&output.content, 200));
                    }
                    self.turn.push_evidence(entry);
                }
            }
            _ => {}
        }
    }

    /// Build evidence for a `git` call, verifying the subcommand's expected
    /// repository effect where practical (see `GitPre`).
    async fn push_git_evidence(
        &mut self,
        call: &ToolCall,
        pre: Option<GitPre>,
        output: &ToolOutput,
    ) {
        let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) else {
            return;
        };
        let sub = a.subcommand.trim().to_ascii_lowercase();
        let mut entry = EvidenceEntry::new(if output.is_error {
            ExecutionEvent::GitCommandFailed
        } else {
            ExecutionEvent::GitCommandSucceeded
        })
        .operation_id(call.id.clone())
        .tool_name("git")
        .git_command(sub.clone());

        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
            self.turn.push_evidence(entry);
            return;
        }

        let verified = match pre {
            Some(GitPre::Head(pre_head)) => {
                let post_head = self.run_git_readonly(&["rev-parse", "HEAD"]).await;
                Some(pre_head != post_head)
            }
            Some(GitPre::Branch(pre_branch)) => {
                let post_branch = self
                    .run_git_readonly(&["rev-parse", "--abbrev-ref", "HEAD"])
                    .await;
                Some(pre_branch != post_branch)
            }
            Some(GitPre::RestorePath(Some(path))) => {
                let still_dirty = self
                    .run_git_readonly(&["diff", "--name-only", "--", &path])
                    .await
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(true);
                Some(!still_dirty)
            }
            _ if sub == "add" => {
                let staged = self
                    .run_git_readonly(&["diff", "--cached", "--name-only"])
                    .await;
                Some(staged.map(|s| !s.trim().is_empty()).unwrap_or(false))
            }
            _ => None,
        };
        entry = entry.git_effect_verified(verified);
        self.turn.push_evidence(entry);
    }

    fn push_search_evidence(&mut self, call: &ToolCall, output: &ToolOutput) {
        let event = if output.is_error {
            ExecutionEvent::SearchFailed
        } else {
            ExecutionEvent::SearchFinished
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(call.name.clone());
        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
        } else {
            let count = search_result_count(&output.content);
            entry = entry.count(count);
        }
        self.turn.push_evidence(entry);
    }

    /// Ensures every `ToolOutput` reaching a `Message` carries a real
    /// `outcome`, even for `Tool` impls that don't set one explicitly — so
    /// not every tool in `forge-tools` needs an individual update.
    pub(crate) fn backfill_tool_outcome(output: &mut ToolOutput) {
        if output.outcome.is_none() {
            output.outcome = Some(if output.is_error {
                ExecutionOutcome::Failed {
                    exit_code: output.exit_code,
                }
            } else {
                ExecutionOutcome::Success
            });
        }
    }

    fn push_bash_evidence(&mut self, call: &ToolCall, output: &ToolOutput) {
        let event = if output.is_error {
            ExecutionEvent::ToolFailed
        } else {
            ExecutionEvent::ToolFinished
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(bash_label(&call.arguments));
        if let Some(code) = output.exit_code {
            entry = entry.exit_code(code);
        }
        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
        }
        self.turn.push_evidence(entry);
    }

    /// Dispatches to the right evidence builder for a successfully-dispatched
    /// tool call (the tool itself may still report `is_error`). No-op for
    /// tools with no completion-relevant side effect (e.g. `read_file`).
    pub(crate) async fn push_success_evidence(
        &mut self,
        call: &ToolCall,
        pre_edit: Option<Vec<(String, Option<u64>)>>,
        pre_git: Option<GitPre>,
        output: &ToolOutput,
    ) {
        match call.name.as_str() {
            "write_file" | "apply_patch" | "edit" => {
                if let Some(pre) = pre_edit {
                    self.push_file_edit_evidence(call, pre, output).await;
                }
            }
            "git" => self.push_git_evidence(call, pre_git, output).await,
            "glob" | "grep" | "rg" => self.push_search_evidence(call, output),
            "bash" => self.push_bash_evidence(call, output),
            _ => {}
        }
    }

    /// Evidence for a call the runtime refused to execute at all (ACL denial,
    /// HITL denial) — no filesystem/process ever ran, so there's nothing to
    /// verify beyond recording the refusal.
    pub(crate) fn push_denied_evidence(&mut self, call: &ToolCall, message: &str) {
        let event = match call.name.as_str() {
            "git" => ExecutionEvent::GitCommandFailed,
            "write_file" | "apply_patch" | "edit" => ExecutionEvent::PatchRejected,
            "glob" | "grep" | "rg" => ExecutionEvent::SearchFailed,
            _ => ExecutionEvent::ToolFailed,
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(call.name.clone())
            .error(truncate(message, 200));
        if call.name == "git" {
            if let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) {
                entry = entry.git_command(a.subcommand.trim().to_ascii_lowercase());
            }
        }
        self.turn.push_evidence(entry);
    }

    /// Returns Some(response) if paused for HITL.
    pub(crate) async fn start_tool_call(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<ToolExecutionStart, LoopError> {
        if self.try_serve_journaled_tool(call).await? {
            return Ok(ToolExecutionStart::Finished(None));
        }
        let call = forge_tools::canonicalize_tool_call(call.clone());
        self.turn.record_call(call.clone());
        let class = self
            .tools
            .get(&call.name)
            .map(|t| t.side_effect_class())
            .unwrap_or(SideEffectClass::Meta);

        if self.enable_gov {
            let decision = self.governance.authorize(&call, class);
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
                        sandbox_escalation: false,
                        denied_host: None,
                    };
                    self.journal
                        .append_hitl_wait(self.session_id, &serde_json::to_value(&payload).unwrap())
                        .await?;
                    let request_id = payload.call_id.clone();
                    self.enter_waiting(
                        WaitReason::Approval {
                            request_id,
                            payload: payload.clone(),
                        },
                        TransitionReason::HitlWait,
                    )
                    .await?;
                    self.events.push(TurnEvent {
                        kind: "hitl_wait".into(),
                        detail: payload.tool.clone(),
                    });
                    self.turn.push_evidence(
                        EvidenceEntry::new(ExecutionEvent::WaitingForUser)
                            .operation_id(call.id.clone())
                            .tool_name(call.name.clone()),
                    );
                    return Ok(ToolExecutionStart::Finished(Some(ModelResponse {
                        text: format!("Awaiting HITL approval for tool {}", call.name),
                        tool_calls: vec![call.clone()],
                        usage: None,
                        thinking: None,
                    })));
                }
                PolicyDecision::Allow => {}
                // `PolicyDecision` is `#[non_exhaustive]`, so the denial path is the
                // wildcard rather than a named `Deny`: this gate must fail CLOSED. An
                // explicit deny and a decision this build does not recognise are both
                // refused here, so neither can fall through to the execution below.
                _ => {
                    let output = ToolOutput::denied(format!("denied by ACL: {}", call.name));
                    self.push_denied_evidence(&call, &output.content);
                    self.journal
                        .append_tool_intent(self.session_id, &call)
                        .await?;
                    self.journal
                        .append_tool_result(self.session_id, &call, &output)
                        .await?;
                    self.remember_tool_result(&call, &output);
                    self.messages
                        .push(Message::from_tool_output(&call, &output));
                    return Ok(ToolExecutionStart::Finished(None));
                }
            }
        }

        self.journal
            .append_tool_intent(self.session_id, &call)
            .await?;

        // `background_run` never reaches `ToolRegistry::call` — it's
        // intercepted here and routed to `spawn_background_shell` instead,
        // so starting it doesn't block this turn. See `background.rs`.
        if call.name == "background_run" {
            return Ok(ToolExecutionStart::Finished(
                self.dispatch_background_run(&call).await?,
            ));
        }

        if call.name == "ask_user_question" {
            return self.dispatch_ask_user_question(&call).await;
        }

        Ok(ToolExecutionStart::Execute(PendingToolExecution {
            call: call.clone(),
            tools: self.tools.clone(),
            tool_ctx: self.tool_ctx.clone(),
            budget: std::mem::take(budget),
        }))
    }

    pub(crate) async fn finish_tool_call(
        &mut self,
        completed: CompletedToolExecution,
    ) -> (ValidationBudget, Result<(), LoopError>) {
        let CompletedToolExecution {
            call,
            budget,
            pre_edit,
            pre_git,
            result,
        } = completed;
        let finish_result = async {
            match result {
                Ok(mut output) => {
                    Self::backfill_tool_outcome(&mut output);
                    self.push_success_evidence(&call, pre_edit, pre_git, &output)
                        .await;
                    if self.enable_context {
                        output.content = self.context.maybe_offload_tool_content(output.content)?;
                    }
                    self.freeze_tool_output(&mut output);
                    self.journal
                        .append_tool_result(self.session_id, &call, &output)
                        .await?;
                    self.remember_tool_result(&call, &output);
                    self.messages.push(Message::from_tool_output(&call, &output));
                    self.events.push(TurnEvent {
                        kind: "tool".into(),
                        detail: format!("{} -> {} chars", call.name, output.content.len()),
                    });
                }
                Err(ToolError::Validation(ve)) => {
                    let msg = tool_validation_failed_content(&ve);
                    self.journal
                        .append_validation_failed(
                            self.session_id,
                            &call.id,
                            &call.name,
                            &msg,
                        )
                        .await?;
                    self.messages.push(Message {
                        outcome: ExecutionOutcome::Failed { exit_code: None },
                        role: MessageRole::Tool,
                        content: msg.clone(),
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        thinking: None,
                        thinking_duration_secs: None,
                        tool_calls: vec![],
            attachments: Vec::new(),
                    });
                    self.events.push(TurnEvent {
                        kind: "validation".into(),
                        detail: msg,
                    });
                }
                Err(error) => {
                    let content = error.to_string();
                    let is_budget = content.contains("validation retry budget exceeded");
                    let outcome = error.as_outcome();
                    let output = ToolOutput {
                        outcome: Some(outcome),
                        content: if is_budget {
                            format!(
                                "{content}. Stop retrying this tool with the same invalid argument shape. \
                                 Either call it with valid structured JSON types or finish with a final answer."
                            )
                        } else {
                            content
                        },
                        is_error: true,
                        exit_code: None,
                        attachments: Vec::new(),
                    };
                    self.journal
                        .append_tool_result(self.session_id, &call, &output)
                        .await?;
                    self.remember_tool_result(&call, &output);
                    self.messages.push(Message::from_tool_output(&call, &output));
                    if is_budget {
                        self.events.push(TurnEvent {
                            kind: "validation_exhausted".into(),
                            detail: output.content.clone(),
                        });
                        self.finalize_turn_failure(
                            "Forge couldn't complete this turn after repeated invalid tool calls.",
                            "validation_exhausted",
                        )
                        .await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        (budget, finish_result)
    }

    #[cfg(test)]
    pub(crate) async fn run_one_tool_exec_only(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<(), LoopError> {
        if let Some(pending) = self.begin_hitl_execution(call, budget).await? {
            let completed = IsolatedTask::spawn(pending.execute())
                .join()
                .await
                .map_err(|error| LoopError::Other(format!("tool task join: {error}")))?
                .ok_or(LoopError::Cancelled)?;
            self.finish_hitl_execution(completed).await?;
        }
        Ok(())
    }

    pub async fn begin_hitl_execution(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<Option<PendingHitlExecution>, LoopError> {
        self.begin_hitl_execution_with_options(call, budget, false, true)
            .await
    }

    pub(crate) async fn begin_hitl_execution_with_options(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
        unconfined: bool,
        append_intent: bool,
    ) -> Result<Option<PendingHitlExecution>, LoopError> {
        if !unconfined && self.try_serve_journaled_tool(call).await? {
            return Ok(None);
        }
        if call.name == "background_run" {
            self.turn.record_call(call.clone());
            if append_intent {
                self.journal
                    .append_tool_intent(self.session_id, call)
                    .await?;
            }
            self.dispatch_background_run(call).await?;
            return Ok(None);
        }
        if call.name == "ask_user_question" {
            self.turn.record_call(call.clone());
            self.dispatch_ask_user_question(call).await?;
            return Ok(None);
        }
        self.turn.record_call(call.clone());
        if append_intent {
            self.journal
                .append_tool_intent(self.session_id, call)
                .await?;
        }
        Ok(Some(PendingHitlExecution {
            execution: PendingToolExecution {
                call: call.clone(),
                tools: self.tools.clone(),
                tool_ctx: if unconfined {
                    self.tool_ctx.clone().with_unconfined_shell()
                } else {
                    self.tool_ctx.clone()
                },
                budget: std::mem::take(budget),
            },
        }))
    }

    pub async fn finish_hitl_execution(
        &mut self,
        completed: CompletedHitlExecution,
    ) -> Result<(), LoopError> {
        let CompletedToolExecution {
            call,
            budget: _,
            pre_edit,
            pre_git,
            result,
        } = completed.execution;
        match result {
            Ok(mut output) => {
                Self::backfill_tool_outcome(&mut output);
                self.push_success_evidence(&call, pre_edit, pre_git, &output)
                    .await;
                if self.enable_context {
                    output.content = self.context.maybe_offload_tool_content(output.content)?;
                }
                self.freeze_tool_output(&mut output);
                self.journal
                    .append_tool_result(self.session_id, &call, &output)
                    .await?;
                self.remember_tool_result(&call, &output);
                if call.name == "update_plan" && !output.is_error {
                    // Stateless checklist broadcast — clients replace whatever they
                    // were showing with this payload. Mirrors codex PlanUpdate.
                    self.events.push(TurnEvent {
                        kind: "plan_update".into(),
                        detail: call.arguments.to_string(),
                    });
                }
                self.messages
                    .push(Message::from_tool_output(&call, &output));
            }
            Err(e) => {
                let outcome = e.as_outcome();
                let output = ToolOutput {
                    outcome: Some(outcome),
                    content: e.to_string(),
                    is_error: true,
                    exit_code: None,
                    attachments: Vec::new(),
                };
                self.journal
                    .append_tool_result(self.session_id, &call, &output)
                    .await?;
                self.remember_tool_result(&call, &output);
            }
        }
        Ok(())
    }

    async fn enter_sandbox_hitl(
        &mut self,
        call: ToolCall,
        reason: String,
        denied_host: Option<String>,
    ) -> Result<ApplyOutcome, LoopError> {
        let payload = HitlPayload {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            args_redacted: self.governance.redact_args(&call.arguments),
            reason,
            sandbox_escalation: denied_host.is_none(),
            denied_host: denied_host.clone(),
        };
        self.journal
            .append_hitl_wait(self.session_id, &serde_json::to_value(&payload).unwrap())
            .await?;
        self.enter_waiting(
            WaitReason::Approval {
                request_id: payload.call_id.clone(),
                payload: payload.clone(),
            },
            TransitionReason::HitlWait,
        )
        .await?;
        self.events.push(TurnEvent {
            kind: "hitl_wait".into(),
            detail: payload.tool.clone(),
        });
        self.turn.push_evidence(
            EvidenceEntry::new(ExecutionEvent::WaitingForUser)
                .operation_id(call.id.clone())
                .tool_name(call.name.clone()),
        );
        let text = match denied_host {
            Some(host) => {
                format!("Sandbox blocked network to {host}; awaiting approval to allow that host")
            }
            None => format!(
                "Sandbox blocked {}; awaiting approval to run unconfined",
                call.name
            ),
        };
        Ok(ApplyOutcome::Hitl(ModelResponse {
            text,
            tool_calls: vec![call],
            usage: None,
            thinking: None,
        }))
    }
}
