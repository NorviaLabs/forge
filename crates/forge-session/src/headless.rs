//! Drive one session turn without a terminal UI.
//!
//! This is the frontend used by automation and evaluation harnesses. It keeps
//! the agent loop, tool execution, persistence, and approval semantics in the
//! normal [`forge_core::AgentSession`] path; it only supplies the policy for a
//! tool approval when no human is attached.

use forge_core::AgentSession;
use forge_types::{ApprovalKind, GrantDecision, HitlPayload, ModelResponse};

/// What to do when a model-authored tool call reaches a human approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalPolicy {
    /// Stop and report the request. Nothing runs without a human.
    #[default]
    Ask,
    /// Deny every request and let the model see the denial as a tool result.
    DenyAll,
    /// Approve policy prompts and grant sandbox-denied network hosts for the
    /// session (the call re-runs still confined). Never approves running a
    /// command outside the sandbox: with no operator attached, that request
    /// is denied and the model sees why. Intended for an externally isolated
    /// evaluation workspace, not for an ordinary repository checkout.
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

/// Tool-result text the model sees when approve-all refuses an escalation.
pub const UNCONFINED_REFUSED_UNDER_APPROVE_ALL: &str = "this command needed to run outside the sandbox, which approve-all never approves in headless mode; it was not run. Find a way to do this inside the workspace sandbox.";

/// Run one user prompt through the normal session loop until it completes or
/// the configured approval policy stops it.
pub async fn run_headless(
    mut session: AgentSession,
    prompt: &str,
    policy: ApprovalPolicy,
) -> anyhow::Result<ModelResponse> {
    run_headless_in(&mut session, prompt, policy).await
}

/// [`run_headless`] on a borrowed session, so the caller can inspect it after.
async fn run_headless_in(
    session: &mut AgentSession,
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
    // Hosts approve-all already granted this turn. A grant that did not help
    // is not repeated; the second denial of the same host is refused.
    let mut granted_hosts = std::collections::HashSet::new();
    let mut response = None;
    for _ in 0..MAX_APPROVAL_ROUNDS {
        // An approved call can raise a follow-up approval itself (a host the
        // gateway refused it); answer that one before running the turn on.
        if response.is_none() || session.pending_hitl().is_none() {
            response = Some(
                session
                    .run_agent_turns(None)
                    .await
                    .map_err(|error| anyhow::anyhow!(error))?,
            );
        }
        let Some(payload) = session.pending_hitl().cloned() else {
            return Ok(response.expect("the turn ran this round"));
        };

        let decision = match policy {
            ApprovalPolicy::Ask => return Err(anyhow::Error::new(ApprovalRequired { payload })),
            ApprovalPolicy::DenyAll => GrantDecision::Deny,
            ApprovalPolicy::ApproveAll => match payload.kind() {
                // Escalation, including every command on a host that cannot
                // confine: approve-all never takes a command out of the sandbox.
                ApprovalKind::SandboxEscalation => {
                    GrantDecision::DenyWithNote(UNCONFINED_REFUSED_UNDER_APPROVE_ALL.into())
                }
                ApprovalKind::NetworkHost => {
                    let host = payload.denied_host.as_deref().unwrap_or_default();
                    if granted_hosts.insert(forge_tools::egress::suggest_host_pattern(host)) {
                        GrantDecision::AllowHostForSession
                    } else {
                        GrantDecision::DenyWithNote(
                            "the network host is still blocked after a session grant".into(),
                        )
                    }
                }
                ApprovalKind::Governance => GrantDecision::AllowOnce,
            },
        };
        session
            .decide_hitl(decision, "headless")
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
    use forge_tools::{Tool, ToolContext, ToolError};
    use forge_types::{ModelResponse, ToolCall};
    use forge_types::{SideEffectClass, ToolOutput};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

    /// Blocked by the sandbox once (network host or filesystem, per
    /// `denied_host`); records whether any run was unconfined.
    struct SandboxDeniedOnce {
        denied_host: Option<&'static str>,
        calls: AtomicU32,
        ran_unconfined: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for SandboxDeniedOnce {
        fn name(&self) -> &str {
            "sandboxed"
        }

        fn description(&self) -> &str {
            "blocked by the sandbox on its first confined run"
        }

        fn input_schema(&self) -> serde_json::Value {
            json!({"type": "object", "additionalProperties": false})
        }

        fn side_effect_class(&self) -> SideEffectClass {
            SideEffectClass::Exec
        }

        async fn call(
            &self,
            ctx: &ToolContext,
            _args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            if ctx.unconfined_shell {
                self.ran_unconfined.store(true, Ordering::SeqCst);
                return Ok(ToolOutput::success("ran unconfined"));
            }
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ToolError::SandboxDenied {
                    content: "Operation not permitted".into(),
                    reason: "blocked by the sandbox".into(),
                    denied_host: self.denied_host.map(Into::into),
                });
            }
            Ok(ToolOutput::success("ran confined"))
        }
    }

    async fn sandboxed_session(
        denied_host: Option<&'static str>,
        ran_unconfined: Arc<AtomicBool>,
    ) -> (AgentSession, TempDir) {
        let dir = tempdir().unwrap();
        let cfg = LoopConfig {
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("journal"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(SandboxDeniedOnce {
            denied_host,
            calls: AtomicU32::new(0),
            ran_unconfined,
        }));
        let script = vec![
            ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "s1".into(),
                    name: "sandboxed".into(),
                    arguments: json!({}),
                }],
                usage: None,
                thinking: None,
            },
            text("done"),
        ];
        let session = AgentSession::create(cfg, Arc::new(MockModelClient::script(script)), tools)
            .await
            .unwrap();
        (session, dir)
    }

    fn tool_result(session: &AgentSession) -> String {
        session
            .messages
            .iter()
            .rev()
            .find(|message| message.tool_call_id.as_deref() == Some("s1"))
            .expect("the call must have a tool result")
            .content
            .clone()
    }

    async fn run_approve_all(denied_host: Option<&'static str>) -> (String, bool, String) {
        let ran_unconfined = Arc::new(AtomicBool::new(false));
        let (mut session, _dir) = sandboxed_session(denied_host, ran_unconfined.clone()).await;
        let response = run_headless_in(&mut session, "go", ApprovalPolicy::ApproveAll)
            .await
            .unwrap();
        (
            tool_result(&session),
            ran_unconfined.load(Ordering::SeqCst),
            response.text,
        )
    }

    #[tokio::test]
    async fn approve_all_grants_a_denied_host_and_retries_confined() {
        let (result, ran_unconfined, final_text) = run_approve_all(Some("api.github.com")).await;
        assert_eq!(result, "ran confined");
        assert!(!ran_unconfined, "a host grant must keep the call sandboxed");
        assert_eq!(final_text, "done");
    }

    /// A gated call approved once, then refused a host by the gateway, raises
    /// the host prompt from inside the approval; approve-all answers it
    /// without running the turn on in between, and the grant re-runs the call.
    #[tokio::test]
    async fn approve_all_answers_a_host_prompt_raised_by_an_approved_call() {
        let ran_unconfined = Arc::new(AtomicBool::new(false));
        let (mut session, _dir) =
            sandboxed_session(Some("api.github.com"), ran_unconfined.clone()).await;
        session.set_governance(Governance::default().require_hitl_for_tool("sandboxed"));
        let response = run_headless_in(&mut session, "go", ApprovalPolicy::ApproveAll)
            .await
            .unwrap();
        assert_eq!(tool_result(&session), "ran confined");
        assert!(!ran_unconfined.load(Ordering::SeqCst));
        assert_eq!(response.text, "done");
    }

    #[tokio::test]
    async fn approve_all_refuses_to_run_outside_the_sandbox() {
        let (result, ran_unconfined, final_text) = run_approve_all(None).await;
        assert!(
            !ran_unconfined,
            "approve-all must never run a command unconfined"
        );
        assert!(
            result.contains(UNCONFINED_REFUSED_UNDER_APPROVE_ALL),
            "the model must see why the escalation was refused: {result}"
        );
        assert_eq!(final_text, "done");
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

    fn git_call(subcommand: &str, args: &[&str]) -> ModelResponse {
        // Call ids must be unique within one script: the session replays
        // journaled tool results by id (`try_serve_journaled_tool`), so two
        // calls sharing an id would serve the first result for the second.
        static NEXT_GIT_CALL_ID: AtomicU32 = AtomicU32::new(0);
        ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("g{}", NEXT_GIT_CALL_ID.fetch_add(1, Ordering::Relaxed)),
                name: "git".into(),
                arguments: json!({"subcommand": subcommand, "args": args}),
            }],
            usage: None,
            thinking: None,
        }
    }

    /// Create a disposable git repository with a committed file plus a dirty
    /// working tree: a modified tracked file and untracked files, so both
    /// destructive git forms have real work to do.
    async fn dirty_git_repo(dir: &std::path::Path) {
        async fn git(dir: &std::path::Path, args: &[&str]) {
            let status = tokio::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .await
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
        git(dir, &["init", "-q"]).await;
        git(dir, &["config", "user.email", "forge@test"]).await;
        git(dir, &["config", "user.name", "Forge Test"]).await;
        std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        git(dir, &["add", "tracked.txt"]).await;
        git(dir, &["commit", "-q", "-m", "init"]).await;
        // Dirty working tree: modify the tracked file and drop in untracked files.
        std::fs::write(dir.join("tracked.txt"), "two\n").unwrap();
        std::fs::create_dir_all(dir.join("tmp")).unwrap();
        std::fs::write(dir.join("tmp/scratch.txt"), "scratch\n").unwrap();
        std::fs::write(dir.join("note.txt"), "untracked\n").unwrap();
    }

    async fn dirty_git_session(script: Vec<ModelResponse>) -> (AgentSession, TempDir) {
        let dir = tempdir().unwrap();
        dirty_git_repo(dir.path()).await;
        let cfg = LoopConfig {
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
        // Default policy: destructive git is gated; other git stays ungated.
        session.set_governance(Governance::default());
        (session, dir)
    }

    /// Reproduces #449: an explicit `git reset --hard` request must surface as
    /// an approval prompt rather than a silent block, and must name the
    /// destructive git call. No approval -> nothing runs.
    #[tokio::test]
    async fn destructive_git_asks_for_approval_and_runs_nothing_without_it() {
        let (session, dir) = dirty_git_session(vec![git_call("reset", &["--hard", "HEAD"])]).await;
        let error = run_headless(session, "reset the working tree", ApprovalPolicy::Ask)
            .await
            .unwrap_err();
        match error.downcast_ref::<ApprovalRequired>() {
            Some(approval) => {
                assert_eq!(approval.payload.tool, "git");
                let args = approval.payload.args_redacted["args"].as_array().unwrap();
                assert!(
                    args.iter().any(|a| a == "--hard"),
                    "the approval must name the destructive reset: {:?}",
                    args
                );
            }
            None => panic!("expected an approval for destructive git, got {error:?}"),
        }
        // Nothing ran: the dirty working tree is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "two\n",
            "the tracked edit must survive an unapproved reset"
        );
        assert!(dir.path().join("tmp/scratch.txt").exists());
    }

    /// Once approved, `git reset --hard` and `git clean -f -d` run for real and
    /// the debris the issue describes is gone: the tracked edit is reverted and
    /// the untracked files are removed.
    #[tokio::test]
    async fn approved_destructive_git_resets_and_cleans_the_workspace() {
        let (session, dir) = dirty_git_session(vec![
            git_call("reset", &["--hard", "HEAD"]),
            git_call("clean", &["-f", "-d"]),
            text("done"),
        ])
        .await;
        run_headless(session, "reset and clean", ApprovalPolicy::ApproveAll)
            .await
            .unwrap();
        // Tracked edit reverted, untracked debris gone, tree clean.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "one\n",
            "approved reset --hard must revert the tracked edit"
        );
        assert!(
            !dir.path().join("tmp").exists() && !dir.path().join("note.txt").exists(),
            "approved clean -f -d must remove untracked files"
        );
        let status = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "the workspace must be clean, got: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    /// A denied destructive git request is surfaced to the model as a visible
    /// tool result (not silently swallowed), so the model cannot fabricate a
    /// false "already clean" summary, and the workspace stays untouched.
    #[tokio::test]
    async fn denied_destructive_git_surfaces_the_block_and_changes_nothing() {
        let (session, dir) = dirty_git_session(vec![
            git_call("reset", &["--hard", "HEAD"]),
            text("denied: git reset --hard HEAD was refused"),
        ])
        .await;
        let response = run_headless(session, "reset hard", ApprovalPolicy::DenyAll)
            .await
            .unwrap();
        // The final answer reflects the denial that was fed back as a tool result.
        assert!(
            response.text.contains("denied") && response.text.contains("reset --hard"),
            "the model must see and report the denial, got: {}",
            response.text
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "two\n",
            "a denied reset must not change the working tree"
        );
        assert!(dir.path().join("tmp/scratch.txt").exists());
    }
}
