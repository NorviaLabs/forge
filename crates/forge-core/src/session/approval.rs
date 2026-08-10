//! Resolving a human-in-the-loop approval, then executing or denying
//! the call it was blocking.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

impl AgentSession {
    /// How many HITL denials in a row within one user turn are tolerated
    /// before the turn is stopped outright (see `consecutive_hitl_denials`).
    const MAX_CONSECUTIVE_HITL_DENIALS: u32 = 2;

    /// DUR-03: resolve pending HITL then optionally execute the tool.
    pub async fn resolve_hitl(
        &mut self,
        decision: HitlDecision,
        actor: &str,
    ) -> Result<(), LoopError> {
        self.resolve_hitl_with_feedback(decision, actor, None).await
    }

    /// Same as [`Self::resolve_hitl`], but a `Deny` can carry a short
    /// message that reaches the agent as the tool result content — context
    /// for what to do differently, folded into this same turn rather than
    /// a bare denial the operator has to re-explain next turn. Modeled on
    /// opencode's `CorrectedError`. Ignored for `Approve`.
    pub async fn resolve_hitl_with_feedback(
        &mut self,
        decision: HitlDecision,
        actor: &str,
        feedback: Option<&str>,
    ) -> Result<(), LoopError> {
        let payload = self
            .pending_hitl()
            .cloned()
            .ok_or(LoopError::NoPendingHitl)?;
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
            let feedback = feedback.map(str::trim).filter(|f| !f.is_empty());
            let output = ToolOutput::denied(match feedback {
                Some(feedback) => format!("HITL denied by {actor}: {feedback}"),
                None => format!("HITL denied by {actor}"),
            });
            let call = ToolCall {
                id: payload.call_id.clone(),
                name: payload.tool.clone(),
                arguments: payload.args_redacted.clone(),
            };
            self.turn.record_call(call.clone());
            self.push_denied_evidence(&call, &output.content);
            self.journal
                .append_tool_intent(self.session_id, &call)
                .await?;
            self.journal
                .append_tool_result(self.session_id, &call, &output)
                .await?;
            self.remember_tool_result(&call, &output);
            self.messages.push(Message {
                outcome: output.effective_outcome(),
                role: MessageRole::Tool,
                content: output.content,
                tool_call_id: Some(payload.call_id),
                name: Some(payload.tool),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            });
            // Stale evidence from the paused call must not leak into a later
            // completion decision within this same turn (see `apply_model_response`).
            self.turn
                .evidence_mut()
                .0
                .retain(|e| e.event() != ExecutionEvent::WaitingForUser);

            self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
                .await?;

            if self.turn.record_hitl_denial() >= Self::MAX_CONSECUTIVE_HITL_DENIALS {
                // A denial is a strong signal the user does not want this
                // approach pursued at all. Without this, the model would
                // keep autonomously searching for a workaround for up to
                // `max_turns` (128 by default) model steps before yielding
                // control back — expensive, slow, and surprising for what
                // was a single "no". Stop the turn now instead.
                self.finalize_turn_failure(
                    "Forge stopped after repeated denied approvals for this turn.",
                    "hitl_denied",
                )
                .await?;
            }
            return Ok(());
        }

        self.turn.reset_hitl_denials();
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
                self.turn
                    .evidence_mut()
                    .0
                    .retain(|e| e.event() != ExecutionEvent::WaitingForUser);
                self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
                    .await?;
                return Err(LoopError::Other(
                    "policy denies tool after HITL approve".into(),
                ));
            }
        }

        // Restore args from pending — we only have redacted; for tests use redacted as args
        let mut budget = ValidationBudget::with_default_max();
        self.turn
            .evidence_mut()
            .0
            .retain(|e| e.event() != ExecutionEvent::WaitingForUser);
        self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
            .await?;
        // Execute with stored args (may be redacted in production; Phase 2 keeps full call in journal intent before wait ideally)
        // Re-fetch from last HitlWait — for approve path re-execute with redacted args is weak;
        // store original args in pending for this implementation:
        self.run_one_tool_exec_only(&call, &mut budget).await?;
        Ok(())
    }
}
