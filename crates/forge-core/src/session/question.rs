//! Pausing the turn for `ask_user_question` and resuming with answers.

use crate::session::tools::ToolExecutionStart;
use crate::*;
use forge_types::{AskUserQuestionArgs, AskUserQuestionResult, QuestionPayload, ToolOutput};

const JOURNAL_KIND: &str = "ask_user_question";

impl AgentSession {
    /// The questionnaire the active task is waiting on, if any.
    pub fn pending_question(&self) -> Option<&QuestionPayload> {
        match &self.active_task.wait_reason {
            Some(WaitReason::Question { payload, .. }) => Some(payload),
            _ => None,
        }
    }

    pub(crate) async fn dispatch_ask_user_question(
        &mut self,
        call: &ToolCall,
    ) -> Result<ToolExecutionStart, LoopError> {
        if self.cancel_token.is_some() {
            let output = ToolOutput::failed_exit(
                "ask_user_question can only be used by the root session. \
                 Include the unresolved question in your final result instead.",
                None,
            );
            self.journal
                .append_tool_intent(self.session_id, call)
                .await?;
            self.journal
                .append_tool_result(self.session_id, call, &output)
                .await?;
            self.remember_tool_result(call, &output);
            self.messages.push(Message::from_tool_output(call, &output));
            return Ok(ToolExecutionStart::Finished(None));
        }

        let args = match parse_question_args(&call.arguments) {
            Ok(args) => args,
            Err(message) => {
                let output = ToolOutput::failed_exit(message, None);
                self.journal
                    .append_tool_intent(self.session_id, call)
                    .await?;
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                self.messages.push(Message::from_tool_output(call, &output));
                return Ok(ToolExecutionStart::Finished(None));
            }
        };

        let payload = QuestionPayload {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            questions: args.questions,
        };
        let journaled = json!({
            "kind": JOURNAL_KIND,
            "call_id": payload.call_id,
            "tool": payload.tool,
            "questions": payload.questions,
        });
        self.journal
            .append_hitl_wait(self.session_id, &journaled)
            .await?;
        self.enter_waiting(
            WaitReason::Question {
                request_id: payload.call_id.clone(),
                payload: payload.clone(),
            },
            TransitionReason::HitlWait,
        )
        .await?;
        self.events.push(TurnEvent {
            kind: "question_wait".into(),
            detail: format!("{} question(s)", payload.questions.len()),
        });
        self.turn.push_evidence(
            EvidenceEntry::new(ExecutionEvent::WaitingForUser)
                .operation_id(call.id.clone())
                .tool_name(call.name.clone()),
        );
        Ok(ToolExecutionStart::Finished(Some(ModelResponse {
            text: format!(
                "Awaiting answers to {} question(s)",
                payload.questions.len()
            ),
            tool_calls: vec![call.clone()],
            usage: None,
            thinking: None,
        })))
    }

    /// Record the user's answers and resume the attempt. `None` means the
    /// user dismissed the questionnaire without answering.
    pub async fn resolve_question(
        &mut self,
        answers: Option<AskUserQuestionResult>,
        actor: &str,
    ) -> Result<(), LoopError> {
        let payload = self
            .pending_question()
            .cloned()
            .ok_or(LoopError::NoPendingQuestion)?;
        let (decision, output) = match answers {
            Some(result) => (
                "answer",
                ToolOutput::success(
                    serde_json::to_string(&result).unwrap_or_else(|_| "{\"answers\":[]}".into()),
                ),
            ),
            None => (
                "dismiss",
                ToolOutput::success("User dismissed the questions without answering."),
            ),
        };
        self.journal
            .append_hitl_resume(self.session_id, decision, actor)
            .await?;

        let call = ToolCall {
            id: payload.call_id.clone(),
            name: payload.tool.clone(),
            arguments: serde_json::to_value(&AskUserQuestionArgs {
                questions: payload.questions.clone(),
            })
            .unwrap_or_else(|_| json!({"questions": []})),
        };
        self.journal
            .append_tool_intent(self.session_id, &call)
            .await?;
        self.journal
            .append_tool_result(self.session_id, &call, &output)
            .await?;
        self.remember_tool_result(&call, &output);
        self.messages
            .push(Message::from_tool_output(&call, &output));
        self.events.push(TurnEvent {
            kind: "question_answered".into(),
            detail: decision.to_string(),
        });
        self.turn
            .evidence_mut()
            .0
            .retain(|e| e.event() != ExecutionEvent::WaitingForUser);
        self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
            .await?;
        Ok(())
    }
}

pub(crate) fn parse_question_args(
    arguments: &serde_json::Value,
) -> Result<AskUserQuestionArgs, String> {
    let args: AskUserQuestionArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid ask_user_question arguments: {error}"))?;
    args.normalize()
}

pub(crate) fn question_from_journal(value: &serde_json::Value) -> Option<QuestionPayload> {
    if value.get("kind").and_then(|kind| kind.as_str()) != Some(JOURNAL_KIND) {
        return None;
    }
    let call_id = value.get("call_id")?.as_str()?.to_string();
    let tool = value
        .get("tool")
        .and_then(|tool| tool.as_str())
        .unwrap_or(JOURNAL_KIND)
        .to_string();
    let questions = serde_json::from_value(value.get("questions")?.clone()).ok()?;
    Some(QuestionPayload {
        call_id,
        tool,
        questions,
    })
}
