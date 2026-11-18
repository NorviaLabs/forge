//! Computer use through real sessions: consent requests raised by a tool
//! become approvals, approve-all never answers them, unavailable tools are
//! hidden, child agents never inherit them, and old screenshots are not
//! resent.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use forge_model::MockModelClient;
use forge_tools::{
    AgentMode, ConsentKind, ConsentRequest, Tool, ToolContext, ToolError, ToolRegistry,
};
use forge_types::{
    ApprovalKind, GrantDecision, ImageRef, Message, MessageRole, ModelResponse, SideEffectClass,
    TaskLifecycle, ToolCall, ToolOutput,
};
use serde_json::{json, Value};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::computer_use::{
    computer_use_tool_permitted, is_computer_use_tool, prune_screenshots, SCREENSHOTS_KEPT,
    SCREENSHOT_OMITTED_NOTE,
};
use crate::*;

/// Acts only with consent for `(kind, scope)`; otherwise asks for it.
struct ConsentTool {
    name: &'static str,
    kind: ConsentKind,
    scope: &'static str,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ConsentTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "acts with consent"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Network
    }
    async fn call(&self, ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        if ctx
            .consents
            .check(self.kind, self.scope, ctx.tool_call_id.as_deref())
        {
            self.runs.fetch_add(1, Ordering::SeqCst);
            return Ok(ToolOutput::success("acted"));
        }
        Err(ToolError::ConsentRequired(Box::new(
            ConsentRequest::new(self.kind, self.scope, "Submit \u{201c}Sign up\u{201d}")
                .with_fact("Sends", "email = ada@example.com")
                .with_reason("sign up"),
        )))
    }
}

/// A tool whose availability a test switches.
struct SwitchableTool {
    name: &'static str,
    available: Arc<AtomicBool>,
    image: bool,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SwitchableTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "switchable"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn is_available(&self, _ctx: &ToolContext) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    fn requires_image_input(&self) -> bool {
        self.image
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::success("ran"))
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

fn call(id: &str, name: &str) -> ModelResponse {
    ModelResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}),
        }],
        usage: None,
        thinking: None,
    }
}

/// Apply one model step that calls `name`, running it to its outcome.
async fn apply_call(session: &mut AgentSession, id: &str, name: &str) -> ModelResponseApplication {
    let application = session
        .begin_model_response_application(call(id, name))
        .await
        .unwrap();
    match application {
        ModelResponseApplication::Execute(pending) => {
            let completed = pending.execute().await;
            session.finish_tool_application(completed).await.unwrap()
        }
        other => other,
    }
}

async fn consent_session(
    dir: &std::path::Path,
    kind: ConsentKind,
    scope: &'static str,
) -> (AgentSession, Arc<AtomicUsize>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ConsentTool {
        name: "browser_submit",
        kind,
        scope,
        runs: runs.clone(),
    }));
    let mut session =
        AgentSession::create(cfg(dir), Arc::new(MockModelClient::script(vec![])), tools)
            .await
            .unwrap();
    session.append_user_message("sign me up").await.unwrap();
    (session, runs)
}

fn result_for<'a>(session: &'a AgentSession, id: &str) -> Option<&'a Message> {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.tool_call_id.as_deref() == Some(id))
}

#[tokio::test]
async fn a_consent_request_parks_the_call_as_an_approval() {
    let dir = tempdir().unwrap();
    let (mut session, runs) =
        consent_session(dir.path(), ConsentKind::FormSubmit, "example.com").await;

    let outcome = apply_call(&mut session, "c1", "browser_submit").await;

    assert!(matches!(
        outcome,
        ModelResponseApplication::Finished(ApplyOutcome::Hitl(_))
    ));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "nothing acts before consent"
    );
    assert_eq!(session.active_task.lifecycle, TaskLifecycle::Waiting);
    let payload = session
        .pending_hitl()
        .expect("an approval is pending")
        .clone();
    assert_eq!(payload.kind(), ApprovalKind::FormSubmit);
    let consent = payload
        .consent
        .expect("the request travels with the payload");
    assert_eq!(consent.scope, "example.com");
    assert_eq!(consent.facts[0].label, "Sends");
    assert_eq!(payload.call_id, "c1");
}

#[tokio::test]
async fn a_session_grant_reruns_the_call_and_covers_the_next_one() {
    let dir = tempdir().unwrap();
    let (mut session, runs) =
        consent_session(dir.path(), ConsentKind::FormSubmit, "example.com").await;
    apply_call(&mut session, "c1", "browser_submit").await;

    session
        .decide_hitl(GrantDecision::AllowConsentForSession, "test")
        .await
        .unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1, "the approved call re-ran");
    assert!(session.pending_hitl().is_none());
    assert_eq!(result_for(&session, "c1").unwrap().content, "acted");
    assert!(session
        .tool_ctx
        .consents
        .is_granted(ConsentKind::FormSubmit, "example.com"));

    apply_call(&mut session, "c2", "browser_submit").await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the grant covers later calls"
    );
    assert!(session.pending_hitl().is_none());
}

#[tokio::test]
async fn a_single_submit_is_allowed_once_only() {
    let dir = tempdir().unwrap();
    let (mut session, runs) =
        consent_session(dir.path(), ConsentKind::FormSubmit, "example.com").await;
    apply_call(&mut session, "c1", "browser_submit").await;

    session
        .decide_hitl(GrantDecision::AllowOnce, "test")
        .await
        .unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    apply_call(&mut session, "c2", "browser_submit").await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the next submit asks again");
    assert_eq!(session.pending_hitl().unwrap().call_id, "c2");
}

#[tokio::test]
async fn a_refusal_with_a_note_reaches_the_agent_and_nothing_acts() {
    let dir = tempdir().unwrap();
    let (mut session, runs) =
        consent_session(dir.path(), ConsentKind::FormSubmit, "example.com").await;
    apply_call(&mut session, "c1", "browser_submit").await;

    session
        .decide_hitl(
            GrantDecision::DenyWithNote("use the staging site".into()),
            "test",
        )
        .await
        .unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let result = result_for(&session, "c1").expect("the call is answered");
    assert!(
        result.content.contains("use the staging site"),
        "{}",
        result.content
    );
    assert!(!session
        .tool_ctx
        .consents
        .is_granted(ConsentKind::FormSubmit, "example.com"));
}

/// Approve-all answers governance prompts; it never hands over the screen
/// or submits a form.
#[tokio::test]
async fn approve_all_never_answers_a_consent_request() {
    let dir = tempdir().unwrap();
    let (mut session, runs) =
        consent_session(dir.path(), ConsentKind::ScreenControl, "screen").await;
    session.set_workspace_trusted(true);
    assert!(session.set_approve_all(true));

    apply_call(&mut session, "c1", "browser_submit").await;

    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let payload = session
        .pending_hitl()
        .expect("still asks under approve-all");
    assert_eq!(payload.kind(), ApprovalKind::ScreenControl);
}

#[tokio::test]
async fn a_consent_approval_offers_only_its_own_grants() {
    let dir = tempdir().unwrap();
    let (mut screen, _) = consent_session(dir.path(), ConsentKind::ScreenControl, "screen").await;
    apply_call(&mut screen, "c1", "browser_submit").await;
    let payload = screen.pending_hitl().unwrap().clone();
    for rejected in [
        GrantDecision::AllowOnce,
        GrantDecision::ApproveAll,
        GrantDecision::AllowPatternForSession,
        GrantDecision::AllowPatternAlways,
        GrantDecision::AllowHostForSession,
        GrantDecision::AllowHostAlways,
        GrantDecision::TrustProgram,
        GrantDecision::AllowPathForSession,
    ] {
        assert!(
            validate_grant_decision(&payload, &rejected).is_err(),
            "{rejected:?} must not answer a screen-control request"
        );
    }
    assert!(validate_grant_decision(&payload, &GrantDecision::AllowConsentForSession).is_ok());
    assert!(validate_grant_decision(&payload, &GrantDecision::Deny).is_ok());

    let dir = tempdir().unwrap();
    let (mut submit, _) = consent_session(dir.path(), ConsentKind::FormSubmit, "example.com").await;
    apply_call(&mut submit, "c1", "browser_submit").await;
    let payload = submit.pending_hitl().unwrap().clone();
    assert!(validate_grant_decision(&payload, &GrantDecision::AllowOnce).is_ok());
    assert!(validate_grant_decision(&payload, &GrantDecision::ApproveAll).is_err());

    // And the consent grant answers nothing else.
    let mut governance = payload.clone();
    governance.consent = None;
    assert!(validate_grant_decision(&governance, &GrantDecision::AllowConsentForSession).is_err());
}

async fn switchable_session(
    dir: &std::path::Path,
    image: bool,
) -> (AgentSession, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let available = Arc::new(AtomicBool::new(false));
    let runs = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(SwitchableTool {
        name: "desktop_screenshot",
        available: available.clone(),
        image,
        runs: runs.clone(),
    }));
    let mut session =
        AgentSession::create(cfg(dir), Arc::new(MockModelClient::script(vec![])), tools)
            .await
            .unwrap();
    session.append_user_message("look").await.unwrap();
    (session, available, runs)
}

fn offered(session: &AgentSession) -> Vec<String> {
    session
        .tools_for_model()
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

/// A tool switched off is not offered, is refused when called, and is
/// offered again once switched back on, all without a new session.
#[tokio::test]
async fn an_unavailable_tool_is_hidden_and_refused() {
    let dir = tempdir().unwrap();
    let (mut session, available, runs) = switchable_session(dir.path(), false).await;

    assert!(!offered(&session).contains(&"desktop_screenshot".to_string()));
    apply_call(&mut session, "c1", "desktop_screenshot").await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let result = result_for(&session, "c1").expect("the call is answered");
    assert!(
        result.content.contains("not available"),
        "{}",
        result.content
    );

    available.store(true, Ordering::SeqCst);
    assert!(offered(&session).contains(&"desktop_screenshot".to_string()));
    apply_call(&mut session, "c2", "desktop_screenshot").await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

/// Providers reject a transcript that calls a tool the request no longer
/// declares: a tool already called stays declared after it is switched off.
#[tokio::test]
async fn a_tool_already_called_stays_declared_when_switched_off() {
    let dir = tempdir().unwrap();
    let (mut session, available, runs) = switchable_session(dir.path(), false).await;
    available.store(true, Ordering::SeqCst);
    apply_call(&mut session, "c1", "desktop_screenshot").await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    available.store(false, Ordering::SeqCst);
    assert!(offered(&session).contains(&"desktop_screenshot".to_string()));
    apply_call(&mut session, "c2", "desktop_screenshot").await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "still refused when called");
}

#[tokio::test]
async fn a_tool_that_needs_images_is_hidden_from_a_text_only_model() {
    let dir = tempdir().unwrap();
    let (mut session, available, runs) = switchable_session(dir.path(), true).await;
    available.store(true, Ordering::SeqCst);
    session.tool_ctx.image_input = false;
    assert!(!offered(&session).contains(&"desktop_screenshot".to_string()));
    apply_call(&mut session, "c1", "desktop_screenshot").await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let result = result_for(&session, "c1").unwrap();
    assert!(result.content.contains("image"), "{}", result.content);

    session.tool_ctx.image_input = true;
    assert!(offered(&session).contains(&"desktop_screenshot".to_string()));
}

#[test]
fn computer_use_tools_are_known_by_name() {
    for tool in [
        "browser_navigate",
        "browser_submit",
        "desktop_click",
        "desktop_screenshot",
    ] {
        assert!(is_computer_use_tool(tool), "{tool}");
    }
    for tool in [
        "bash",
        "web_fetch",
        "browser",
        "desktop",
        "mcp:browser_click",
    ] {
        assert!(!is_computer_use_tool(tool), "{tool}");
    }
}

#[test]
fn only_a_child_named_for_the_tool_may_use_it() {
    assert!(computer_use_tool_permitted(false, None, "desktop_click"));
    assert!(
        !computer_use_tool_permitted(true, None, "desktop_click"),
        "a built-in child inherits no computer use"
    );
    let limit: BTreeSet<String> = ["browser_click".to_string()].into();
    assert!(computer_use_tool_permitted(
        true,
        Some(&limit),
        "browser_click"
    ));
    assert!(!computer_use_tool_permitted(
        true,
        Some(&limit),
        "desktop_click"
    ));
    assert!(
        computer_use_tool_permitted(true, None, "bash"),
        "other tools are not this rule's business"
    );
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

/// A built-in child is offered no computer-use tool and gets a consent
/// store of its own: the parent's grant does not reach it.
#[tokio::test]
async fn a_built_in_child_inherits_neither_the_tools_nor_the_consent() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let (session, available, _) = switchable_session(dir.path(), false).await;
    available.store(true, Ordering::SeqCst);
    session
        .tool_ctx
        .consents
        .grant(ConsentKind::ScreenControl, "screen");

    let child = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &SubagentSpec {
                role: "worker".into(),
                prompt: "go".into(),
                mode: AgentMode::ReadOnly,
                tool_allowlist: None,
                profile: Default::default(),
            },
        )
        .await
        .unwrap();

    assert!(!offered(&child).contains(&"desktop_screenshot".to_string()));
    assert!(!child
        .tool_ctx
        .consents
        .is_granted(ConsentKind::ScreenControl, "screen"));

    let named = session
        .create_child(
            forge_durable::new_session_id(),
            dir.path().to_path_buf(),
            CancellationToken::new(),
            &SubagentSpec {
                role: "viewer".into(),
                prompt: "go".into(),
                mode: AgentMode::ReadOnly,
                tool_allowlist: Some(vec!["desktop_screenshot".into()]),
                profile: Default::default(),
            },
        )
        .await
        .unwrap();
    assert!(offered(&named).contains(&"desktop_screenshot".to_string()));
}

fn screenshot_result(id: &str, tool: &str, file: &str) -> Message {
    let mut output = ToolOutput::success(format!("{tool} done"));
    output.attachments = vec![ImageRef::new(file, "image/png", 10)];
    Message::from_tool_output(
        &ToolCall {
            id: id.into(),
            name: tool.into(),
            arguments: json!({}),
        },
        &output,
    )
}

#[test]
fn only_the_latest_screenshots_are_kept() {
    let mut messages = vec![Message::new(MessageRole::User, "look")];
    for index in 0..8 {
        let tool = if index % 2 == 0 {
            "desktop_screenshot"
        } else {
            "browser_screenshot"
        };
        messages.push(screenshot_result(
            &format!("c{index}"),
            tool,
            &format!("s{index}.png"),
        ));
    }
    // Someone else's image is never dropped.
    messages.push(screenshot_result("v", "view_image", "diagram.png"));

    // Eight screenshots: the oldest three go, in one batch.
    let pruned = prune_screenshots(&messages, SCREENSHOTS_KEPT).expect("three are dropped");
    let kept: Vec<&str> = pruned
        .iter()
        .flat_map(|message| message.attachments.iter().map(|image| image.path.as_str()))
        .collect();
    assert_eq!(
        kept,
        [
            "s3.png",
            "s4.png",
            "s5.png",
            "s6.png",
            "s7.png",
            "diagram.png"
        ]
    );
    for old in &pruned[1..4] {
        assert!(
            old.content.contains(SCREENSHOT_OMITTED_NOTE),
            "{}",
            old.content
        );
    }
    assert!(!pruned[4].content.contains(SCREENSHOT_OMITTED_NOTE));
    assert_eq!(pruned.len(), messages.len());

    assert!(
        prune_screenshots(&messages[..6], SCREENSHOTS_KEPT).is_none(),
        "five screenshots: nothing to drop, the transcript stays shared"
    );
}

/// Dropping one old screenshot per new one rewrote an earlier message on
/// every step, so the provider's prompt cache missed from there each time.
/// Screenshots go in batches: the newest stay, between 3 and 5 of them, a
/// dropped one stays dropped, and what is dropped changes only every third
/// screenshot.
#[test]
fn screenshots_are_dropped_in_batches() {
    let mut messages = vec![Message::new(MessageRole::User, "look")];
    let mut previous: Vec<String> = Vec::new();
    for n in 1..=30usize {
        messages.push(screenshot_result(
            &format!("c{n}"),
            "desktop_screenshot",
            &format!("s{n}.png"),
        ));
        let dropped: Vec<String> = match prune_screenshots(&messages, SCREENSHOTS_KEPT) {
            None => Vec::new(),
            Some(pruned) => pruned
                .iter()
                .filter(|message| message.content.contains(SCREENSHOT_OMITTED_NOTE))
                .filter_map(|message| message.tool_call_id.clone())
                .collect(),
        };
        let kept = n - dropped.len();
        assert!(
            (n.min(SCREENSHOTS_KEPT)..=SCREENSHOTS_KEPT + 2).contains(&kept),
            "{n} screenshots: {kept} kept"
        );
        let oldest: Vec<String> = (1..=dropped.len()).map(|i| format!("c{i}")).collect();
        assert_eq!(dropped, oldest, "{n}: the oldest go");
        assert!(
            previous.iter().all(|call| dropped.contains(call)),
            "{n}: a dropped screenshot stays dropped"
        );
        if dropped != previous {
            assert_eq!(
                n % SCREENSHOTS_KEPT,
                0,
                "{n}: the cut moved off a batch boundary"
            );
        }
        previous = dropped;
    }
}

#[tokio::test]
async fn a_request_carries_only_the_latest_screenshots() {
    let dir = tempdir().unwrap();
    let mut session = AgentSession::create(
        cfg(dir.path()),
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    for index in 0..7 {
        session.messages.push(screenshot_result(
            &format!("c{index}"),
            "desktop_screenshot",
            &format!("s{index}.png"),
        ));
    }
    let request = session.build_model_request();
    let with_images = request
        .messages
        .iter()
        .filter(|message| !message.attachments.is_empty())
        .count();
    assert_eq!(
        with_images, 4,
        "seven screenshots: the oldest three dropped"
    );
    let in_session = session
        .messages
        .iter()
        .filter(|message| !message.attachments.is_empty())
        .count();
    assert_eq!(
        in_session, 7,
        "the transcript itself keeps every screenshot"
    );
}

/// Records the turns that ended.
struct TurnWatcher {
    ended: Arc<std::sync::Mutex<Vec<forge_types::SessionId>>>,
}

#[async_trait]
impl Tool for TurnWatcher {
    fn name(&self) -> &str {
        "watcher"
    }
    fn description(&self) -> &str {
        "watches turns"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::success("watched"))
    }
    async fn turn_ended(&self, session_id: forge_types::SessionId) {
        self.ended.lock().unwrap().push(session_id);
    }
}

/// Tools hear when a turn is over, so screen control can end with it; a
/// turn paused on an approval is not over.
#[tokio::test]
async fn tools_hear_when_a_turn_ends_but_not_when_it_pauses() {
    let dir = tempdir().unwrap();
    let ended = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runs = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(TurnWatcher {
        ended: ended.clone(),
    }));
    tools.register(Arc::new(ConsentTool {
        name: "browser_submit",
        kind: ConsentKind::FormSubmit,
        scope: "example.com",
        runs: runs.clone(),
    }));
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
        call("c1", "browser_submit"),
    ]));
    let mut session = AgentSession::create(cfg(dir.path()), model, tools)
        .await
        .unwrap();
    session
        .run_user_message_with_stream("hello", None)
        .await
        .unwrap();
    assert_eq!(*ended.lock().unwrap(), vec![session.session_id]);

    session
        .run_user_message_with_stream("submit", None)
        .await
        .unwrap();
    assert!(session.pending_hitl().is_some(), "paused on the consent");
    assert_eq!(
        ended.lock().unwrap().len(),
        1,
        "a paused turn has not ended"
    );
}
