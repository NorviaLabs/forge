//! Session resume: journaled tool replay and incomplete-intent reconciliation.

use forge_durable::ToolResultPayload;
use forge_tools::{ToolError, ValidationBudget};
use forge_types::{ExecutionOutcome, Message, MessageRole, ToolCall, ToolOutput};

use crate::{
    compress_recognized_command_output, tool_validation_failed_content, AgentSession, LoopError,
    TurnEvent,
};

const INTERRUPTED_TOOL_MSG: &str =
    "Tool execution was interrupted before a result was recorded. Forge did not re-run this tool because it is not marked idempotent.";

impl AgentSession {
    /// Finish tool intents that were journaled but never received a result.
    pub(crate) async fn reconcile_incomplete_intents(
        &mut self,
        incomplete: &[String],
    ) -> Result<(), LoopError> {
        if incomplete.is_empty() {
            return Ok(());
        }
        let mut budget = ValidationBudget::with_default_max();
        for call_id in incomplete {
            if self.journaled_tool_results.contains_key(call_id) {
                continue;
            }
            let Some(call) = self.find_tool_call(call_id) else {
                warn_orphan_intent(call_id);
                continue;
            };
            let idempotent = self
                .tools
                .get(&call.name)
                .map(|tool| tool.idempotent())
                .unwrap_or(false);
            if idempotent {
                self.complete_journaled_intent(&call, &mut budget).await?;
            } else {
                self.fail_interrupted_intent(&call).await?;
            }
        }
        Ok(())
    }

    /// Serve a previously journaled tool result instead of executing again.
    pub(crate) async fn try_serve_journaled_tool(
        &mut self,
        call: &ToolCall,
    ) -> Result<bool, LoopError> {
        let Some(cached) = self.journaled_tool_results.get(&call.id).cloned() else {
            return Ok(false);
        };
        self.turn.record_call(call.clone());
        if !self.has_tool_message(&call.id) {
            self.messages
                .push(Message::from_tool_output(call, &cached.output));
            self.events.push(TurnEvent {
                kind: "tool_replay".into(),
                detail: format!("{} (journaled)", call.name),
            });
        }
        Ok(true)
    }

    pub(crate) fn remember_tool_result(&mut self, call: &ToolCall, output: &ToolOutput) {
        self.journaled_tool_results.insert(
            call.id.clone(),
            ToolResultPayload {
                call_id: call.id.clone(),
                name: call.name.clone(),
                output: output.clone(),
            },
        );
    }

    fn find_tool_call(&self, call_id: &str) -> Option<ToolCall> {
        self.messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .flat_map(|message| message.tool_calls.iter())
            .find(|call| call.id == call_id)
            .cloned()
    }

    fn has_tool_message(&self, call_id: &str) -> bool {
        self.messages.iter().any(|message| {
            message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id)
        })
    }

    async fn fail_interrupted_intent(&mut self, call: &ToolCall) -> Result<(), LoopError> {
        let output =
            ToolOutput::spawn_failed(INTERRUPTED_TOOL_MSG, "interrupted before result recorded");
        self.journal
            .append_tool_result(self.session_id, call, &output)
            .await?;
        self.remember_tool_result(call, &output);
        if !self.has_tool_message(&call.id) {
            self.messages.push(Message {
                outcome: output.effective_outcome(),
                role: MessageRole::Tool,
                content: output.content.clone(),
                tool_call_id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            });
        }
        self.events.push(TurnEvent {
            kind: "tool_interrupted".into(),
            detail: call.name.clone(),
        });
        Ok(())
    }

    /// Execute a tool whose intent was already journaled before a crash.
    async fn complete_journaled_intent(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<(), LoopError> {
        if self.try_serve_journaled_tool(call).await? {
            return Ok(());
        }
        self.turn.record_call(call.clone());
        let (pre_edit, pre_git) = self.pre_tool_state(call).await;
        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(mut output) => {
                Self::backfill_tool_outcome(&mut output);
                self.push_success_evidence(call, pre_edit, pre_git, &output)
                    .await;
                if self.enable_context {
                    output.content = compress_recognized_command_output(call, output.content);
                    output.content = self.context.maybe_offload_tool_content(output.content)?;
                }
                self.freeze_tool_output(&mut output);
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                if !self.has_tool_message(&call.id) {
                    self.messages.push(Message {
                        outcome: output.effective_outcome(),
                        role: MessageRole::Tool,
                        content: output.content.clone(),
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        thinking: None,
                        thinking_duration_secs: None,
                        tool_calls: vec![],
                        attachments: Vec::new(),
                    });
                }
                self.events.push(TurnEvent {
                    kind: "tool".into(),
                    detail: format!("{} -> {} chars (resumed)", call.name, output.content.len()),
                });
            }
            Err(ToolError::Validation(ve)) => {
                let msg = tool_validation_failed_content(&ve);
                self.journal
                    .append_validation_failed(self.session_id, &call.id, &call.name, &msg)
                    .await?;
                if !self.has_tool_message(&call.id) {
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
                }
                self.events.push(TurnEvent {
                    kind: "validation".into(),
                    detail: msg,
                });
            }
            Err(error) => {
                let outcome = error.as_outcome();
                let output = ToolOutput {
                    outcome: Some(outcome),
                    content: error.to_string(),
                    is_error: true,
                    exit_code: None,
                    attachments: Vec::new(),
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                if !self.has_tool_message(&call.id) {
                    self.messages.push(Message {
                        outcome: output.effective_outcome(),
                        role: MessageRole::Tool,
                        content: output.content.clone(),
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        thinking: None,
                        thinking_duration_secs: None,
                        tool_calls: vec![],
                        attachments: Vec::new(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn warn_orphan_intent(call_id: &str) {
    tracing::warn!(
        call_id = %call_id,
        "incomplete tool intent on resume with no matching assistant call"
    );
}
