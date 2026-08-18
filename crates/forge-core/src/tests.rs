//! `AgentSession` unit tests.
//!
//! Split out of `lib.rs`; moved verbatim.

use super::*;
use async_trait::async_trait;
use forge_governance::AclPolicy;
use forge_model::MockModelClient;
use forge_types::{Message, MessageRole, SideEffectClass, ToolCall, ToolOutput, Usage};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::tempdir;
use tokio::sync::Notify;

struct GatedTool {
    started: Arc<AtomicBool>,
    release: Arc<Notify>,
}

/// Emits more events than the core relay can buffer before returning. This
/// models a provider burst and guards the relay-drain ordering from regressing
/// into a completion-time deadlock.
struct BurstStreamingModel {
    events: usize,
}

#[async_trait]
impl forge_model::ModelClient for BurstStreamingModel {
    async fn complete(
        &self,
        _req: forge_model::ModelRequest,
    ) -> Result<ModelResponse, forge_model::ModelError> {
        unreachable!("streaming path only")
    }

    async fn complete_with_stream(
        &self,
        _req: forge_model::ModelRequest,
        tx: Option<forge_model::StreamEventTx>,
    ) -> Result<ModelResponse, forge_model::ModelError> {
        let tx = tx.expect("core supplies a stream sender");
        for _ in 0..self.events {
            tx.send(forge_types::ModelStreamEvent::TextDelta { text: "x".into() })
                .unwrap();
        }
        tx.send(forge_types::ModelStreamEvent::MessageEnd).unwrap();
        Ok(ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        })
    }

    fn clear_provider_env(&self) {}
}

#[async_trait]
impl forge_tools::Tool for GatedTool {
    fn name(&self) -> &str {
        "gated"
    }

    fn description(&self) -> &str {
        "Wait until the test releases execution"
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
        self.started.store(true, Ordering::SeqCst);
        self.release.notified().await;
        Ok(ToolOutput {
            outcome: Default::default(),
            content: "released".into(),
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

#[test]
fn system_prompt_uses_forge_policy() {
    let prompt = assemble_system_prompt("", &[]);
    assert!(prompt.starts_with("You are a coding agent running in the Forge"));
    assert!(prompt.contains("Forge is an open source project led by NorviaLabs."));
    assert!(!prompt.contains("# Project Instructions"));
    // Codex leftover: a leading "always use apply_patch" would override the
    // File edits section that prefers `edit` for a focused change.
    assert!(!prompt.contains("Use the `apply_patch` tool to edit files"));
    assert!(prompt.contains("## File edits"));
    assert!(prompt.contains("**`edit`**"));
    assert!(prompt.contains("**`write_file`**"));
    assert!(prompt.contains("**`apply_patch`** — Multi-hunk or multi-file diffs."));
}

#[test]
fn default_system_prompt_stays_under_the_token_budget() {
    let prompt = assemble_system_prompt("", &[]);
    let tokens = forge_context::estimate_tokens(&prompt);
    assert!(
        tokens < 6_500,
        "baseline system prompt is {tokens} tokens; keep discovery text short"
    );
}

#[tokio::test]
async fn model_response_application_releases_session_while_tool_runs() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(Notify::new());
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(GatedTool {
        started: started.clone(),
        release: release.clone(),
    }));
    let mut cfg = base_cfg(dir.path());
    cfg.enable_governance = false;
    let mut session = AgentSession::create(cfg, model, tools).await.unwrap();
    session
        .append_user_message("run the gated tool")
        .await
        .unwrap();

    let application = session
        .begin_model_response_application(ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call-gated".into(),
                name: "gated".into(),
                arguments: json!({}),
            }],
            usage: None,
            thinking: None,
        })
        .await
        .unwrap();
    let ModelResponseApplication::Execute(pending) = application else {
        panic!("tool execution should be returned to the caller");
    };

    let handle = tokio::spawn((*pending).execute());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Working);
    assert!(session.messages.iter().any(|message| {
        message.role == MessageRole::Assistant && !message.tool_calls.is_empty()
    }));

    release.notify_one();
    let completed = handle.await.unwrap();
    let application = session.finish_tool_application(completed).await.unwrap();
    assert!(matches!(
        application,
        ModelResponseApplication::Finished(ApplyOutcome::Continue)
    ));
    assert!(session
        .messages
        .iter()
        .any(|message| { message.role == MessageRole::Tool && message.content == "released" }));
}

#[test]
fn system_prompt_appends_project_instructions() {
    let prompt = assemble_system_prompt("Run cargo test", &[]);
    assert!(prompt.starts_with("You are a coding agent running in the Forge"));
    assert!(prompt.ends_with("AGENTS.md:\nRun cargo test"));
}

fn legacy_skill(name: &str, body: &str) -> forge_context::SkillManifest {
    forge_context::SkillManifest {
        name: name.to_string(),
        description: String::new(),
        dir: std::path::PathBuf::new(),
        body: body.to_string(),
        has_frontmatter: false,
        metadata: None,
        compatibility: None,
        license: None,
    }
}

fn manifest_skill(name: &str, description: &str, body: &str) -> forge_context::SkillManifest {
    forge_context::SkillManifest {
        name: name.to_string(),
        description: description.to_string(),
        dir: std::path::PathBuf::new(),
        body: body.to_string(),
        has_frontmatter: true,
        metadata: None,
        compatibility: None,
        license: None,
    }
}

/// A skill with no YAML frontmatter has no `description` to show in
/// discovery, so its full body is injected eagerly — the pre-#226
/// behavior, preserved for backward compatibility.
#[test]
fn system_prompt_appends_skills_without_frontmatter_eagerly() {
    let skills = vec![legacy_skill("ponytail", "# Ponytail\nUse less code.")];
    let prompt = assemble_system_prompt("", &skills);
    assert!(prompt.contains("# Skills"));
    assert!(prompt.contains("## ponytail"));
    assert!(prompt.ends_with("# Ponytail\nUse less code."));
}

/// A skill with frontmatter only surfaces name + description (discovery
/// stage) — its full body must NOT appear in the system prompt, since the
/// model is expected to fetch it via `load_skill` on demand.
#[test]
fn system_prompt_shows_only_name_and_description_for_skills_with_frontmatter() {
    let skills = vec![manifest_skill(
        "reviewer",
        "Reviews pull requests for style issues.",
        "# Reviewer\n\nFull instructions that should stay out of the prompt.",
    )];
    let prompt = assemble_system_prompt("", &skills);
    assert!(prompt.contains("# Skills"));
    assert!(prompt.contains("## reviewer"));
    assert!(prompt.contains("Reviews pull requests for style issues."));
    assert!(!prompt.contains("Full instructions that should stay out of the prompt"));
    assert!(prompt.contains("load_skill"));
}

fn base_cfg(dir: &std::path::Path) -> LoopConfig {
    LoopConfig {
        max_turns: 5,
        workspace: dir.to_path_buf(),
        journal_dir: dir.join("j"),
        enable_context_lifecycle: true,
        enable_governance: true,
        ..Default::default()
    }
}

/// `tool_count` is the cheap form of `list_tools().len()` and must stay
/// exactly equivalent to it, including the governance filter — the status
/// bar reports it as the number of tools the model can see.
#[tokio::test]
async fn tool_count_matches_the_length_of_the_listed_tools() {
    for enable_governance in [true, false] {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let cfg = LoopConfig {
            enable_governance,
            ..base_cfg(dir.path())
        };
        let s = AgentSession::create(cfg, model, ToolRegistry::new())
            .await
            .unwrap();
        assert_eq!(
            s.tool_count(),
            s.list_tools().len(),
            "tool_count diverged from list_tools with enable_governance={enable_governance}"
        );
    }
}

#[tokio::test]
async fn growing_the_last_message_refreshes_the_context_token_estimate() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut session = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session
        .messages
        .push(Message::new(MessageRole::User, "short"));
    let before = session.context_usage_ratio();
    session
        .messages
        .last_mut()
        .unwrap()
        .content
        .push_str(&"x".repeat(8_000));
    let after = session.context_usage_ratio();
    assert!(
        after > before,
        "a growing last message must raise the in-context estimate ({before} -> {after})"
    );
}

#[tokio::test]
async fn appending_messages_updates_the_context_estimate_from_the_tail() {
    let dir = tempdir().unwrap();
    let mut session = idle_session(dir.path()).await;
    session
        .messages
        .push(Message::new(MessageRole::User, "first message"));
    let before = session.context_usage_ratio();
    let cache_before = {
        let cache = session.ctx_tokens_cache.lock().unwrap();
        let cached = cache.as_ref().unwrap();
        (cached.storage_id, cached.fingerprint)
    };

    session
        .messages
        .push(Message::new(MessageRole::Tool, "second message"));
    let after = session.context_usage_ratio();
    let cache_after = session
        .ctx_tokens_cache
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .fingerprint;

    assert!(after > before);
    assert_eq!(cache_before.0, session.messages.storage_id());
    assert_eq!(cache_after.messages, cache_before.1.messages + 1);
}

#[tokio::test]
async fn session_omits_mock_web_search_with_default_config() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let names = s.list_tools();
    assert!(
        !names.iter().any(|n| n == "web_search"),
        "mock web_search must not be advertised, got {names:?}"
    );
}

#[tokio::test]
async fn session_omits_web_search_when_disabled() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut cfg = base_cfg(dir.path());
    cfg.web_search.enabled = false;
    let s = AgentSession::create(cfg, model, ToolRegistry::new())
        .await
        .unwrap();
    assert!(!s.list_tools().iter().any(|n| n == "web_search"));
}

#[tokio::test]
async fn build_model_request_carries_reasoning_effort_when_set() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.set_reasoning_effort(Some("high".into()));
    assert_eq!(
        s.build_model_request().reasoning_effort,
        Some("high".to_string())
    );
}

#[tokio::test]
async fn consecutive_requests_preserve_stripped_prompt_prefix() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("first").await.unwrap();
    s.prepare_model_step(1).await.unwrap();
    let first = s.last_prompt_snapshot_for_tests();
    s.messages.push(Message::new(MessageRole::User, "second"));
    s.prepare_model_step(2).await.unwrap();
    let second = s.last_prompt_snapshot_for_tests();
    let common = forge_model::common_prefix_len(&first.1, &second.1);
    assert_eq!(
        common,
        first.1.len(),
        "request 1 must be a prefix of request 2"
    );
}

#[tokio::test]
async fn effort_toggle_does_not_change_compared_prompt_snapshot() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("hi").await.unwrap();
    s.prepare_model_step(1).await.unwrap();
    let first = s.last_prompt_snapshot_for_tests();
    s.set_reasoning_effort(Some("high".into()));
    s.prepare_model_step(2).await.unwrap();
    let second = s.last_prompt_snapshot_for_tests();
    assert_eq!(
        forge_model::common_prefix_len(&first.1, &second.1),
        first.1.len()
    );
}

/// The prefix must stay append-only across a *tool* turn, not just across
/// appended user text.
///
/// `consecutive_requests_preserve_stripped_prompt_prefix` only ever appends
/// user messages, which is the one shape that could hardly break. A real turn
/// interleaves an assistant tool call, a tool result and an answer, and any of
/// those rewriting an earlier message would cost the whole cached prefix —
/// tools and system sit in front of the messages, so a break anywhere lands
/// before everything.
#[tokio::test]
async fn the_prompt_prefix_is_append_only_across_a_tool_turn() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;

    let mut prev: Option<Vec<u8>> = None;
    let mut step = 0usize;
    let report = |s: &AgentSession, prev: &mut Option<Vec<u8>>, step: &mut usize, what: &str| {
        let (_, bytes) = s.last_prompt_snapshot_for_tests();
        if let Some(p) = prev.as_deref() {
            let common = forge_model::common_prefix_len(p, &bytes);
            assert_eq!(
                common,
                p.len(),
                "step {step} ({what}) rewrote the prompt {} bytes in, discarding \
                 the cached prefix from there on",
                common
            );
        }
        *prev = Some(bytes);
        *step += 1;
    };

    s.append_user_message("first").await.unwrap();
    s.prepare_model_step(1).await.unwrap();
    report(&s, &mut prev, &mut step, "user 1");

    s.messages.push(assistant_with_tool_call("read_file"));
    s.prepare_model_step(2).await.unwrap();
    report(&s, &mut prev, &mut step, "assistant tool call");

    let mut result = Message::new(MessageRole::Tool, "file contents here");
    result.tool_call_id = Some("c1".into());
    s.messages.push(result);
    s.prepare_model_step(3).await.unwrap();
    report(&s, &mut prev, &mut step, "tool result");

    s.messages
        .push(Message::new(MessageRole::Assistant, "done"));
    s.prepare_model_step(4).await.unwrap();
    report(&s, &mut prev, &mut step, "assistant answer");

    s.messages.push(Message::new(MessageRole::User, "second"));
    s.prepare_model_step(5).await.unwrap();
    report(&s, &mut prev, &mut step, "user 2");
}

#[tokio::test]
async fn prefix_diagnostics_name_the_first_mutated_path() {
    let previous = serde_json::json!({"messages":[{"role":"system","content":"a"}]});
    let current = serde_json::json!({"messages":[{"role":"system","content":"b"}]});
    assert_eq!(
        forge_model::first_json_pointer(&previous, &current).as_deref(),
        Some("/messages/0/content")
    );
}

#[tokio::test]
async fn build_model_request_omits_reasoning_effort_when_none() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert_eq!(s.build_model_request().reasoning_effort, None);
}

#[tokio::test]
async fn view_image_stays_listed_when_image_input_is_disabled() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert!(s.list_tools().iter().any(|n| n == "view_image"));
    let without = s.build_model_request().tools;
    assert!(without.iter().any(|t| t.name == "view_image"));
    s.set_image_input_supported(true);
    let with = s.build_model_request().tools;
    assert_eq!(
        without
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        with.iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn model_request_shares_the_session_transcript_without_attachments() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message("look").await.unwrap();

    let req = s.build_model_request();

    assert!(req.messages.shares_storage_with(&s.messages));
}

#[tokio::test]
async fn missing_pasted_image_is_noted_at_insert_and_stays_frozen() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message_with_attachments(
        "look",
        vec![forge_types::ImageRef::new("gone.png", "image/png", 8)],
    )
    .await
    .unwrap();

    let req = s.build_model_request();
    let request_user = req
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    let session_user = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();

    assert!(req.messages.shares_storage_with(&s.messages));
    assert!(request_user.attachments.is_empty());
    assert!(request_user.content.contains("no longer available"));
    assert_eq!(session_user.content, request_user.content);
    let first = session_user.content.clone();
    let req2 = s.build_model_request();
    let request_user2 = req2
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    assert_eq!(request_user2.content, first);
}

#[tokio::test]
async fn pasted_image_snapshot_survives_deleting_the_original() {
    let dir = tempdir().unwrap();
    let png = forge_types::sample_png_bytes();
    std::fs::write(dir.path().join("shot.png"), &png).unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message_with_attachments(
        "look",
        vec![forge_types::ImageRef::new(
            "shot.png",
            "image/png",
            png.len() as u64,
        )],
    )
    .await
    .unwrap();
    let session_user = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    assert_eq!(session_user.attachments.len(), 1);
    assert_ne!(session_user.attachments[0].path, "shot.png");
    std::fs::remove_file(dir.path().join("shot.png")).unwrap();

    let req = s.build_model_request();
    let request_user = req
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    assert_eq!(request_user.attachments.len(), 1);
    assert_eq!(
        request_user.attachments[0].path,
        session_user.attachments[0].path
    );
    assert!(!request_user.content.contains("no longer available"));
}

/// A streaming step must forward every event to `forward`, including when
/// the model returns immediately and `select!` breaks on the model handle
/// with events still in the relay.
///
/// This is a property test, not a reproducer: the loss it guards against
/// is a timing race between the relay thread and the select loop, and it
/// does not reproduce on demand. It was observed once as a streaming turn
/// that emitted no deltas at all, which is what prompted joining the relay
/// before draining in `stream.rs`.
#[tokio::test]
async fn a_streaming_step_forwards_its_deltas() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "streamed answer".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut session = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session
        .messages
        .push(Message::new(MessageRole::User, "hello"));

    let (tx, rx) = std::sync::mpsc::channel();
    let response = session
        .run_model_step_with_stream(0, Some(tx))
        .await
        .unwrap();
    assert_eq!(response.text, "streamed answer");

    let forwarded: Vec<_> = rx.into_iter().collect();
    assert!(
        forwarded.iter().any(|event| matches!(
            event,
            forge_types::ModelStreamEvent::TextDelta { text }
                if text == "streamed answer"
        )),
        "the text delta was dropped; forwarded: {forwarded:?}"
    );
}

/// A bounded relay must keep draining after the model returns: joining it
/// first would block forever once its bounded queue filled.
#[tokio::test]
async fn streaming_burst_over_relay_capacity_completes_and_preserves_deltas() {
    let dir = tempdir().unwrap();
    let event_count = 256;
    let model = Arc::new(BurstStreamingModel {
        events: event_count,
    });
    let mut session = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session
        .messages
        .push(Message::new(MessageRole::User, "hello"));
    let (tx, rx) = std::sync::mpsc::channel();

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        session.run_model_step_with_stream(0, Some(tx)),
    )
    .await
    .expect("a full relay must not deadlock completion")
    .unwrap();
    assert_eq!(response.text, "done");

    let forwarded: String = rx
        .into_iter()
        .filter_map(|event| match event {
            forge_types::ModelStreamEvent::TextDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, "x".repeat(event_count));
}

#[tokio::test]
async fn run_model_step_with_stream_merges_stream_usage() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "streamed".into(),
        tool_calls: vec![],
        usage: Some(Usage {
            prompt_tokens: 11,
            completion_tokens: 4,
            ..Default::default()
        }),
        thinking: Some("trace".into()),
    }]));
    let mut session = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    session
        .messages
        .push(Message::new(MessageRole::User, "hello"));
    let response = session.run_model_step_with_stream(0, None).await.unwrap();
    assert_eq!(response.text, "streamed");
    assert_eq!(response.usage.unwrap().prompt_tokens, 11);
    assert_eq!(response.thinking.as_deref(), Some("trace"));
}

#[tokio::test]
async fn loop_text_only_completes() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "all done".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let r = s.run_user_message("hello").await.unwrap();
    assert_eq!(r.text, "all done");
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
}

#[tokio::test]
async fn promote_next_queued_starts_a_new_task_and_removes_the_item() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);

    let item = s.enqueue_task("do the next thing").await.unwrap();
    assert_eq!(s.queue().len(), 1);

    let task_id = s.promote_next_queued().await.unwrap().unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
    assert_eq!(s.active_task.task_id, task_id);
    // Promotion removed exactly the one item from the visible queue.
    assert_eq!(s.queue().len(), 0);
    assert!(s.queue().visible().all(|q| q.id != item.id));
    assert!(s.messages.iter().any(|m| m.content == "do the next thing"));
}

#[tokio::test]
async fn promote_next_queued_on_empty_queue_is_a_no_op() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    assert_eq!(s.promote_next_queued().await.unwrap(), None);
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);
}

#[tokio::test]
async fn cancel_queued_at_removes_by_visible_position() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.enqueue_task("a").await.unwrap();
    let b = s.enqueue_task("b").await.unwrap();
    s.enqueue_task("c").await.unwrap();

    let removed = s.cancel_queued_at(2).await.unwrap().unwrap();
    assert_eq!(removed.id, b.id);
    let remaining: Vec<&str> = s.queue().visible().map(|q| q.text.as_str()).collect();
    assert_eq!(remaining, vec!["a", "c"]);
}

#[tokio::test]
async fn queue_items_survive_resume_without_duplication() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message("first task").await.unwrap();
    s.enqueue_task("queued one").await.unwrap();
    s.enqueue_task("queued two").await.unwrap();
    assert_eq!(s.queue().len(), 2);

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();
    assert_eq!(resumed.queue().len(), 2);
    let texts: Vec<&str> = resumed.queue().visible().map(|q| q.text.as_str()).collect();
    assert_eq!(texts, vec!["queued one", "queued two"]);
}

/// End-to-end crash-recovery test: a crash between the `QueuePromoting`
/// journal write and the confirming `QueuePromoted` one — but *after*
/// the task's user message already landed — must not resurrect the item
/// as `Queued` on resume. That would let a later `promote_next_queued`
/// create a second task from the same instruction, violating "a queue
/// item cannot execute twice."
#[tokio::test]
async fn crash_after_task_created_but_before_promoted_confirmation_does_not_duplicate() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let item = s.enqueue_task("do the thing").await.unwrap();

    // Simulate a crash: journal exactly what `promote_next_queued`'s
    // first steps write (mark Promoting, then the task's user message)
    // but never reach the confirming `QueuePromoted` event.
    let journal = forge_durable::Journal::open(s.journal_dir(), s.session_id)
        .await
        .unwrap();
    journal
        .append_queue_promoting(s.session_id, item.id)
        .await
        .unwrap();
    journal
        .append_user_message(s.session_id, &item.text)
        .await
        .unwrap();

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();

    // Not visible in the queue again — the task was already created.
    assert!(resumed.queue().is_empty());
    assert!(resumed.queue().peek_next_queued().is_none());
    // The task's user message did survive, exactly once.
    assert_eq!(
        resumed
            .messages
            .iter()
            .filter(|m| m.content == "do the thing")
            .count(),
        1
    );
}

/// Contrasting case: a crash *before* the task's user message was ever
/// journaled must return the item to `Queued` so it isn't lost.
#[tokio::test]
async fn crash_before_task_created_reverts_item_to_queued() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let queued = s.enqueue_task("not started yet").await.unwrap();
    let journal = forge_durable::Journal::open(s.journal_dir(), s.session_id)
        .await
        .unwrap();
    journal
        .append_queue_promoting(s.session_id, queued.id)
        .await
        .unwrap();
    // No `append_user_message` — the crash happened before the task
    // itself was created.

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();

    assert_eq!(resumed.queue().len(), 1);
    assert_eq!(
        resumed.queue().peek_next_queued().map(|q| q.text.as_str()),
        Some("not started yet")
    );
}

#[tokio::test]
async fn cancelling_the_active_task_preserves_the_queue() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message("first task").await.unwrap();
    s.enqueue_task("queued while busy").await.unwrap();
    assert_eq!(s.queue().len(), 1);

    s.mark_cancelled().await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
    // Cancellation must not silently clear the queue.
    assert_eq!(s.queue().len(), 1);
}

#[tokio::test]
async fn loop_runs_tool_then_finishes() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "data").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "f.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "read ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let r = s.run_user_message("read it").await.unwrap();
    assert_eq!(r.text, "read ok");
    let assistant = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant.tool_calls[0].id, "1");
}

#[tokio::test]
async fn malformed_read_file_offset_is_rejected_and_does_not_execute() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello\nworld\n").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "bad".into(),
                name: "read_file".into(),
                // Exact observed failure class — composite string must not be salvaged.
                arguments: json!({"path": "README.md", "offset": "1arglimit\">100"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "ok".into(),
                name: "read_file".into(),
                arguments: json!({"path": "README.md", "offset": 1, "limit": 100}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "Forge is a Rust workspace.".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let r = s.run_user_message("Summarize this codebase").await.unwrap();
    assert_eq!(r.text, "Forge is a Rust workspace.");
    let tool_msgs: Vec<_> = s
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .collect();
    assert!(
        tool_msgs.iter().any(|m| m.content.contains("validation")),
        "expected validation rejection: {tool_msgs:?}"
    );
    assert!(
        tool_msgs.iter().any(|m| m.content.contains("hello")),
        "valid retry should execute and return file content: {tool_msgs:?}"
    );
    // Validation feedback may quote the bad value; execution must not have
    // salvaged it into a successful read of the wrong slice.
    assert!(
        tool_msgs
            .iter()
            .filter(|m| m.content.contains("hello"))
            .all(|m| !m.content.contains("validation")),
        "successful tool result must not be a validation message"
    );
}

#[tokio::test]
async fn repeated_malformed_read_file_exhausts_validation_budget() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "x\n").unwrap();
    let bad = || ToolCall {
        id: "bad".into(),
        name: "read_file".into(),
        arguments: json!({"path": "README.md", "offset": "1arglimit\">50"}),
    };
    // Four invalid attempts across model steps → budget exhausts → terminal failure.
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![bad()],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![bad()],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![bad()],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![bad()],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let _ = s.run_user_message("read it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert!(
        s.messages
            .iter()
            .any(|m| m.content.starts_with(TURN_FAILED_MARKER)),
        "expected durable failure summary: {:?}",
        s.messages
    );
    assert!(
        s.events.iter().any(|e| e.kind == "turn_failed"),
        "expected turn_failed event"
    );
    assert!(
        s.messages.iter().any(|m| {
            m.role == MessageRole::Tool && m.content.contains("validation retry budget exceeded")
        }) || s.events.iter().any(|e| e.kind == "validation_exhausted"),
        "expected budget exhaustion signal"
    );
}

#[tokio::test]
async fn empty_final_after_tools_is_terminal_failure_not_success() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "data").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "f.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        // Model ends with no answer after tools.
        ModelResponse {
            text: "".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let _ = s.run_user_message("read it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert!(s
        .messages
        .iter()
        .any(|m| m.content.starts_with(TURN_FAILED_MARKER)));
}

#[tokio::test]
async fn resume_restores_conversation_context_and_usage() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "data").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "f.txt"}),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                ..Default::default()
            }),
            thinking: Some("inspect".into()),
        },
        ModelResponse {
            text: "read ok".into(),
            tool_calls: vec![],
            usage: Some(Usage {
                prompt_tokens: 20,
                completion_tokens: 4,
                ..Default::default()
            }),
            thinking: None,
        },
    ]));
    let cfg = base_cfg(dir.path());
    let mut session = AgentSession::create(cfg.clone(), model, ToolRegistry::new())
        .await
        .unwrap();
    session.run_user_message("read it").await.unwrap();
    let session_id = session.session_id;
    drop(session);

    let resumed = AgentSession::resume(
        cfg,
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    let roles = resumed
        .messages
        .iter()
        .map(|message| message.role)
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![
            MessageRole::System,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::Tool,
            MessageRole::Assistant,
        ]
    );
    assert_eq!(resumed.messages[2].tool_calls[0].id, "1");
    assert_eq!(resumed.messages[4].content, "read ok");
    assert_eq!(resumed.token_usage.prompt_tokens, 30);
    assert_eq!(resumed.token_usage.completion_tokens, 6);
    assert_eq!(resumed.token_usage.model_steps, 2);
}

#[tokio::test]
async fn resume_replays_validation_failed_tool_message() {
    use forge_durable::{new_session_id, Journal};

    let dir = tempdir().unwrap();
    let journal_dir = dir.path().join("j");
    let sid = new_session_id();
    let journal = Journal::open(&journal_dir, sid).await.unwrap();
    journal.append_session_created(sid).await.unwrap();
    journal.append_user_message(sid, "run").await.unwrap();
    let content = tool_validation_failed_content(&"offset must be an integer");
    journal
        .append_validation_failed(sid, "c1", "read_file", &content)
        .await
        .unwrap();

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        sid,
    )
    .await
    .unwrap();
    let tool = resumed
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("validation-failed tool message");
    assert_eq!(tool.tool_call_id.as_deref(), Some("c1"));
    assert_eq!(tool.name.as_deref(), Some("read_file"));
    assert_eq!(tool.content, content);
}

#[tokio::test]
async fn resume_serves_journaled_tool_without_reexecuting() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "first").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "f.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("read").await.unwrap();
    let session_id = s.session_id;
    std::fs::write(dir.path().join("f.txt"), "second").unwrap();

    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "f.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done again".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut resumed =
        AgentSession::resume(base_cfg(dir.path()), model, ToolRegistry::new(), session_id)
            .await
            .unwrap();
    resumed.run_user_message("read again").await.unwrap();
    let tool_messages: Vec<_> = resumed
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 1);
    assert!(tool_messages[0].content.contains("first"));
    assert!(!tool_messages[0].content.contains("second"));
}

#[tokio::test]
async fn resume_reconciles_non_idempotent_incomplete_intent() {
    use forge_durable::{new_session_id, Journal};

    let dir = tempdir().unwrap();
    let journal_dir = dir.path().join("j");
    let sid = new_session_id();
    let journal = Journal::open(&journal_dir, sid).await.unwrap();
    journal.append_session_created(sid).await.unwrap();
    journal.append_user_message(sid, "run").await.unwrap();
    let call = ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        arguments: json!({"command": "echo hi"}),
    };
    let response = ModelResponse {
        text: String::new(),
        tool_calls: vec![call.clone()],
        usage: None,
        thinking: None,
    };
    journal
        .append_model_response(sid, serde_json::to_value(&response).unwrap())
        .await
        .unwrap();
    journal.append_tool_intent(sid, &call).await.unwrap();

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        sid,
    )
    .await
    .unwrap();
    let tool = resumed
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("synthetic tool result");
    assert!(tool.content.contains("not marked idempotent"));
    let state = Journal::open(&journal_dir, sid)
        .await
        .unwrap()
        .replay(sid)
        .await
        .unwrap();
    assert!(state.incomplete_intents.is_empty());
}

#[tokio::test]
async fn resume_session_report_includes_composer_lines_in_order() {
    use forge_durable::{new_session_id, Journal};

    let dir = tempdir().unwrap();
    let journal_dir = dir.path().join("j");
    let sid = new_session_id();
    let journal = Journal::open(&journal_dir, sid).await.unwrap();
    journal.append_session_created(sid).await.unwrap();
    // A mix of a local-only command and real messages — all should come
    // back, since ComposerLineSubmitted fires for every submission.
    journal.append_composer_line(sid, "/status").await.unwrap();
    journal.append_composer_line(sid, "second").await.unwrap();
    journal.append_composer_line(sid, "third").await.unwrap();

    let mut current = AgentSession::create(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
    )
    .await
    .unwrap();

    let report = current.resume_session(sid).await.unwrap();
    assert_eq!(
        report.composer_lines,
        vec![
            "/status".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );
}

#[tokio::test]
async fn resume_session_into_self_derives_composer_lines_from_own_journal() {
    let dir = tempdir().unwrap();
    let mut session = AgentSession::create(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    session.record_composer_line("only message").await.unwrap();
    let sid = session.session_id;

    let report = session.resume_session(sid).await.unwrap();
    assert_eq!(report.composer_lines, vec!["only message".to_string()]);
}

#[tokio::test]
async fn resume_retries_idempotent_incomplete_intent() {
    use forge_durable::{new_session_id, Journal};

    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "payload").unwrap();
    let journal_dir = dir.path().join("j");
    let sid = new_session_id();
    let journal = Journal::open(&journal_dir, sid).await.unwrap();
    journal.append_session_created(sid).await.unwrap();
    journal.append_user_message(sid, "read").await.unwrap();
    let call = ToolCall {
        id: "c1".into(),
        name: "read_file".into(),
        arguments: json!({"path": "f.txt"}),
    };
    let response = ModelResponse {
        text: String::new(),
        tool_calls: vec![call.clone()],
        usage: None,
        thinking: None,
    };
    journal
        .append_model_response(sid, serde_json::to_value(&response).unwrap())
        .await
        .unwrap();
    journal.append_tool_intent(sid, &call).await.unwrap();

    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        sid,
    )
    .await
    .unwrap();
    let tool = resumed
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("retried tool result");
    assert!(tool.content.contains("payload"));
    let state = Journal::open(&journal_dir, sid)
        .await
        .unwrap()
        .replay(sid)
        .await
        .unwrap();
    assert!(state.incomplete_intents.is_empty());
}

/// F-RECOVERY-01: denying one trivial approval used to let the model
/// keep autonomously retrying for up to `max_turns` (128 by default)
/// steps before yielding control back — a single "no" shouldn't cost
/// that much. Two denials in a row within the same turn must now stop
/// the turn outright instead of continuing to churn.
#[tokio::test]
async fn repeated_hitl_denials_stop_the_turn_instead_of_retrying_to_max_turns() {
    let dir = tempdir().unwrap();
    let push = ToolCall {
        id: "1".into(),
        name: "bash".into(),
        arguments: json!({"command": "git push origin main"}),
    };
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![push.clone()],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "2".into(),
                ..push.clone()
            }],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("push").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
    assert_eq!(
        s.active_task.lifecycle,
        TaskLifecycle::Working,
        "a single denial must not fail the turn"
    );

    s.run_agent_turns(None).await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
    assert_eq!(
        s.active_task.lifecycle,
        TaskLifecycle::Failed,
        "a second consecutive denial must stop the turn"
    );
    assert!(s
        .messages
        .last()
        .unwrap()
        .content
        .contains("repeated denied approvals"));
}

#[tokio::test]
async fn hitl_pauses_on_git_push() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "".into(),
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git push origin main"}),
        }],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let r = s.run_user_message("push").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);
    assert!(s.pending_hitl().is_some());
    assert!(r.text.contains("HITL"));
    s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
    assert!(s.pending_hitl().is_none());
}

#[tokio::test]
async fn inspection_bash_runs_without_a_prompt_in_accept_edits() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls -la"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut tools = ToolRegistry::new();
    for tool in forge_tools::default_builtins() {
        tools.register(tool);
    }
    let mut s = AgentSession::create(base_cfg(dir.path()), model, tools)
        .await
        .unwrap();
    // `Governance::default()` reports Accept Edits but still gates shell until
    // a mode is applied — conservative on purpose, since nothing has yet
    // established that a sandbox is confining anything. Declare it here.
    s.apply_permission_mode(forge_governance::PermissionMode::AcceptEdits);

    s.run_user_message("look around").await.unwrap();

    // This used to pass because `readonly.rs` recognised `ls` and rewrote it
    // onto the dedicated tool, dodging the prompt. There is no classifier now:
    // Accept Edits does not gate shell at all, because it is only reachable
    // when a sandbox is confining it.
    assert!(
        s.pending_hitl().is_none(),
        "inspection bash must not prompt in Accept Edits"
    );
    assert_ne!(s.active_task.lifecycle, TaskLifecycle::Waiting);
    assert!(
        s.messages.iter().any(|m| m.content.contains("README.md")),
        "the command must actually run, got {:?}",
        s.messages
    );
}

#[tokio::test]
async fn rg_tool_name_is_a_synonym_for_grep() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "rg".into(),
                arguments: json!({"pattern": "hello"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut tools = ToolRegistry::new();
    for tool in forge_tools::default_builtins() {
        tools.register(tool);
    }
    let mut s = AgentSession::create(base_cfg(dir.path()), model, tools)
        .await
        .unwrap();
    s.run_user_message("search").await.unwrap();
    assert!(s.pending_hitl().is_none(), "rg synonym must not prompt");
    assert!(
        s.messages.iter().any(|m| {
            m.role == MessageRole::Tool
                && m.name.as_deref() == Some("grep")
                && m.content.contains("hello")
        }),
        "rg must run as the grep tool, got {:?}",
        s.messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .map(|m| (m.name.as_deref(), m.content.as_str()))
            .collect::<Vec<_>>()
    );
}

/// A deny with feedback folds the operator's note into the same tool
/// result message the agent sees, so it can act on it this turn instead
/// of needing to be re-prompted next turn.
#[tokio::test]
async fn hitl_deny_with_feedback_reaches_the_agent_as_tool_result_content() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "".into(),
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git push origin main"}),
        }],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("push").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl_with_feedback(
        HitlDecision::Deny,
        "test",
        Some("use --force-with-lease instead"),
    )
    .await
    .unwrap();

    let tool_message = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool result should record the denial");
    assert!(tool_message.content.contains("HITL denied by test"));
    assert!(tool_message
        .content
        .contains("use --force-with-lease instead"));
}

/// Whitespace-only feedback is treated the same as no feedback — the
/// message stays the plain denial rather than trailing an empty colon.
#[tokio::test]
async fn hitl_deny_with_blank_feedback_omits_it_from_the_message() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "".into(),
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git push origin main"}),
        }],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("push").await.unwrap();

    s.resolve_hitl_with_feedback(HitlDecision::Deny, "test", Some("   "))
        .await
        .unwrap();

    let tool_message = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .unwrap();
    assert_eq!(tool_message.content, "HITL denied by test");
}

/// Before this fix, the `WaitingForUser` evidence pushed at the HITL
/// pause point lingered in `turn_evidence` for the rest of the turn.
/// Approving and continuing would then have the completion evaluator see
/// stale `WaitingForUser` evidence and misroute the next no-tool-calls
/// model step through `finalize_turn_failure` as if the turn were
/// waiting/failed, even though the attempt had already resumed Working
/// and finished cleanly. `resolve_hitl` now strips that stale evidence on
/// resume, so the turn completes normally instead.
#[tokio::test]
async fn resuming_from_hitl_does_not_leak_stale_waiting_evidence_into_completion() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "echo ok"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
    // The stale WaitingForUser entry from the pause must be gone —
    // otherwise the next completion decision would misread it.
    assert!(!s
        .turn
        .evidence()
        .0
        .iter()
        .any(|e| e.event() == ExecutionEvent::WaitingForUser));

    let outcome = s.run_agent_turns(None).await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(outcome.text, "done");
}

#[tokio::test]
async fn hitl_large_tool_output_is_offloaded_before_insert() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "python3 -c \"print('x'*20000)\""}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);
    s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
    s.run_agent_turns(None).await.unwrap();

    let tool = s
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("HITL tool result");
    assert!(
        tool.content.contains("[offloaded tool output"),
        "expected CTX-01 stub, got {}",
        tool.content
    );
    let first = tool.content.clone();
    let req = s.build_model_request();
    let req_tool = req
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .unwrap();
    assert_eq!(req_tool.content, first);
}

#[tokio::test]
async fn acl_hides_denied_tools() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let mut acl = AclPolicy::allow_all();
    acl.deny("bash".into());
    s.set_governance(Governance::default().with_acl(acl));
    let names = s.list_tools();
    assert!(!names.iter().any(|n| n == "bash"));
    assert!(names.iter().any(|n| n == "read_file"));
}

/// The ACL denial arm in `run_one_tool` is the trailing wildcard, so this pins the
/// behaviour: a denied tool is refused at execution time, not merely hidden from the
/// catalogue. `acl_hides_denied_tools` covers listing; this covers execution.
#[tokio::test]
async fn acl_denied_tool_call_is_refused_at_execution_time() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "SENTINEL-CONTENT").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "secret.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let mut acl = AclPolicy::allow_all();
    acl.deny("read_file".into());
    s.set_governance(Governance::default().with_acl(acl));
    s.run_user_message("read it").await.unwrap();

    let tool_contents: Vec<&str> = s
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        tool_contents.iter().any(|c| c.contains("denied by ACL")),
        "expected an ACL denial result, got {tool_contents:?}"
    );
    assert!(
        !tool_contents.iter().any(|c| c.contains("SENTINEL-CONTENT")),
        "a denied tool must never execute"
    );
}

/// `resolve_hitl` derives approval explicitly rather than testing `== Deny`, so this
/// pins that an approval does **not** take the denial path. Together with
/// `hitl_pauses_on_git_push` (which covers deny) both branches are now exercised.
#[tokio::test]
async fn hitl_approve_does_not_take_the_denial_path() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                // `bash` is in the default `hitl_tools`, and since #26 that requires
                // approval for *every* command, so a benign one is enough to reach
                // the gate. No need to shell out to git.
                arguments: json!({"command": "echo ok"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
    assert!(s.pending_hitl().is_none());

    let tool_contents: Vec<&str> = s
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        !tool_contents.iter().any(|c| c.contains("HITL denied")),
        "approval must not be routed through the denial path, got {tool_contents:?}"
    );
    assert!(
        !tool_contents.is_empty(),
        "approval should reach execution and record a tool result"
    );
}

#[tokio::test]
async fn session_pattern_allow_skips_hitl_for_the_command_family() {
    let dir = tempdir().unwrap();
    let first = json!({"command": "git push -u origin main"});
    let sibling = json!({"command": "git push origin feature"});
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: first.clone(),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "first done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "2".into(),
                name: "bash".into(),
                arguments: sibling,
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "second done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert!(s.pending_hitl().is_some());
    assert_eq!(
        s.allow_suggested_pattern_for_session(&ToolCall {
            id: "remember".into(),
            name: "bash".into(),
            arguments: first,
        }),
        "bash(git push *)"
    );
    s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
    let _ = s.run_agent_turns(None).await.unwrap();
    assert!(
        s.pending_hitl().is_none(),
        "a matching command family must not prompt again after Allow pattern"
    );
}

#[tokio::test]
async fn offload_large_tool_output() {
    let dir = tempdir().unwrap();
    // Offloading now routes through the runtime-storage resolver, which
    // falls back to the platform application-data directory outside a
    // Git repository — git-init keeps this test's writes inside the
    // tempdir instead of touching the real host machine.
    init_repo(dir.path()).await;
    let big = "z".repeat(25_000);
    std::fs::write(dir.path().join("big.txt"), &big).unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "big.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("read big").await.unwrap();
    let tool_msg = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .unwrap();
    assert!(tool_msg.content.contains("offloaded tool output"));
}

#[tokio::test]
async fn accumulates_prompt_cache_tokens_in_session_usage() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "one".into(),
        tool_calls: vec![],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 3,
            prompt_cache_read_tokens: 7,
            prompt_cache_write_tokens: 2,
        }),
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("hi").await.unwrap();
    assert_eq!(s.token_usage.prompt_cache_hits, 7);
    assert_eq!(s.token_usage.prompt_cache_writes, 2);
}

#[tokio::test]
async fn accumulates_api_token_usage_for_cost() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "one".into(),
        tool_calls: vec![],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 3,
            ..Default::default()
        }),
        thinking: Some("hmm".into()),
    }]));
    // Need two responses if we call twice — first call only.
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("hi").await.unwrap();
    assert_eq!(s.token_usage.prompt_tokens, 10);
    assert_eq!(s.token_usage.completion_tokens, 3);
    assert_eq!(s.token_usage.model_steps, 1);
    assert_eq!(s.token_usage.model_calls_with_usage, 1);
    assert!(s.token_usage.thinking_tokens_est >= 1);
    let lines = s.token_usage_lines();
    assert!(lines.iter().any(|l| l.contains("prompt/input")));
    assert!(lines.iter().any(|l| l.contains("completion/output")));
    assert!(lines.iter().any(|l| l.contains("In-context estimate")));
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("$0") || l.contains("USD") || l.contains("price")),
        "should not report dollar cost: {lines:?}"
    );
    let report = s.token_usage_report();
    assert!(report.user_tokens_est >= 1);
    assert!(report.system_tokens_est >= 1);
}

#[tokio::test]
async fn mark_interrupted_if_stale_converts_running() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "hi".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);
    s.append_user_message("hi").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
    s.mark_interrupted_if_stale().await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Interrupted);
    // Idempotent on terminal interrupted.
    s.mark_interrupted_if_stale().await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Interrupted);
}

#[tokio::test]
async fn mark_cancelled_persists_terminal_state() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "hi".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.append_user_message("hi").await.unwrap();
    s.mark_cancelled().await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
    let s2 = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();
    assert_eq!(s2.active_task.lifecycle, TaskLifecycle::Cancelled);
}

#[tokio::test]
async fn resume_running_session_becomes_interrupted() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "partial".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    // Fresh session journal is Running with no completion event.
    let resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();
    assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Interrupted);
}

/// A persisted `Waiting` (HITL) status is a legitimately recoverable
/// state, not a stale crash — it must restore as `Waiting` with its
/// `WaitReason::Approval` correlation intact, so the operator's pending
/// approval can still be resolved after a restart.
#[tokio::test]
async fn resume_restores_a_valid_waiting_session_and_can_resolve_it() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "".into(),
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "echo hi"}),
        }],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);
    let request_id = s.pending_hitl().unwrap().call_id.clone();

    let mut resumed = AgentSession::resume(
        base_cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }])),
        ToolRegistry::new(),
        s.session_id,
    )
    .await
    .unwrap();

    assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Waiting);
    assert!(resumed.active_task.is_active_wait(&request_id));
    assert!(resumed.pending_hitl().is_some());

    // The restored wait can actually be resolved — correlation survived.
    resumed
        .resolve_hitl(HitlDecision::Approve, "test")
        .await
        .unwrap();
    assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Working);
}

/// A session with no scripted model turns; enough to exercise state helpers
/// that never reach the provider.
async fn idle_session(dir: &std::path::Path) -> AgentSession {
    AgentSession::create(
        base_cfg(dir),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
    )
    .await
    .unwrap()
}

fn assistant_with_tool_call(name: &str) -> Message {
    let mut m = Message::new(MessageRole::Assistant, "calling");
    m.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: json!({}),
    }];
    m
}

#[test]
fn strip_protocol_markers_removes_confidence_annotations() {
    assert_eq!(strip_protocol_markers("done \\confidence{0.9}"), "done");
    assert_eq!(
        strip_protocol_markers("a \\confidence{0.1}b \\confidence{0.2}c"),
        "a b c"
    );
    assert_eq!(strip_protocol_markers("\\confidence{0.5}only"), "only");
    assert_eq!(strip_protocol_markers("  plain  "), "plain");
}

/// Regression: an unterminated marker used to duplicate the text before it,
/// because `rest` was not rewound before breaking out of the scan.
#[test]
fn unterminated_confidence_marker_is_kept_verbatim_once() {
    assert_eq!(
        strip_protocol_markers("keep \\confidence{oops"),
        "keep \\confidence{oops"
    );
    assert_eq!(
        strip_protocol_markers("a \\confidence{0.1}b \\confidence{trunc"),
        "a b \\confidence{trunc"
    );
}

#[tokio::test]
async fn current_turn_has_tool_activity_stops_at_the_user_boundary() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;

    // No messages at all: nothing to scan.
    s.messages.clear();
    assert!(!s.current_turn_has_tool_activity());

    // Only plain assistant text since the last user message.
    s.messages = vec![
        Message::new(MessageRole::User, "hi"),
        Message::new(MessageRole::Assistant, "hello"),
    ]
    .into();
    assert!(!s.current_turn_has_tool_activity());

    // An assistant turn carrying tool calls counts as activity.
    s.messages = vec![
        Message::new(MessageRole::User, "hi"),
        assistant_with_tool_call("read_file"),
    ]
    .into();
    assert!(s.current_turn_has_tool_activity());

    // Tool activity from a *previous* turn must not leak into this one:
    // the scan walks backwards and stops at the newer user message.
    s.messages = vec![
        Message::new(MessageRole::User, "first"),
        assistant_with_tool_call("read_file"),
        Message::new(MessageRole::Tool, "contents"),
        Message::new(MessageRole::User, "second"),
        Message::new(MessageRole::Assistant, "plain reply"),
    ]
    .into();
    assert!(!s.current_turn_has_tool_activity());
}

#[tokio::test]
async fn finalize_turn_failure_keeps_the_first_summary() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("do something").await.unwrap();

    s.finalize_turn_failure("first failure", "cat_a")
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    let after_first = s.messages.len();

    // Idempotent: a second failure must not append another marker message
    // or overwrite the original summary.
    s.finalize_turn_failure("second failure", "cat_b")
        .await
        .unwrap();
    assert_eq!(s.messages.len(), after_first);
    let markers: Vec<&str> = s
        .messages
        .iter()
        .filter(|m| m.content.starts_with(TURN_FAILED_MARKER))
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(markers.len(), 1);
    assert!(markers[0].contains("first failure"));
    assert!(!markers[0].contains("second failure"));
}

#[tokio::test]
async fn fail_max_turns_records_a_step_limit_failure() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("do something").await.unwrap();

    s.fail_max_turns().await.unwrap();

    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    let marker = s
        .messages
        .iter()
        .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
        .expect("a turn_failed marker should be recorded");
    assert!(marker.content.contains("step limit"));
    assert!(s
        .events
        .iter()
        .any(|e| e.kind == "turn_failed" && e.detail.starts_with("max_turns:")));
}

#[tokio::test]
async fn prepare_model_step_leaves_a_small_context_alone() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.messages = vec![Message::new(MessageRole::User, "short")].into();

    s.prepare_model_step(1).await.unwrap();

    assert!(!s.events.iter().any(|e| e.kind == "context_compacted"));
    assert_eq!(s.compaction_telemetry().compaction_count, 0);
    assert_eq!(s.messages.len(), 1);
}

#[tokio::test]
async fn journal_cursor_is_exposed() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;

    let before = s.journal_cursor().await.unwrap();
    s.append_user_message("advance the journal").await.unwrap();
    let after = s.journal_cursor().await.unwrap();
    assert!(
        after > before,
        "cursor should advance after a journalled append ({before} -> {after})"
    );
}

#[tokio::test]
async fn token_usage_report_buckets_tool_messages_separately() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;

    let mut thinking_turn = Message::new(MessageRole::Assistant, "answer");
    thinking_turn.thinking = Some("pondering at some length".into());

    s.messages = vec![
        Message::new(MessageRole::System, "system preamble"),
        Message::new(MessageRole::User, "a question"),
        thinking_turn,
        Message::new(MessageRole::Tool, "tool output one"),
        Message::new(MessageRole::Tool, "tool output two"),
    ]
    .into();

    let report = s.token_usage_report();

    assert_eq!(report.tool_message_count, 2);
    assert!(report.tool_tokens_est > 0);
    assert!(report.system_tokens_est > 0);
    assert!(report.user_tokens_est > 0);
    assert!(report.assistant_tokens_est > 0);
    assert!(
        report.thinking_in_context_est > 0,
        "assistant thinking should be counted in the context estimate"
    );
}

#[tokio::test]
async fn exec_only_tool_failure_is_journalled_without_a_tool_message() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    let before = s.messages.len();
    let mut budget = ValidationBudget::default();

    let call = ToolCall {
        id: "c1".into(),
        name: "no_such_tool".into(),
        arguments: json!({}),
    };
    // A failing call must not surface as tool output in the conversation,
    // but it still has to be journalled for replay.
    s.run_one_tool_exec_only(&call, &mut budget).await.unwrap();

    assert_eq!(s.messages.len(), before);
    assert!(s.journal_cursor().await.unwrap() > 0);
}

#[tokio::test]
async fn update_plan_emits_plan_update_event_and_ack_message() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    let mut budget = ValidationBudget::default();
    let call = ToolCall {
        id: "plan-1".into(),
        name: "update_plan".into(),
        arguments: json!({
            "explanation": "kickoff",
            "plan": [
                {"step": "scout", "status": "in_progress"},
                {"step": "ship", "status": "pending"}
            ]
        }),
    };
    s.run_one_tool_exec_only(&call, &mut budget).await.unwrap();

    assert!(
        s.events
            .iter()
            .any(|e| e.kind == "plan_update" && e.detail.contains("scout")),
        "expected plan_update event, got {:?}",
        s.events
    );
    let tool_msg = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .expect("tool message");
    assert_eq!(tool_msg.content, "Plan updated");
    assert_eq!(tool_msg.name.as_deref(), Some("update_plan"));
}

// --- Verified Task Completion: integration tests --------------------

/// Governance/HITL gating is orthogonal to completion verification —
/// these tests disable it so a tool call executes directly and the
/// evaluator's evidence-based decision is what's under test.
fn no_gov_cfg(dir: &std::path::Path) -> LoopConfig {
    LoopConfig {
        enable_governance: false,
        ..base_cfg(dir)
    }
}

fn script(responses: Vec<ModelResponse>) -> Arc<MockModelClient> {
    Arc::new(MockModelClient::script(responses))
}

fn text_only(text: &str) -> ModelResponse {
    ModelResponse {
        text: text.into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }
}

fn tool_call_response(calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        text: "".into(),
        tool_calls: calls,
        usage: None,
        thinking: None,
    }
}

async fn git(dir: &std::path::Path, args: &[&str]) {
    let status = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .await
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

async fn init_repo(dir: &std::path::Path) {
    git(dir, &["init", "-q"]).await;
    git(dir, &["config", "user.email", "forge@example.com"]).await;
    git(dir, &["config", "user.name", "Forge Test"]).await;
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]).await;
    git(dir, &["commit", "-q", "-m", "init"]).await;
}

// Model claims a write succeeded ("Done") but the tool never actually
// performed one — a turn with a file-edit expectation but no matching
// verified evidence must fail, never trust the narration.
#[tokio::test]
async fn model_claims_success_without_a_verified_edit_fails() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "apply_patch".into(),
            arguments: json!({
                "patch": "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch"
            }),
        }]),
        text_only("Done — file removed."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("delete missing.txt").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert!(s
        .messages
        .iter()
        .any(|m| m.content.starts_with(TURN_FAILED_MARKER)));
}

#[tokio::test]
async fn write_file_success_completes() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "new.txt", "content": "hello\n"}),
        }]),
        text_only("Created new.txt."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("create new.txt").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::EditVerified
    );
}

#[tokio::test]
async fn bash_nonzero_exit_fails_with_exit_code_in_message() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "exit 7"}),
        }]),
        text_only("Ran the command."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    let failure = s
        .messages
        .iter()
        .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
        .unwrap();
    assert!(
        failure.content.contains("exited with code 7"),
        "{}",
        failure.content
    );
    let tool_message = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("the bash tool result should be recorded");
    assert_eq!(
        tool_message.outcome,
        ExecutionOutcome::Failed { exit_code: Some(7) }
    );
}

#[tokio::test]
async fn bash_exit_zero_completes_and_reports_success() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "exit 0"}),
        }]),
        text_only("Ran the command."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    let tool_message = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("the bash tool result should be recorded");
    assert_eq!(tool_message.outcome, ExecutionOutcome::Success);
}

#[tokio::test]
async fn bash_exit_127_reports_spawn_failed_command_not_found() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "definitely_not_a_real_command_xyz; exit 127"}),
        }]),
        text_only("Ran the command."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("run it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    let tool_message = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("the bash tool result should be recorded");
    assert_eq!(
        tool_message.outcome,
        ExecutionOutcome::SpawnFailed {
            reason: "command not found".into()
        }
    );
}

#[tokio::test]
async fn hitl_denial_message_carries_denied_outcome() {
    let dir = tempdir().unwrap();
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "".into(),
        tool_calls: vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git push origin main"}),
        }],
        usage: None,
        thinking: None,
    }]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("push").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

    s.resolve_hitl_with_feedback(HitlDecision::Deny, "test", None)
        .await
        .unwrap();

    let tool_message = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool result should record the denial");
    assert!(matches!(
        tool_message.outcome,
        ExecutionOutcome::Denied { .. }
    ));
}

#[tokio::test]
async fn acl_denial_message_carries_denied_outcome() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "SENTINEL-CONTENT").unwrap();
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "secret.txt"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    let mut acl = AclPolicy::allow_all();
    acl.deny("read_file".into());
    s.set_governance(Governance::default().with_acl(acl));
    s.run_user_message("read it").await.unwrap();

    let tool_message = s
        .messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool result should record the denial");
    assert!(matches!(
        tool_message.outcome,
        ExecutionOutcome::Denied { .. }
    ));
}

#[tokio::test]
async fn write_then_failing_validation_completes_with_the_check_reported_as_incomplete() {
    // `classify_turn` picks exactly one `TaskExpectation` category per
    // turn by precedence (git > file-edit > tool-execution > search >
    // read-only). A turn that both writes a file (succeeds) and runs a
    // failing validation command classifies as `FileEdit` only.
    //
    // The failing bash still has to surface — but it is *not* part of the
    // expectation, so it must not turn a verified write into a failed turn.
    // Reporting "Failed" here is the false-failure bug: the file really is on
    // disk, and telling the user nothing was applied sends them to `git diff`
    // to find out Forge lied. Complete the turn, and carry the unfinished
    // check separately.
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "ok.txt", "content": "fine\n"}),
            },
            ToolCall {
                id: "2".into(),
                name: "bash".into(),
                arguments: json!({"command": "exit 1"}),
            },
        ]),
        text_only("Wrote the file and ran the tests."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("write and validate").await.unwrap();

    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    let completion = s.last_completion.as_ref().unwrap();
    assert_eq!(
        completion.reason,
        CompletionReason::CompletedWithIncompleteChecks
    );
    // The unfinished check is named by its command, not swallowed — the
    // command label is what the user recognizes ("exit 1", "pytest ..."),
    // not the tool that ran it.
    assert_eq!(completion.evidence_summary.incomplete, vec!["exit 1"]);
    assert!(
        completion.evidence_summary.detail.contains("didn't finish"),
        "detail should say the check didn't finish, got: {}",
        completion.evidence_summary.detail
    );
    // ...and the turn never claims the edit failed.
    assert!(
        !completion
            .evidence_summary
            .detail
            .contains("No file modifications"),
        "a verified write must never be reported as no modifications"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
        "fine\n"
    );
}

#[tokio::test]
async fn edit_tool_produces_file_evidence_so_a_real_edit_is_not_reported_as_failed() {
    // Regression: `pre_edit_snapshot` captured pre-state for `edit`, but
    // `push_file_edit_evidence` only matched `write_file`/`apply_patch`, so an
    // `edit` call pushed no evidence at all. Its own `FileEdit` expectation was
    // then permanently unverifiable and every successful `edit` reported
    // "No file modifications were successfully applied."
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("edit_me.txt"), "before\n").unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "edit".into(),
            arguments: json!({
                "path": "edit_me.txt",
                "old_string": "before",
                "new_string": "after"
            }),
        }]),
        text_only("Edited the file."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("edit it").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("edit_me.txt")).unwrap(),
        "after\n",
        "precondition: the edit must actually land on disk"
    );
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::EditVerified
    );
}

#[tokio::test]
async fn a_failed_git_inspection_does_not_fail_a_turn_that_edited_a_file() {
    // Caught while re-recording the README demo: the agent fixed a function,
    // then ran `git log` to review its own work. The inspection failed, and
    // because `classify_turn` ranked any git call above the file edit the turn
    // was judged solely on it — the demo's closing frame showed the fix working
    // in the terminal above the words "1 of 2 required git operations
    // succeeded; log failed", with the status Failed.
    //
    // Reviewing your work is not the work.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("fix_me.txt"), "before\n").unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "edit".into(),
                arguments: json!({
                    "path": "fix_me.txt",
                    "old_string": "before",
                    "new_string": "after"
                }),
            },
            ToolCall {
                id: "2".into(),
                name: "git".into(),
                arguments: json!({"subcommand": "log", "args": ["--oneline", "-1"]}),
            },
        ]),
        text_only("Fixed it and looked at the log."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("fix it").await.unwrap();

    // `git log` fails here: the tempdir is not a repository.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fix_me.txt")).unwrap(),
        "after\n",
        "precondition: the edit must land on disk"
    );
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    let completion = s.last_completion.as_ref().unwrap();
    assert!(
        !completion
            .evidence_summary
            .detail
            .contains("git operations"),
        "a failed inspection must not be reported as the turn's outcome: {}",
        completion.evidence_summary.detail
    );
}

#[tokio::test]
async fn a_failed_git_mutation_still_fails_the_turn() {
    // The guardrail for the test above: `commit` changes the repository, so it
    // is the work, and its failure is the turn's — even alongside an edit.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("fix_me.txt"), "before\n").unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "edit".into(),
                arguments: json!({
                    "path": "fix_me.txt",
                    "old_string": "before",
                    "new_string": "after"
                }),
            },
            ToolCall {
                id: "2".into(),
                name: "git".into(),
                arguments: json!({"subcommand": "commit", "args": ["-m", "fix"]}),
            },
        ]),
        text_only("Fixed and committed."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("fix and commit").await.unwrap();

    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
}

#[tokio::test]
async fn a_missing_verification_command_is_reported_without_failing_the_edit() {
    // The exact shape from the usability report: the model edits a file, then
    // runs a checker that isn't installed. `bash` itself dispatches fine, so
    // the tool is not `is_error` — only the non-zero exit marks the command as
    // having not finished. The edit must still read as done, and the missing
    // check must still be named.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("mod.txt"), "before\n").unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "edit".into(),
                arguments: json!({
                    "path": "mod.txt",
                    "old_string": "before",
                    "new_string": "after"
                }),
            },
            ToolCall {
                id: "2".into(),
                name: "bash".into(),
                arguments: json!({"command": "definitely-not-installed --check"}),
            },
        ]),
        text_only("Edited the file and tried to verify it."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("edit and verify").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("mod.txt")).unwrap(),
        "after\n"
    );
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    let completion = s.last_completion.as_ref().unwrap();
    assert_eq!(
        completion.reason,
        CompletionReason::CompletedWithIncompleteChecks
    );
    assert_eq!(completion.evidence_summary.incomplete.len(), 1);
}

#[tokio::test]
async fn search_zero_matches_completes() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "grep".into(),
            arguments: json!({"pattern": "definitely_not_present_anywhere"}),
        }]),
        text_only("No matches found."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("search for it").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().evidence_summary.detail,
        "Search completed with 0 matches."
    );
}

#[tokio::test]
async fn two_edits_one_fails_is_partial_failure_not_completed() {
    let dir = tempdir().unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "ok.txt", "content": "fine\n"}),
            },
            ToolCall {
                id: "2".into(),
                name: "apply_patch".into(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch"
                }),
            },
        ]),
        text_only("Updated both files."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("update both").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::PartialFailure
    );
    // The successful half of the turn still happened on disk — the
    // evaluator fails the turn without pretending the edit didn't occur.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
        "fine\n"
    );
}

#[tokio::test]
async fn read_only_turn_completes_without_tool_calls() {
    let dir = tempdir().unwrap();
    let model = script(vec![text_only("Forge is a Rust workspace.")]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("what is this repo?").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::NoChangesRequired
    );
}

// Regression test for the false-completion bug found in the 2026-08-01
// usability audit: a small/local model that doesn't reliably use the
// structured tool-calling wire format instead dumps a JSON-ish blob
// naming a real tool as plain assistant text. `last.tool_calls` is empty
// (the model never actually invoked anything), so before this fix it fell
// through to `TaskExpectation::ReadOnly`, which completes on any
// non-empty text — reporting success while `greeter.py` was never
// touched. It must now fail instead.
#[tokio::test]
async fn dangling_tool_call_text_does_not_complete() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("greeter.py"),
        "def greet(name):\n    return f\"Hello, {name}!\"\n",
    )
    .unwrap();
    let model = script(vec![text_only(
        "```json\n{\"write_file\", {\"path\": \"greeter.py\", \"content\": \"class Greeter:\\n\\tdef greet(self, name):\\n\\t\\treturn f'Hi there, {name}!'\"}}\n```",
    )]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("change the greeting in greeter.py")
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    // This path bypasses the evidence-based evaluator entirely (same as
    // the sibling `no_final_answer` branch just above it in
    // `apply_model_response`), so `last_completion` stays `None` — the
    // real signal is the terminal lifecycle plus the journalled event
    // category, checked below.
    assert!(s.events.iter().any(|e| e.kind == "turn_failed"
        && e.detail
            .starts_with(CompletionReason::DanglingToolCallText.as_category())));
    // The file must be provably untouched — no silent partial write.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("greeter.py")).unwrap(),
        "def greet(name):\n    return f\"Hello, {name}!\"\n"
    );
    let failure = s
        .messages
        .iter()
        .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
        .unwrap();
    assert!(
        failure.content.contains("didn't format the call correctly"),
        "{}",
        failure.content
    );
}

// A legitimate answer that merely *mentions* a tool by name in prose
// (no call-shaped quote+punctuation adjacency) must still complete
// normally — the detection heuristic must not be trigger-happy.
#[tokio::test]
async fn prose_mentioning_a_tool_name_still_completes() {
    let dir = tempdir().unwrap();
    let model = script(vec![text_only(
        "You can ask me to use \"write_file\" to create that for you.",
    )]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("how do I create a file?").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::NoChangesRequired
    );
}

#[tokio::test]
async fn git_add_with_no_changes_fails_effect_not_observed() {
    let dir = tempdir().unwrap();
    init_repo(dir.path()).await;
    let model = script(vec![
        tool_call_response(vec![ToolCall {
            id: "1".into(),
            name: "git".into(),
            arguments: json!({"subcommand": "add", "args": ["a.txt"]}),
        }]),
        text_only("Staged a.txt."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("stage a.txt").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::GitEffectNotObserved
    );
}

#[tokio::test]
async fn git_add_then_commit_completes_with_verified_effect() {
    let dir = tempdir().unwrap();
    init_repo(dir.path()).await;
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    let model = script(vec![
        tool_call_response(vec![
            ToolCall {
                id: "1".into(),
                name: "git".into(),
                arguments: json!({"subcommand": "add", "args": ["a.txt"]}),
            },
            ToolCall {
                id: "2".into(),
                name: "git".into(),
                arguments: json!({"subcommand": "commit", "args": ["-m", "update a.txt"]}),
            },
        ]),
        text_only("Committed the change."),
    ]);
    let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
        .await
        .unwrap();
    s.run_user_message("commit the change").await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    assert_eq!(
        s.last_completion.as_ref().unwrap().reason,
        CompletionReason::GitEffectVerified
    );
}

// A failed turn later receiving narration claiming success must never
// flip to Completed — terminal states are not overwritten by later
// model text, even via a direct (non-`run_agent_turns`) re-entry.
#[tokio::test]
async fn failed_turn_is_not_overwritten_by_later_success_narration() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("run the tests").await.unwrap();
    s.finalize_turn_failure("cargo test exited with code 101.", "tool_exited_nonzero")
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);

    let outcome = s
        .apply_model_response(text_only("Actually, all tests passed now!"))
        .await
        .unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
    assert!(matches!(outcome, ApplyOutcome::Done(_)));
}

#[tokio::test]
async fn cancellation_yields_interrupted_and_never_completes() {
    let dir = tempdir().unwrap();
    let mut s = idle_session(dir.path()).await;
    s.append_user_message("do something").await.unwrap();
    s.mark_cancelled().await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);

    // A later model step must not resurrect the turn into Completed.
    let outcome = s.apply_model_response(text_only("Done!")).await.unwrap();
    assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
    assert!(matches!(outcome, ApplyOutcome::Done(_)));
}
