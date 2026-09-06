//! Runtime bookkeeping scoped to one user turn.
//!
//! This state is intentionally separate from the session transcript, durable
//! lifecycle, and background-task registries. A new user message resets it;
//! resume reconstructs it as empty because it is not persisted independently.

use forge_tools::ValidationBudget;
use forge_types::ToolCall;
use std::collections::{HashMap, HashSet};

use crate::{EvidenceEntry, ExecutionEvidence};

pub(crate) struct TurnState {
    validation_budget: ValidationBudget,
    calls: Vec<ToolCall>,
    evidence: ExecutionEvidence,
    consecutive_hitl_denials: u32,
    failed_confined_bash: HashMap<String, ToolCall>,
    failed_unconfined_call_shapes: HashSet<String>,
    pub(crate) approved_retry_calls: HashMap<String, ToolCall>,
    pub(crate) retry_requests: HashMap<String, ToolCall>,
}

impl TurnState {
    pub(crate) fn new() -> Self {
        Self {
            validation_budget: ValidationBudget::with_default_max(),
            calls: Vec::new(),
            evidence: ExecutionEvidence::new(),
            consecutive_hitl_denials: 0,
            failed_confined_bash: HashMap::new(),
            failed_unconfined_call_shapes: HashSet::new(),
            approved_retry_calls: HashMap::new(),
            retry_requests: HashMap::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.validation_budget = ValidationBudget::with_default_max();
        self.calls.clear();
        self.evidence = ExecutionEvidence::new();
        self.consecutive_hitl_denials = 0;
        self.failed_confined_bash.clear();
        self.failed_unconfined_call_shapes.clear();
        self.approved_retry_calls.clear();
        self.retry_requests.clear();
    }

    pub(crate) fn calls(&self) -> &[ToolCall] {
        &self.calls
    }

    pub(crate) fn record_call(&mut self, call: ToolCall) {
        self.calls.push(call);
    }

    pub(crate) fn record_failed_confined_bash(&mut self, call: ToolCall) {
        if call.name == "bash" && !self.failed_unconfined_call_matches(&call) {
            self.failed_confined_bash.insert(call.id.clone(), call);
        }
    }

    pub(crate) fn take_failed_confined_bash(&mut self, call_id: &str) -> Option<ToolCall> {
        self.failed_confined_bash.remove(call_id)
    }

    pub(crate) fn record_failed_unconfined_call(&mut self, call: &ToolCall) {
        self.failed_confined_bash
            .retain(|_, candidate| call_shape(candidate) != call_shape(call));
        self.failed_unconfined_call_shapes.insert(call_shape(call));
    }

    pub(crate) fn failed_unconfined_call_matches(&self, call: &ToolCall) -> bool {
        self.failed_unconfined_call_shapes
            .contains(&call_shape(call))
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

fn call_shape(call: &ToolCall) -> String {
    format!("{}:{}", call.name, call.arguments)
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

    #[test]
    fn failed_unconfined_shape_invalidates_old_and_new_retry_references_until_reset() {
        let mut state = TurnState::new();
        let mut call = ToolCall {
            id: "first".into(),
            name: "bash".into(),
            arguments: json!({"command": "open fixture.html"}),
        };
        state.record_failed_confined_bash(call.clone());
        state.record_failed_unconfined_call(&call);
        assert!(state.take_failed_confined_bash("first").is_none());
        call.id = "second".into();
        state.record_failed_confined_bash(call.clone());
        assert!(state.take_failed_confined_bash("second").is_none());
        assert!(state.failed_unconfined_call_matches(&call));
        let mut different = call.clone();
        different.arguments = json!({"command": "open different.html"});
        assert!(!state.failed_unconfined_call_matches(&different));
        state.reset();
        assert!(!state.failed_unconfined_call_matches(&call));
        state.record_failed_confined_bash(call);
        assert!(state.take_failed_confined_bash("second").is_some());
    }
}
