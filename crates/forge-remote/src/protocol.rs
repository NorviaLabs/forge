//! Remote control's wire format, version 1. A phone app is built against
//! this: change a shape only with a new protocol version.
//!
//! Requests and responses are JSON. Events travel as Server-Sent Events:
//! `id: <epoch>.<seq>`, `event: <type>`, `data: <StreamEvent as JSON>`.

use forge_types::SessionId;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// The most messages one page (or a `session` event) carries.
pub const MAX_PAGE: usize = 100;

/// Messages in a page when the client does not say.
pub const DEFAULT_PAGE: usize = 30;

/// `POST /v1/pair`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
}

/// `POST /v1/pair` accepted the code: the device shows `confirmation` and
/// polls `GET /v1/pair/{pairing_id}` until the desktop decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStarted {
    pub pairing_id: String,
    /// Six digits; the desktop shows the same number.
    pub confirmation: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /v1/pair/{pairing_id}` while the desktop has not decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairPending {
    /// Always `pending`.
    pub status: String,
}

/// Collected from `GET /v1/pair/{pairing_id}` once the desktop allowed it.
/// The token is shown to the device this once; Forge keeps only its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    pub token: String,
    pub device_id: String,
    pub device_name: String,
}

/// `GET /v1/hello`: checks a token and names the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub device_name: String,
}

/// One row of `GET /v1/sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListItem {
    pub session_id: SessionId,
    pub project: String,
    pub label: String,
    pub turn_state: String,
    /// The session waits on an approval.
    pub needs_approval: bool,
    /// `host:folder` when the session runs on an SSH host; absent on the
    /// computer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionListItem>,
}

/// `POST /v1/sessions/{id}/prompt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRequest {
    pub text: String,
}

/// The only decisions a remote device can make. Anything that would run a
/// command outside the sandbox, open a host, or remember a grant stays on
/// the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDecision {
    AllowOnce,
    Deny,
    DenyWithNote,
}

impl RemoteDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::Deny => "deny",
            Self::DenyWithNote => "deny_with_note",
        }
    }
}

/// `POST /v1/sessions/{id}/approval`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The request being answered; a different pending request is a 409.
    pub call_id: String,
    pub decision: RemoteDecision,
    /// Required for `deny_with_note`; reaches the agent as the tool result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

/// One transcript message, trimmed for a small screen. Tool output is cut
/// as on the desktop; attachments are counted, never located.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMessage {
    /// `user`, `assistant` or `tool`.
    pub role: String,
    /// The tool's name, for a tool message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attachments: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// The approval the session waits on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteApproval {
    pub call_id: String,
    pub tool: String,
    /// The arguments, pretty-printed, with invisible characters made visible.
    pub arguments: String,
    pub reason: String,
    /// `governance`, `sandbox_escalation` or `network_host`.
    pub kind: String,
    /// What this device may answer with, in the order to show them.
    pub remote_options: Vec<RemoteDecision>,
    /// The request also offers options only the desktop can give (running
    /// outside the sandbox, opening a host, remembering a grant).
    pub desktop_required: bool,
    /// The SSH host the call runs on, outside Forge's sandbox with that
    /// machine's network and credentials; absent on the computer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<String>,
}

/// A session as the device shows it. Each one replaces the last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: SessionId,
    pub project: String,
    pub label: String,
    pub turn_state: String,
    /// The newest messages only; older ones come from the messages route.
    pub messages: Vec<RemoteMessage>,
    /// The index of `messages[0]` among all the session's messages.
    pub message_offset: usize,
    /// How many messages the session has. A smaller number than before
    /// means the transcript was compacted: drop older pages.
    pub message_total: usize,
    pub pending_approval: Option<RemoteApproval>,
    /// Prompts waiting for the turn to finish.
    pub queue: Vec<String>,
    /// What the session waits on when no approval card says it.
    pub waiting_on: Option<String>,
    /// `host:folder` when the session runs on an SSH host; absent on the
    /// computer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<String>,
}

/// `GET /v1/sessions/{id}/messages`: messages before an index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<RemoteMessage>,
    /// The index of `messages[0]`.
    pub message_offset: usize,
    pub message_total: usize,
}

/// The up to `limit` (at most [`MAX_PAGE`]) items just before index
/// `before` (the end when `None`), and the index of the first.
pub fn page<T: Clone>(items: &[T], before: Option<usize>, limit: usize) -> (usize, Vec<T>) {
    let end = before.unwrap_or(items.len()).min(items.len());
    let start = end.saturating_sub(limit.min(MAX_PAGE));
    (start, items[start..end].to_vec())
}

/// `attention` (the session needs the operator) or `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Attention,
    Error,
}

/// What `GET /v1/sessions/{id}/events` sends, one per SSE event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// The whole session. `resync` is true when it replaces events the
    /// device missed; the device drops what it had and starts from here.
    Session {
        #[serde(flatten)]
        state: Box<SessionState>,
        resync: bool,
    },
    /// Streamed assistant text, appended to the reply in progress.
    Text {
        session_id: SessionId,
        delta: String,
    },
    Notice {
        session_id: SessionId,
        kind: NoticeKind,
        message: String,
    },
    /// The session closed or was archived; the stream ends after this.
    Gone { session_id: SessionId },
}

impl StreamEvent {
    /// The SSE `event:` name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Session { .. } => "session",
            Self::Text { .. } => "text",
            Self::Notice { .. } => "notice",
            Self::Gone { .. } => "gone",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id() -> SessionId {
        "6a1f2d3c-0000-4000-8000-000000000001".parse().unwrap()
    }

    #[test]
    fn requests_and_responses_have_the_documented_shape() {
        let pair: PairRequest =
            serde_json::from_value(json!({"code": "ABCD-EFGH", "device_name": "Phone"})).unwrap();
        assert_eq!(pair.code, "ABCD-EFGH");
        assert_eq!(
            serde_json::to_value(PairResponse {
                token: "fr1_x".into(),
                device_id: "d".into(),
                device_name: "Phone".into()
            })
            .unwrap(),
            json!({"token": "fr1_x", "device_id": "d", "device_name": "Phone"})
        );
        let approval: ApprovalRequest = serde_json::from_value(
            json!({"call_id": "c1", "decision": "deny_with_note", "note": "no"}),
        )
        .unwrap();
        assert_eq!(approval.decision, RemoteDecision::DenyWithNote);
        let approval: ApprovalRequest =
            serde_json::from_value(json!({"call_id": "c1", "decision": "allow_once"})).unwrap();
        assert_eq!(approval.note, None);
        assert!(
            serde_json::from_value::<ApprovalRequest>(
                json!({"call_id": "c1", "decision": "allow_host_always"})
            )
            .is_err(),
            "only the three remote decisions parse"
        );
    }

    #[test]
    fn pairing_is_started_then_collected() {
        let at: chrono::DateTime<chrono::Utc> = "2026-11-14T10:02:00Z".parse().unwrap();
        assert_eq!(
            serde_json::to_value(PairStarted {
                pairing_id: "p".repeat(43),
                confirmation: "482913".into(),
                expires_at: at,
            })
            .unwrap(),
            json!({"pairing_id": "p".repeat(43), "confirmation": "482913", "expires_at": "2026-11-14T10:02:00Z"})
        );
        assert_eq!(
            serde_json::to_value(PairPending {
                status: "pending".into()
            })
            .unwrap(),
            json!({"status": "pending"})
        );
    }

    #[test]
    fn a_page_is_the_items_just_before_an_index() {
        let items: Vec<usize> = (0..100).collect();
        assert_eq!(page(&items, None, 30), (70, (70..100).collect::<Vec<_>>()));
        assert_eq!(
            page(&items, Some(70), 30),
            (40, (40..70).collect::<Vec<_>>())
        );
        assert_eq!(page(&items, Some(10), 30), (0, (0..10).collect::<Vec<_>>()));
        assert_eq!(
            page(&items, Some(500), 5),
            (95, (95..100).collect::<Vec<_>>())
        );
        assert_eq!(page(&items, Some(0), 30), (0, vec![]));
        assert_eq!(
            page(&items, Some(100), 1000).1.len(),
            MAX_PAGE,
            "a page is never larger than the maximum"
        );
        assert_eq!(page(&items, None, 0).1.len(), 0);
    }

    #[test]
    fn stream_events_are_tagged_and_a_session_is_flat() {
        let state = SessionState {
            session_id: id(),
            project: "billing-api".into(),
            label: "Fix test".into(),
            turn_state: "waiting".into(),
            messages: vec![RemoteMessage {
                role: "user".into(),
                name: None,
                text: "hi".into(),
                attachments: 0,
            }],
            message_offset: 41,
            message_total: 42,
            pending_approval: Some(RemoteApproval {
                call_id: "c1".into(),
                tool: "git".into(),
                arguments: "{}".into(),
                reason: "destructive".into(),
                kind: "governance".into(),
                remote_options: vec![RemoteDecision::AllowOnce, RemoteDecision::Deny],
                desktop_required: true,
                runs_on: None,
            }),
            queue: vec![],
            waiting_on: None,
            runs_on: None,
        };
        let event = StreamEvent::Session {
            state: Box::new(state),
            resync: false,
        };
        assert_eq!(event.name(), "session");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "session",
                "resync": false,
                "session_id": id(),
                "project": "billing-api",
                "label": "Fix test",
                "turn_state": "waiting",
                "messages": [{"role": "user", "text": "hi"}],
                "message_offset": 41,
                "message_total": 42,
                "pending_approval": {
                    "call_id": "c1", "tool": "git", "arguments": "{}",
                    "reason": "destructive", "kind": "governance",
                    "remote_options": ["allow_once", "deny"],
                    "desktop_required": true
                },
                "queue": [],
                "waiting_on": null
            })
        );
        assert_eq!(
            serde_json::to_value(StreamEvent::Text {
                session_id: id(),
                delta: "tok".into()
            })
            .unwrap(),
            json!({"type": "text", "session_id": id(), "delta": "tok"})
        );
        assert_eq!(
            serde_json::to_value(StreamEvent::Gone { session_id: id() }).unwrap(),
            json!({"type": "gone", "session_id": id()})
        );
        let round: StreamEvent =
            serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        assert_eq!(round, event);
    }
}
