//! Tasks bottom-panel navigation, cancellation, and poll wiring tests.

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
        if let Some(task) = app.session.background.get(id) {
            if matches_status(&task.status) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("task {id:?} never reached the expected status");
}

#[tokio::test]
async fn approving_the_selected_waiting_task_from_the_tasks_tab_lets_it_finish() {
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
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    let id = app
        .session
        .spawn_subagent(forge_core::SubagentSpec {
            role: "risky-runner".into(),
            prompt: "run the risky command".into(),
            tool_allowlist: None,
            max_turns: None,
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

    app.bottom_panel.open_tab(BottomPanelTab::Tasks);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('a'), KeyModifiers::NONE))
        .await
        .unwrap();

    wait_for_task_status(&mut app, id, |s| s.is_terminal()).await;
    let task = app.session.background.get(id).unwrap();
    match &task.status {
        forge_core::BackgroundTaskStatus::Succeeded { summary } => {
            assert_eq!(summary, "finished after tui approval");
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }
}

#[tokio::test]
async fn tasks_tab_down_then_cancel_targets_the_selected_row() {
    let (_dir, mut app) = focus_test_app().await;
    let first = app
        .session
        .spawn_background_shell("sleep 5".into(), "first".into())
        .await
        .unwrap();
    app.session
        .spawn_background_shell("sleep 5".into(), "second".into())
        .await
        .unwrap();

    app.bottom_panel.open_tab(BottomPanelTab::Tasks);
    app.focus_block(FocusBlock::BottomPanel);
    // Down from no selection lands on row index 1 (the second-spawned task) —
    // `move_tasks_selection`'s wrap starts `cur` at 0, so `+1` selects index 1.
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    // `cancel()` only flips the task's `CancellationToken` — the status
    // transition to `Cancelled` happens asynchronously once the spawned job
    // reacts and `poll_background_tasks` drains the result.
    let second_id = app
        .session
        .background
        .list()
        .find(|t| t.label == "second")
        .unwrap()
        .id;
    for _ in 0..200 {
        app.poll_background_tasks().await.unwrap();
        if app
            .session
            .background
            .get(second_id)
            .is_some_and(|t| t.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let first_task = app.session.background.get(first).unwrap();
    assert_eq!(first_task.status, forge_core::BackgroundTaskStatus::Running);
    let second_task = app.session.background.get(second_id).unwrap();
    assert_eq!(
        second_task.status,
        forge_core::BackgroundTaskStatus::Cancelled
    );
}

#[tokio::test]
async fn cancel_key_is_a_no_op_outside_the_tasks_tab() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session
        .spawn_background_shell("sleep 5".into(), "job".into())
        .await
        .unwrap();

    app.bottom_panel.open_tab(BottomPanelTab::Run);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.session.background.get(id).unwrap().status,
        forge_core::BackgroundTaskStatus::Running
    );
}

#[tokio::test]
async fn poll_background_tasks_surfaces_a_finished_job_in_the_tasks_tab() {
    let (_dir, mut app) = focus_test_app().await;
    let id = app
        .session
        .spawn_background_shell("echo done".into(), "echo".into())
        .await
        .unwrap();

    for _ in 0..200 {
        app.poll_background_tasks().await.unwrap();
        if app
            .session
            .background
            .get(id)
            .is_some_and(|t| t.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let task = app.session.background.get(id).unwrap();
    assert!(matches!(
        task.status,
        forge_core::BackgroundTaskStatus::Succeeded { .. }
    ));

    app.bottom_panel.open_tab(BottomPanelTab::Tasks);
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("echo"), "{rendered}");
    assert!(rendered.contains("succeeded"), "{rendered}");
}
