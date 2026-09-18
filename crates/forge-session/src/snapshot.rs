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

use chrono::{DateTime, Utc};
use forge_core::{
    AgentSession, BackgroundTaskHandle, CompactionTelemetry, QueuedTask, SessionContextState,
    TokenUsageReport, TurnEvent,
};
use forge_model::SharedMessages;
use forge_types::{HitlPayload, Message, QuestionPayload, SessionId, TaskLifecycle};

/// What a frontend needs to draw one frame, minus the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    /// Authoritative task state. Frontends must render this rather than
    /// deriving their own from busy/streaming flags.
    pub lifecycle: TaskLifecycle,
    /// Per-session approve-all mode: HITL auto-approved, shell unconfined.
    pub approve_all: bool,
    /// The outstanding approval request, if the session is waiting on one.
    pub pending_hitl: Option<HitlPayload>,
    /// The outstanding `ask_user_question` request, if the session is waiting
    /// on answers.
    pub pending_question: Option<QuestionPayload>,
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
            approve_all: session.approve_all(),
            pending_hitl: session.pending_hitl().cloned(),
            pending_question: session.pending_question().cloned(),
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

    pub fn is_awaiting_question(&self) -> bool {
        self.pending_question.is_some()
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
            approve_all: false,
            pending_hitl: None,
            pending_question: None,
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

/// Detail needed by operator actions and expanded status surfaces.
///
/// Kept separate from `SessionSnapshot`: the cheap frame snapshot is captured
/// frequently in legacy direct mode, while these vectors/reports are only
/// refreshed at supervisor actor checkpoints.
#[derive(Debug, Clone)]
pub struct SessionDetailsSnapshot {
    pub journal_dir: PathBuf,
    pub active_model: String,
    pub active_route_id: String,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: bool,
    pub image_input_supported: bool,
    pub token_usage_report: TokenUsageReport,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub skill_descriptions: Vec<(String, String)>,
    pub context_state: SessionContextState,
    pub compaction: CompactionTelemetry,
    pub queue: Vec<QueuedTask>,
    pub background: Vec<BackgroundTaskSnapshot>,
    pub session_pattern_allow_count: usize,
}

impl SessionDetailsSnapshot {
    pub fn capture(session: &AgentSession) -> Self {
        Self {
            journal_dir: session.journal_dir().to_path_buf(),
            skill_descriptions: session
                .loaded_skills()
                .into_iter()
                .map(|skill| (skill.name, skill.description))
                .collect(),
            active_model: session.active_model.clone(),
            active_route_id: session.active_route_id.clone(),
            reasoning_effort: session.reasoning_effort().map(str::to_string),
            thinking_enabled: session.thinking_enabled(),
            image_input_supported: session.image_input_supported(),
            token_usage_report: session.token_usage_report(),
            tools: session.list_tools(),
            skills: session.loaded_skill_names(),
            context_state: session.context_state().clone(),
            compaction: session.compaction_telemetry().clone(),
            queue: session.queue().visible().cloned().collect(),
            background: session
                .background()
                .list()
                .map(BackgroundTaskSnapshot::capture)
                .collect(),
            session_pattern_allow_count: session.session_pattern_allow_count(),
        }
    }
}

/// Background presentation data, with no cancellation token or shared mutable state.
#[derive(Debug, Clone)]
pub struct BackgroundTaskSnapshot {
    pub id: forge_types::BackgroundTaskId,
    pub label: String,
    pub kind: forge_core::BackgroundTaskKind,
    pub status: forge_core::BackgroundTaskStatus,
    pub child_session_id: Option<SessionId>,
    pub latest_message: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub worktree_branch: Option<String>,
    /// When the task was spawned. Feeds the strip's live elapsed column.
    pub started_at: DateTime<Utc>,
    /// When the task reached a terminal status. `None` while it is still
    /// running, and the clock the strip's row-expiry timer reads — a finished
    /// row ages from when it *finished*, never from when it started.
    pub finished_at: Option<DateTime<Utc>>,
}

impl BackgroundTaskSnapshot {
    pub fn capture(task: &BackgroundTaskHandle) -> Self {
        Self {
            id: task.id,
            label: task.label.clone(),
            kind: task.kind.clone(),
            status: task.status.clone(),
            child_session_id: task.child_session_id,
            latest_message: task
                .latest_message
                .lock()
                .ok()
                .and_then(|message| message.clone()),
            worktree_path: task.worktree_path.clone(),
            worktree_branch: task.worktree_branch.clone(),
            started_at: task.started_at,
            finished_at: task.finished_at,
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
    messages: SharedMessages,
    events: Arc<[TurnEvent]>,
    fingerprint: Option<TranscriptFingerprint>,
    revision: u64,
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
    messages_storage_id: u64,
    messages: usize,
    events: usize,
    last_message_content: usize,
    last_message_thinking: usize,
    last_event_detail: usize,
}

impl TranscriptFingerprint {
    fn of(session: &AgentSession) -> Self {
        Self {
            messages_storage_id: session.messages.storage_id(),
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
        // `SharedMessages` is copy-on-write. The live session clones its
        // storage only when a snapshot still holds the previous version, so a
        // refresh shares the new allocation instead of cloning every message.
        self.messages = session.messages.shared();
        self.events = Arc::from(session.events.as_slice());
        self.fingerprint = Some(fingerprint);
        self.revision = self.revision.wrapping_add(1);
    }

    /// A snapshot taken now, for callers outside a frame.
    pub fn capture(session: &AgentSession) -> Self {
        let mut snapshot = Self::default();
        snapshot.refresh(session);
        snapshot
    }

    /// A transcript for a session this process does not own, re-projected from
    /// a journal replay.
    ///
    /// This is the read-only path for looking at a background subagent's
    /// session. The child owns that session and may still be writing to it, so
    /// the TUI must not open a second runtime against it — `open_session`
    /// builds a runtime, it does not merely read. Replaying the journal is a
    /// second *reader*, which is what the durable store's WAL mode permits.
    ///
    /// Two consequences of coming from a replay rather than a live session:
    ///
    /// - **`events` is empty.** `TurnEvent`s are not replayed, so this shows
    ///   the conversation without the live activity rows the owning session
    ///   carries. That is the right trade for a view that cannot act.
    /// - **`revision` is the caller's**, and the caller must pass a value
    ///   distinct from the snapshot it is replacing: the conversation render
    ///   cache keys on revision, so reusing the parent's would serve the
    ///   parent's rendered lines for the child.
    pub fn from_messages(messages: Vec<Message>, revision: u64) -> Self {
        Self {
            messages: SharedMessages::from(messages),
            events: Arc::from(Vec::<TurnEvent>::new()),
            // Never refreshed from a session, so there is no fingerprint to
            // compare against and no reason to copy-on-write.
            fingerprint: None,
            revision,
        }
    }

    pub fn messages(&self) -> &[Message] {
        self.messages.as_slice()
    }

    pub fn events(&self) -> &[TurnEvent] {
        &self.events
    }

    /// Monotonic identity for the current transcript projection inputs.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// A read-only transcript for a session this process does not own, rebuilt from
/// its journal.
///
/// The TUI's view of a background subagent's session. Read-only by
/// construction: this replays the journal rather than opening a runtime, so it
/// cannot become a second writer against a session the child still owns.
///
/// `revision` identifies the projection and must differ from the snapshot the
/// caller is replacing — see [`TranscriptSnapshot::from_messages`].
pub async fn replayed_transcript(
    journal_dir: &std::path::Path,
    session_id: SessionId,
    revision: u64,
) -> Option<TranscriptSnapshot> {
    let messages = forge_core::session_messages(journal_dir, session_id).await?;
    Some(TranscriptSnapshot::from_messages(messages, revision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::{Message, MessageRole, ModelResponse, TaskId, ToolCall};
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

    /// The read-only view a child session is rendered from: a replay's
    /// messages, no live events, and a revision the caller controls.
    #[test]
    fn a_replayed_transcript_carries_messages_and_no_events() {
        let messages = vec![
            Message::new(MessageRole::User, "audit the auth deps"),
            Message::new(MessageRole::Assistant, "checking the manifest"),
        ];

        let snapshot = TranscriptSnapshot::from_messages(messages, 9);

        assert_eq!(snapshot.messages().len(), 2);
        assert_eq!(snapshot.messages()[1].content, "checking the manifest");
        assert!(
            snapshot.events().is_empty(),
            "a replay carries no live turn events"
        );
        assert_eq!(
            snapshot.revision(),
            9,
            "the caller owns the revision so it can differ from the parent's"
        );
    }

    /// An in-flight child has no assistant message yet, and the view still has
    /// to render — an empty transcript, not a panic.
    #[test]
    fn a_replayed_transcript_can_be_empty() {
        let snapshot = TranscriptSnapshot::from_messages(Vec::new(), 1);

        assert!(snapshot.messages().is_empty());
        assert!(snapshot.events().is_empty());
    }

    /// The strip ages a finished row from `finished_at` and counts a running
    /// one up from `started_at`, so `capture` has to carry both — the handle
    /// has, and the snapshot used to drop them.
    #[test]
    fn capture_carries_timestamps_for_the_strip() {
        let started_at = Utc::now() - chrono::Duration::seconds(90);
        let finished_at = started_at + chrono::Duration::seconds(31);
        let handle = BackgroundTaskHandle {
            id: forge_types::BackgroundTaskId(7),
            parent_task_id: TaskId(3),
            kind: forge_core::BackgroundTaskKind::Shell {
                command: "cargo clippy --all-targets".into(),
            },
            label: "cargo clippy --all-targets".into(),
            status: forge_core::BackgroundTaskStatus::Succeeded {
                summary: "clean".into(),
            },
            started_at,
            finished_at: Some(finished_at),
            cancel: tokio_util::sync::CancellationToken::new(),
            child_session_id: None,
            latest_message: Arc::new(std::sync::Mutex::new(None)),
            worktree_path: None,
            worktree_branch: None,
            auto_continue_on_completion: false,
        };

        let snapshot = BackgroundTaskSnapshot::capture(&handle);

        assert_eq!(snapshot.started_at, started_at);
        assert_eq!(snapshot.finished_at, Some(finished_at));
    }

    /// A running task has no `finished_at`, and the strip must read that as
    /// "still counting up" rather than "finished long ago".
    #[test]
    fn capture_leaves_finished_at_open_while_a_task_runs() {
        let handle = BackgroundTaskHandle {
            id: forge_types::BackgroundTaskId(1),
            parent_task_id: TaskId(1),
            kind: forge_core::BackgroundTaskKind::Subagent {
                role: "explore".into(),
                prompt: "find auth code".into(),
            },
            label: "explore".into(),
            status: forge_core::BackgroundTaskStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            child_session_id: Some(SessionId::new_v4()),
            latest_message: Arc::new(std::sync::Mutex::new(Some("reading".into()))),
            worktree_path: None,
            worktree_branch: None,
            auto_continue_on_completion: false,
        };

        let snapshot = BackgroundTaskSnapshot::capture(&handle);

        assert_eq!(snapshot.finished_at, None);
        assert_eq!(snapshot.latest_message.as_deref(), Some("reading"));
    }

    async fn session_with(script: Vec<ModelResponse>, dir: &Path) -> AgentSession {
        let cfg = LoopConfig {
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
        session
            .set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));
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
        let revision = snapshot.revision();
        snapshot.refresh(&session);
        assert_eq!(
            first,
            snapshot.messages().as_ptr(),
            "an unchanged transcript must not be copied again"
        );
        assert_eq!(snapshot.revision(), revision);
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
        let revision = snapshot.revision();

        session.run_user_message("second").await.unwrap();
        snapshot.refresh(&session);
        assert!(snapshot.revision() > revision);
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
