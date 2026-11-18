//! The decisions a pending approval offers, shared by every frontend.
//!
//! The backend, not a page or a remote device, decides which grants exist for
//! a request. The desktop's approval card and the remote-control API both
//! read them from here, so the two can never disagree about what a request
//! allows or which options need a second, privileged confirmation.

use forge_types::{ApprovalKind, HitlPayload};
use serde::Serialize;

/// Approvals whose every affirmative answer needs the native confirmation
/// (AC-4.12): running unconfined, or opening a network host.
pub fn requires_native_confirm(payload: &HitlPayload) -> bool {
    matches!(
        payload.kind(),
        ApprovalKind::SandboxEscalation | ApprovalKind::NetworkHost | ApprovalKind::ScreenControl
    )
}

/// One decision the approval card offers. The backend, not the page, decides
/// which grants exist for a request; the page only names one.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApprovalOption {
    /// The `GrantDecision` name sent back to `decide_approval`.
    pub decision: String,
    pub label: String,
    /// The rule this would remember, when the option grants one.
    pub detail: String,
    pub help: String,
    /// Needs the card's second confirmation step: it runs unconfined or opens
    /// a host.
    pub privileged: bool,
}

fn option(
    decision: &str,
    label: String,
    detail: &str,
    help: &str,
    privileged: bool,
) -> ApprovalOption {
    ApprovalOption {
        decision: decision.into(),
        label,
        detail: detail.into(),
        help: help.into(),
        privileged,
    }
}

/// The grants this request can carry, in the order the card shows them.
///
/// Mirrors the terminal's menu: a denied host is granted for the session at
/// least (running it once would only meet the same denial), a sandbox
/// escalation always runs outside the sandbox, and approve-all is never
/// offered for one.
pub fn approval_options(payload: &HitlPayload) -> Vec<ApprovalOption> {
    let mut options = Vec::new();
    match payload.kind() {
        ApprovalKind::NetworkHost => {
            let host = payload.denied_host.as_deref().unwrap_or("this host");
            options.push(option(
                "allow_host_for_session",
                format!("Allow {host} this session"),
                &format!("host({host})"),
                "The sandbox stays on; the call reruns confined. You will be asked again next session.",
                true,
            ));
            options.push(option(
                "allow_host_always",
                format!("Always allow {host}"),
                &format!("host({host})"),
                "Writes the rule to your personal permissions file. Kept next session.",
                true,
            ));
        }
        ApprovalKind::SandboxEscalation => {
            options.push(option(
                "allow_once",
                "Approve command".into(),
                "",
                "Retries this command outside the filesystem sandbox. That can run destructive commands.",
                true,
            ));
            if let Some(dir) = payload
                .denied_path
                .as_deref()
                .and_then(|path| forge_tools::write_grants::grant_for_denied_path(path).ok())
            {
                let dir = dir.display().to_string();
                // Widening by one named directory comes first: the command
                // stays in the sandbox.
                options.insert(
                    0,
                    option(
                        "allow_path_for_session",
                        format!("Allow writing {dir} this session"),
                        &format!("write {dir}"),
                        "The sandbox stays on; the command reruns inside it with this one \
                         directory writable. You will be asked again next session.",
                        true,
                    ),
                );
                options.insert(
                    1,
                    option(
                        "allow_path_always",
                        format!("Always allow writing {dir} in this project"),
                        &format!("write {dir}"),
                        "The sandbox stays on. Saved for this project, outside the workspace.",
                        true,
                    ),
                );
            }
            if let Some(program) = forge_core::trustable_program(payload) {
                options.push(option(
                    "trust_program",
                    format!("Always run {program} outside the sandbox in this project"),
                    &format!("trusted program {program}"),
                    &format!(
                        "A full escape: every single {program} command in this project runs \
                         unconfined without asking, including ones you have not seen. Saved \
                         for this project."
                    ),
                    true,
                ));
            }
        }
        ApprovalKind::ScreenControl => {
            options.push(option(
                "allow_consent_for_session",
                "Allow for this turn".into(),
                "screen control",
                "The agent sees your screen and uses your mouse and keyboard until this turn \
                 ends, you press Stop, or you move the mouse. The next turn asks again.",
                true,
            ));
            options.push(option(
                "deny",
                "Don't allow".into(),
                "",
                "The agent is told it may not use the screen.",
                false,
            ));
            options.push(option(
                "deny_with_note",
                "Don't allow, and say why".into(),
                "",
                "The note reaches the agent as the tool result.",
                false,
            ));
            return options;
        }
        ApprovalKind::FormSubmit => {
            let site = payload
                .consent
                .as_ref()
                .map(|consent| escape_invisible(&consent.scope))
                .unwrap_or_else(|| "this site".into());
            options.push(option(
                "allow_once",
                "Submit once".into(),
                "",
                "Submits this form. The next submit asks again.",
                false,
            ));
            options.push(option(
                "allow_consent_for_session",
                format!("Allow submits on {site} this session"),
                &format!("form submits on {site}"),
                "Forms on this site are submitted without asking until the session ends.",
                false,
            ));
            options.push(option(
                "deny",
                "Don't submit".into(),
                "",
                "The agent is told the form was not submitted.",
                false,
            ));
            options.push(option(
                "deny_with_note",
                "Don't submit, and say why".into(),
                "",
                "The note reaches the agent as the tool result.",
                false,
            ));
            return options;
        }
        // A command on an SSH host: nothing there is sandboxed, and a
        // personal rule is about this machine, so no "always".
        ApprovalKind::Governance if payload.target.is_some() => {
            options.push(option(
                "allow_once",
                "Run once".into(),
                "",
                "Runs now. You will be asked again.",
                false,
            ));
            options.push(option(
                "allow_pattern_for_session",
                "Allow this kind of call this session".into(),
                "",
                "Calls like this one run without asking until the session ends.",
                false,
            ));
            options.push(option(
                "approve_all",
                "Trust all commands for this session".into(),
                "",
                "Lasts until the session ends; turn it off from the Session menu.",
                // The host's only boundary is this prompt: giving it up for
                // the session takes the card's second confirmation.
                true,
            ));
        }
        ApprovalKind::Governance => {
            options.push(option(
                "allow_once",
                "Run once".into(),
                "",
                "Runs now. You will be asked again.",
                false,
            ));
            options.push(option(
                "allow_pattern_for_session",
                "Allow this kind of call this session".into(),
                "",
                "The sandbox stays on. You will be asked again next session.",
                false,
            ));
            options.push(option(
                "allow_pattern_always",
                "Always allow this kind of call".into(),
                "",
                "Writes the rule to your personal permissions file. Kept next session.",
                false,
            ));
            options.push(option(
                "approve_all",
                "Trust all commands for this session".into(),
                "",
                "Approves review prompts until the session ends without asking. The sandbox stays on: what needs to leave it fails.",
                false,
            ));
        }
    }
    options.push(option(
        "deny",
        "Don't run".into(),
        "",
        "The agent is told the call was denied.",
        false,
    ));
    options.push(option(
        "deny_with_note",
        "Don't run, and say why".into(),
        "",
        "The note reaches the agent as the tool result.",
        false,
    ));
    options
}

/// Make characters that can disguise what a command does visible:
/// bidi controls, zero-width characters and the byte-order mark become
/// `⟨U+XXXX⟩`.
pub fn escape_invisible(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}' => format!("⟨U+{:04X}⟩", c as u32),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(sandbox_escalation: bool, denied_host: Option<&str>) -> HitlPayload {
        HitlPayload {
            call_id: "call-1".into(),
            tool: "bash".into(),
            args_redacted: serde_json::json!({"command": "curl example.com"}),
            reason: "needs approval".into(),
            failure: None,
            sandbox_escalation,
            denied_host: denied_host.map(str::to_string),
            denied_path: None,
            target: None,
            consent: None,
        }
    }

    fn decisions(options: &[ApprovalOption]) -> Vec<&str> {
        options.iter().map(|o| o.decision.as_str()).collect()
    }

    #[test]
    fn a_governance_request_offers_every_ordinary_grant_and_none_is_privileged() {
        let options = approval_options(&payload(false, None));
        assert_eq!(
            decisions(&options),
            [
                "allow_once",
                "allow_pattern_for_session",
                "allow_pattern_always",
                "approve_all",
                "deny",
                "deny_with_note"
            ]
        );
        assert!(options.iter().all(|option| !option.privileged));
    }

    #[test]
    fn a_sandbox_escalation_runs_once_only_with_a_privileged_confirmation() {
        let options = approval_options(&payload(true, None));
        let once = options
            .iter()
            .find(|option| option.decision == "allow_once")
            .expect("an escalation can be approved once");
        assert!(once.privileged, "running outside the sandbox is privileged");
        assert!(!options
            .iter()
            .any(|option| option.decision == "approve_all"));
        let deny = options.iter().find(|o| o.decision == "deny").unwrap();
        assert!(!deny.privileged, "refusing is never privileged");
    }

    #[test]
    fn a_denied_host_is_granted_per_session_at_least_and_never_run_once() {
        let options = approval_options(&payload(false, Some("example.com")));
        assert_eq!(
            decisions(&options),
            [
                "allow_host_for_session",
                "allow_host_always",
                "deny",
                "deny_with_note"
            ]
        );
        assert!(options[0].label.contains("example.com"));
        assert!(options[0].privileged && options[1].privileged);
    }

    #[test]
    fn a_refused_directory_is_offered_before_leaving_the_sandbox() {
        let dir = std::env::temp_dir().join(format!("forge-approvals-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = dir.canonicalize().unwrap();
        let mut escalation = payload(true, None);
        escalation.denied_path = Some(resolved.join("cache.bin").display().to_string());
        let options = approval_options(&escalation);
        assert_eq!(
            &decisions(&options)[..3],
            ["allow_path_for_session", "allow_path_always", "allow_once"]
        );
        assert!(options[0].label.contains(&resolved.display().to_string()));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn escalations_and_host_grants_need_a_second_confirmation() {
        assert!(requires_native_confirm(&payload(true, None)));
        assert!(requires_native_confirm(&payload(
            false,
            Some("example.com")
        )));
        assert!(!requires_native_confirm(&payload(false, None)));
    }

    #[test]
    fn invisible_and_bidi_characters_are_made_visible() {
        let disguised = "echo safe\u{202E}fr- mr\u{200B}";
        assert_eq!(
            escape_invisible(disguised),
            "echo safe⟨U+202E⟩fr- mr⟨U+200B⟩"
        );
        assert_eq!(escape_invisible("plain ascii"), "plain ascii");
        assert_eq!(
            escape_invisible("\u{2066}x\u{2069}\u{FEFF}"),
            "⟨U+2066⟩x⟨U+2069⟩⟨U+FEFF⟩"
        );
    }
}
