//! The IPC bridge: Forge's supervisor on one side, the webview on the other.
//!
//! Provisional. The event shape here is the walking skeleton's own and is
//! not frozen; phase P replaces it with the versioned `forge-app` protocol.

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use forge_session::{SupervisorCommand, SupervisorEvent, SupervisorHandle};
use forge_types::SessionId;

/// What the webview receives on its channel.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopEvent {
    /// Streamed model output, passed through as the core emits it.
    Stream {
        session_id: SessionId,
        event: serde_json::Value,
    },
    /// The session's turn state and how many messages its transcript holds.
    Session {
        session_id: SessionId,
        turn_state: &'static str,
        messages: usize,
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

/// Translate one supervisor event. `None` for events the skeleton does not
/// show yet.
pub fn translate(event: SupervisorEvent) -> Option<DesktopEvent> {
    match event {
        SupervisorEvent::Stream { session_id, event } => Some(DesktopEvent::Stream {
            session_id,
            event: serde_json::to_value(event).ok()?,
        }),
        SupervisorEvent::SessionUpdated(snapshot) => Some(DesktopEvent::Session {
            session_id: snapshot.task.session_id,
            turn_state: snapshot.task.turn_state.label(),
            messages: snapshot.transcript.messages().len(),
        }),
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
    pub session_id: SessionId,
    pub self_test: bool,
    /// Self-test: the page has submitted its prompt through IPC.
    pub self_test_submitted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// What `subscribe` returns before events start flowing.
#[derive(Debug, Serialize)]
pub struct Hello {
    pub session_id: SessionId,
    pub self_test: bool,
}

/// Stream supervisor events into `channel` until the window goes away.
#[tauri::command]
pub fn subscribe(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    channel: Channel<DesktopEvent>,
) -> Hello {
    let mut events = state.handle.subscribe();
    let self_test = state.self_test.then(|| state.self_test_submitted.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => translate(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    Some(DesktopEvent::Lagged { missed })
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if let Some(event) = event {
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
                if let Some(submitted) = &self_test {
                    if completed {
                        self_test_verdict(
                            &app,
                            submitted.load(std::sync::atomic::Ordering::SeqCst),
                        );
                    }
                }
            }
        }
    });
    Hello {
        session_id: state.session_id,
        self_test: state.self_test,
    }
}

#[tauri::command]
pub async fn submit_prompt(state: State<'_, DesktopState>, text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    if state.self_test {
        state
            .self_test_submitted
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

/// Self-test: the journey is complete once the page has (1) subscribed,
/// which proves its script ran and IPC reaches Rust, (2) submitted a prompt
/// through IPC, and (3) been sent the completed turn on its channel. No
/// extra command exists for this, so a self-test build has exactly the
/// release command surface.
fn self_test_verdict(app: &tauri::AppHandle, submitted: bool) {
    static DECIDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DECIDED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if submitted {
        println!("SELF_TEST PASS turn completed and delivered to the page");
        app.exit(0);
    } else {
        eprintln!("SELF_TEST FAIL a turn completed without the page submitting");
        app.exit(1);
    }
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
}
