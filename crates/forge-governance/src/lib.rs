//! Governance & sandbox (governance.md) — SEC-01, SEC-02, SEC-03. Phase 2 only.

mod acl;
mod audit;
mod sandbox;
mod secrets;

pub use acl::{AclPolicy, AclRule};
pub use audit::{AuditEvent, AuditLog};
pub use sandbox::{light_sandbox_exec, ExecRequest, ExecResult};
pub use secrets::{materialize_secrets, SecretMaterial, SecretRef};

use forge_types::{PolicyDecision, Principal, SideEffectClass, ToolCall, ToolDescriptor};

/// High-level governance facade for the tool path.
#[derive(Debug, Clone)]
pub struct Governance {
    pub principal: Principal,
    pub acl: AclPolicy,
    pub hitl_tools: Vec<String>,
    pub hitl_classes: Vec<SideEffectClass>,
    pub audit: AuditLog,
}

impl Default for Governance {
    fn default() -> Self {
        Self {
            principal: Principal::local_dev(),
            acl: AclPolicy::allow_all(),
            hitl_tools: vec!["bash".into()], // narrow default: bash may need HITL when flagged
            hitl_classes: vec![],
            audit: AuditLog::default(),
        }
    }
}

impl Governance {
    pub fn with_principal(mut self, p: Principal) -> Self {
        self.principal = p;
        self
    }

    pub fn with_acl(mut self, acl: AclPolicy) -> Self {
        self.acl = acl;
        self
    }

    pub fn require_hitl_for_tool(mut self, name: impl Into<String>) -> Self {
        self.hitl_tools.push(name.into());
        self
    }

    /// Filter tool list for the model (SEC-02).
    pub fn filter_tools(&self, tools: Vec<ToolDescriptor>) -> Vec<ToolDescriptor> {
        tools
            .into_iter()
            .filter(|t| {
                self.acl
                    .is_allowed(&self.principal, &t.name, t.side_effect_class)
            })
            .collect()
    }

    /// Authorize a tool call: allow / deny / hitl.
    pub fn authorize(&self, call: &ToolCall, class: SideEffectClass) -> PolicyDecision {
        if !self.acl.is_allowed(&self.principal, &call.name, class) {
            return PolicyDecision::Deny;
        }
        if self
            .hitl_tools
            .iter()
            .any(|t| t == &call.name || glob_match(t, &call.name))
            || self.hitl_classes.contains(&class)
        {
            // High-risk: bash with git push-ish args
            if call.name == "bash" {
                if let Some(cmd) = call.arguments.get("command").and_then(|c| c.as_str()) {
                    if cmd.contains("git push") || cmd.contains("rm -rf /") {
                        return PolicyDecision::Hitl;
                    }
                }
            } else {
                return PolicyDecision::Hitl;
            }
        }
        // Explicit hitl tools always
        if self.hitl_tools.iter().any(|t| t == &call.name) && call.name != "bash" {
            return PolicyDecision::Hitl;
        }
        PolicyDecision::Allow
    }

    pub fn redact_args(&self, args: &serde_json::Value) -> serde_json::Value {
        secrets::redact_value(args)
    }

    pub fn record_audit(&self, event: AuditEvent) {
        self.audit.push(event);
    }
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::ToolCall;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn acl_filters_tools() {
        let g = Governance::default().with_acl({
            let mut a = AclPolicy::new();
            a.allow("read_*".into());
            a.allow("read_file".into());
            a.deny("bash".into());
            a
        });
        let tools = vec![
            ToolDescriptor {
                name: "read_file".into(),
                description: "".into(),
                input_schema: json!({}),
                side_effect_class: SideEffectClass::Read,
                idempotent: true,
            },
            ToolDescriptor {
                name: "bash".into(),
                description: "".into(),
                input_schema: json!({}),
                side_effect_class: SideEffectClass::Exec,
                idempotent: false,
            },
        ];
        let visible = g.filter_tools(tools);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "read_file");
        assert_eq!(
            g.authorize(&call("bash", json!({})), SideEffectClass::Exec),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn hitl_on_git_push_bash() {
        let g = Governance::default();
        let d = g.authorize(
            &call("bash", json!({"command": "git push origin main"})),
            SideEffectClass::Exec,
        );
        assert_eq!(d, PolicyDecision::Hitl);
    }

    #[test]
    fn allow_safe_read() {
        let g = Governance::default();
        assert_eq!(
            g.authorize(
                &call("read_file", json!({"path": "a"})),
                SideEffectClass::Read
            ),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn redact_secrets_in_args() {
        let g = Governance::default();
        let red = g.redact_args(&json!({"token": "sk-secret", "path": "a"}));
        assert_eq!(red["token"], "[REDACTED]");
        assert_eq!(red["path"], "a");
    }

    #[test]
    fn secret_broker_env() {
        std::env::set_var("FORGE_TEST_DEMO", "value123");
        let m = materialize_secrets(&[SecretRef {
            name: "demo".into(),
            env_key: "FORGE_TEST_DEMO".into(),
        }])
        .unwrap();
        assert_eq!(m.get("demo"), Some("value123"));
        std::env::remove_var("FORGE_TEST_DEMO");
    }

    #[test]
    fn light_sandbox_runs() {
        let r = light_sandbox_exec(&ExecRequest {
            command: "echo hi".into(),
            cwd: std::env::current_dir().unwrap(),
            env: vec![],
        })
        .unwrap();
        assert!(r.stdout.contains("hi"));
        assert!(!r.is_error);
    }

    #[test]
    fn audit_log_records() {
        let g = Governance::default();
        g.record_audit(AuditEvent {
            session_id: "s".into(),
            principal: "p".into(),
            tool: "bash".into(),
            args_redacted: json!({}),
            decision: PolicyDecision::Allow,
            policy_id: "default".into(),
            result: "ok".into(),
            duration_ms: 1,
            trace_id: None,
        });
        assert_eq!(g.audit.len(), 1);
    }
}
