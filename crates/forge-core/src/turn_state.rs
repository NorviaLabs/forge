//! Runtime bookkeeping scoped to one user turn.
//!
//! This state is intentionally separate from the session transcript, durable
//! lifecycle, and background-task registries. A new user message resets it;
//! resume reconstructs it as empty because it is not persisted independently.

use forge_tools::ValidationBudget;
use forge_types::ToolCall;

use crate::{EvidenceEntry, ExecutionEvidence};

pub(crate) struct TurnState {
    validation_budget: ValidationBudget,
    calls: Vec<ToolCall>,
    evidence: ExecutionEvidence,
    consecutive_hitl_denials: u32,
    /// Calls that have already taken their one automatic unconfined retry
    /// after a sandbox denial (see `claim_auto_unconfined_retry`).
    auto_unconfined_retries: std::collections::HashSet<String>,
}

impl TurnState {
    pub(crate) fn new() -> Self {
        Self {
            validation_budget: ValidationBudget::with_default_max(),
            calls: Vec::new(),
            evidence: ExecutionEvidence::new(),
            consecutive_hitl_denials: 0,
            auto_unconfined_retries: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.validation_budget = ValidationBudget::with_default_max();
        self.calls.clear();
        self.evidence = ExecutionEvidence::new();
        self.consecutive_hitl_denials = 0;
        self.auto_unconfined_retries.clear();
    }

    /// Whether `call_id` may take an automatic unconfined retry, recording
    /// that it has. Returns `false` on every later ask for the same call.
    ///
    /// A retry outside the sandbox should not be able to raise the sandbox
    /// denial that triggered it, but "should not" is not a termination
    /// argument: without this, a tool that reports `SandboxDenied`
    /// unconditionally would be re-run forever, since the allow rule that
    /// authorised the first retry still authorises the next. One retry, then
    /// the denial is reported like any other tool failure.
    pub(crate) fn claim_auto_unconfined_retry(&mut self, call_id: &str) -> bool {
        self.auto_unconfined_retries.insert(call_id.to_owned())
    }

    pub(crate) fn calls(&self) -> &[ToolCall] {
        &self.calls
    }

    pub(crate) fn record_call(&mut self, call: ToolCall) {
        self.calls.push(call);
    }

    pub(crate) fn evidence(&self) -> &ExecutionEvidence {
        &self.evidence
    }

    pub(crate) fn evidence_mut(&mut self) -> &mut ExecutionEvidence {
        &mut self.evidence
    }

    pub(crate) fn push_evidence(&mut self, entry: EvidenceEntry) {
        self.evidence.push(entry);
    }

    pub(crate) fn take_validation_budget(&mut self) -> ValidationBudget {
        std::mem::take(&mut self.validation_budget)
    }

    pub(crate) fn restore_validation_budget(&mut self, budget: ValidationBudget) {
        self.validation_budget = budget;
    }

    pub(crate) fn reset_hitl_denials(&mut self) {
        self.consecutive_hitl_denials = 0;
    }

    pub(crate) fn record_hitl_denial(&mut self) -> u32 {
        self.consecutive_hitl_denials = self.consecutive_hitl_denials.saturating_add(1);
        self.consecutive_hitl_denials
    }
}

#[cfg(test)]
mod tests {
    use super::TurnState;
    use forge_types::ToolCall;
    use serde_json::json;

    #[test]
    fn reset_discards_turn_local_calls_and_evidence() {
        let mut state = TurnState::new();
        state.record_call(ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "README.md"}),
        });
        state.push_evidence(crate::EvidenceEntry::new(
            crate::ExecutionEvent::AssistantResponseProduced,
        ));
        assert_eq!(state.calls().len(), 1);
        assert_eq!(state.evidence().0.len(), 1);

        state.reset();

        assert!(state.calls().is_empty());
        assert!(state.evidence().0.is_empty());
    }
}
