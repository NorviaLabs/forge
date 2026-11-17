//! What the server needs from Forge. [`crate::supervisor::SupervisorBackend`]
//! is the real one; tests use [`crate::testing::FakeBackend`].

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use forge_types::SessionId;
use tokio::sync::mpsc;

use crate::protocol::{MessagePage, NoticeKind, RemoteDecision, SessionListItem, SessionState};

/// A change to one session, in the order Forge made it.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    /// The whole session as it now stands.
    Updated(Box<SessionState>),
    /// Streamed assistant text.
    Text {
        session_id: SessionId,
        delta: String,
    },
    Notice {
        session_id: SessionId,
        kind: NoticeKind,
        message: String,
    },
    /// The session closed, was archived, or its project closed.
    Gone(SessionId),
    /// Events for these sessions were dropped on the way; their state must
    /// be read again.
    Lagged { sessions: Vec<SessionId> },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("that session does not exist")]
    UnknownSession,
    #[error("that request is no longer pending")]
    NotPending,
    /// The decision is not one this device may make for this request.
    #[error("{0}")]
    NotAllowed(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unavailable(String),
}

/// The sessions someone is following. The hub keeps it; a backend reads it
/// to skip building states nobody will receive.
#[derive(Debug, Clone, Default)]
pub struct Interest(Arc<RwLock<HashSet<SessionId>>>);

impl Interest {
    pub fn contains(&self, session_id: SessionId) -> bool {
        self.0
            .read()
            .map(|set| set.contains(&session_id))
            .unwrap_or(false)
    }

    pub fn insert(&self, session_id: SessionId) {
        if let Ok(mut set) = self.0.write() {
            set.insert(session_id);
        }
    }

    pub fn remove(&self, session_id: SessionId) {
        if let Ok(mut set) = self.0.write() {
            set.remove(&session_id);
        }
    }

    pub fn sessions(&self) -> Vec<SessionId> {
        self.0
            .read()
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[async_trait]
pub trait RemoteBackend: Send + Sync + 'static {
    /// The active sessions of every open project.
    async fn sessions(&self) -> Result<Vec<SessionListItem>, BackendError>;

    /// One session as it stands now.
    async fn session_state(&self, session_id: SessionId) -> Result<SessionState, BackendError>;

    /// Up to `limit` of the session's messages before index `before` (the
    /// newest when `None`), numbered as in [`SessionState::message_offset`].
    async fn messages(
        &self,
        session_id: SessionId,
        before: Option<usize>,
        limit: usize,
    ) -> Result<MessagePage, BackendError>;

    /// Every session's changes from now on, in order. Called once per
    /// service start; the stream ends when the backend goes away.
    fn subscribe(&self, interest: Interest) -> mpsc::Receiver<BackendEvent>;

    /// Queue a prompt, exactly as the desktop's Send does.
    async fn submit_prompt(&self, session_id: SessionId, text: String) -> Result<(), BackendError>;

    /// Answer the pending approval `call_id`. The backend checks that the
    /// request is still pending and that this decision is one a remote
    /// device may make for it.
    async fn decide(
        &self,
        session_id: SessionId,
        call_id: &str,
        decision: RemoteDecision,
        note: Option<String>,
        actor: &str,
    ) -> Result<(), BackendError>;

    /// Stop the running turn.
    async fn stop(&self, session_id: SessionId) -> Result<(), BackendError>;
}
