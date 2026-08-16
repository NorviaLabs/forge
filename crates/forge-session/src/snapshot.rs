//! An immutable, per-frame view of session state for frontends to render from.
//!
//! Rendering used to read the live `AgentSession` directly, which coupled the
//! UI to the session's ownership: as long as a frame reads `session.foo()`,
//! whoever draws must own the session outright. A snapshot breaks that — the
//! frame reads a value, and where that value came from stops mattering.
//!
//! It is also cheaper. The reads it replaces are not all field accesses:
//! `loaded_skills_count` used to clone a `Vec<SkillManifest>` to take its length,
//! `tool_count` walks the registry through the governance filter, and
//! `context_usage_ratio` sums the transcript estimate. Scattered across a
//! frame those ran once per call site — `pending_hitl` alone had 26. Captured
//! once per frame, they run once.
//!
//! Deliberately excluded:
//!
//! - **The transcript** (`messages`, `events`). Copying it per frame would
//!   cost more than the reads this saves. It needs a revision-keyed
//!   projection, which is the conversation view-model work, not this.
//! - **On-demand detail** (`list_tools`, `loaded_skill_names`, `journal_dir`,
//!   `token_usage_report`). Only reachable from a slash command, so paying for
//!   it every frame would be strictly worse than the direct read.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_core::{AgentSession, TurnEvent};
use forge_types::{HitlPayload, Message, SessionId, TaskLifecycle};

/// What a frontend needs to draw one frame, minus the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    /// Authoritative task state. Frontends must render this rather than
    /// deriving their own from busy/streaming flags.
    pub lifecycle: TaskLifecycle,
    /// The outstanding approval request, if the session is waiting on one.
    pub pending_hitl: Option<HitlPayload>,
    pub queue_len: usize,
    pub background_len: usize,
    pub workspace_root: PathBuf,
    /// Tools the model can currently see, after the governance filter.
    pub tool_count: usize,
    pub loaded_skills_count: usize,
    /// Estimated share of the context window in use, 0.0..=1.0.
    pub context_usage_ratio: f64,
    /// API-reported session totals. Field reads — not `token_usage_report()`,
    /// which walks the transcript and is reserved for slash-command detail.
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
}

impl SessionSnapshot {
    /// Read the session once, for one frame.
    pub fn capture(session: &AgentSession) -> Self {
        Self {
            session_id: session.session_id,
            lifecycle: session.active_task.lifecycle,
            pending_hitl: session.pending_hitl().cloned(),
            queue_len: session.queue().len(),
            background_len: session.background().len(),
            workspace_root: session.workspace_root().to_path_buf(),
            tool_count: session.tool_count(),
            loaded_skills_count: session.loaded_skills_count(),
            context_usage_ratio: session.context_usage_ratio(),
            prompt_tokens: session.token_usage.prompt_tokens,
            completion_tokens: session.token_usage.completion_tokens,
            prompt_cache_hits: session.token_usage.prompt_cache_hits,
            prompt_cache_writes: session.token_usage.prompt_cache_writes,
        }
    }

    /// Whether an approval is outstanding — by far the most common read.
    pub fn is_awaiting_approval(&self) -> bool {
        self.pending_hitl.is_some()
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

impl Default for SessionSnapshot {
    /// An empty snapshot, for constructing a frontend before its first
    /// capture. Nothing pending, nothing queued.
    fn default() -> Self {
        Self {
            session_id: SessionId::nil(),
            lifecycle: TaskLifecycle::Ready,
            pending_hitl: None,
            queue_len: 0,
            background_len: 0,
            workspace_root: PathBuf::new(),
            tool_count: 0,
            loaded_skills_count: 0,
            context_usage_ratio: 0.0,
            prompt_tokens: 0,
            completion_tokens: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        }
    }
}

/// The transcript, shared rather than copied.
///
/// A frame needs the messages and events themselves to rebuild its projection
/// when they change — a length or a hash is not enough. But copying the whole
/// transcript every frame would cost more than the projection it feeds. So it
/// is held behind `Arc` and re-cloned only when a cheap fingerprint says the
/// transcript actually moved.
///
/// Kept separate from [`SessionSnapshot`] because the two have different
/// costs and different callers: the cheap snapshot is also built on demand by
/// paths like `/status`, which have no use for the transcript and should not
/// pay to copy it.
#[derive(Debug, Clone, Default)]
pub struct TranscriptSnapshot {
    messages: Arc<[Message]>,
    events: Arc<[TurnEvent]>,
    fingerprint: Option<TranscriptFingerprint>,
}

/// What the transcript looked like last time it was copied.
///
/// Messages only ever append or get replaced wholesale, so lengths plus the
/// size of the tail catch every change that matters — including the growing
/// last message during streaming. This mirrors the assumption the TUI's own
/// conversation render cache has always keyed on; it is the same trade, not a
/// new one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TranscriptFingerprint {
    messages: usize,
    events: usize,
    last_message_content: usize,
    last_message_thinking: usize,
    last_event_detail: usize,
}

impl TranscriptFingerprint {
    fn of(session: &AgentSession) -> Self {
        Self {
            messages: session.messages.len(),
            events: session.events.len(),
            last_message_content: session.messages.last().map_or(0, |m| m.content.len()),
            last_message_thinking: session
                .messages
                .last()
                .and_then(|m| m.thinking.as_ref())
                .map_or(0, String::len),
            last_event_detail: session.events.last().map_or(0, |e| e.detail.len()),
        }
    }
}

impl TranscriptSnapshot {
    /// Bring the snapshot up to date, copying only if the transcript moved.
    pub fn refresh(&mut self, session: &AgentSession) {
        let fingerprint = TranscriptFingerprint::of(session);
        // `Option` rather than comparing against a default fingerprint: an
        // empty transcript has the default one, so a bare equality check would
        // re-copy on every frame until the first message arrived.
        if self.fingerprint == Some(fingerprint) {
            return;
        }
        self.messages = Arc::from(session.messages.as_slice());
        self.events = Arc::from(session.events.as_slice());
        self.fingerprint = Some(fingerprint);
    }

    /// A snapshot taken now, for callers outside a frame.
    pub fn capture(session: &AgentSession) -> Self {
        let mut snapshot = Self::default();
        snapshot.refresh(session);
        snapshot
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn events(&self) -> &[TurnEvent] {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::{ModelResponse, ToolCall};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn text(body: &str) -> ModelResponse {
        ModelResponse {
            text: body.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    async fn session_with(script: Vec<ModelResponse>, dir: &Path) -> AgentSession {
        let cfg = LoopConfig {
            max_turns: 5,
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        AgentSession::create(
            cfg,
            Arc::new(MockModelClient::script(script)),
            ToolRegistry::new(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn capture_reports_a_fresh_session_as_idle_with_nothing_pending() {
        let dir = tempdir().unwrap();
        let session = session_with(vec![], dir.path()).await;
        let snapshot = SessionSnapshot::capture(&session);

        assert_eq!(snapshot.session_id, session.session_id);
        assert!(!snapshot.is_awaiting_approval());
        assert_eq!(snapshot.queue_len, 0);
        assert_eq!(snapshot.background_len, 0);
        assert_eq!(snapshot.prompt_tokens, 0);
        assert_eq!(snapshot.completion_tokens, 0);
        assert_eq!(snapshot.workspace_root(), dir.path());
    }

    /// The snapshot must carry the pending approval through, since that is
    /// what the frontend renders the approval prompt from.
    #[tokio::test]
    async fn capture_carries_an_outstanding_approval() {
        let dir = tempdir().unwrap();
        let mut session = session_with(
            vec![ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "git push origin main"}),
                }],
                usage: None,
                thinking: None,
            }],
            dir.path(),
        )
        .await;
        session.run_user_message("push").await.unwrap();

        let snapshot = SessionSnapshot::capture(&session);
        assert!(snapshot.is_awaiting_approval());
        assert_eq!(snapshot.pending_hitl.as_ref().unwrap().tool, "bash");
        assert_eq!(snapshot.lifecycle, TaskLifecycle::Waiting);
    }

    /// The point of the fingerprint: an unchanged transcript must not be
    /// re-copied, or every frame pays for the whole history.
    #[tokio::test]
    async fn refreshing_an_unchanged_transcript_reuses_the_same_allocation() {
        let dir = tempdir().unwrap();
        let mut session = session_with(vec![text("hi")], dir.path()).await;
        session.run_user_message("hello").await.unwrap();

        let mut snapshot = TranscriptSnapshot::capture(&session);
        let first = snapshot.messages().as_ptr();
        snapshot.refresh(&session);
        assert_eq!(
            first,
            snapshot.messages().as_ptr(),
            "an unchanged transcript must not be copied again"
        );
    }

    /// ...and a changed one must be picked up, including the last message
    /// growing, which is what streaming looks like.
    #[tokio::test]
    async fn refreshing_picks_up_new_and_growing_messages() {
        let dir = tempdir().unwrap();
        let mut session = session_with(vec![text("one"), text("two")], dir.path()).await;
        session.run_user_message("first").await.unwrap();

        let mut snapshot = TranscriptSnapshot::capture(&session);
        let before = snapshot.messages().len();

        session.run_user_message("second").await.unwrap();
        snapshot.refresh(&session);
        assert!(
            snapshot.messages().len() > before,
            "a longer transcript must be picked up"
        );

        // Streaming appends to the last message rather than adding one.
        let grown = snapshot.messages().len();
        session
            .messages
            .last_mut()
            .unwrap()
            .content
            .push_str(" more");
        snapshot.refresh(&session);
        assert_eq!(snapshot.messages().len(), grown);
        assert!(
            snapshot
                .messages()
                .last()
                .unwrap()
                .content
                .ends_with(" more"),
            "a growing last message must be picked up"
        );
    }

    /// An empty transcript has the default fingerprint, so a naive equality
    /// check re-copies on every frame until the first message lands — which
    /// is exactly the idle case a frame budget cares about.
    #[tokio::test]
    async fn refreshing_an_empty_transcript_settles_after_the_first_call() {
        let dir = tempdir().unwrap();
        let session = session_with(vec![], dir.path()).await;
        let mut snapshot = TranscriptSnapshot::capture(&session);
        let first = snapshot.messages().as_ptr();
        snapshot.refresh(&session);
        snapshot.refresh(&session);
        assert_eq!(first, snapshot.messages().as_ptr());
    }

    /// `workspace_root` is fixed at construction and never reassigned, which
    /// is what makes it safe for callers to read from a snapshot taken at an
    /// arbitrary earlier moment — unlike the mutable fields, where a snapshot
    /// read after a mutation in the same tick would be stale.
    #[tokio::test]
    async fn workspace_root_is_stable_across_a_turn() {
        let dir = tempdir().unwrap();
        let mut session = session_with(vec![text("hi")], dir.path()).await;
        let before = SessionSnapshot::capture(&session);
        session.run_user_message("hello").await.unwrap();
        let after = SessionSnapshot::capture(&session);

        assert_eq!(before.workspace_root(), after.workspace_root());
        assert_eq!(before.workspace_root(), session.workspace_root());
    }

    /// A snapshot is a value: capturing the same unchanged session twice must
    /// produce equal snapshots, so a frontend can diff them to decide whether
    /// anything needs redrawing.
    #[tokio::test]
    async fn capturing_an_unchanged_session_twice_is_stable() {
        let dir = tempdir().unwrap();
        let session = session_with(vec![], dir.path()).await;
        assert_eq!(
            SessionSnapshot::capture(&session),
            SessionSnapshot::capture(&session)
        );
    }
}
