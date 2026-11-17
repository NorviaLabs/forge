//! Turn records, turn signals, model tiers and strong-tier escalation,
//! through real sessions.
//!
//! The model answers by the model id each request names, so a test can see
//! which model ran each step however parent and child interleave; tiers come
//! from a fixed resolver (fast → `test/small` @ low, default → `test/mid` @
//! medium, strong → `test/big` @ high).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_durable::{Journal, TurnRecord};
use forge_model::{ModelClient, ModelError, ModelRequest};
use forge_tools::{AgentMode, SpawnAgentArgs, ToolContext, ToolError, ToolRegistry};
use forge_types::{
    BackgroundTaskId, HitlDecision, MessageRole, ModelResponse, ModelTier, SessionId,
    SideEffectClass, TaskKind, TaskLifecycle, ToolCall, ToolOutput, Usage,
};
use serde_json::json;
use tempfile::TempDir;

use crate::{
    AgentSession, BackgroundTaskStatus, ChildModel, ChildProfile, LoopConfig,
    ModelExecutionSelection, ModelTierResolver, SubagentSpec, TierBasis, TierRequest,
    TierResolution,
};

// ---------------------------------------------------------------- fixtures

struct FakeTiers;

impl ModelTierResolver for FakeTiers {
    fn resolve_tier(&self, request: &TierRequest<'_>) -> TierResolution {
        let (model, effort) = match request.tier {
            ModelTier::Fast => ("test/small", "low"),
            ModelTier::Default => ("test/mid", "medium"),
            ModelTier::Strong => ("test/big", "high"),
        };
        TierResolution {
            tier: request.tier,
            model: ChildModel {
                model: model.into(),
                profile_id: "test".into(),
                reasoning_effort: Some(effort.into()),
                context_window: Some(100_000),
                output_reserve: None,
                image_input: Some(false),
            },
            basis: TierBasis::Catalog,
        }
    }
}

#[derive(Debug, Clone)]
struct Seen {
    model: String,
    effort: Option<String>,
    tools: Vec<String>,
    last_user: String,
}

/// Scripted replies per requested model; `done` once a script runs out.
/// `test/slow` never answers, so a test can stop it mid-step; `test/broken`
/// fails with a provider error that is not retried.
#[derive(Default)]
struct TierModel {
    scripts: Mutex<HashMap<String, VecDeque<ModelResponse>>>,
    seen: Mutex<Vec<Seen>>,
}

impl TierModel {
    fn new(scripts: &[(&str, Vec<ModelResponse>)]) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(
                scripts
                    .iter()
                    .map(|(model, replies)| (model.to_string(), replies.clone().into()))
                    .collect(),
            ),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn models(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.model).collect()
    }
}

#[async_trait]
impl ModelClient for TierModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        self.seen.lock().unwrap().push(Seen {
            model: request.model.clone(),
            effort: request.reasoning_effort.clone(),
            tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            last_user,
        });
        if request.model == "test/slow" {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if request.model == "test/broken" {
            return Err(ModelError::Protocol("unexpected end of stream".into()));
        }
        let reply = self
            .scripts
            .lock()
            .unwrap()
            .get_mut(&request.model)
            .and_then(VecDeque::pop_front);
        Ok(reply.unwrap_or_else(|| text("done")))
    }
}

/// `bash` without a shell: a command containing `fail` exits 1, anything
/// else exits 0.
struct ScriptedBash;

#[async_trait]
impl forge_tools::Tool for ScriptedBash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Scripted shell"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Exec
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let command = args["command"].as_str().unwrap_or_default();
        if command.contains("fail") {
            Ok(ToolOutput::failed_exit("1 test failed", Some(1)))
        } else {
            Ok(ToolOutput {
                exit_code: Some(0),
                ..ToolOutput::success("ok")
            })
        }
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

/// Final text that names a tool in call syntax: the completion evaluator
/// fails the turn (`dangling_tool_call_text`).
fn dangling() -> ModelResponse {
    text(r#"{"name": "read_file", "path": "a.txt"}"#)
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ModelResponse {
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

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn init_repo(dir: &std::path::Path) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "forge@example.com"]);
    git(dir, &["config", "user.name", "Forge Test"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "init"]);
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

fn selection(model: &str, effort: &str) -> ModelExecutionSelection {
    ModelExecutionSelection {
        model: model.into(),
        profile_id: "test".into(),
        reasoning_effort: Some(effort.into()),
        thinking_enabled: true,
    }
}

/// A top-level session on `test/mid` @ medium with the fake tiers and the
/// scripted shell.
async fn session(dir: &std::path::Path, model: Arc<TierModel>) -> AgentSession {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedBash));
    let mut session = AgentSession::create(cfg(dir), model, tools).await.unwrap();
    session.set_model_selection(selection("test/mid", "medium"));
    session.set_model_tier_resolver(Arc::new(FakeTiers));
    session
}

async fn records(session: &AgentSession) -> Vec<TurnRecord> {
    session
        .turn_records()
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.record)
        .collect()
}

async fn child_records(dir: &std::path::Path, child: SessionId) -> Vec<TurnRecord> {
    let journal = Journal::open(&dir.join("j"), child).await.unwrap();
    journal
        .turn_records(child)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.record)
        .collect()
}

fn signal_kinds(record: &TurnRecord) -> Vec<String> {
    record
        .signals
        .as_array()
        .unwrap()
        .iter()
        .map(|signal| signal["kind"].as_str().unwrap().to_string())
        .collect()
}

fn signal_detail(record: &TurnRecord, kind: &str) -> Option<String> {
    record
        .signals
        .as_array()
        .unwrap()
        .iter()
        .find(|signal| signal["kind"] == kind)
        .and_then(|signal| signal["detail"].as_str().map(str::to_string))
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

/// Spawn a writer child on `tier`, resolved the way `spawn_agent` does.
async fn spawn_on_tier(
    session: &mut AgentSession,
    tier: ModelTier,
) -> (BackgroundTaskId, SessionId) {
    let resolution = session.resolve_model_tier(tier, &session.model_selection());
    let id = session
        .spawn_subagent(SubagentSpec {
            role: "fixer".into(),
            prompt: "fix the bug".into(),
            mode: AgentMode::Writer,
            tool_allowlist: None,
            profile: ChildProfile {
                model: Some(resolution.model),
                tier: Some(tier),
                ..ChildProfile::default()
            },
        })
        .await
        .unwrap();
    let child = session
        .background()
        .get(id)
        .and_then(|task| task.child_session_id)
        .unwrap();
    (id, child)
}

// ------------------------------------------------------------ turn records

#[tokio::test]
async fn a_finished_turn_writes_one_record_with_model_effort_tokens_and_duration() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![ModelResponse {
            usage: Some(Usage {
                prompt_tokens: 1_200,
                completion_tokens: 80,
                prompt_cache_read_tokens: 1_000,
                prompt_cache_write_tokens: 0,
            }),
            ..text("Hello.")
        }],
    )]);
    let mut session = session(dir.path(), model).await;
    session
        .run_user_message_with_stream("hi", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Completed);

    let records = records(&session).await;
    assert_eq!(records.len(), 1, "{records:#?}");
    let record = &records[0];
    assert_eq!(record.session_id, session.session_id);
    assert_eq!(record.parent_session_id, None);
    assert_eq!(record.model, "test/mid");
    assert_eq!(record.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(record.tier, None);
    assert_eq!(record.outcome, "completed");
    assert_eq!(record.completion_reason, "no_changes_required");
    assert!(!record.escalated && !record.escalation_retry);
    assert_eq!(record.prompt_tokens, Some(1_200));
    assert_eq!(record.completion_tokens, Some(80));
    assert_eq!(record.cache_read_tokens, Some(1_000));
    assert_eq!(record.model_calls, Some(1));
    assert!(record.started_at.is_some() && record.duration_ms.is_some());
    assert_eq!(record.waiting_ms, Some(0));

    // The next turn's tokens are its own, not the session's running total.
    session
        .run_user_message_with_stream("again", None)
        .await
        .unwrap();
    let records = self::records(&session).await;
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].prompt_tokens, Some(0));
    assert_eq!(records[1].model_calls, Some(1));
    assert!(records[1].task_id > records[0].task_id);
}

#[tokio::test]
async fn verification_commands_and_their_results_are_signals() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![
            call("1", "bash", json!({"command": "cargo test -p core"})),
            call("2", "bash", json!({"command": "npm test -- fail"})),
            call("3", "bash", json!({"command": "ls fail-dir"})),
            text("Tests are red."),
        ],
    )]);
    let mut session = session(dir.path(), model).await;
    session
        .run_user_message_with_stream("run the tests", None)
        .await
        .unwrap();

    let records = records(&session).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        signal_kinds(record),
        ["verification_passed", "verification_failed", "tool_failed"],
        "{:#?}",
        record.signals
    );
    assert_eq!(
        signal_detail(record, "verification_passed").as_deref(),
        Some("cargo test -p core")
    );
    assert_eq!(
        signal_detail(record, "tool_failed").as_deref(),
        Some("bash")
    );
    // The required commands failed: the evaluator fails the turn. A
    // top-level session never escalates.
    assert_eq!(record.outcome, "failed");
    assert!(!record.escalated);
    assert!(!signal_kinds(record).contains(&"escalated".to_string()));
}

#[tokio::test]
async fn the_projects_validation_command_is_a_verification_command() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![
            call("1", "bash", json!({"command": "./scripts/check.sh --all"})),
            text("Checked."),
        ],
    )]);
    let mut session = session(dir.path(), model).await;
    session.set_verification_commands(vec!["./scripts/check.sh".into()]);
    session
        .run_user_message_with_stream("check", None)
        .await
        .unwrap();
    let record = &records(&session).await[0];
    assert_eq!(signal_kinds(record), ["verification_passed"]);
}

#[tokio::test]
async fn approvals_asked_and_denied_are_signals_and_the_wait_is_timed() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![
            call("1", "bash", json!({"command": "rm -rf build"})),
            text("Left the build directory alone."),
        ],
    )]);
    let mut session = session(dir.path(), model).await;
    session.set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));
    session
        .run_user_message_with_stream("clean up", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Waiting);
    assert!(
        records(&session).await.is_empty(),
        "a turn waiting on the user is not over"
    );

    tokio::time::sleep(Duration::from_millis(60)).await;
    session
        .resolve_hitl(HitlDecision::Deny, "tester")
        .await
        .unwrap();
    session.run_agent_turns(None).await.unwrap();

    let records = records(&session).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        signal_kinds(record),
        ["approval_requested", "approval_denied"]
    );
    assert_eq!(
        signal_detail(record, "approval_denied").as_deref(),
        Some("bash")
    );
    assert!(record.waiting_ms.unwrap() >= 50, "{record:#?}");
    assert!(record.duration_ms.unwrap() >= record.waiting_ms.unwrap());
}

#[tokio::test]
async fn stopping_a_turn_records_that_the_user_cancelled_it() {
    let dir = TempDir::new().unwrap();
    let mut session = session(dir.path(), TierModel::new(&[])).await;
    session.append_user_message("long job").await.unwrap();
    session.mark_cancelled().await.unwrap();

    let record = &records(&session).await[0];
    assert_eq!(record.outcome, "cancelled");
    assert_eq!(record.completion_reason, "user_cancelled");
    assert_eq!(signal_kinds(record), ["user_cancelled"]);
}

#[tokio::test]
async fn a_turn_found_running_after_a_restart_records_the_interruption() {
    let dir = TempDir::new().unwrap();
    let session_id = {
        let mut session = session(dir.path(), TierModel::new(&[])).await;
        session.append_user_message("long job").await.unwrap();
        session.session_id
    };
    let resumed = AgentSession::resume(
        cfg(dir.path()),
        TierModel::new(&[]),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Interrupted);
    let records = records(&resumed).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.outcome, "interrupted");
    assert_eq!(record.completion_reason, "interrupted_by_restart");
    assert_eq!(signal_kinds(record), ["interrupted_by_restart"]);
    // This process never saw the turn start.
    assert_eq!(record.started_at, None);
    assert_eq!(record.duration_ms, None);
    assert_eq!(record.prompt_tokens, None);
}

// ------------------------------------------------------------------ tiers

#[tokio::test]
async fn the_user_asking_for_a_stronger_model_switches_this_turn_only() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[
        (
            "test/mid",
            vec![
                call("t", "set_model_tier", json!({"tier": "strong"})),
                text("Back on the usual model."),
            ],
        ),
        ("test/big", vec![text("Thought hard about it.")]),
    ]);
    let mut session = session(dir.path(), model.clone()).await;
    session
        .run_user_message_with_stream("use your strongest model for this one", None)
        .await
        .unwrap();

    let seen = model.seen();
    assert_eq!(seen[0].model, "test/mid");
    assert!(seen[0].tools.contains(&"set_model_tier".to_string()));
    assert_eq!(
        seen[1].model, "test/big",
        "the tier applies from the next step"
    );
    assert_eq!(seen[1].effort.as_deref(), Some("high"));
    // Back on the user's own model once the turn is over.
    assert_eq!(session.model_selection(), selection("test/mid", "medium"));
    assert_eq!(session.active_model_tier(), None);

    let record = &records(&session).await[0];
    assert_eq!(record.outcome, "completed");
    assert_eq!(record.model, "test/big");
    assert_eq!(record.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(record.tier.as_deref(), Some("strong"));
    assert_eq!(
        signal_detail(record, "tier_applied").as_deref(),
        Some("strong: test/big @ high")
    );

    session
        .run_user_message_with_stream("thanks", None)
        .await
        .unwrap();
    assert_eq!(model.models().last().map(String::as_str), Some("test/mid"));
    let records = records(&session).await;
    assert_eq!(records[1].tier, None);
    assert_eq!(records[1].model, "test/mid");
}

#[tokio::test]
async fn a_tier_with_nothing_to_change_is_reported_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![
            call("t", "set_model_tier", json!({"tier": "fast"})),
            text("ok"),
        ],
    )]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedBash));
    // No resolver installed: the offline mock has no catalog.
    let mut session = AgentSession::create(cfg(dir.path()), model.clone(), tools)
        .await
        .unwrap();
    session.set_model_selection(selection("test/mid", "medium"));
    session
        .run_user_message_with_stream("be quick", None)
        .await
        .unwrap();
    assert_eq!(model.models(), ["test/mid", "test/mid"]);
    let tool_result = session
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("t"))
        .unwrap();
    assert!(
        tool_result.content.contains("\"basis\":\"unavailable\""),
        "{}",
        tool_result.content
    );
    let record = &records(&session).await[0];
    assert_eq!(record.tier, None);
    assert_eq!(signal_kinds(record), ["tier_unavailable"]);
}

#[tokio::test]
async fn the_users_own_model_choice_replaces_a_turn_tier() {
    let dir = TempDir::new().unwrap();
    let mut session = session(dir.path(), TierModel::new(&[])).await;
    session.append_user_message("go").await.unwrap();
    session.switch_turn_tier(ModelTier::Strong);
    assert_eq!(session.model_selection(), selection("test/big", "high"));

    // Toggling thinking keeps the tier, and the user's setting survives it.
    session.set_thinking_enabled(false);
    assert_eq!(session.active_model_tier(), Some(ModelTier::Strong));
    assert_eq!(session.model_selection().model, "test/big");

    // Changing effort is a choice about the user's model, not the tier's.
    session.set_reasoning_effort(Some("low".into()));
    assert_eq!(session.active_model_tier(), None);
    let mut expected = selection("test/mid", "low");
    expected.thinking_enabled = false;
    assert_eq!(session.model_selection(), expected);

    // Ending the turn has nothing left to restore.
    session.mark_cancelled().await.unwrap();
    assert_eq!(session.model_selection(), expected);
}

#[tokio::test]
async fn spawn_agent_resolves_a_tier_on_this_sessions_provider() {
    let dir = TempDir::new().unwrap();
    let session = session(dir.path(), TierModel::new(&[])).await;
    let args = |value| serde_json::from_value::<SpawnAgentArgs>(value).unwrap();

    let spec = session
        .subagent_spec_for(args(
            json!({"task_name": "t", "message": "m", "tier": "fast"}),
        ))
        .unwrap();
    assert_eq!(spec.profile.tier, Some(ModelTier::Fast));
    let model = spec.profile.model.unwrap();
    assert_eq!(model.model, "test/small");
    assert_eq!(model.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(model.context_window, Some(100_000));

    let error = session
        .subagent_spec_for(args(json!({
            "task_name": "t", "message": "m", "tier": "fast", "model": "test/big"
        })))
        .unwrap_err();
    assert!(
        error.to_string().contains("either `tier` or `model`"),
        "{error}"
    );
}

// ------------------------------------------------------------- escalation

#[tokio::test]
async fn a_failed_subagent_turn_is_rerun_once_on_the_strong_tier_in_the_same_child() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[
        ("test/small", vec![dangling(), text("Follow-up handled.")]),
        ("test/big", vec![text("Fixed it on the strong tier.")]),
    ]);
    let mut session = session(dir.path(), model.clone()).await;
    let (id, child) = spawn_on_tier(&mut session, ModelTier::Fast).await;

    let status = wait_terminal(&mut session, id).await;
    assert_eq!(
        status,
        BackgroundTaskStatus::Succeeded {
            summary: "Fixed it on the strong tier.".into()
        }
    );
    let seen = model.seen();
    assert_eq!(model.models(), ["test/small", "test/big"]);
    assert_eq!(seen[1].effort.as_deref(), Some("high"));
    assert!(
        seen[1]
            .last_user
            .starts_with("[REPAIR TASK: strong-tier retry on test/big @ high]"),
        "{}",
        seen[1].last_user
    );
    assert!(seen[1]
        .last_user
        .contains("didn't format the call correctly"));
    // A subagent is not offered the tier switch (or nesting).
    assert!(!seen[0].tools.contains(&"set_model_tier".to_string()));
    assert!(!seen[0].tools.contains(&"spawn_agent".to_string()));
    // Same child, same worktree: one task, one worktree.
    assert_eq!(session.background().len(), 1);
    assert_eq!(session.descendant_worktrees().len(), 1);

    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 2, "{records:#?}");
    let (failed, retry) = (&records[0], &records[1]);
    assert_eq!(failed.parent_session_id, Some(session.session_id));
    assert_eq!(failed.outcome, "failed");
    assert_eq!(failed.completion_reason, "dangling_tool_call_text");
    assert_eq!(failed.model, "test/small");
    assert_eq!(failed.tier.as_deref(), Some("fast"));
    assert!(failed.escalated && !failed.escalation_retry);
    assert_eq!(
        signal_detail(failed, "escalated").as_deref(),
        Some("strong: test/big @ high")
    );
    assert_eq!(retry.parent_session_id, Some(session.session_id));
    assert_eq!(retry.outcome, "completed");
    assert_eq!(retry.model, "test/big");
    assert_eq!(retry.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(retry.tier.as_deref(), Some("strong"));
    assert!(retry.escalation_retry && !retry.escalated);

    // The strong tier was for that turn only: a follow-up runs on the
    // child's own tier again.
    let next = session
        .followup_retained_subagent(child, "one more thing".into())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        wait_terminal(&mut session, next).await,
        BackgroundTaskStatus::Succeeded { .. }
    ));
    assert_eq!(
        model.models().last().map(String::as_str),
        Some("test/small")
    );
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].model, "test/small");
    assert_eq!(records[2].tier.as_deref(), Some("fast"));
    assert!(!records[2].escalated && !records[2].escalation_retry);
}

#[tokio::test]
async fn a_strong_tier_retry_that_fails_again_is_not_retried() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[
        ("test/small", vec![dangling()]),
        ("test/big", vec![dangling()]),
    ]);
    let mut session = session(dir.path(), model.clone()).await;
    let (id, child) = spawn_on_tier(&mut session, ModelTier::Fast).await;

    assert!(matches!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Failed { .. }
    ));
    assert_eq!(
        model.models(),
        ["test/small", "test/big"],
        "escalated only once"
    );
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 2);
    assert!(records[0].escalated);
    assert_eq!(records[1].outcome, "failed");
    assert!(records[1].escalation_retry && !records[1].escalated);
    // The retry is not considered for escalation at all.
    let kinds = signal_kinds(&records[1]);
    assert!(
        !kinds.contains(&"escalated".to_string())
            && !kinds.contains(&"escalation_skipped".to_string()),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn a_child_already_on_the_strong_tier_is_not_escalated() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[("test/big", vec![dangling()])]);
    let mut session = session(dir.path(), model.clone()).await;
    let (id, child) = spawn_on_tier(&mut session, ModelTier::Strong).await;

    assert!(matches!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Failed { .. }
    ));
    assert_eq!(model.models(), ["test/big"]);
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 1);
    assert!(!records[0].escalated);
    assert_eq!(
        signal_detail(&records[0], "escalation_skipped").as_deref(),
        Some("the turn already ran on the strong tier")
    );
}

#[tokio::test]
async fn a_child_the_parent_interrupts_records_the_interruption() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let mut session = session(dir.path(), model.clone()).await;
    let id = session
        .spawn_subagent(SubagentSpec {
            role: "slow".into(),
            prompt: "take your time".into(),
            mode: AgentMode::ReadOnly,
            tool_allowlist: None,
            profile: ChildProfile {
                model: Some(ChildModel {
                    model: "test/slow".into(),
                    profile_id: "test".into(),
                    reasoning_effort: None,
                    context_window: None,
                    output_reserve: None,
                    image_input: None,
                }),
                ..ChildProfile::default()
            },
        })
        .await
        .unwrap();
    let child = session
        .background()
        .get(id)
        .and_then(|task| task.child_session_id)
        .unwrap();
    for _ in 0..200 {
        if !model.seen().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(session
        .coordinator
        .interrupt(session.session_id, child)
        .unwrap());
    assert_eq!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Cancelled
    );
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "cancelled");
    assert_eq!(records[0].completion_reason, "interrupted_by_parent");
    assert_eq!(signal_kinds(&records[0]), ["interrupted_by_parent"]);
    assert_eq!(records[0].parent_session_id, Some(session.session_id));
}

#[tokio::test]
async fn a_child_whose_provider_fails_fails_with_the_error() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let mut session = session(dir.path(), model.clone()).await;
    let id = session
        .spawn_subagent(SubagentSpec {
            role: "unlucky".into(),
            prompt: "try".into(),
            mode: AgentMode::ReadOnly,
            tool_allowlist: None,
            profile: ChildProfile {
                model: Some(ChildModel {
                    model: "test/broken".into(),
                    profile_id: "test".into(),
                    reasoning_effort: None,
                    context_window: None,
                    output_reserve: None,
                    image_input: None,
                }),
                ..ChildProfile::default()
            },
        })
        .await
        .unwrap();
    let child = session
        .background()
        .get(id)
        .and_then(|task| task.child_session_id)
        .unwrap();
    let status = wait_terminal(&mut session, id).await;
    assert!(
        matches!(&status, BackgroundTaskStatus::Failed { error } if error.contains("unexpected end of stream")),
        "a provider failure fails the child, with why: {status:?}"
    );
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].outcome, "failed");
    assert_eq!(records[0].completion_reason, "model_call_failed");
    assert_eq!(signal_kinds(&records[0]), ["model_call_failed"]);
    assert!(signal_detail(&records[0], "model_call_failed")
        .unwrap()
        .contains("unexpected end of stream"));
    // Not a strong-tier candidate: the evaluator never judged the turn.
    assert!(!records[0].escalated);
}

#[tokio::test]
async fn a_message_the_user_queues_during_a_turn_is_recorded_on_that_turn() {
    let dir = TempDir::new().unwrap();
    let mut session = session(dir.path(), TierModel::new(&[])).await;
    let inbox = session.turn_signal_inbox();
    assert!(
        !inbox.record(crate::TurnSignalKind::UserMessageQueued, None),
        "no turn in progress"
    );

    session.append_user_message("long job").await.unwrap();
    assert!(inbox.record(
        crate::TurnSignalKind::UserMessageQueued,
        Some("with 1 attachment".into())
    ));
    session.mark_cancelled().await.unwrap();
    assert!(
        !inbox.record(crate::TurnSignalKind::UserMessageQueued, None),
        "the turn is over"
    );

    // The queued message runs as the next turn, which it did not interrupt.
    session
        .run_user_message_with_stream("next", None)
        .await
        .unwrap();
    let records = records(&session).await;
    assert_eq!(records.len(), 2);
    assert_eq!(
        signal_kinds(&records[0]),
        ["user_message_queued", "user_cancelled"]
    );
    assert_eq!(
        signal_detail(&records[0], "user_message_queued").as_deref(),
        Some("with 1 attachment")
    );
    assert!(signal_kinds(&records[1]).is_empty());
}

// ---------------------------------------------------------- errors

/// A tool whose task panics: the turn stops on an internal error, not a
/// provider failure.
struct PanickingTool;

#[async_trait]
impl forge_tools::Tool for PanickingTool {
    fn name(&self) -> &str {
        "explode"
    }

    fn description(&self) -> &str {
        "Panics"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({"type": "object", "additionalProperties": false})
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        panic!("tool bug");
    }
}

async fn session_with_panicking_tool(dir: &std::path::Path, model: Arc<TierModel>) -> AgentSession {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(PanickingTool));
    let mut session = AgentSession::create(cfg(dir), model, tools).await.unwrap();
    session.set_model_selection(selection("test/mid", "medium"));
    session
}

#[tokio::test]
async fn a_provider_error_fails_the_turn_as_model_call_failed() {
    let dir = TempDir::new().unwrap();
    let mut session = session(dir.path(), TierModel::new(&[])).await;
    session.append_user_message("hi").await.unwrap();
    let error = crate::LoopError::Model(ModelError::Protocol("unexpected end of stream".into()));
    session.fail_turn_on_error(&error).await.unwrap();

    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Failed);
    let record = &records(&session).await[0];
    assert_eq!(record.outcome, "failed");
    assert_eq!(record.completion_reason, "model_call_failed");
    assert_eq!(signal_kinds(record), ["model_call_failed"]);
    assert!(!record.escalated);
}

#[tokio::test]
async fn an_internal_error_fails_the_turn_as_a_runtime_error() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[("test/mid", vec![call("boom", "explode", json!({}))])]);
    let mut session = session_with_panicking_tool(dir.path(), model).await;
    let error = session
        .run_user_message_with_stream("go", None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, crate::LoopError::Other(_)),
        "a panicking tool is an internal error: {error}"
    );
    // The frontend's part: end the turn on the error.
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Working);
    session.fail_turn_on_error(&error).await.unwrap();

    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Failed);
    let record = &records(&session).await[0];
    assert_eq!(record.outcome, "failed");
    assert_eq!(record.completion_reason, "runtime_error");
    assert!(signal_detail(record, "runtime_error")
        .unwrap()
        .contains("tool task join"));
    let last = session.messages.iter().last().unwrap();
    assert!(
        last.content.starts_with(crate::TURN_FAILED_MARKER),
        "the transcript says why: {}",
        last.content
    );
    // Ending it again changes nothing: the turn is already over.
    session.fail_turn_on_error(&error).await.unwrap();
    assert_eq!(records(&session).await.len(), 1);
}

#[tokio::test]
async fn an_error_while_waiting_on_the_user_leaves_the_request_pending() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[(
        "test/mid",
        vec![call("1", "bash", json!({"command": "rm x"}))],
    )]);
    let mut session = session(dir.path(), model).await;
    session.set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));
    session
        .run_user_message_with_stream("go", None)
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Waiting);

    session
        .fail_turn_on_error(&crate::LoopError::Other("owner failed".into()))
        .await
        .unwrap();
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Waiting);
    assert!(session.pending_hitl().is_some());
    assert!(records(&session).await.is_empty());
}

#[tokio::test]
async fn a_child_that_hits_an_internal_error_fails_with_it() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[("test/small", vec![call("boom", "explode", json!({}))])]);
    let mut session = session_with_panicking_tool(dir.path(), model).await;
    session.set_model_tier_resolver(Arc::new(FakeTiers));
    let (id, child) = spawn_on_tier(&mut session, ModelTier::Fast).await;

    let status = wait_terminal(&mut session, id).await;
    assert!(
        matches!(&status, BackgroundTaskStatus::Failed { error } if error.contains("tool task join")),
        "{status:?}"
    );
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "failed");
    assert_eq!(records[0].completion_reason, "runtime_error");
    assert!(!records[0].escalated, "not the evaluator's failure");
}

/// A child started without a tier runs on the model the user chose, not on
/// a tier the parent's turn switched to.
#[tokio::test]
async fn a_child_spawned_during_a_tier_turn_inherits_the_users_model() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let mut session = session(dir.path(), model.clone()).await;
    session
        .append_user_message("use your strongest model")
        .await
        .unwrap();
    session.switch_turn_tier(ModelTier::Strong);
    assert_eq!(session.model_selection().model, "test/big");

    let id = session
        .spawn_subagent(SubagentSpec {
            role: "helper".into(),
            prompt: "look around".into(),
            mode: AgentMode::ReadOnly,
            tool_allowlist: None,
            profile: ChildProfile::default(),
        })
        .await
        .unwrap();
    wait_terminal(&mut session, id).await;
    let seen = model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].model, "test/mid");
    assert_eq!(seen[0].effort.as_deref(), Some("medium"));
    // The parent's own turn is still on the tier.
    assert_eq!(session.model_selection().model, "test/big");
}

/// A strong-tier retry that a restart interrupts is still recorded as the
/// escalation's retry, on the strong model it ran on — and the session goes
/// back to its own model.
#[tokio::test]
async fn a_retry_a_restart_interrupts_is_recorded_as_the_retry() {
    let dir = TempDir::new().unwrap();
    let session_id = {
        let mut session = session(dir.path(), TierModel::new(&[])).await;
        let strong = FakeTiers.resolve_tier(&TierRequest {
            tier: ModelTier::Strong,
            current: &session.model_selection(),
        });
        let note = crate::tiers::escalation_note("cargo test failed", &strong);
        session.append_user_message(&note).await.unwrap();
        session.session_id
    };
    let resumed = AgentSession::resume(
        cfg(dir.path()),
        TierModel::new(&[]),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    let records = records(&resumed).await;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.completion_reason, "interrupted_by_restart");
    assert!(record.escalation_retry);
    assert_eq!(record.tier.as_deref(), Some("strong"));
    assert_eq!(record.model, "test/big");
    assert_eq!(record.reasoning_effort.as_deref(), Some("high"));
    assert_ne!(resumed.model_selection().model, "test/big");
    assert_eq!(resumed.active_model_tier(), None);
}

/// An ordinary turn a restart interrupts is not mistaken for a retry.
#[tokio::test]
async fn an_ordinary_interrupted_turn_is_not_a_retry() {
    let dir = TempDir::new().unwrap();
    let session_id = {
        let mut session = session(dir.path(), TierModel::new(&[])).await;
        session
            .append_user_message("please retry on a strong model")
            .await
            .unwrap();
        session.session_id
    };
    let resumed = AgentSession::resume(
        cfg(dir.path()),
        TierModel::new(&[]),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    let record = &records(&resumed).await[0];
    assert!(!record.escalation_retry);
    assert_eq!(record.tier, None);
}

// -------------------------------------------------------------- task kinds

/// A named custom agent's profile, for a child spawned directly.
fn custom_agent(name: &str, model: Option<ChildModel>) -> ChildProfile {
    ChildProfile {
        agent: Some(name.into()),
        capabilities: Some("read-only".into()),
        instructions: Some("Review the change.".into()),
        model,
        ..ChildProfile::default()
    }
}

fn slow_model() -> ChildModel {
    ChildModel {
        model: "test/slow".into(),
        profile_id: "test".into(),
        reasoning_effort: None,
        context_window: None,
        output_reserve: None,
        image_input: None,
    }
}

async fn spawn_child(
    session: &mut AgentSession,
    mode: AgentMode,
    profile: ChildProfile,
) -> (BackgroundTaskId, SessionId) {
    let id = session
        .spawn_subagent(SubagentSpec {
            role: "helper".into(),
            prompt: "look around".into(),
            mode,
            tool_allowlist: None,
            profile,
        })
        .await
        .unwrap();
    let child = session
        .background()
        .get(id)
        .and_then(|task| task.child_session_id)
        .unwrap();
    (id, child)
}

#[tokio::test]
async fn a_main_turn_records_the_main_kind() {
    let dir = TempDir::new().unwrap();
    let model = TierModel::new(&[("test/mid", vec![text("Hello.")])]);
    let mut session = session(dir.path(), model).await;
    assert_eq!(session.task_kind(), &TaskKind::Main);
    session
        .run_user_message_with_stream("hi", None)
        .await
        .unwrap();
    let records = records(&session).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_kind, TaskKind::Main);
}

/// Built-in children are one kind, whether they write or only read.
#[tokio::test]
async fn writer_and_read_only_children_both_record_the_subagent_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let mut session = session(dir.path(), model).await;
    for mode in [AgentMode::Writer, AgentMode::ReadOnly] {
        let (id, child) = spawn_child(&mut session, mode, ChildProfile::default()).await;
        wait_terminal(&mut session, id).await;
        let records = child_records(dir.path(), child).await;
        assert!(!records.is_empty(), "{mode:?}");
        for record in &records {
            assert_eq!(record.task_kind, TaskKind::Subagent, "{mode:?}");
            assert_eq!(record.parent_session_id, Some(session.session_id));
        }
    }
}

/// A custom agent is its own kind, named after the agent, in either mode.
#[tokio::test]
async fn a_custom_agent_child_records_its_name_as_its_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let mut session = session(dir.path(), model).await;
    for (mode, name) in [
        (AgentMode::ReadOnly, "reviewer"),
        (AgentMode::Writer, "test-fixer"),
    ] {
        let (id, child) = spawn_child(&mut session, mode, custom_agent(name, None)).await;
        wait_terminal(&mut session, id).await;
        let records = child_records(dir.path(), child).await;
        assert!(!records.is_empty(), "{name}");
        for record in &records {
            assert_eq!(record.task_kind, TaskKind::Custom(name.into()));
        }
    }
    // The parent's own turns stay `main`.
    assert_eq!(session.task_kind(), &TaskKind::Main);
}

/// The strong-tier retry runs in the same child, so it is the same kind.
#[tokio::test]
async fn an_escalation_retry_keeps_its_childs_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[
        ("test/small", vec![dangling()]),
        ("test/big", vec![text("Fixed it on the strong tier.")]),
    ]);
    let mut session = session(dir.path(), model).await;
    let resolution = session.resolve_model_tier(ModelTier::Fast, &session.model_selection());
    let mut profile = custom_agent("test-fixer", Some(resolution.model));
    profile.tier = Some(ModelTier::Fast);
    let (id, child) = spawn_child(&mut session, AgentMode::Writer, profile).await;
    assert!(matches!(
        wait_terminal(&mut session, id).await,
        BackgroundTaskStatus::Succeeded { .. }
    ));
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 2, "{records:#?}");
    assert!(records[0].escalated && records[1].escalation_retry);
    for record in &records {
        assert_eq!(record.task_kind, TaskKind::Custom("test-fixer".into()));
    }
}

/// A custom agent's child that a restart interrupts is reopened from its own
/// journal; the record of the interrupted turn still names the agent.
///
/// A writer: a restart reopens an unfinished child through the workspace its
/// parent recorded for it, and only a writer's worktree is recorded — a
/// read-only child that a restart interrupts is marked cancelled in its
/// parent without being reopened, so it writes no turn record.
#[tokio::test]
async fn a_custom_agent_child_interrupted_by_a_restart_keeps_its_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[]);
    let (parent, child) = {
        let mut session = session(dir.path(), model.clone()).await;
        let (_, child) = spawn_child(
            &mut session,
            AgentMode::Writer,
            custom_agent("reviewer", Some(slow_model())),
        )
        .await;
        for _ in 0..200 {
            if !model.seen().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!model.seen().is_empty(), "the child's turn started");
        (session.session_id, child)
    };
    let resumed = AgentSession::resume(
        cfg(dir.path()),
        TierModel::new(&[]),
        ToolRegistry::new(),
        parent,
    )
    .await
    .unwrap();
    assert_eq!(resumed.task_kind(), &TaskKind::Main);
    let records = child_records(dir.path(), child).await;
    let interrupted = records
        .iter()
        .find(|record| record.completion_reason == "interrupted_by_restart")
        .unwrap_or_else(|| panic!("no interrupted record: {records:#?}"));
    assert_eq!(interrupted.task_kind, TaskKind::Custom("reviewer".into()));
}

/// A top-level session found running after a restart records `main`.
#[tokio::test]
async fn a_main_turn_interrupted_by_a_restart_records_the_main_kind() {
    let dir = TempDir::new().unwrap();
    let session_id = {
        let mut session = session(dir.path(), TierModel::new(&[])).await;
        session.append_user_message("long job").await.unwrap();
        session.session_id
    };
    let resumed = AgentSession::resume(
        cfg(dir.path()),
        TierModel::new(&[]),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    let records = records(&resumed).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].completion_reason, "interrupted_by_restart");
    assert_eq!(records[0].task_kind, TaskKind::Main);
}

// ------------------------------------------------------------ model choice

/// An advisor that answers every request with `advice`, and remembers what
/// it was asked.
struct FakeAdvisor {
    advice: crate::ModelAdvice,
    asked: Mutex<Vec<(TaskKind, ModelExecutionSelection)>>,
}

impl FakeAdvisor {
    fn new(advice: crate::ModelAdvice) -> Arc<Self> {
        Arc::new(Self {
            advice,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<(TaskKind, ModelExecutionSelection)> {
        self.asked.lock().unwrap().clone()
    }
}

impl crate::SubagentModelAdvisor for FakeAdvisor {
    fn advise(&self, request: &crate::ModelAdviceRequest<'_>) -> crate::ModelAdvice {
        self.asked
            .lock()
            .unwrap()
            .push((request.kind.clone(), request.parent.clone()));
        self.advice.clone()
    }
}

fn evidence() -> crate::ModelChoiceEvidence {
    let candidate = |model: &str, effort: &str, turns, score, rank| crate::RankedCandidate {
        model: model.into(),
        provider: "test".into(),
        effort: Some(effort.into()),
        turns,
        score,
        rank,
    };
    crate::ModelChoiceEvidence {
        window_days: 90,
        turns: 40,
        ranked_groups: 2,
        leader: Some(candidate("test/big", "high", 25, 1.25, 1)),
        runner_up: Some(candidate("test/mid", "medium", 15, 1.75, 2)),
        min_group_turns: 5,
        min_kind_turns: 20,
        min_ranked_groups: 2,
    }
}

/// The advisor picks the strong model (`test/big` @ high).
fn ranked_pick() -> crate::ModelAdvice {
    let strong = FakeTiers.resolve_tier(&TierRequest {
        tier: ModelTier::Strong,
        current: &selection("test/mid", "medium"),
    });
    crate::ModelAdvice {
        pick: Some(strong.model),
        choice: crate::ModelChoice {
            source: crate::ModelChoiceSource::Ranking,
            reason: "test/big @ high ranks first for subagent work".into(),
            kind: TaskKind::Subagent,
            evidence: Some(evidence()),
        },
    }
}

fn fallback(reason: &str, kind: TaskKind) -> crate::ModelAdvice {
    crate::ModelAdvice {
        pick: None,
        choice: crate::ModelChoice {
            source: crate::ModelChoiceSource::Fallback,
            reason: reason.into(),
            kind,
            evidence: None,
        },
    }
}

/// The parent (on `test/mid`) calls `spawn_agent` with `arguments`, then
/// finishes; returns the spawn's result and the child's id once it is done.
async fn spawn_through_the_tool(
    session: &mut AgentSession,
    arguments: serde_json::Value,
) -> (serde_json::Value, SessionId) {
    session
        .run_user_message_with_stream("delegate it", None)
        .await
        .unwrap();
    let result = session
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("spawn-1"))
        .unwrap_or_else(|| panic!("no spawn_agent result for {arguments}"))
        .content
        .clone();
    let result: serde_json::Value =
        serde_json::from_str(&result).unwrap_or_else(|error| panic!("{error}: {result}"));
    let task = session.background().list().last().unwrap().id;
    wait_terminal(session, task).await;
    let child = session
        .background()
        .get(task)
        .and_then(|task| task.child_session_id)
        .unwrap();
    (result, child)
}

fn delegating_model(arguments: serde_json::Value) -> Arc<TierModel> {
    TierModel::new(&[(
        "test/mid",
        vec![
            call("spawn-1", "spawn_agent", arguments),
            text("Delegated."),
        ],
    )])
}

async fn child_profile(dir: &std::path::Path, child: SessionId) -> serde_json::Value {
    let journal = Journal::open(&dir.join("j"), child).await.unwrap();
    journal
        .replay(child)
        .await
        .unwrap()
        .agent_profile
        .expect("the child's profile is journaled")
}

#[tokio::test]
async fn a_subagent_started_without_a_model_runs_on_the_advisors_pick() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = delegating_model(json!({"task_name": "helper", "message": "look around"}));
    let mut session = session(dir.path(), model.clone()).await;
    let advisor = FakeAdvisor::new(ranked_pick());
    session.set_subagent_model_advisor(advisor.clone());

    let (result, child) =
        spawn_through_the_tool(&mut session, json!({"task_name": "helper"})).await;

    // Asked once, for subagent work, relative to the parent's own model.
    assert_eq!(
        advisor.asked(),
        [(TaskKind::Subagent, selection("test/mid", "medium"))]
    );
    // The child ran on the pick.
    let child_requests: Vec<Seen> = model
        .seen()
        .into_iter()
        .filter(|seen| seen.model != "test/mid")
        .collect();
    assert!(!child_requests.is_empty());
    assert!(child_requests
        .iter()
        .all(|seen| seen.model == "test/big" && seen.effort.as_deref() == Some("high")));
    // The spawn tells the parent what was picked and why.
    assert_eq!(result["model"], "test/big");
    assert_eq!(result["reasoning_effort"], "high");
    assert_eq!(result["model_choice"]["source"], "ranking");
    assert_eq!(
        result["model_choice"]["reason"],
        "test/big @ high ranks first for subagent work"
    );
    // The child's journal keeps the whole decision.
    let profile = child_profile(dir.path(), child).await;
    assert_eq!(profile["model_choice"]["source"], "ranking");
    assert_eq!(profile["model_choice"]["kind"], "subagent");
    assert_eq!(
        profile["model_choice"]["evidence"]["leader"]["model"],
        "test/big"
    );
    assert_eq!(profile["model_choice"]["evidence"]["min_kind_turns"], 20);
    // Each of the child's turns says how its model was chosen.
    let child_turns = child_records(dir.path(), child).await;
    assert!(!child_turns.is_empty());
    for record in &child_turns {
        assert_eq!(
            signal_detail(record, "model_chosen").as_deref(),
            Some("ranking: test/big @ high ranks first for subagent work")
        );
    }
    // The parent's own turns are not subagent turns.
    assert!(records_without_signal(
        &records(&session).await,
        "model_chosen"
    ));
}

fn records_without_signal(records: &[TurnRecord], kind: &str) -> bool {
    records
        .iter()
        .all(|record| !signal_kinds(record).iter().any(|signal| signal == kind))
}

/// A custom agent's model is chosen from its own kind's ranking.
#[tokio::test]
async fn a_custom_agent_is_advised_for_its_own_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let agents = dir.path().join(".forge/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews the diff.\nmode: [read-only]\n\
         tools: [read_file]\n---\nReview carefully.\n",
    )
    .unwrap();
    let model = delegating_model(json!({"agent": "code-reviewer", "message": "review"}));
    let mut session = session(dir.path(), model).await;
    let kind = TaskKind::Custom("code-reviewer".into());
    let advisor = FakeAdvisor::new(fallback(
        "only 4 turns of custom:code-reviewer work; 20 are needed",
        kind.clone(),
    ));
    session.set_subagent_model_advisor(advisor.clone());

    let (result, _) = spawn_through_the_tool(&mut session, json!({})).await;
    assert_eq!(advisor.asked().len(), 1);
    assert_eq!(advisor.asked()[0].0, kind);
    assert_eq!(result["agent"], "code-reviewer");
    assert_eq!(result["model_choice"]["source"], "fallback");
}

/// Thin evidence: the child keeps today's behaviour (the parent's model and
/// effort), and says why.
#[tokio::test]
async fn a_fallback_leaves_the_child_on_the_parents_model_and_says_why() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = delegating_model(json!({"task_name": "helper", "message": "look around"}));
    let mut session = session(dir.path(), model.clone()).await;
    session.set_subagent_model_advisor(FakeAdvisor::new(fallback(
        "only 12 turns of subagent work on test in the last 90 days; 20 are needed",
        TaskKind::Subagent,
    )));

    let (result, child) = spawn_through_the_tool(&mut session, json!({})).await;
    assert!(model
        .seen()
        .iter()
        .all(|seen| seen.model == "test/mid" && seen.effort.as_deref() == Some("medium")));
    assert_eq!(result["model_choice"]["source"], "fallback");
    let profile = child_profile(dir.path(), child).await;
    assert_eq!(profile["model_choice"]["source"], "fallback");
    assert!(profile["model"].is_null(), "no model was picked: {profile}");
    let records = child_records(dir.path(), child).await;
    assert_eq!(
        signal_detail(&records[0], "model_chosen").as_deref(),
        Some("fallback: only 12 turns of subagent work on test in the last 90 days; 20 are needed")
    );
}

/// A tier, a model or an effort is the caller's choice: the advisor is not
/// asked, and nothing is recorded about a choice it did not make.
#[tokio::test]
async fn an_explicit_tier_model_or_effort_is_not_second_guessed() {
    for arguments in [
        json!({"task_name": "helper", "message": "m", "tier": "fast"}),
        json!({"task_name": "helper", "message": "m", "model": "test/small"}),
        json!({"task_name": "helper", "message": "m", "reasoning_effort": "low"}),
    ] {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let model = delegating_model(arguments.clone());
        let mut session = session(dir.path(), model).await;
        let advisor = FakeAdvisor::new(ranked_pick());
        session.set_subagent_model_advisor(advisor.clone());

        let (result, child) = spawn_through_the_tool(&mut session, arguments.clone()).await;
        assert!(advisor.asked().is_empty(), "{arguments}");
        assert!(
            result.get("model_choice").is_none(),
            "{arguments}: {result}"
        );
        let profile = child_profile(dir.path(), child).await;
        assert!(
            profile.get("model_choice").is_none(),
            "{arguments}: {profile}"
        );
        assert!(records_without_signal(
            &child_records(dir.path(), child).await,
            "model_chosen"
        ));
    }
}

/// Without an advisor (the offline mock, a direct API caller) nothing
/// changes: the child inherits the parent's model and nothing is recorded.
#[tokio::test]
async fn without_an_advisor_nothing_changes() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = delegating_model(json!({"task_name": "helper", "message": "look around"}));
    let mut session = session(dir.path(), model.clone()).await;
    let (result, child) = spawn_through_the_tool(&mut session, json!({})).await;
    assert!(model.seen().iter().all(|seen| seen.model == "test/mid"));
    assert!(result.get("model_choice").is_none());
    assert!(child_profile(dir.path(), child)
        .await
        .get("model_choice")
        .is_none());
}

/// A child reopened from its journal runs on the model it was picked, and
/// still knows why.
#[tokio::test]
async fn a_reopened_child_keeps_its_pick_and_the_reason() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let mut session = session(dir.path(), TierModel::new(&[])).await;
    session.set_subagent_model_advisor(FakeAdvisor::new(ranked_pick()));
    let args: SpawnAgentArgs =
        serde_json::from_value(json!({"task_name": "helper", "message": "m"})).unwrap();
    let spec = session.subagent_spec_for(args).unwrap();
    assert_eq!(spec.profile.model_choice, Some(ranked_pick().choice));
    let child_id = forge_durable::new_session_id();
    let child = session
        .create_child(
            child_id,
            dir.path().to_path_buf(),
            tokio_util::sync::CancellationToken::new(),
            &spec,
        )
        .await
        .unwrap();
    assert_eq!(child.model_selection().model, "test/big");
    drop(child);

    let resumed = session
        .resume_child(
            child_id,
            dir.path().to_path_buf(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(resumed.model_selection().model, "test/big");
    assert_eq!(
        resumed.model_selection().reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(
        resumed
            .child_profile()
            .and_then(|profile| profile.model_choice.clone()),
        Some(ranked_pick().choice)
    );
}

/// A picked child that fails is still re-run on the strong tier, and both
/// turns say how the child's model was chosen.
#[tokio::test]
async fn a_picked_child_still_escalates() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let model = TierModel::new(&[
        (
            "test/mid",
            vec![
                call(
                    "spawn-1",
                    "spawn_agent",
                    json!({"task_name": "helper", "message": "fix it"}),
                ),
                text("Delegated."),
            ],
        ),
        ("test/small", vec![dangling()]),
        ("test/big", vec![text("Fixed it on the strong tier.")]),
    ]);
    let mut session = session(dir.path(), model.clone()).await;
    let fast = FakeTiers.resolve_tier(&TierRequest {
        tier: ModelTier::Fast,
        current: &selection("test/mid", "medium"),
    });
    let mut advice = ranked_pick();
    advice.pick = Some(fast.model);
    session.set_subagent_model_advisor(FakeAdvisor::new(advice));

    let (_, child) = spawn_through_the_tool(&mut session, json!({})).await;
    let records = child_records(dir.path(), child).await;
    assert_eq!(records.len(), 2, "{records:#?}");
    assert!(records[0].escalated && records[1].escalation_retry);
    assert_eq!(records[0].model, "test/small");
    assert_eq!(records[1].model, "test/big");
    for record in &records {
        assert!(signal_kinds(record)
            .iter()
            .any(|kind| kind == "model_chosen"));
    }
}
