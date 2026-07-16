use forge_governance::AuditEvent;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SiemEncoding {
    #[default]
    JsonlOtlp,
    Cef,
}

pub struct SiemPlugin {
    path: PathBuf,
    encoding: SiemEncoding,
}

impl SiemPlugin {
    pub fn new(path: PathBuf, encoding: SiemEncoding) -> Self {
        Self { path, encoding }
    }

    pub fn export_audit(&self, events: &[AuditEvent]) -> std::io::Result<usize> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut n = 0;
        for e in events {
            let line = match self.encoding {
                SiemEncoding::JsonlOtlp => {
                    serde_json::to_string(&jsonl_otlp(e)).map_err(std::io::Error::other)?
                }
                SiemEncoding::Cef => cef_line(e),
            };
            writeln!(f, "{line}")?;
            n += 1;
        }
        Ok(n)
    }
}

#[derive(Serialize)]
struct OtlpLog<'a> {
    body: String,
    attributes: serde_json::Value,
    severity: &'a str,
}

fn jsonl_otlp(e: &AuditEvent) -> OtlpLog<'_> {
    OtlpLog {
        body: format!("forge.audit tool={} decision={:?}", e.tool, e.decision),
        attributes: serde_json::json!({
            "session_id": e.session_id,
            "principal": e.principal,
            "tool": e.tool,
            "args": e.args_redacted,
            "decision": format!("{:?}", e.decision),
            "policy_id": e.policy_id,
            "result": e.result,
            "duration_ms": e.duration_ms,
            "trace_id": e.trace_id,
        }),
        severity: "INFO",
    }
}

fn cef_line(e: &AuditEvent) -> String {
    // CEF:Version|Device Vendor|Device Product|Device Version|Signature ID|Name|Severity|Extension
    format!(
        "CEF:0|NorviaLabs|Forge|0.3|audit|tool_{}|3|suser={} cs1={} cs1Label=tool msg={}",
        e.tool,
        e.principal,
        e.tool,
        e.result.replace('|', "_")
    )
}

/// Trait alias for exporters.
pub trait SiemExporter {
    fn export_audit(&self, events: &[AuditEvent]) -> std::io::Result<usize>;
}

impl SiemExporter for SiemPlugin {
    fn export_audit(&self, events: &[AuditEvent]) -> std::io::Result<usize> {
        SiemPlugin::export_audit(self, events)
    }
}
