use forge_types::PolicyDecision;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub session_id: String,
    pub principal: String,
    pub tool: String,
    pub args_redacted: Value,
    pub decision: PolicyDecision,
    pub policy_id: String,
    pub result: String,
    pub duration_ms: u64,
    pub trace_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    events: Mutex<Vec<AuditEvent>>,
}

impl Clone for AuditLog {
    fn clone(&self) -> Self {
        let events = self.events.lock().unwrap().clone();
        Self {
            events: Mutex::new(events),
        }
    }
}

impl AuditLog {
    pub fn push(&self, e: AuditEvent) {
        self.events.lock().unwrap().push(e);
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.events.lock().unwrap().clone()
    }
}
