//! A session whose tools run on an SSH host.
//!
//! No SSH here: a fake remote target stands in for the host, and the tools
//! record which target they were handed. What is tested is the session's
//! side: who asks, what an approval names and offers, what the model is
//! told, and what a child agent inherits. `forge-tools/tests/ssh_target.rs`
//! covers the target itself against a real `sshd`.

use super::*;
use crate::session::grant::{validate_grant_decision, GrantRejection};
use async_trait::async_trait;
use forge_governance::{parse_pattern_rules, Governance};
use forge_model::MockModelClient;
use forge_tools::shell_spawn::ShellSpawnRequest;
use forge_tools::target::{Access, DirEntry, FileMeta, Target, TargetPath, TargetShell};
use forge_tools::AgentMode;
use forge_types::{ExecutionTarget, GrantDecision, SideEffectClass, ToolCall, ToolOutput};
use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;

/// Stands in for `devbox:/srv/app`. The fake tools below only read its
/// label; anything else reaching it is a bug.
#[derive(Debug)]
struct FakeDevbox;

fn unreachable_here<T>() -> Result<T, ToolError> {
    Err(ToolError::Execution(
        "the fake devbox was asked directly".into(),
    ))
}

#[async_trait]
impl Target for FakeDevbox {
    fn remote_host(&self) -> Option<&str> {
        Some("devbox")
    }
    fn label(&self) -> String {
        "devbox:/srv/app".into()
    }
    async fn shell(&self, _: ShellSpawnRequest<'_>) -> Result<TargetShell, ToolError> {
        unreachable_here()
    }
    async fn resolve(&self, _: &ToolContext, _: &str, _: Access) -> Result<TargetPath, ToolError> {
        unreachable_here()
    }
    async fn metadata(&self, _: &TargetPath) -> Result<Option<FileMeta>, ToolError> {
        unreachable_here()
    }
    async fn read(&self, _: &TargetPath, _: u64) -> Result<Vec<u8>, ToolError> {
        unreachable_here()
    }
    async fn read_lines(
        &self,
        _: &TargetPath,
        _: u64,
        _: Option<u64>,
    ) -> Result<String, ToolError> {
        unreachable_here()
    }
    async fn write(&self, _: &TargetPath, _: &[u8]) -> Result<(), ToolError> {
        unreachable_here()
    }
    async fn remove(&self, _: &TargetPath) -> Result<(), ToolError> {
        unreachable_here()
    }
    async fn list(&self, _: &TargetPath) -> Result<Vec<DirEntry>, ToolError> {
        unreachable_here()
    }
}

type Seen = Arc<Mutex<Vec<String>>>;

/// A tool that reports, and records, the target it was run on.
struct Recording {
    name: &'static str,
    class: SideEffectClass,
    seen: Seen,
}

#[async_trait]
impl forge_tools::Tool for Recording {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "records its target"
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn side_effect_class(&self) -> SideEffectClass {
        self.class
    }
    async fn call(&self, ctx: &ToolContext, _: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let label = ctx.target.label();
        self.seen
            .lock()
            .unwrap()
            .push(format!("{}@{label}", self.name));
        Ok(ToolOutput::success(format!("ran on {label}")))
    }
}

fn devbox(ask: bool) -> ExecutionTarget {
    ExecutionTarget::Ssh {
        host: "devbox".into(),
        path: "/srv/app".into(),
        ask,
    }
}

fn cfg(dir: &std::path::Path) -> LoopConfig {
    LoopConfig {
        workspace: dir.to_path_buf(),
        journal_dir: dir.join("j"),
        enable_context_lifecycle: true,
        enable_governance: true,
        ..Default::default()
    }
}

fn tools(seen: &Seen) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    for (name, class) in [
        ("bash", SideEffectClass::Exec),
        ("exec_command", SideEffectClass::Exec),
        ("background_run", SideEffectClass::Exec),
        ("git", SideEffectClass::Write),
        ("read_file", SideEffectClass::Read),
        ("write_file", SideEffectClass::Write),
    ] {
        tools.register(Arc::new(Recording {
            name,
            class,
            seen: seen.clone(),
        }));
    }
    tools.register(
        forge_tools::default_builtins()
            .into_iter()
            .find(|tool| tool.name() == "request_unconfined_retry")
            .unwrap(),
    );
    tools
}

/// A session on the fake devbox. Its local workspace is an empty directory,
/// as it is for a real SSH session.
async fn remote_session(
    dir: &std::path::Path,
    ask: bool,
    script: Vec<ModelResponse>,
) -> (AgentSession, Seen) {
    let seen = Seen::default();
    let model = Arc::new(MockModelClient::script(script));
    let mut session = AgentSession::create(cfg(dir), model, tools(&seen))
        .await
        .unwrap();
    session.set_execution_target(devbox(ask), Arc::new(FakeDevbox));
    session
        .append_user_message("work on the server")
        .await
        .unwrap();
    (session, seen)
}

fn calling(id: &str, name: &str, arguments: serde_json::Value) -> ModelResponse {
    ModelResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }],
        usage: None,
        thinking: None,
    }
}

fn text(text: &str) -> ModelResponse {
    ModelResponse {
        text: text.into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }
}

/// Apply one call: `true` when it ran, `false` when it stopped to ask.
async fn apply(session: &mut AgentSession, id: &str, name: &str, args: serde_json::Value) -> bool {
    match session
        .begin_model_response_application(calling(id, name, args))
        .await
        .unwrap()
    {
        ModelResponseApplication::Execute(pending) => {
            let completed = pending.execute().await;
            session.finish_tool_application(completed).await.unwrap();
            true
        }
        ModelResponseApplication::Finished(ApplyOutcome::Hitl(_)) => false,
        ModelResponseApplication::Finished(_) => panic!("{name} neither ran nor asked"),
    }
}

fn result_of(session: &AgentSession, id: &str) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.tool_call_id.as_deref() == Some(id))
        .map(|message| message.content.clone())
        .unwrap_or_else(|| panic!("no result for {id}"))
}

#[tokio::test]
async fn each_command_asks_and_the_approval_names_the_host() {
    for (name, args) in [
        ("bash", json!({"command": "cargo test"})),
        ("exec_command", json!({"cmd": "ls"})),
        ("background_run", json!({"command": "make"})),
        ("git", json!({"subcommand": "status", "args": []})),
    ] {
        let dir = tempdir().unwrap();
        let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
        assert!(
            !apply(&mut session, "c1", name, args).await,
            "{name} must ask first"
        );
        let payload = session.pending_hitl().expect("an approval");
        assert_eq!(payload.target.as_deref(), Some("devbox"), "{name}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "{name} ran before approval"
        );
    }
}

#[tokio::test]
async fn an_approved_command_runs_on_the_host() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
    assert!(!apply(&mut session, "c1", "bash", json!({"command": "uname -a"})).await);
    session
        .decide_hitl(GrantDecision::AllowOnce, "test")
        .await
        .unwrap();
    assert_eq!(result_of(&session, "c1"), "ran on devbox:/srv/app");
    assert_eq!(*seen.lock().unwrap(), ["bash@devbox:/srv/app"]);
}

#[tokio::test]
async fn a_remote_command_is_allowed_for_the_session_at_most() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
    assert!(
        !apply(
            &mut session,
            "c1",
            "bash",
            json!({"command": "cargo test --all"})
        )
        .await
    );
    let payload = session.pending_hitl().unwrap().clone();
    assert_eq!(
        validate_grant_decision(&payload, &GrantDecision::AllowPatternAlways),
        Err(GrantRejection::NotKeptForRemote)
    );
    for offered in [
        GrantDecision::AllowOnce,
        GrantDecision::AllowPatternForSession,
        GrantDecision::ApproveAll,
        GrantDecision::Deny,
    ] {
        assert_eq!(
            validate_grant_decision(&payload, &offered),
            Ok(()),
            "{offered:?}"
        );
    }
    assert!(session
        .decide_hitl(GrantDecision::AllowPatternAlways, "test")
        .await
        .is_err());
    assert!(
        session.pending_hitl().is_some(),
        "a refused decision leaves it pending"
    );

    let outcome = session
        .decide_hitl(GrantDecision::AllowPatternForSession, "test")
        .await
        .unwrap();
    assert!(
        outcome.persisted_rule.is_none(),
        "nothing written to the permissions file"
    );
    assert!(
        apply(
            &mut session,
            "c2",
            "bash",
            json!({"command": "cargo test --workspace"})
        )
        .await,
        "the session rule covers the next one"
    );
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn with_asking_off_commands_run_without_a_prompt() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), false, vec![]).await;
    assert!(apply(&mut session, "c1", "bash", json!({"command": "make"})).await);
    assert!(
        apply(
            &mut session,
            "c2",
            "git",
            json!({"subcommand": "status", "args": []})
        )
        .await
    );
    assert_eq!(result_of(&session, "c1"), "ran on devbox:/srv/app");
    assert_eq!(
        *seen.lock().unwrap(),
        ["bash@devbox:/srv/app", "git@devbox:/srv/app"]
    );
}

#[tokio::test]
async fn file_tools_never_ask_and_run_on_the_host() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
    assert!(apply(&mut session, "r", "read_file", json!({"path": "a"})).await);
    assert!(
        apply(
            &mut session,
            "w",
            "write_file",
            json!({"path": "a", "content": ""})
        )
        .await
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["read_file@devbox:/srv/app", "write_file@devbox:/srv/app"]
    );
}

#[tokio::test]
async fn personal_rules_do_not_cover_the_host_whenever_they_are_set() {
    let rules =
        || Governance::default().with_pattern_rules(parse_pattern_rules(&["bash(*)"]), vec![]);
    // Settings reload replaces the governance after the target is set.
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
    session.set_governance(rules());
    assert!(!apply(&mut session, "c1", "bash", json!({"command": "ls"})).await);
    assert_eq!(
        session.pending_hitl().unwrap().target.as_deref(),
        Some("devbox")
    );

    // And set before it.
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut session = AgentSession::create(cfg(dir.path()), model, tools(&seen))
        .await
        .unwrap();
    session.set_governance(rules());
    session.set_execution_target(devbox(true), Arc::new(FakeDevbox));
    session.append_user_message("go").await.unwrap();
    assert!(!apply(&mut session, "c1", "bash", json!({"command": "ls"})).await);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn there_is_no_sandbox_to_retry_outside_of() {
    let dir = tempdir().unwrap();
    let (mut session, _) = remote_session(dir.path(), true, vec![]).await;
    assert!(session.list_tools().contains(&"bash".to_string()));
    assert!(
        !session
            .list_tools()
            .contains(&"request_unconfined_retry".to_string()),
        "{:?}",
        session.list_tools()
    );
    let application = session
        .begin_model_response_application(calling(
            "retry",
            "request_unconfined_retry",
            json!({"retry_of": "c0", "reason": "needs it"}),
        ))
        .await
        .unwrap();
    assert!(
        !matches!(
            application,
            ModelResponseApplication::Finished(ApplyOutcome::Hitl(_))
        ),
        "never an escalation prompt"
    );
    assert!(session.pending_hitl().is_none());
    assert!(result_of(&session, "retry").contains("devbox"));
}

#[tokio::test]
async fn the_model_is_told_where_its_commands_run() {
    let dir = tempdir().unwrap();
    let (session, _) = remote_session(dir.path(), true, vec![]).await;
    let system = &session.messages[0].content;
    assert!(system.contains("devbox"), "{system}");
    assert!(system.contains("/srv/app"), "{system}");
    assert!(
        system.contains("$TMPDIR"),
        "the scratch directory is the host's"
    );
    let local_tmp = session
        .tool_ctx
        .session_tmp
        .as_ref()
        .unwrap()
        .path()
        .display()
        .to_string();
    assert!(
        !system.contains(&local_tmp),
        "no local path is offered to the model"
    );
    assert!(system.contains("outside Forge's sandbox"), "{system}");
    assert_eq!(session.execution_target(), &devbox(true));
}

async fn wait_for_approval(
    session: &mut AgentSession,
    id: forge_types::BackgroundTaskId,
) -> forge_types::HitlPayload {
    for _ in 0..300 {
        session.poll_background_tasks().await.unwrap();
        if let Some(task) = session.background().get(id) {
            if let BackgroundTaskStatus::WaitingForApproval { payload } = &task.status {
                return payload.clone();
            }
            assert!(!task.status.is_terminal(), "{:?}", task.status);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the child never asked");
}

fn spec(mode: AgentMode) -> crate::subagent::SubagentSpec {
    crate::subagent::SubagentSpec {
        role: "helper".into(),
        prompt: "look around".into(),
        mode,
        tool_allowlist: None,
        profile: Default::default(),
    }
}

#[tokio::test]
async fn a_writer_child_is_refused_on_a_remote_session() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(dir.path(), true, vec![]).await;
    let error = session
        .spawn_subagent(spec(AgentMode::Writer))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("devbox"), "{error}");
    assert!(error.contains("read-only"), "says what does work: {error}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_read_only_child_runs_on_the_same_host_and_asks_the_same_way() {
    let dir = tempdir().unwrap();
    let (mut session, seen) = remote_session(
        dir.path(),
        true,
        vec![
            calling("k1", "bash", json!({"command": "ls"})),
            text("looked"),
        ],
    )
    .await;
    let id = session
        .spawn_subagent(spec(AgentMode::ReadOnly))
        .await
        .expect("a read-only child needs no checkout of its own");
    let payload = wait_for_approval(&mut session, id).await;
    assert_eq!(payload.target.as_deref(), Some("devbox"));
    assert!(seen.lock().unwrap().is_empty());
    assert!(session
        .background_control()
        .resolve_hitl(id, forge_types::HitlDecision::Approve));
    for _ in 0..300 {
        session.poll_background_tasks().await.unwrap();
        if session.background().get(id).unwrap().status.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*seen.lock().unwrap(), ["bash@devbox:/srv/app"]);
}
