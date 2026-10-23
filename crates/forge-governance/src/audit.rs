use forge_types::PolicyDecision;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};

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

/// Append-only audit trail. Clones share one underlying log, so an event
/// recorded through any copy (a forked session, a subagent's governance) is
/// visible from every other copy.
#[derive(Debug, Default, Clone)]
pub struct AuditLog {
    events: Arc<Mutex<Vec<AuditEvent>>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::PolicyDecision;
    use serde_json::json;

    fn event(tool: &str) -> AuditEvent {
        AuditEvent {
            session_id: "session-1".into(),
            principal: "local-dev".into(),
            tool: tool.into(),
            args_redacted: json!({"path": "file.txt"}),
            decision: PolicyDecision::Allow,
            policy_id: "policy-1".into(),
            result: "ok".into(),
            duration_ms: 42,
            trace_id: Some("trace-1".into()),
        }
    }

    #[test]
    fn audit_log_tracks_length_and_snapshot() {
        let log = AuditLog::default();
        assert!(log.is_empty());
        log.push(event("read_file"));
        assert_eq!(log.len(), 1);
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].tool, "read_file");
    }

    #[test]
    fn audit_log_clone_preserves_events() {
        let log = AuditLog::default();
        log.push(event("write_file"));
        let cloned = log.clone();
        assert_eq!(cloned.len(), 1);
        assert_eq!(cloned.snapshot()[0].tool, "write_file");
    }

    #[test]
    fn audit_log_clones_share_one_log() {
        let log = AuditLog::default();
        let cloned = log.clone();
        cloned.push(event("run_shell"));
        assert_eq!(log.len(), 1);
        assert_eq!(log.snapshot()[0].tool, "run_shell");
        log.push(event("read_file"));
        assert_eq!(cloned.len(), 2);
    }
}
