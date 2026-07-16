//! Fleet plugins (fleet-plugins.md) — FLEET-01. Phase 3 only.
//!
//! SCIM provisioning hooks + SIEM audit export without forking core.

mod scim;
mod siem;

pub use scim::{InMemoryScimStore, ScimGroup, ScimPlugin, ScimUser};
pub use siem::{SiemEncoding, SiemExporter, SiemPlugin};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FleetError {
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetConfig {
    #[serde(default)]
    pub scim_enabled: bool,
    #[serde(default)]
    pub siem_enabled: bool,
    #[serde(default)]
    pub siem_path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub siem_encoding: SiemEncoding,
}

/// Registry of fleet plugins loaded from config (no core fork).
pub struct FleetPluginRegistry {
    pub scim: Option<ScimPlugin>,
    pub siem: Option<SiemPlugin>,
}

impl FleetPluginRegistry {
    pub fn load(config: &FleetConfig) -> Result<Self, FleetError> {
        let scim = if config.scim_enabled {
            Some(ScimPlugin::new(InMemoryScimStore::default()))
        } else {
            None
        };
        let siem = if config.siem_enabled {
            let path = config
                .siem_path
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".forge/siem/audit.jsonl"));
            Some(SiemPlugin::new(path, config.siem_encoding))
        } else {
            None
        };
        Ok(Self { scim, siem })
    }

    pub fn list_plugins(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.scim.is_some() {
            v.push("scim");
        }
        if self.siem.is_some() {
            v.push("siem");
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_governance::{AuditEvent, AuditLog};
    use forge_types::PolicyDecision;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn load_plugins_from_config() {
        let dir = tempdir().unwrap();
        let reg = FleetPluginRegistry::load(&FleetConfig {
            scim_enabled: true,
            siem_enabled: true,
            siem_path: Some(dir.path().join("a.jsonl")),
            siem_encoding: SiemEncoding::JsonlOtlp,
        })
        .unwrap();
        assert_eq!(reg.list_plugins(), vec!["scim", "siem"]);
    }

    #[test]
    fn scim_provision_user_and_group() {
        let mut p = ScimPlugin::new(InMemoryScimStore::default());
        let u = p
            .create_user(ScimUser {
                id: "u1".into(),
                user_name: "alice".into(),
                active: true,
                roles: vec!["dev".into()],
            })
            .unwrap();
        assert_eq!(u.user_name, "alice");
        p.add_user_to_group("g1", "engineers", "u1").unwrap();
        let g = p.get_group("g1").unwrap();
        assert!(g.members.contains(&"u1".to_string()));
        p.deactivate_user("u1").unwrap();
        assert!(!p.get_user("u1").unwrap().active);
    }

    #[test]
    fn siem_exports_redacted_audit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("siem.jsonl");
        let p = SiemPlugin::new(path.clone(), SiemEncoding::JsonlOtlp);
        let log = AuditLog::default();
        log.push(AuditEvent {
            session_id: "s1".into(),
            principal: "p".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "echo hi", "token": "[REDACTED]"}),
            decision: PolicyDecision::Allow,
            policy_id: "default".into(),
            result: "ok".into(),
            duration_ms: 2,
            trace_id: Some("t1".into()),
        });
        let n = p.export_audit(&log.snapshot()).unwrap();
        assert_eq!(n, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bash"));
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("sk-"));
    }

    #[test]
    fn empty_config_loads_no_plugins() {
        let reg = FleetPluginRegistry::load(&FleetConfig::default()).unwrap();
        assert!(reg.list_plugins().is_empty());
    }
}
