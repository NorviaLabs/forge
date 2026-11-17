//! Where a session runs its shell commands and edits its files.

use serde::{Deserialize, Serialize};

/// A session's execution target, saved with the session so resuming
/// reconnects to the same place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionTarget {
    /// This machine, under Forge's sandbox: today's behaviour.
    #[default]
    Local,
    /// A host from the user's SSH config, and a folder on it.
    Ssh {
        /// The `Host` alias from `~/.ssh/config`.
        host: String,
        /// The remote workspace: an absolute path on `host`.
        path: String,
        /// Ask before each command (the default): remote commands run outside
        /// Forge's sandbox.
        #[serde(default = "ask_by_default")]
        ask: bool,
    },
}

fn ask_by_default() -> bool {
    true
}

impl ExecutionTarget {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// The SSH host alias, for a remote target.
    pub fn host(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Ssh { host, .. } => Some(host),
        }
    }

    /// Whether remote commands need approval.
    pub fn asks_before_commands(&self) -> bool {
        match self {
            Self::Local => false,
            Self::Ssh { ask, .. } => *ask,
        }
    }

    /// `this Mac` or `devbox:/srv/app`.
    pub fn label(&self) -> String {
        match self {
            Self::Local => "this Mac".into(),
            Self::Ssh { host, path, .. } => format!("{host}:{path}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ssh() -> ExecutionTarget {
        ExecutionTarget::Ssh {
            host: "devbox".into(),
            path: "/srv/app".into(),
            ask: true,
        }
    }

    #[test]
    fn local_is_the_default_and_needs_no_fields() {
        assert_eq!(ExecutionTarget::default(), ExecutionTarget::Local);
        assert_eq!(
            serde_json::to_value(ExecutionTarget::Local).unwrap(),
            json!({"kind": "local"})
        );
        assert!(ExecutionTarget::Local.is_local());
        assert_eq!(ExecutionTarget::Local.host(), None);
        assert!(!ExecutionTarget::Local.asks_before_commands());
        assert_eq!(ExecutionTarget::Local.label(), "this Mac");
    }

    #[test]
    fn an_ssh_target_round_trips_and_asks_by_default() {
        let value = serde_json::to_value(ssh()).unwrap();
        assert_eq!(
            value,
            json!({"kind": "ssh", "host": "devbox", "path": "/srv/app", "ask": true})
        );
        assert_eq!(
            serde_json::from_value::<ExecutionTarget>(value).unwrap(),
            ssh()
        );
        let without_ask: ExecutionTarget =
            serde_json::from_value(json!({"kind": "ssh", "host": "devbox", "path": "/srv/app"}))
                .unwrap();
        assert!(without_ask.asks_before_commands(), "asking is the default");
        assert!(!ssh().is_local());
        assert_eq!(ssh().host(), Some("devbox"));
        assert_eq!(ssh().label(), "devbox:/srv/app");
        let trusted = ExecutionTarget::Ssh {
            host: "vm".into(),
            path: "/w".into(),
            ask: false,
        };
        assert!(!trusted.asks_before_commands());
    }

    #[test]
    fn an_approval_names_the_host_it_would_run_on_and_old_ones_still_load() {
        let old = json!({
            "call_id": "c", "tool": "bash", "args_redacted": {"command": "ls"},
            "reason": "policy requires human approval"
        });
        let payload: crate::HitlPayload = serde_json::from_value(old).unwrap();
        assert_eq!(payload.target, None, "journals written before targets load");
        let mut remote = payload.clone();
        remote.target = Some("devbox".into());
        let value = serde_json::to_value(&remote).unwrap();
        assert_eq!(value["target"], "devbox");
        assert!(
            serde_json::to_value(&payload)
                .unwrap()
                .get("target")
                .is_none(),
            "a local payload serializes as before"
        );
    }
}
