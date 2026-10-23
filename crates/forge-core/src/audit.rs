//! Durable audit trail for a session's tool calls.
//!
//! Governance decisions are still recorded in the in-memory
//! [`forge_governance::AuditLog`] for its existing consumers. The same
//! decisions, plus approval resolutions, grants, tool results and gateway
//! egress events, are queued here and appended to the session's
//! forge-durable database at the next durable point (a tool result, an
//! approval wait or resolution, a model step, the end of a turn). Several
//! recorders run from `&self` paths, so the queue is behind a mutex; the
//! write itself happens on the session's async paths.

use std::sync::Mutex;

use forge_durable::{AuditEntry, AuditKind, AuditRow};
use forge_types::{GrantDecision, HitlPayload, PolicyDecision, ToolCall};
use serde_json::json;

use crate::{AgentSession, LoopError};

#[derive(Debug, Default)]
pub(crate) struct SessionAudit {
    pending: Mutex<Vec<AuditEntry>>,
    /// The typed decision being resolved, so the resolution row names it
    /// rather than the bare approve/deny it reduces to.
    resolving: Mutex<Option<GrantDecision>>,
}

impl SessionAudit {
    fn queue(&self, entry: AuditEntry) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
    }

    fn take(&self) -> Vec<AuditEntry> {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Put rows a failed write did not store back in front of anything queued
    /// since, so a retry keeps the original order.
    fn restore(&self, mut entries: Vec<AuditEntry>) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.append(&mut pending);
        *pending = entries;
    }

    pub(crate) fn set_resolving(&self, decision: Option<GrantDecision>) {
        *self
            .resolving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = decision;
    }

    fn take_resolving(&self) -> Option<GrantDecision> {
        self.resolving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

fn entry(
    kind: AuditKind,
    call_id: Option<&str>,
    tool: Option<&str>,
    detail: serde_json::Value,
) -> AuditEntry {
    AuditEntry {
        kind,
        call_id: call_id.map(str::to_string),
        tool: tool.map(str::to_string),
        detail,
    }
}

impl AgentSession {
    /// A governance decision. `args_redacted` must be the governance-redacted
    /// copy, never the raw arguments.
    pub(crate) fn audit_authorize(
        &self,
        call: &ToolCall,
        decision: PolicyDecision,
        policy_id: &str,
        args_redacted: &serde_json::Value,
    ) {
        self.audit.queue(entry(
            AuditKind::Authorize,
            Some(&call.id),
            Some(&call.name),
            json!({
                "decision": format!("{decision:?}"),
                "policy_id": policy_id,
                "args_redacted": args_redacted,
            }),
        ));
    }

    pub(crate) fn audit_hitl_request(&self, payload: &HitlPayload) {
        self.audit.queue(entry(
            AuditKind::HitlRequest,
            Some(&payload.call_id),
            Some(&payload.tool),
            json!({
                "kind": payload.kind(),
                "reason": payload.reason,
                "denied_host": payload.denied_host,
            }),
        ));
    }

    /// The resolution of `payload`. Names the typed decision when one is being
    /// resolved, otherwise the bare `approve`/`deny`.
    pub(crate) fn audit_hitl_resolution(
        &self,
        payload: &HitlPayload,
        bare: &str,
        actor: &str,
        note: Option<&str>,
    ) {
        let decision = self
            .audit
            .take_resolving()
            .map(|decision| decision.as_str())
            .unwrap_or(bare);
        self.audit.queue(entry(
            AuditKind::HitlResolution,
            Some(&payload.call_id),
            Some(&payload.tool),
            json!({ "decision": decision, "actor": actor, "note": note }),
        ));
    }

    /// A host permitted for the session (`rule` is `None`) or written to the
    /// personal permissions file.
    pub(crate) fn audit_host_grant(
        &self,
        call_id: Option<&str>,
        pattern: &str,
        persistent: Option<(&str, Option<&str>)>,
        source: &str,
    ) {
        self.audit.queue(entry(
            AuditKind::HostGrant,
            call_id,
            None,
            grant_detail(pattern, persistent, source),
        ));
    }

    pub(crate) fn audit_pattern_grant(
        &self,
        call_id: Option<&str>,
        tool: Option<&str>,
        pattern: &str,
        persistent: Option<(&str, Option<&str>)>,
    ) {
        self.audit.queue(entry(
            AuditKind::PatternGrant,
            call_id,
            tool,
            grant_detail(pattern, persistent, "approval"),
        ));
    }

    pub(crate) fn audit_approve_all(&self, call_id: &str) {
        self.audit.queue(entry(
            AuditKind::ApproveAll,
            Some(call_id),
            None,
            json!({ "on": true }),
        ));
    }

    pub(crate) fn audit_tool_result(
        &self,
        call: &ToolCall,
        result: &Result<forge_types::ToolOutput, forge_tools::ToolError>,
        duration_ms: u64,
        unconfined: bool,
    ) {
        let (outcome, exit_code, denied_host) = match result {
            Ok(output) if output.is_error => ("error", output.exit_code, None),
            Ok(output) => ("ok", output.exit_code, None),
            Err(forge_tools::ToolError::SandboxDenied { denied_host, .. }) => {
                ("sandbox_denied", None, denied_host.clone())
            }
            Err(forge_tools::ToolError::Validation(_)) => ("validation_error", None, None),
            Err(_) => ("execution_error", None, None),
        };
        self.audit.queue(entry(
            AuditKind::ToolResult,
            Some(&call.id),
            Some(&call.name),
            json!({
                "outcome": outcome,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "unconfined": unconfined,
                "denied_host": denied_host,
            }),
        ));
    }

    pub(crate) fn audit_egress_events(&self, events: &[forge_tools::egress::EgressEvent]) {
        use forge_tools::egress::EgressOutcome;
        for event in events {
            let (outcome, reason) = match &event.outcome {
                EgressOutcome::Allowed => ("allowed", None),
                EgressOutcome::Denied(denial) => ("denied", Some(format!("{denial:?}"))),
                EgressOutcome::UpstreamFailed(error) => ("upstream_failed", Some(error.clone())),
            };
            self.audit.queue(entry(
                AuditKind::Egress,
                event.label.as_deref(),
                None,
                json!({
                    "host": event.host,
                    "port": event.port,
                    "outcome": outcome,
                    "reason": reason,
                    "upstream": event.upstream.map(|addr| addr.to_string()),
                    "bytes_up": event.bytes_up,
                    "bytes_down": event.bytes_down,
                    "duration_ms": event.duration.as_millis() as u64,
                }),
            ));
        }
    }

    /// Append every queued audit row to the journal.
    pub(crate) async fn flush_audit(&self) -> Result<(), LoopError> {
        let entries = self.audit.take();
        if entries.is_empty() {
            return Ok(());
        }
        if let Err(error) = self.journal.append_audit(self.session_id, &entries).await {
            self.audit.restore(entries);
            return Err(error.into());
        }
        Ok(())
    }

    /// A turn boundary: take the gateway's events into the trail, then write
    /// it.
    pub(crate) async fn flush_audit_at_boundary(&self) -> Result<(), LoopError> {
        if let Some(runtime) = &self.egress {
            self.audit_egress_events(&runtime.drain_events());
        }
        self.flush_audit().await
    }

    /// This session's durable audit trail, oldest first. Rows still queued
    /// (and gateway events not yet drained) are written first.
    pub async fn audit_events(&self) -> Result<Vec<AuditRow>, LoopError> {
        self.flush_audit_at_boundary().await?;
        Ok(self.journal.audit_events(self.session_id).await?)
    }
}

fn grant_detail(
    pattern: &str,
    persistent: Option<(&str, Option<&str>)>,
    source: &str,
) -> serde_json::Value {
    match persistent {
        None => json!({ "pattern": pattern, "scope": "session", "source": source }),
        Some((rule, error)) => json!({
            "pattern": pattern,
            "scope": "persistent",
            "rule": rule,
            "error": error,
            "source": source,
        }),
    }
}
