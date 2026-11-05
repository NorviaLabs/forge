//! The IPC bridge: Forge's supervisor on one side, the webview on the other.
//!
//! Provisional. The event shape here is the walking skeleton's own and is
//! not frozen; phase P replaces it with the versioned `forge-app` protocol
//! (revision-keyed deltas instead of the whole transcript per update).

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use forge_session::{SessionRuntimeSnapshot, SupervisorCommand, SupervisorEvent, SupervisorHandle};
use forge_types::{ApprovalKind, HitlDecision, HitlPayload, MessageRole, SessionId};

/// Tool output longer than this is cut in the transcript view.
const TOOL_TEXT_LIMIT: usize = 2000;

/// What the webview receives on its channel.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopEvent {
    /// Streamed model output, passed through as the core emits it.
    Stream {
        session_id: SessionId,
        event: serde_json::Value,
    },
    /// The session as it stands: turn state, transcript and any approval
    /// waiting on the user.
    Session {
        session_id: SessionId,
        turn_state: &'static str,
        messages: Vec<MessageView>,
        pending_approval: Option<ApprovalView>,
    },
    /// The session needs the user's attention.
    Attention {
        session_id: SessionId,
        turn_state: &'static str,
        message: String,
    },
    Error {
        message: String,
    },
    /// The broadcast dropped events for a slow consumer; the view must
    /// treat its state as stale.
    Lagged {
        missed: u64,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MessageView {
    pub role: &'static str,
    /// Markdown for user and assistant messages; plain text for tools.
    pub text: String,
}

/// A pending approval as the page shows it. `call_id` is issued by the
/// backend and is the only thing the page can approve or deny.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApprovalView {
    pub call_id: String,
    pub tool: String,
    /// The arguments, pretty-printed, with invisible and bidi-control
    /// characters made visible.
    pub arguments: String,
    pub reason: String,
    /// A sandbox escalation or network grant. Approving it needs the native
    /// confirmation (M2), so until then the page can only deny it.
    pub privileged: bool,
}

/// Approvals whose every affirmative answer needs the native confirmation
/// (AC-4.12): running unconfined, or opening a network host.
pub fn requires_native_confirm(payload: &HitlPayload) -> bool {
    matches!(
        payload.kind(),
        ApprovalKind::SandboxEscalation | ApprovalKind::NetworkHost
    )
}

/// Why the app refused to forward an approval decision.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    NoApprovalId,
    /// Nothing is pending, or the pending request is a different one.
    NotPending,
    /// Approve on a privileged request; only deny is possible from the page.
    NeedsNativeConfirm,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NoApprovalId => "no approval id",
            Refusal::NotPending => "that request is no longer pending",
            Refusal::NeedsNativeConfirm => {
                "this request needs a native confirmation, which arrives in a later release: \
                 approve it in the terminal, or deny it here"
            }
        })
    }
}

/// Decide, against the request actually pending, whether the page's
/// decision may be forwarded. Deny is always allowed for the pending
/// request; Approve is refused for privileged requests.
pub fn check_decision(
    pending: Option<&HitlPayload>,
    call_id: &str,
    decision: &HitlDecision,
) -> Result<(), Refusal> {
    if call_id.trim().is_empty() {
        return Err(Refusal::NoApprovalId);
    }
    let Some(pending) = pending.filter(|payload| payload.call_id == call_id) else {
        return Err(Refusal::NotPending);
    };
    if matches!(decision, HitlDecision::Approve) && requires_native_confirm(pending) {
        return Err(Refusal::NeedsNativeConfirm);
    }
    Ok(())
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

fn session_event(snapshot: &SessionRuntimeSnapshot) -> DesktopEvent {
    let messages = snapshot
        .transcript
        .messages()
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                _ => return None,
            };
            let mut text = message.content.clone();
            if message.role == MessageRole::Tool && text.chars().count() > TOOL_TEXT_LIMIT {
                text = text.chars().take(TOOL_TEXT_LIMIT).collect::<String>() + "\n…";
            }
            Some(MessageView { role, text })
        })
        .collect();
    let pending_approval = snapshot.session.pending_hitl.as_ref().map(|payload| {
        let arguments = serde_json::to_string_pretty(&payload.args_redacted)
            .unwrap_or_else(|_| payload.args_redacted.to_string());
        ApprovalView {
            call_id: payload.call_id.clone(),
            tool: escape_invisible(&payload.tool),
            arguments: escape_invisible(&arguments),
            reason: escape_invisible(&payload.reason),
            privileged: requires_native_confirm(payload),
        }
    });
    DesktopEvent::Session {
        session_id: snapshot.task.session_id,
        turn_state: snapshot.task.turn_state.label(),
        messages,
        pending_approval,
    }
}

/// Translate one supervisor event. `None` for events the skeleton does not
/// show yet.
pub fn translate(event: SupervisorEvent) -> Option<DesktopEvent> {
    match event {
        SupervisorEvent::Stream { session_id, event } => Some(DesktopEvent::Stream {
            session_id,
            event: serde_json::to_value(event).ok()?,
        }),
        SupervisorEvent::SessionUpdated(snapshot) => Some(session_event(&snapshot)),
        SupervisorEvent::Attention {
            session_id,
            state,
            message,
        } => Some(DesktopEvent::Attention {
            session_id,
            turn_state: state.label(),
            message,
        }),
        SupervisorEvent::Error { message, .. } => Some(DesktopEvent::Error { message }),
        _ => None,
    }
}

/// Process-wide state the commands share.
pub struct DesktopState {
    pub handle: SupervisorHandle,
    /// Read for the live session state; taken (write) only at exit to shut
    /// the supervisor down.
    pub supervisor:
        std::sync::Arc<tokio::sync::RwLock<Option<forge_session::RepositorySupervisor>>>,
    pub session_id: SessionId,
    /// The session as it was when the window opened, so the page can show
    /// the transcript before the first update arrives.
    pub initial: DesktopEvent,
    #[cfg(feature = "self-test")]
    pub self_test: Option<std::sync::Arc<crate::self_test::Probe>>,
}

impl DesktopState {
    pub fn new(
        handle: SupervisorHandle,
        supervisor: std::sync::Arc<
            tokio::sync::RwLock<Option<forge_session::RepositorySupervisor>>,
        >,
        session_id: SessionId,
        initial: &SessionRuntimeSnapshot,
    ) -> Self {
        Self {
            handle,
            supervisor,
            session_id,
            initial: session_event(initial),
            #[cfg(feature = "self-test")]
            self_test: None,
        }
    }
}

/// What `subscribe` returns before events start flowing.
#[derive(Debug, Serialize)]
pub struct Hello {
    pub session_id: SessionId,
    pub initial: DesktopEvent,
    /// Self-test builds only: the probe's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_test: Option<serde_json::Value>,
}

/// Stream supervisor events into `channel` until the window goes away.
#[tauri::command]
pub fn subscribe(state: State<'_, DesktopState>, channel: Channel<DesktopEvent>) -> Hello {
    let mut events = state.handle.subscribe();
    #[cfg(feature = "self-test")]
    let probe = state.self_test.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => translate(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    Some(DesktopEvent::Lagged { missed })
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let Some(event) = event else { continue };
            #[cfg(feature = "self-test")]
            let completed = matches!(
                event,
                DesktopEvent::Session {
                    turn_state: "completed",
                    ..
                }
            );
            if channel.send(event).is_err() {
                // The webview is gone.
                break;
            }
            #[cfg(feature = "self-test")]
            if completed {
                if let Some(probe) = &probe {
                    probe.completed_delivered();
                }
            }
        }
    });
    #[cfg(feature = "self-test")]
    let self_test = state.self_test.as_ref().map(|probe| probe.hello());
    #[cfg(not(feature = "self-test"))]
    let self_test = None;
    Hello {
        session_id: state.session_id,
        initial: state.initial.clone(),
        self_test,
    }
}

#[tauri::command]
pub async fn submit_prompt(state: State<'_, DesktopState>, text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    #[cfg(feature = "self-test")]
    if let Some(probe) = &state.self_test {
        probe.submitted();
    }
    state
        .handle
        .submit(SupervisorCommand::SubmitPrompt {
            session_id: state.session_id,
            text,
        })
        // Fire and forget: the outcome arrives on the event channel.
        .map(drop)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn stop_turn(state: State<'_, DesktopState>) -> Result<(), String> {
    state
        .handle
        .submit(SupervisorCommand::StopTurn {
            session_id: state.session_id,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

async fn resolve(
    state: &DesktopState,
    call_id: String,
    decision: HitlDecision,
) -> Result<(), String> {
    let pending = {
        let supervisor = state.supervisor.read().await;
        let Some(supervisor) = supervisor.as_ref() else {
            return Err("Forge is shutting down".into());
        };
        supervisor
            .snapshot(state.session_id)
            .await
            .and_then(|snapshot| snapshot.session.pending_hitl)
    };
    check_decision(pending.as_ref(), &call_id, &decision).map_err(|refusal| refusal.to_string())?;
    state
        .handle
        .submit(SupervisorCommand::ResolveApproval {
            session_id: state.session_id,
            // Always bound to the request the page showed: if it changed
            // after the check above, the session actor refuses it.
            expected_call_id: Some(call_id),
            decision,
            actor: "desktop".into(),
            feedback: None,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Approve the displayed request once. Refused for privileged requests
/// (sandbox escalation, network grant), which need the native confirmation
/// (M2); broad grants (always-allow, approve-all) have no command at all.
#[tauri::command]
pub async fn approve_once(state: State<'_, DesktopState>, call_id: String) -> Result<(), String> {
    resolve(&state, call_id, HitlDecision::Approve).await
}

#[tauri::command]
pub async fn deny(state: State<'_, DesktopState>, call_id: String) -> Result<(), String> {
    resolve(&state, call_id, HitlDecision::Deny).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_events_pass_through_as_json() {
        let session_id = SessionId::new_v4();
        let event = translate(SupervisorEvent::Stream {
            session_id,
            event: forge_types::ModelStreamEvent::TextDelta {
                text: "hello".into(),
            },
        })
        .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "stream");
        assert_eq!(json["event"]["type"], "text_delta");
        assert_eq!(json["event"]["text"], "hello");
    }

    #[test]
    fn errors_and_attention_are_forwarded() {
        let session_id = SessionId::new_v4();
        assert_eq!(
            translate(SupervisorEvent::Error {
                session_id: None,
                message: "boom".into()
            }),
            Some(DesktopEvent::Error {
                message: "boom".into()
            })
        );
        let attention = translate(SupervisorEvent::Attention {
            session_id,
            state: forge_session::SupervisorTurnState::Waiting,
            message: "approve?".into(),
        })
        .unwrap();
        assert_eq!(
            serde_json::to_value(&attention).unwrap()["turn_state"],
            forge_session::SupervisorTurnState::Waiting.label()
        );
    }

    #[test]
    fn events_the_skeleton_does_not_show_are_dropped() {
        assert_eq!(translate(SupervisorEvent::Selected(None)), None);
    }

    fn payload(sandbox_escalation: bool, denied_host: Option<&str>) -> HitlPayload {
        HitlPayload {
            call_id: "call-1".into(),
            tool: "bash".into(),
            args_redacted: serde_json::json!({"command": "curl example.com"}),
            reason: "needs approval".into(),
            failure: None,
            sandbox_escalation,
            denied_host: denied_host.map(str::to_string),
        }
    }

    #[test]
    fn privileged_requests_can_only_be_denied_from_the_page() {
        for privileged in [payload(true, None), payload(false, Some("example.com"))] {
            assert!(requires_native_confirm(&privileged));
            assert_eq!(
                check_decision(Some(&privileged), "call-1", &HitlDecision::Approve),
                Err(Refusal::NeedsNativeConfirm),
                "{privileged:?}"
            );
            assert_eq!(
                check_decision(Some(&privileged), "call-1", &HitlDecision::Deny),
                Ok(())
            );
        }
    }

    #[test]
    fn a_plain_request_approves_once() {
        let plain = payload(false, None);
        assert!(!requires_native_confirm(&plain));
        assert_eq!(
            check_decision(Some(&plain), "call-1", &HitlDecision::Approve),
            Ok(())
        );
    }

    #[test]
    fn decisions_need_the_request_that_is_actually_pending() {
        let plain = payload(false, None);
        for decision in [HitlDecision::Approve, HitlDecision::Deny] {
            assert_eq!(
                check_decision(None, "call-1", &decision),
                Err(Refusal::NotPending)
            );
            assert_eq!(
                check_decision(Some(&plain), "call-2", &decision),
                Err(Refusal::NotPending)
            );
            assert_eq!(
                check_decision(Some(&plain), " ", &decision),
                Err(Refusal::NoApprovalId)
            );
        }
    }

    #[test]
    fn invisible_and_bidi_characters_are_made_visible() {
        // "rm -rf ~ #" disguised with a right-to-left override and a
        // zero-width space.
        let disguised = "echo safe\u{202E}fr- mr\u{200B}";
        let shown = escape_invisible(disguised);
        assert_eq!(shown, "echo safe⟨U+202E⟩fr- mr⟨U+200B⟩");
        assert!(!shown.chars().any(|c| matches!(c, '\u{202E}' | '\u{200B}')));
        assert_eq!(escape_invisible("plain ascii"), "plain ascii");
        assert_eq!(
            escape_invisible("\u{2066}x\u{2069}\u{FEFF}"),
            "⟨U+2066⟩x⟨U+2069⟩⟨U+FEFF⟩"
        );
    }
}
