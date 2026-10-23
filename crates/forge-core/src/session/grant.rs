//! The operator's answer to a pending approval, applied in one place.
//!
//! Every frontend (TUI, supervisor, headless, CLI) used to map a menu choice
//! to its own sequence of `grant_egress_host`, session pattern allows,
//! personal-permissions writes and `resolve_hitl` calls. They now hand the
//! session a [`GrantDecision`]; the session checks it against the pending
//! payload, applies the grant it carries, and resolves the approval. Clients
//! keep only rendering and key handling.

use crate::*;

/// Why a [`GrantDecision`] does not fit the pending approval. Nothing was
/// granted or resolved when this is returned.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GrantRejection {
    /// A host grant on a prompt that did not name a denied host.
    #[error("this approval has no denied network host to allow")]
    NoDeniedHost,
    /// A one-off approval of a denied host would rerun into the same denial.
    #[error("a denied network host is allowed for the session or always, not once")]
    HostNeedsGrant,
    /// The call has no pattern that could ever match it again.
    #[error("this call has no session pattern to remember; use Run once or Don't run")]
    NoSessionPattern,
    /// Approve-all never covers leaving the sandbox.
    #[error("approve-all never approves running a command outside the sandbox")]
    ApproveAllEscalation,
}

/// The grant a resolved decision applied, for a frontend to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedGrant {
    /// Nothing beyond the decision itself (a denial or a one-off approval).
    None,
    /// `pattern` was permitted on the session's egress proxy.
    Host { pattern: String },
    /// `pattern` was remembered as a session allow rule.
    Pattern { pattern: String },
}

/// What [`AgentSession::apply_grant_decision`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantOutcome {
    pub decision: GrantDecision,
    pub grant: AppliedGrant,
    /// The rule written to the personal permissions file, when the decision
    /// asked for one. Present even when the write failed.
    pub persisted_rule: Option<String>,
    /// Why the personal permissions write failed. The session grant still
    /// applies; the rule just does not outlive the session.
    pub persist_error: Option<String>,
}

impl GrantOutcome {
    pub fn approved(&self) -> bool {
        self.decision.is_approval()
    }
}

/// Whether a remembered pattern could cover this call again. The redacted
/// display arguments are what a pattern is suggested from, so a call whose
/// arguments were redacted is never eligible.
pub fn pattern_grant_eligible(payload: &HitlPayload) -> bool {
    !contains_redaction_marker(&payload.args_redacted)
        && forge_governance::suggest_pattern(&payload_call(payload)).is_some()
}

/// Check `decision` against the menu each kind of approval offers.
pub fn validate_grant_decision(
    payload: &HitlPayload,
    decision: &GrantDecision,
) -> Result<(), GrantRejection> {
    let kind = payload.kind();
    match decision {
        GrantDecision::Deny | GrantDecision::DenyWithNote(_) => Ok(()),
        GrantDecision::AllowOnce if kind == ApprovalKind::NetworkHost => {
            Err(GrantRejection::HostNeedsGrant)
        }
        GrantDecision::AllowOnce => Ok(()),
        GrantDecision::AllowHostForSession | GrantDecision::AllowHostAlways => {
            if kind == ApprovalKind::NetworkHost {
                Ok(())
            } else {
                Err(GrantRejection::NoDeniedHost)
            }
        }
        GrantDecision::AllowPatternForSession | GrantDecision::AllowPatternAlways => {
            if kind == ApprovalKind::Governance && pattern_grant_eligible(payload) {
                Ok(())
            } else {
                Err(GrantRejection::NoSessionPattern)
            }
        }
        GrantDecision::ApproveAll if kind == ApprovalKind::SandboxEscalation => {
            Err(GrantRejection::ApproveAllEscalation)
        }
        GrantDecision::ApproveAll => Ok(()),
    }
}

/// The `ToolCall` a payload displays, for pattern suggestion and matching.
fn payload_call(payload: &HitlPayload) -> ToolCall {
    ToolCall {
        id: payload.call_id.clone(),
        name: payload.tool.clone(),
        arguments: payload.args_redacted.clone(),
    }
}

/// Whether `value` carries the governance redaction placeholder anywhere.
pub(crate) fn contains_redaction_marker(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s == "[REDACTED]",
        serde_json::Value::Array(items) => items.iter().any(contains_redaction_marker),
        serde_json::Value::Object(map) => map.values().any(contains_redaction_marker),
        _ => false,
    }
}

impl AgentSession {
    /// Validate `decision` against the pending approval and apply the grant it
    /// carries, without resolving the approval. Callers that resolve it
    /// themselves (the supervisor, which runs the approved call under its own
    /// cancellation scope) use this; others use [`Self::decide_hitl`].
    pub fn apply_grant_decision(
        &mut self,
        decision: &GrantDecision,
    ) -> Result<GrantOutcome, LoopError> {
        let payload = self
            .pending_hitl()
            .cloned()
            .ok_or(LoopError::NoPendingHitl)?;
        self.audit.set_resolving(None);
        validate_grant_decision(&payload, decision)
            .map_err(|rejection| LoopError::Other(rejection.to_string()))?;
        let call_id = payload.call_id.as_str();
        let mut outcome = GrantOutcome {
            decision: decision.clone(),
            grant: AppliedGrant::None,
            persisted_rule: None,
            persist_error: None,
        };
        match decision {
            GrantDecision::Deny | GrantDecision::DenyWithNote(_) | GrantDecision::AllowOnce => {}
            GrantDecision::ApproveAll => {
                if !self.set_approve_all(true) {
                    return Err(LoopError::Other(
                        "approve-all requires a trusted workspace".into(),
                    ));
                }
                self.audit_approve_all(call_id);
                if let Some(host) = payload.denied_host.as_deref() {
                    let pattern = forge_tools::egress::suggest_host_pattern(host);
                    self.grant_egress_host_for(&pattern, Some(call_id), "approve_all");
                    outcome.grant = AppliedGrant::Host { pattern };
                }
            }
            GrantDecision::AllowHostForSession | GrantDecision::AllowHostAlways => {
                let host = payload
                    .denied_host
                    .as_deref()
                    .expect("validated: a host grant has a denied host");
                let pattern = forge_tools::egress::suggest_host_pattern(host);
                if matches!(decision, GrantDecision::AllowHostAlways) {
                    let rule = forge_tools::egress::host_allow_rule(host);
                    outcome.persist_error = forge_config::append_user_allow_rule(&rule)
                        .err()
                        .map(|error| error.to_string());
                    self.audit_host_grant(
                        Some(call_id),
                        &pattern,
                        Some((&rule, outcome.persist_error.as_deref())),
                        "approval",
                    );
                    outcome.persisted_rule = Some(rule);
                }
                self.grant_egress_host_for(&pattern, Some(call_id), "approval");
                outcome.grant = AppliedGrant::Host { pattern };
            }
            GrantDecision::AllowPatternForSession | GrantDecision::AllowPatternAlways => {
                let pattern = self
                    .allow_suggested_pattern_for_session(&payload_call(&payload))
                    .ok_or_else(|| {
                        LoopError::Other(GrantRejection::NoSessionPattern.to_string())
                    })?;
                // The session grant is applied either way: a rule written to
                // the permissions file is not reloaded mid-session, so without
                // it an "always" grant would still prompt again for the next
                // matching call in this very session.
                if matches!(decision, GrantDecision::AllowPatternAlways) {
                    outcome.persist_error = forge_config::append_user_allow_rule(&pattern)
                        .err()
                        .map(|error| error.to_string());
                    self.audit_pattern_grant(
                        Some(call_id),
                        Some(&payload.tool),
                        &pattern,
                        Some((&pattern, outcome.persist_error.as_deref())),
                    );
                    outcome.persisted_rule = Some(pattern.clone());
                }
                outcome.grant = AppliedGrant::Pattern { pattern };
            }
        }
        self.audit.set_resolving(Some(decision.clone()));
        Ok(outcome)
    }

    /// Apply `decision` and resolve the pending approval, running an approved
    /// call to completion inline.
    pub async fn decide_hitl(
        &mut self,
        decision: GrantDecision,
        actor: &str,
    ) -> Result<GrantOutcome, LoopError> {
        let outcome = self.apply_grant_decision(&decision)?;
        let (hitl, note) = Self::hitl_resolution(&decision);
        self.resolve_hitl_with_feedback(hitl, actor, note).await?;
        Ok(outcome)
    }

    /// Apply `decision`; an approval is prepared but not executed, so the
    /// caller can run it off its event loop (see
    /// [`Self::prepare_approved_hitl`]). A denial is resolved here and
    /// returns no execution.
    pub async fn prepare_hitl_decision(
        &mut self,
        decision: GrantDecision,
        actor: &str,
    ) -> Result<(GrantOutcome, Option<PendingHitlExecution>), LoopError> {
        let outcome = self.apply_grant_decision(&decision)?;
        if !outcome.approved() {
            let (hitl, note) = Self::hitl_resolution(&decision);
            self.resolve_hitl_with_feedback(hitl, actor, note).await?;
            return Ok((outcome, None));
        }
        let pending = self.prepare_approved_hitl(actor).await?;
        Ok((outcome, pending))
    }

    fn hitl_resolution(decision: &GrantDecision) -> (HitlDecision, Option<&str>) {
        match decision {
            GrantDecision::Deny => (HitlDecision::Deny, None),
            GrantDecision::DenyWithNote(note) => (HitlDecision::Deny, Some(note.as_str())),
            _ => (HitlDecision::Approve, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(args: serde_json::Value) -> HitlPayload {
        HitlPayload {
            call_id: "c".into(),
            tool: "bash".into(),
            args_redacted: args,
            reason: "review".into(),
            failure: None,
            sandbox_escalation: false,
            denied_host: None,
        }
    }

    fn accepts(payload: &HitlPayload) -> Vec<&'static str> {
        [
            GrantDecision::Deny,
            GrantDecision::DenyWithNote("no".into()),
            GrantDecision::AllowOnce,
            GrantDecision::AllowPatternForSession,
            GrantDecision::AllowPatternAlways,
            GrantDecision::AllowHostForSession,
            GrantDecision::AllowHostAlways,
            GrantDecision::ApproveAll,
        ]
        .into_iter()
        .filter(|decision| validate_grant_decision(payload, decision).is_ok())
        .map(|decision| decision.as_str())
        .collect()
    }

    #[test]
    fn a_denied_host_accepts_host_grants_but_not_a_single_run() {
        let mut host = payload(serde_json::json!({"command": "gh pr create"}));
        host.denied_host = Some("api.github.com".into());
        // The escalation flag never widens a host prompt's menu.
        host.sandbox_escalation = true;
        assert_eq!(
            accepts(&host),
            [
                "deny",
                "deny_with_note",
                "allow_host_for_session",
                "allow_host_always",
                "approve_all"
            ]
        );
        assert_eq!(
            validate_grant_decision(&host, &GrantDecision::AllowOnce),
            Err(GrantRejection::HostNeedsGrant)
        );
    }

    #[test]
    fn a_sandbox_escalation_is_answered_for_this_command_only() {
        let mut escalation = payload(serde_json::json!({"command": "cargo build"}));
        escalation.sandbox_escalation = true;
        assert_eq!(
            accepts(&escalation),
            ["deny", "deny_with_note", "allow_once"]
        );
        assert_eq!(
            validate_grant_decision(&escalation, &GrantDecision::ApproveAll),
            Err(GrantRejection::ApproveAllEscalation)
        );
    }

    #[test]
    fn a_governance_prompt_offers_patterns_only_when_one_could_match_again() {
        let simple = payload(serde_json::json!({"command": "cargo test"}));
        assert_eq!(
            accepts(&simple),
            [
                "deny",
                "deny_with_note",
                "allow_once",
                "allow_pattern_for_session",
                "allow_pattern_always",
                "approve_all"
            ]
        );
        for args in [
            serde_json::json!({"command": "cargo test | tail"}),
            serde_json::json!({"command": "[REDACTED]"}),
        ] {
            let ineligible = payload(args);
            assert!(!pattern_grant_eligible(&ineligible));
            assert_eq!(
                validate_grant_decision(&ineligible, &GrantDecision::AllowPatternForSession),
                Err(GrantRejection::NoSessionPattern)
            );
        }
    }
}
