//! Drive one session turn without a terminal UI.
//!
//! This is the frontend used by automation and evaluation harnesses. It keeps
//! the agent loop, tool execution, persistence, and approval semantics in the
//! normal [`forge_core::AgentSession`] path; it only supplies the policy for a
//! tool approval when no human is attached.

use forge_core::AgentSession;
use forge_types::{HitlDecision, HitlPayload, ModelResponse};

/// What to do when a model-authored tool call reaches a human approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalPolicy {
    /// Stop and report the request. Nothing runs without a human.
    #[default]
    Ask,
    /// Deny every request and let the model see the denial as a tool result.
    DenyAll,
    /// Approve every request. Intended for an externally isolated evaluation
    /// workspace, not for an ordinary repository checkout.
    ApproveAll,
}

/// A headless turn stopped because it requires an approval that the caller did
/// not authorize automatically.
#[derive(Debug)]
pub struct ApprovalRequired {
    pub payload: HitlPayload,
}

impl std::fmt::Display for ApprovalRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "approval required for {}: {}",
            self.payload.tool, self.payload.reason
        )
    }
}

impl std::error::Error for ApprovalRequired {}

/// Run one user prompt through the normal session loop until it completes or
/// the configured approval policy stops it.
pub async fn run_headless(
    mut session: AgentSession,
    prompt: &str,
    policy: ApprovalPolicy,
) -> anyhow::Result<ModelResponse> {
    session
        .append_user_message(prompt)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

    // A model can request a sequence of approvals in one user turn. The core
    // bounds denial loops, but approving an agent-authored loop needs a
    // separate backstop so an unattended benchmark cannot run forever.
    const MAX_APPROVAL_ROUNDS: usize = 64;
    for _ in 0..MAX_APPROVAL_ROUNDS {
        let response = session
            .run_agent_turns(None)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(payload) = session.pending_hitl().cloned() else {
            return Ok(response);
        };

        let decision = match policy {
            ApprovalPolicy::Ask => return Err(anyhow::Error::new(ApprovalRequired { payload })),
            ApprovalPolicy::DenyAll => HitlDecision::Deny,
            ApprovalPolicy::ApproveAll => HitlDecision::Approve,
        };
        session
            .resolve_hitl(decision, "headless")
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
    }

    anyhow::bail!("stopped after {MAX_APPROVAL_ROUNDS} approval rounds in a single headless turn")
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_governance::Governance;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::{ModelResponse, ToolCall};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    fn text(body: &str) -> ModelResponse {
        ModelResponse {
            text: body.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    fn wants_approval() -> ModelResponse {
        ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }
    }

    async fn session_with(script: Vec<ModelResponse>) -> (AgentSession, TempDir) {
        let dir = tempdir().unwrap();
        let cfg = LoopConfig {
            max_turns: 5,
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("journal"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        let mut session = AgentSession::create(
            cfg,
            Arc::new(MockModelClient::script(script)),
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        session.set_governance(Governance::default().require_hitl_for_tool("bash"));
        (session, dir)
    }

    #[tokio::test]
    async fn completes_a_plain_prompt() {
        let (session, _dir) = session_with(vec![text("done")]).await;
        let response = run_headless(session, "hello", ApprovalPolicy::Ask)
            .await
            .unwrap();
        assert_eq!(response.text, "done");
    }

    #[tokio::test]
    async fn ask_policy_reports_the_pending_tool_without_running_it() {
        let (session, _dir) = session_with(vec![wants_approval()]).await;
        let error = run_headless(session, "push", ApprovalPolicy::Ask)
            .await
            .unwrap_err();
        let approval = error.downcast_ref::<ApprovalRequired>().unwrap();
        assert_eq!(approval.payload.tool, "bash");
        assert!(approval.payload.reason.contains("approval"));
    }

    #[tokio::test]
    async fn deny_policy_resumes_the_same_turn() {
        let (session, _dir) = session_with(vec![wants_approval(), text("adapted")]).await;
        let response = run_headless(session, "push", ApprovalPolicy::DenyAll)
            .await
            .unwrap();
        assert_eq!(response.text, "adapted");
    }
}
