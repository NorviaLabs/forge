//! Custom agents through real sessions: the `# Agents` list and its
//! warnings, the model starting an agent by name, the child's tool limit,
//! network and model, and the profile surviving a resume.
//!
//! Hermetic: agent files are written into each test's own project, and a
//! session reads user agents only from a directory it is given, which these
//! tests never point at the developer's own.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_model::{MockModelClient, ModelClient, ModelError, ModelRequest};
use forge_tools::{AgentMode, SpawnAgentArgs, ToolRegistry};
use forge_types::{BackgroundTaskId, MessageRole, ModelResponse, TaskLifecycle, ToolCall};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::{AgentSession, ApplyOutcome, BackgroundTaskStatus, LoopConfig};

const REVIEWER: &str = "---\nname: code-reviewer\ndescription: Reviews the current diff for correctness.\nmode: [read-only]\ntools: [read_file, grep, edit]\n---\nROLE-MARKER: review carefully and report findings.\n";

const FIXER: &str = "---\nname: test-fixer\ndescription: Fixes failing tests.\nmode: [write, exec]\ntools: [read_file, edit, bash, web_fetch]\n---\nFix the tests.\n";

const RESEARCHER: &str = "---\nname: researcher\ndescription: Reads documentation on the web.\nmode: [network]\ntools: [read_file, web_fetch, edit]\n---\nResearch.\n";

fn write_agent(project: &Path, name: &str, text: &str) {
    let dir = project.join(".forge/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "forge@example.com"]);
    git(dir, &["config", "user.name", "Forge Test"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn cfg(dir: &Path) -> LoopConfig {
    LoopConfig {
        workspace: dir.to_path_buf(),
        journal_dir: dir.join(".forge/local/journal"),
        enable_context_lifecycle: true,
        enable_governance: true,
        ..Default::default()
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

fn system_prompt(session: &AgentSession) -> String {
    let first = session.messages.iter().next().unwrap();
    assert_eq!(first.role, MessageRole::System);
    first.content.clone()
}

fn offered(session: &AgentSession) -> Vec<String> {
    let mut names: Vec<String> = session
        .tools_for_model()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.sort();
    names
}

fn spawn_args(value: serde_json::Value) -> SpawnAgentArgs {
    serde_json::from_value(value).unwrap()
}

/// Answers by who is asking: a child (its system prompt has a role section)
/// gets a short text reply; the parent calls `spawn_agent` once, then
/// finishes. Every request is recorded, so a test can see exactly what the
/// child was sent. Deterministic however the parent and child interleave.
struct RoutingModel {
    spawn: serde_json::Value,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RoutingModel {
    fn new(spawn: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            spawn,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn request_system(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .find(|message| message.role == MessageRole::System)
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

#[async_trait]
impl ModelClient for RoutingModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().unwrap().push(request.clone());
        if request_system(&request).contains("# Your role") {
            return Ok(text("Reviewed: no problems found."));
        }
        let spawned = request
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        if spawned {
            return Ok(text("Started the reviewer."));
        }
        Ok(ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "spawn-1".into(),
                name: "spawn_agent".into(),
                arguments: self.spawn.clone(),
            }],
            usage: None,
            thinking: None,
        })
    }
}

async fn wait_terminal(session: &mut AgentSession, id: BackgroundTaskId) -> BackgroundTaskStatus {
    for _ in 0..500 {
        session.poll_background_tasks().await.unwrap();
        if let Some(task) = session.background().get(id) {
            if task.status.is_terminal() {
                return task.status.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("background task {id:?} did not finish");
}

#[tokio::test]
async fn a_session_lists_its_agents_and_a_bad_file_only_warns() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    write_agent(
        dir.path(),
        "broken",
        "---\nname: broken\ndescription: missing the rest\n",
    );
    let model = Arc::new(MockModelClient::script(vec![
        text("Hello."),
        text("Again."),
    ]));
    let mut session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();

    let system = system_prompt(&session);
    assert!(system.contains("# Agents"), "{system}");
    assert!(system.contains(
        "## code-reviewer\n\nCapabilities: read-only\nReviews the current diff for correctness."
    ));
    assert!(
        !system.contains("ROLE-MARKER"),
        "instructions are not disclosed up front"
    );
    assert!(!system.contains("broken"), "a skipped file is not offered");

    let warnings = session.custom_agent_warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].starts_with("Skipped project agent file "),
        "{warnings:?}"
    );
    assert!(warnings[0].contains("broken.md"), "{warnings:?}");
    assert!(
        warnings[1].contains("tool `edit` needs a capability not in mode `read-only`"),
        "{warnings:?}"
    );

    // The bad file does not break the session.
    session
        .run_user_message_with_stream("hi", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Completed);

    // Resuming rebuilds the same list and warnings.
    let id = session.session_id;
    let model = Arc::new(MockModelClient::script(vec![]));
    let resumed = AgentSession::resume(cfg(dir.path()), model, ToolRegistry::new(), id)
        .await
        .unwrap();
    assert!(system_prompt(&resumed).contains("## code-reviewer"));
    assert_eq!(resumed.custom_agent_warnings().len(), 2);
}

#[tokio::test]
async fn a_session_without_agents_has_no_agents_section() {
    let dir = TempDir::new().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert!(!system_prompt(&session).contains("# Agents"));
    assert!(session.custom_agent_warnings().is_empty());
}

/// The whole path: the model reads the agent list, calls `spawn_agent` by
/// name with the user's model and effort, and the child runs as that agent.
#[tokio::test]
async fn the_model_starts_a_custom_agent_by_name() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    let model = RoutingModel::new(json!({
        "agent": "code-reviewer",
        "message": "Review a.txt",
        "model": "anthropic/claude-test",
        "reasoning_effort": "high",
    }));
    let mut session = AgentSession::create(cfg(dir.path()), model.clone(), ToolRegistry::new())
        .await
        .unwrap();

    session
        .run_user_message_with_stream("Please review my change", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Completed);

    // The parent saw the agent list, and the spawn answered with the agent.
    let requests = model.requests();
    assert!(request_system(&requests[0]).contains("## code-reviewer"));
    let started = session
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("spawn-1"))
        .expect("spawn_agent result")
        .content
        .clone();
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["status"], "running");
    assert_eq!(started["agent"], "code-reviewer");
    assert_eq!(started["model"], "anthropic/claude-test");
    assert_eq!(started["reasoning_effort"], "high");

    let id = session.background().list().next().unwrap().id;
    assert!(matches!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Succeeded { .. }
    ));
    let task = session.background().get(id).unwrap();
    assert_eq!(task.label, "code-reviewer");
    let child_meta = task.child.as_ref().unwrap();
    assert_eq!(child_meta.agent.as_deref(), Some("code-reviewer"));
    assert_eq!(child_meta.mode, Some(AgentMode::ReadOnly));
    assert_eq!(child_meta.model.as_deref(), Some("anthropic/claude-test"));
    assert_eq!(child_meta.reasoning_effort.as_deref(), Some("high"));
    assert!(
        task.worktree_path.is_none(),
        "read-only shares the workspace"
    );

    // What the child was actually sent.
    let child_requests: Vec<_> = model
        .requests()
        .into_iter()
        .filter(|request| request_system(request).contains("# Your role"))
        .collect();
    assert!(!child_requests.is_empty());
    let child_request = &child_requests[0];
    let system = request_system(child_request);
    assert!(system.contains("custom agent `code-reviewer` (capabilities: read-only)"));
    assert!(system.contains("ROLE-MARKER: review carefully"));
    assert!(system.contains("no network access"));
    assert!(!system.contains("# Agents"), "a child cannot start agents");
    let mut tools: Vec<_> = child_request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    tools.sort();
    assert_eq!(tools, vec!["grep", "read_file"], "edit exceeds read-only");
    assert_eq!(child_request.model, "anthropic/claude-test");
    assert_eq!(child_request.reasoning_effort.as_deref(), Some("high"));

    // The child's journal records what it was started as.
    let child_id = task.child_session_id.unwrap();
    let journal = forge_durable::Journal::open(&dir.path().join(".forge/local/journal"), child_id)
        .await
        .unwrap();
    let profile = journal
        .replay(child_id)
        .await
        .unwrap()
        .agent_profile
        .unwrap();
    assert_eq!(profile["agent"], "code-reviewer");
    assert_eq!(profile["tool_limit"], json!(["read_file", "grep"]));
    assert_eq!(profile["no_network"], true);
}

/// A spawn the session cannot honour is a tool error the model can read and
/// correct, never a failed turn.
#[tokio::test]
async fn an_unknown_agent_is_a_tool_error_and_the_turn_goes_on() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    let model = RoutingModel::new(json!({"agent": "nobody", "message": "hi"}));
    let mut session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session
        .run_user_message_with_stream("go", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Completed);
    let result = session
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("spawn-1"))
        .unwrap();
    assert!(
        result
            .content
            .contains("no custom agent named `nobody`; available agents: code-reviewer"),
        "{}",
        result.content
    );
    assert_eq!(session.background().list().count(), 0);
}

#[tokio::test]
async fn spawn_requests_are_checked_before_anything_starts() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    write_agent(dir.path(), "broken", "---\nname: broken\n");
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let error = |value| {
        session
            .subagent_spec_for(spawn_args(value))
            .unwrap_err()
            .to_string()
    };
    assert!(error(json!({"agent": "broken", "message": "m"})).contains("did not load"));
    assert!(error(json!({"message": "m"})).contains("`task_name` is required"));
    assert!(
        error(json!({"agent": "code-reviewer", "message": "m", "model": "claude"}))
            .contains("<provider>/<model>")
    );
    assert!(error(
        json!({"agent": "code-reviewer", "message": "m", "reasoning_effort": "extreme"})
    )
    .contains("not one of"));

    // A caller's allow-list narrows the agent's tools and cannot add any.
    let spec = session
        .subagent_spec_for(spawn_args(json!({
            "agent": "code-reviewer",
            "message": "m",
            "tool_allowlist": ["grep", "bash"],
            "mode": "writer",
        })))
        .unwrap();
    assert_eq!(spec.tool_allowlist, Some(vec!["grep".to_string()]));
    assert_eq!(
        spec.mode,
        AgentMode::ReadOnly,
        "`mode` cannot raise the agent"
    );
    assert_eq!(spec.role, "code-reviewer");
}

/// Each capability set becomes scheduling, a tool set and network access,
/// and the child is held to them.
#[tokio::test]
async fn capabilities_become_the_childs_tools_network_and_scheduling() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "test-fixer", FIXER);
    write_agent(dir.path(), "researcher", RESEARCHER);
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert!(
        session.tool_ctx.egress.is_some(),
        "the parent has a network gateway to withhold"
    );

    let fixer = session
        .subagent_spec_for(spawn_args(json!({"agent": "test-fixer", "message": "m"})))
        .unwrap();
    assert_eq!(
        fixer.mode,
        AgentMode::Writer,
        "write and exec get a worktree"
    );
    assert!(fixer.profile.no_network);
    let child = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &fixer,
        )
        .await
        .unwrap();
    assert_eq!(offered(&child), vec!["bash", "edit", "read_file"]);
    assert!(child.tool_ctx.egress.is_none(), "exec without network");

    let researcher = session
        .subagent_spec_for(spawn_args(json!({"agent": "researcher", "message": "m"})))
        .unwrap();
    assert_eq!(researcher.mode, AgentMode::ReadOnly, "network alone shares");
    assert!(!researcher.profile.no_network);
    let child = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &researcher,
        )
        .await
        .unwrap();
    assert_eq!(offered(&child), vec!["read_file", "web_fetch"]);
    assert!(child.tool_ctx.egress.is_some(), "network keeps the gateway");
}

/// The limit holds at execution, not only in the tool list the model sees.
#[tokio::test]
async fn a_call_outside_the_tool_limit_is_refused() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let spec = session
        .subagent_spec_for(spawn_args(
            json!({"agent": "code-reviewer", "message": "m"}),
        ))
        .unwrap();
    let mut child = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &spec,
        )
        .await
        .unwrap();
    let outcome = child
        .apply_model_response(ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "w-1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "a.txt", "content": "changed\n"}),
            }],
            usage: None,
            thinking: None,
        })
        .await
        .unwrap();
    assert!(matches!(outcome, ApplyOutcome::Continue));
    let result = child
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("w-1"))
        .unwrap();
    assert!(
        result
            .content
            .contains("`write_file` is not one of the tools the agent `code-reviewer` was given"),
        "{}",
        result.content
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "one\n"
    );
}

/// After a restart the child is rebuilt from its own journal with the same
/// instructions, tools, network and model — it does not fall back to the
/// parent's full tool set.
#[tokio::test]
async fn a_resumed_child_keeps_its_agent_profile() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "test-fixer", FIXER);
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let spec = session
        .subagent_spec_for(spawn_args(json!({
            "agent": "test-fixer",
            "message": "m",
            "model": "anthropic/claude-test",
            "reasoning_effort": "low",
        })))
        .unwrap();
    let child_id = forge_durable::new_session_id();
    let child = session
        .create_child(
            child_id,
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &spec,
        )
        .await
        .unwrap();
    // The scratch directory in the prompt is new on every resume; the role
    // section is what must carry over.
    let role = |session: &AgentSession| {
        let system = system_prompt(session);
        system[system.find("# Your role").expect("role section")..].to_string()
    };
    let before = (offered(&child), role(&child));
    drop(child);

    // The agent file changing later does not rewrite a running agent.
    write_agent(
        dir.path(),
        "test-fixer",
        RESEARCHER.replace("researcher", "test-fixer").as_str(),
    );

    let resumed = session
        .resume_child(child_id, dir.path().to_path_buf(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(offered(&resumed), before.0);
    assert_eq!(role(&resumed), before.1);
    assert!(system_prompt(&resumed).contains("Fix the tests."));
    assert!(resumed.tool_ctx.egress.is_none());
    assert_eq!(
        resumed.child_profile().and_then(|p| p.agent.as_deref()),
        Some("test-fixer")
    );
    let selection = resumed.model_selection();
    assert_eq!(selection.model, "anthropic/claude-test");
    assert_eq!(selection.reasoning_effort.as_deref(), Some("low"));
}

/// A model chosen for a child gets that model's limits from the resolver.
#[tokio::test]
async fn a_resolver_validates_the_model_and_supplies_its_limits() {
    struct Catalog;
    impl crate::ChildModelResolver for Catalog {
        fn resolve(
            &self,
            request: &crate::ChildModelRequest<'_>,
        ) -> Result<crate::ChildModel, String> {
            match request.model {
                Some("anthropic/known") => Ok(crate::ChildModel {
                    model: "anthropic/known".into(),
                    profile_id: "anthropic".into(),
                    reasoning_effort: request.reasoning_effort.map(str::to_string),
                    context_window: Some(1_000_000),
                    output_reserve: Some(32_000),
                    image_input: Some(true),
                }),
                other => Err(format!("no provider serves `{}`", other.unwrap_or(""))),
            }
        }
    }
    let dir = TempDir::new().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session.set_child_model_resolver(Arc::new(Catalog));

    let error = session
        .subagent_spec_for(spawn_args(
            json!({"task_name": "t", "message": "m", "model": "nope/x"}),
        ))
        .unwrap_err();
    assert!(error.to_string().contains("no provider serves `nope/x`"));

    let spec = session
        .subagent_spec_for(spawn_args(
            json!({"task_name": "t", "message": "m", "model": "anthropic/known"}),
        ))
        .unwrap();
    let child = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &spec,
        )
        .await
        .unwrap();
    assert_eq!(child.model_selection().model, "anthropic/known");
    assert_eq!(child.compaction_policy.context_window, 1_000_000);
    assert!(child.tool_ctx.image_input);
    assert_eq!(
        child.tool_limit(),
        None,
        "an unnamed child keeps every tool"
    );
}

/// User agents come only from the directory the session is given: none by
/// default, which is what keeps tests away from a developer's own agents. A
/// project agent still overrides a user agent of the same name, a user agent
/// starts by name, and a fork keeps the directory.
#[tokio::test]
async fn user_agents_come_from_the_directory_the_session_is_given() {
    let dir = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    write_agent(dir.path(), "code-reviewer", REVIEWER);
    std::fs::write(
        user.path().join("docs-writer.md"),
        "---\nname: docs-writer\ndescription: Writes the docs.\nmode: [write]\ntools: [read_file, edit]\n---\nWrite.\n",
    )
    .unwrap();
    std::fs::write(
        user.path().join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: USER-REVIEWER.\nmode: [read-only]\ntools: [read_file]\n---\n",
    )
    .unwrap();

    let model = Arc::new(MockModelClient::script(vec![]));
    let default = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let system = system_prompt(&default);
    assert!(system.contains("## code-reviewer"));
    assert!(
        !system.contains("docs-writer"),
        "no user directory, no user agents"
    );

    let with_user = LoopConfig {
        user_agents_dir: Some(user.path().to_path_buf()),
        ..cfg(dir.path())
    };
    let model = Arc::new(MockModelClient::script(vec![]));
    let session = AgentSession::create(with_user, model, ToolRegistry::new())
        .await
        .unwrap();
    let system = system_prompt(&session);
    assert!(system.contains("## docs-writer\n\nCapabilities: write\nWrites the docs."));
    assert!(
        !system.contains("USER-REVIEWER"),
        "the project agent overrides the user one"
    );
    let spec = session
        .subagent_spec_for(spawn_args(json!({"agent": "docs-writer", "message": "m"})))
        .unwrap();
    assert_eq!(spec.profile.agent.as_deref(), Some("docs-writer"));
    assert_eq!(
        spec.tool_allowlist,
        Some(vec!["read_file".into(), "edit".into()])
    );

    let fork = session.fork().await.unwrap();
    assert!(system_prompt(&fork).contains("## docs-writer"));
}

/// An agent saved while a session is open is offered from the session's
/// next turn, without a restart or a new session.
#[tokio::test]
async fn an_agent_saved_mid_session_is_used_on_the_next_turn() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = RoutingModel::new(json!({"agent": "code-reviewer", "message": "Review a.txt"}));
    let mut session = AgentSession::create(cfg(dir.path()), model.clone(), ToolRegistry::new())
        .await
        .unwrap();
    assert!(!system_prompt(&session).contains("# Agents"));

    write_agent(dir.path(), "code-reviewer", REVIEWER);
    session
        .run_user_message_with_stream("Please review my change", None)
        .await
        .unwrap();

    assert!(request_system(&model.requests()[0]).contains("## code-reviewer"));
    let started = session
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("spawn-1"))
        .unwrap()
        .content
        .clone();
    assert!(started.contains("\"agent\":\"code-reviewer\""), "{started}");
    let id = session.background().list().next().unwrap().id;
    assert!(matches!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Succeeded { .. }
    ));
}

/// The `# Agents` section follows the files, and the rest of the system
/// prompt never changes with it. An unchanged list leaves the prompt exactly
/// as it was, so it costs no prompt-cache miss.
#[tokio::test]
async fn the_agents_section_is_rewritten_only_when_the_agents_change() {
    let dir = TempDir::new().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut session = AgentSession::create(cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let base = system_prompt(&session);
    assert!(!base.contains("# Agents"));

    write_agent(dir.path(), "code-reviewer", REVIEWER);
    session.refresh_custom_agents();
    let with_agent = system_prompt(&session);
    assert!(
        with_agent.starts_with(&base),
        "the base prompt is untouched"
    );
    assert!(with_agent.contains("## code-reviewer"));
    assert!(session.context_usage_ratio() > 0.0);
    let estimate = session.context_usage_ratio();

    session.refresh_custom_agents();
    assert_eq!(
        system_prompt(&session),
        with_agent,
        "nothing changed, nothing rewritten"
    );

    write_agent(
        dir.path(),
        "code-reviewer",
        &REVIEWER.replace(
            "Reviews the current diff for correctness.",
            "Reviews every diff with a very long and much more detailed description of the job.",
        ),
    );
    session.refresh_custom_agents();
    let edited = system_prompt(&session);
    assert!(edited.starts_with(&base));
    assert!(edited.contains("much more detailed description"));
    assert!(!edited.contains("Reviews the current diff for correctness."));
    assert!(
        session.context_usage_ratio() > estimate,
        "the context estimate follows the rewritten prompt"
    );

    std::fs::remove_file(dir.path().join(".forge/agents/code-reviewer.md")).unwrap();
    write_agent(dir.path(), "broken", "not an agent");
    session.refresh_custom_agents();
    assert_eq!(system_prompt(&session), base, "no agents, no section");
    assert_eq!(
        session.custom_agent_warnings().len(),
        1,
        "and the warning is current"
    );
}
