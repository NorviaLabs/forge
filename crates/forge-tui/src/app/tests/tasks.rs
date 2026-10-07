//! Sidebar background-task navigation, cancellation, and poll wiring tests
//! (relocated from the bottom panel's now-deleted Tasks tab).

use super::prelude::*;

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
    git(dir, &["init", "-q", "--initial-branch=main"]).await;
    git(dir, &["config", "user.email", "forge@example.com"]).await;
    git(dir, &["config", "user.name", "Forge Test"]).await;
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]).await;
    git(dir, &["commit", "-q", "-m", "init"]).await;
}

async fn wait_for_task_status(
    app: &mut TuiApp,
    id: forge_types::BackgroundTaskId,
    mut matches_status: impl FnMut(&forge_core::BackgroundTaskStatus) -> bool,
) {
    for _ in 0..300 {
        app.poll_background_tasks().await.unwrap();
        if let Some(task) = app.session_runtime.background().get(id) {
            if matches_status(&task.status) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("task {id:?} never reached the expected status");
}

async fn confirm_named_stop(app: &mut TuiApp) {
    let text = render_app_text(app, 80, 18);
    assert!(
        text.contains("Keep running") || text.contains("Keep waiting"),
        "{text}"
    );
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
}

#[tokio::test]
async fn task_selection_remains_on_the_named_job_when_a_sibling_finishes() {
    let (_dir, mut app) = focus_test_app().await;
    let first = app
        .session_runtime
        .spawn_background_shell("printf 'finished first\\n'".into(), "first".into())
        .await
        .unwrap();
    let selected = app
        .session_runtime
        .spawn_background_shell("sleep 5".into(), "selected".into())
        .await
        .unwrap();
    app.move_tasks_selection(1);
    assert_eq!(
        app.task_selection.task(app.selected_session_id),
        Some(selected)
    );
    wait_for_task_status(&mut app, first, |status| status.is_terminal()).await;
    // The first job is now below the running job in attention order.
    assert_eq!(
        app.task_selection.task(app.selected_session_id),
        Some(selected)
    );
    app.cancel_selected_task().await;
    confirm_named_stop(&mut app).await;
    wait_for_task_status(&mut app, selected, |status| {
        matches!(status, forge_core::BackgroundTaskStatus::Cancelled)
    })
    .await;
    assert!(matches!(
        app.session_runtime.background().get(first).unwrap().status,
        forge_core::BackgroundTaskStatus::Succeeded { .. }
    ));
}

#[tokio::test]
async fn approving_the_selected_waiting_task_from_the_sidebar_lets_it_finish() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path()).await;
    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: "".into(),
            tool_calls: vec![forge_types::ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "echo risky"}),
            }],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "finished after tui approval".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));
    let session = session_for_workspace_with_model(dir.path(), model).await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // This test is about approving from the sidebar, so it needs a prompt to
    // approve. Shell is not gated by default; add bash to HITL for this
    // scenario.
    app.session_runtime
        .set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));

    let id = app
        .session_runtime
        .spawn_subagent(forge_core::SubagentSpec {
            role: "risky-runner".into(),
            prompt: "run the risky command".into(),
            mode: forge_tools::AgentMode::Writer,
            tool_allowlist: None,
        })
        .await
        .unwrap();
    wait_for_task_status(&mut app, id, |s| {
        matches!(
            s,
            forge_core::BackgroundTaskStatus::WaitingForApproval { .. }
        )
    })
    .await;

    app.focus_block(FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('a'), KeyModifiers::NONE))
        .await
        .unwrap();

    // The confirmation names the agent rather than its row id.
    assert!(
        app.feedback.text.contains("risky-runner"),
        "approval feedback must name the task: {:?}",
        app.feedback.text
    );

    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;
    let task = app.session_runtime.background().get(id).unwrap();
    match &task.status {
        forge_core::BackgroundTaskStatus::Succeeded { summary } => {
            assert_eq!(summary, "finished after tui approval");
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }
}

/// An app whose only background task is a subagent that blocks on a `bash`
/// approval, gated by governance so it cannot run unasked.
async fn app_with_a_blocking_subagent(
    dir: &TempDir,
    script: Vec<ModelResponse>,
) -> (TuiApp, forge_types::BackgroundTaskId) {
    init_repo(dir.path()).await;
    let model = Arc::new(MockModelClient::script(script));
    let session = session_for_workspace_with_model(dir.path(), model).await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.session_runtime
        .set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));
    let id = app
        .session_runtime
        .spawn_subagent(forge_core::SubagentSpec {
            role: "risky-runner".into(),
            prompt: "run the risky command".into(),
            mode: forge_tools::AgentMode::Writer,
            tool_allowlist: None,
        })
        .await
        .unwrap();
    (app, id)
}

/// A model step that calls `bash`, which governance will hold for approval.
fn risky_bash_call() -> ModelResponse {
    ModelResponse {
        text: "".into(),
        tool_calls: vec![forge_types::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "echo risky"}),
        }],
        usage: None,
        thinking: None,
    }
}

fn is_waiting(status: &forge_core::BackgroundTaskStatus) -> bool {
    matches!(
        status,
        forge_core::BackgroundTaskStatus::WaitingForApproval { .. }
    )
}

/// The feature's whole point: an operator in another window learns which agent
/// is blocked, and what it wants.
///
/// Focus and mode live in thread-locals, and `#[tokio::test]`'s default
/// current-thread runtime keeps this test's future on one thread, so the
/// values set here are the ones the app reads.
#[tokio::test]
async fn a_block_notifies_an_unfocused_operator() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;

    let _ = crate::notify::take_captured();
    crate::notify::install(forge_config::NotifyMode::Bell);
    crate::notify::set_focused(false);

    wait_for_task_status(&mut app, id, is_waiting).await;
    let captured = crate::notify::take_captured();

    crate::notify::set_focused(true);
    crate::notify::install(forge_config::NotifyMode::Auto);

    assert_eq!(
        captured,
        vec!["risky-runner needs approval: bash".to_string()]
    );
}

/// Alerting someone who is looking at Forge is the noise this feature exists
/// to avoid.
#[tokio::test]
async fn a_block_is_silent_while_the_operator_is_watching() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;

    let _ = crate::notify::take_captured();
    crate::notify::install(forge_config::NotifyMode::Both);
    crate::notify::set_focused(true);

    wait_for_task_status(&mut app, id, is_waiting).await;
    let captured = crate::notify::take_captured();

    crate::notify::install(forge_config::NotifyMode::Auto);

    assert!(captured.is_empty(), "a focused terminal must stay quiet");
    // The in-app notice still fires, because the two surfaces answer
    // different questions.
    assert!(app.feedback.text.contains("risky-runner"));
}

/// A subagent that stops for approval has to say so, and say who it is. Before
/// this, a block changed nothing the operator could read: the footer chip's
/// `· 1 need` was the only evidence, and that does not name the asker.
#[tokio::test]
async fn a_subagent_stopping_for_approval_names_itself() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;
    wait_for_task_status(&mut app, id, is_waiting).await;

    assert!(
        app.feedback.text.contains("risky-runner"),
        "the block must name the agent that is asking: {:?}",
        app.feedback.text
    );
    assert!(
        app.feedback.text.contains("bash"),
        "and say what it wants to run: {:?}",
        app.feedback.text
    );
    assert_eq!(app.feedback.severity, FeedbackSeverity::Warn);
}

/// The operator can read what a subagent actually did, without this TUI
/// pretending to own a session it does not.
#[tokio::test]
async fn opening_a_subagent_shows_its_own_session_read_only() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;
    wait_for_task_status(&mut app, id, is_waiting).await;

    let parent_messages = app.transcript_view.messages().len();
    let parent_revision = app.transcript_view.revision();

    app.focus_block(FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(
        app.child_view.is_some(),
        "the view did not open: {}",
        app.feedback.text
    );
    assert_ne!(
        app.transcript_view.revision(),
        parent_revision,
        "the render cache keys on revision, so the child's must differ"
    );
    assert!(
        app.transcript_view.messages().len() > parent_messages,
        "the child's own conversation is on screen: {}",
        app.feedback.text
    );
    assert!(
        app.input.hint.contains("read-only"),
        "the composer has to say it cannot act: {}",
        app.input.hint
    );

    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.child_view.is_none());
    assert_eq!(
        app.transcript_view.revision(),
        parent_revision,
        "the parent's snapshot came back, not a re-capture"
    );
    assert_eq!(app.transcript_view.messages().len(), parent_messages);
    assert!(!app.input.hint.contains("read-only"));
}

/// A shell task has no session of its own, and saying so beats opening an
/// empty view.
#[tokio::test]
async fn a_shell_task_has_no_session_to_open() {
    let (_dir, mut app) = focus_test_app().await;
    app.session_runtime
        .spawn_background_shell("sleep 5".into(), "first".into())
        .await
        .unwrap();

    app.focus_block(FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.child_view.is_none());
    assert!(
        app.feedback.text.contains("no session to open"),
        "{}",
        app.feedback.text
    );
}

/// The composer must not look as though it can change a session this TUI does
/// not own.
#[tokio::test]
async fn the_composer_refuses_input_while_viewing_a_subagent() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;
    wait_for_task_status(&mut app, id, is_waiting).await;

    app.focus_block(FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.child_view.is_some(), "{}", app.feedback.text);

    app.focus_block(FocusBlock::Composer);
    app.handle_key(press(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(
        app.feedback.text.contains("read-only"),
        "the guard has to fire before the composer takes the character: {}",
        app.feedback.text
    );
}

/// A live child's transcript advances under the operator's eyes: the view is
/// re-read on the poll tick while the task is non-terminal.
#[tokio::test]
async fn a_running_child_view_advances_across_a_poll() {
    let dir = TempDir::new().unwrap();
    let (mut app, id) = app_with_a_blocking_subagent(&dir, vec![risky_bash_call()]).await;
    wait_for_task_status(&mut app, id, is_waiting).await;

    app.focus_block(FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.child_view.is_some(), "{}", app.feedback.text);

    let messages_open = app.transcript_view.messages().len();
    let revision_open = app.transcript_view.revision();

    // The subagent is still blocked on approval, so nothing new is written —
    // a refresh then leaves the revision untouched.
    app.poll_background_tasks().await.unwrap();
    assert_eq!(
        app.transcript_view.revision(),
        revision_open,
        "an unchanged journal must not invalidate the render cache"
    );

    // Denying ends the child, and the view still stands: it keeps its final
    // snapshot rather than re-reading or closing out from under the operator.
    app.resolve_selected_task_hitl(HitlDecision::Deny);
    app.poll_background_tasks().await.unwrap();
    app.poll_background_tasks().await.unwrap();

    assert!(app.child_view.is_some(), "{}", app.feedback.text);
    assert!(
        app.transcript_view.messages().len() >= messages_open,
        "the view should never lose messages: {}",
        app.feedback.text
    );
    assert!(
        app.conversation_view.scroll == 0,
        "a refresh must not steal the operator's place"
    );
}

/// `session_messages` returns `None` for an unreadable journal rather than an
/// error, so one bad session cannot fail the caller. The journal lives in a
/// file named for the session, so pointing at someone else's file is the
/// unreadable case: `open` succeeds (the file exists, has the wrong shape) and
/// the replay reads it as nothing.
#[tokio::test]
async fn an_unreadable_journal_reads_as_nothing() {
    let dir = TempDir::new().unwrap();
    let path = dir
        .path()
        .join(format!("{}.db", forge_types::SessionId::new_v4()));
    std::fs::write(&path, b"this is not a sqlite database").unwrap();

    let missing = forge_core::session_messages(
        dir.path(),
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse().ok())
            .unwrap(),
    )
    .await;

    assert!(
        missing.is_none(),
        "the caller falls back; it never fails the whole listing"
    );
}

#[tokio::test]
async fn sidebar_down_then_cancel_targets_the_selected_row() {
    let (_dir, mut app) = focus_test_app().await;
    let first = app
        .session_runtime
        .spawn_background_shell("sleep 5".into(), "first".into())
        .await
        .unwrap();
    app.session_runtime
        .spawn_background_shell("sleep 5".into(), "second".into())
        .await
        .unwrap();

    app.focus_block(FocusBlock::Sidebar);
    // Down from no selection lands on row index 1 (the second-spawned task) —
    // `move_tasks_selection`'s wrap starts `cur` at 0, so `+1` selects index 1.
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    confirm_named_stop(&mut app).await;

    // `cancel()` only flips the task's `CancellationToken` — the status
    // transition to `Cancelled` happens asynchronously once the spawned job
    // reacts and `poll_background_tasks` drains the result.
    let second_id = app
        .session_runtime
        .background()
        .list()
        .find(|t| t.label == "second")
        .unwrap()
        .id;
    for _ in 0..200 {
        app.poll_background_tasks().await.unwrap();
        if app
            .session_runtime
            .background()
            .get(second_id)
            .is_some_and(|t| t.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let first_task = app.session_runtime.background().get(first).unwrap();
    assert_eq!(first_task.status, forge_core::BackgroundTaskStatus::Running);
    let second_task = app.session_runtime.background().get(second_id).unwrap();
    assert_eq!(
        second_task.status,
        forge_core::BackgroundTaskStatus::Cancelled
    );
}

#[tokio::test]
async fn cancel_key_is_a_no_op_outside_the_sidebar() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session_runtime
        .spawn_background_shell("sleep 5".into(), "job".into())
        .await
        .unwrap();

    app.open_bottom_panel();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.session_runtime.background().get(id).unwrap().status,
        forge_core::BackgroundTaskStatus::Running
    );
}

#[tokio::test]
async fn poll_background_tasks_keeps_finished_job_in_session_state() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session_runtime
        .spawn_background_shell("echo done".into(), "echo".into())
        .await
        .unwrap();

    for _ in 0..200 {
        app.poll_background_tasks().await.unwrap();
        if app
            .session_runtime
            .background()
            .get(id)
            .is_some_and(|t| t.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let task = app.session_runtime.background().get(id).unwrap();
    assert!(matches!(
        task.status,
        forge_core::BackgroundTaskStatus::Succeeded { .. }
    ));
}

#[tokio::test]
async fn attach_moves_a_finished_background_result_into_the_composer() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session_runtime
        .spawn_background_shell("echo attached-output".into(), "echo".into())
        .await
        .unwrap();
    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;

    app.focus_block(FocusBlock::Sidebar);
    app.move_tasks_selection(1);
    app.attach_selected_task();

    assert!(
        app.input.text.contains("attached-output"),
        "result should land in the composer: {:?}",
        app.input.text
    );
    assert!(
        app.status_state.message.contains("attached"),
        "{:?}",
        app.status_state.message
    );
}

#[tokio::test]
async fn attach_is_a_no_op_for_a_running_task() {
    let (_dir, mut app) = focus_test_app().await;
    app.session_runtime
        .spawn_background_shell("sleep 5".into(), "sleep".into())
        .await
        .unwrap();

    app.focus_block(FocusBlock::Sidebar);
    app.move_tasks_selection(1);
    app.attach_selected_task();

    assert!(app.input.text.is_empty(), "{:?}", app.input.text);
    assert!(
        app.status_state.message.contains("hasn't finished"),
        "{:?}",
        app.status_state.message
    );
}

#[tokio::test]
async fn jobs_stop_defaults_to_keep_and_confirms_the_original_named_task() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("parent draft λ");
    app.input.cursor = 3;
    let queued = app
        .session_runtime
        .enqueue_task("next prompt")
        .await
        .unwrap();
    let first = app
        .session_runtime
        .spawn_background_shell("printf 'partial ✓\\n'; sleep 30".into(), "first".into())
        .await
        .unwrap();
    let second = app
        .session_runtime
        .spawn_background_shell("sleep 30".into(), "sibling".into())
        .await
        .unwrap();
    app.task_selection
        .select_task(app.selected_session_id, first);
    app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Jobs));
    app.cancel_selected_task().await;
    assert!(matches!(
        app.overlay,
        Some(Overlay::TaskStop { stop: false, .. })
    ));
    render_app_text(&mut app, 80, 18);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::Tasks { .. })));
    assert!(!app
        .session_runtime
        .background()
        .get(first)
        .unwrap()
        .cancel
        .is_cancelled());
    assert_eq!(app.input.text, "parent draft λ");
    assert_eq!(app.input.cursor, 3);

    let capture = app
        .session_runtime
        .background()
        .get(first)
        .unwrap()
        .shell
        .as_ref()
        .unwrap()
        .output
        .clone();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while capture.snapshot().stdout.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.cancel_selected_task().await;
    // A row reorder or later selection cannot redirect the captured target.
    app.task_selection
        .select_task(app.selected_session_id, second);
    confirm_named_stop(&mut app).await;
    wait_for_task_status(&mut app, first, |s| s.is_terminal()).await;
    assert_eq!(
        app.session_runtime.background().get(first).unwrap().status,
        forge_core::BackgroundTaskStatus::Cancelled
    );
    assert!(!app
        .session_runtime
        .background()
        .get(second)
        .unwrap()
        .cancel
        .is_cancelled());
    assert_eq!(
        app.session_runtime.queue().peek_next_queued().unwrap().id,
        queued.id
    );
    assert_eq!(app.input.text, "parent draft λ");
    assert_eq!(app.input.cursor, 3);
    app.task_selection
        .select_task(app.selected_session_id, first);
    app.dismiss_overlay();
    app.attach_selected_task();
    assert!(app.input.text.starts_with("parent draft λ\nJob #"));
    assert!(app
        .input
        .text
        .contains("Partial output · cancelled\nstdout:\npartial ✓"));
    assert!(app.input.text.contains("Exit code: not observed"));
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.session_runtime.queue().len(), 1);
    app.session_runtime.cancel_background_task(second);
    wait_for_task_status(&mut app, second, |s| s.is_terminal()).await;
}

#[tokio::test]
async fn jobs_stop_requires_a_painted_owner_and_rechecks_completion() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session_runtime
        .spawn_background_shell("sleep 0.2".into(), "finishing".into())
        .await
        .unwrap();
    app.task_selection.select_task(app.selected_session_id, id);
    app.cancel_selected_task().await;
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app
        .session_runtime
        .background()
        .get(id)
        .unwrap()
        .cancel
        .is_cancelled());
    render_app_text(&mut app, 80, 18);
    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.overlay.is_none());
    assert!(app.feedback.text.contains("nothing stopped"));
    assert!(!app
        .session_runtime
        .background()
        .get(id)
        .unwrap()
        .cancel
        .is_cancelled());
}

#[tokio::test]
async fn jobs_completion_is_observational_and_retained_output_is_reachable_after_dismissal() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("draft 東京");
    app.input.cursor = 2;
    app.focus_block(FocusBlock::Sidebar);
    let id = app.session_runtime.spawn_background_shell("printf '\\033[31m\\342\\200\\256unsafe\\033[0m\\n'; i=0; while [ $i -lt 100 ]; do printf 'line %s\\n' $i; i=$((i+1)); done; printf 'final output\\n'; exit 7".into(), "output".into()).await.unwrap();
    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;
    assert_eq!(app.input.text, "draft 東京");
    assert_eq!(app.input.cursor, 2);
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
    assert!(app.overlay.is_none());
    assert!(app.session_runtime.queue().is_empty());
    app.task_selection.select_task(app.selected_session_id, id);
    app.cancel_selected_task().await;
    let tasks = app.selected_background_tasks();
    assert!(app
        .ordered_dock_tasks(&tasks, chrono::Utc::now())
        .is_empty());
    assert!(app.session_runtime.background().get(id).is_some());
    app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Jobs));
    let first = render_app_text(&mut app, 80, 18);
    assert!(first.contains("output"));
    app.handle_key(press(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    let end = render_app_text(&mut app, 80, 18);
    assert!(end.contains("final output"), "{end}");
    assert!(end.contains("Esc return"));
    app.handle_key(press(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.starts_with("draft 東京\nJob #"));
    assert!(app.input.text.contains("Exit code: 7"));
    assert!(app.input.text.contains("final output"));
    assert!(!app.input.text.contains('\u{1b}'));
    assert!(!app.input.text.contains('\u{202e}'));
    assert_eq!(app.input.cursor, app.input.text.len());
    assert!(app.session_runtime.queue().is_empty());
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn jobs_stop_mouse_acts_on_the_painted_target_and_parent_change_refuses_action() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session_runtime
        .spawn_background_shell("sleep 30".into(), "named".into())
        .await
        .unwrap();
    app.task_selection.select_task(app.selected_session_id, id);
    app.cancel_selected_task().await;
    render_app_text(&mut app, 80, 18);
    let stop = app.task_stop_paint.as_ref().unwrap().choices[1];
    let parent = app.selected_session_id;
    app.selected_session_id = uuid::Uuid::new_v4();
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: stop.x,
        row: stop.y,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert!(!app
        .session_runtime
        .background()
        .get(id)
        .unwrap()
        .cancel
        .is_cancelled());
    app.selected_session_id = parent;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: stop.x,
        row: stop.y,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;
    assert_eq!(
        app.session_runtime.background().get(id).unwrap().status,
        forge_core::BackgroundTaskStatus::Cancelled
    );
}
