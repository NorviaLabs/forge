//! An immutable, per-frame view of session state for frontends to render from.
//!
//! Rendering used to read the live `AgentSession` directly, which coupled the
//! UI to the session's ownership: as long as a frame reads `session.foo()`,
//! whoever draws must own the session outright. A snapshot breaks that — the
//! frame reads a value, and where that value came from stops mattering.
//!
//! It is also cheaper. The reads it replaces are not all field accesses:
//! `loaded_skills_count` clones a `Vec<SkillManifest>` to take its length,
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

use forge_core::AgentSession;
use forge_types::{HitlPayload, SessionId, TaskLifecycle};

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
            background_len: session.background().list().count(),
            workspace_root: session.workspace_root().to_path_buf(),
            tool_count: session.tool_count(),
            loaded_skills_count: session.loaded_skills_count(),
            context_usage_ratio: session.context_usage_ratio(),
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
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        }
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
