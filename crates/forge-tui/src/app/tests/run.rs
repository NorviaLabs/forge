//! Run panel lifecycle and run-adjacent navigation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn run_starts_only_from_run_workspace_not_bottom_panel() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));

    app.bottom_panel.open_tab(BottomPanelTab::Terminal);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));

    app.run_current_draft();
    let id = app.run.current.as_ref().unwrap().id.clone();
    app.cancel_run();
    app.run.draft.command_input = "true".into();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(id)))
        .await
        .unwrap();
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));
}

#[tokio::test]
async fn run_cancel_from_run_workspace() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Cancelled));
}

#[tokio::test]
async fn restored_running_run_becomes_cancelled() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: Some(CommandConfig {
                executable: "true".into(),
                args: vec![],
            }),
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    app.normalize_restored_run();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Cancelled));
    assert!(!app.run_execution.execution.pending_validation);
    assert!(app.run_execution.execution.rx.is_none());
}

#[tokio::test]
async fn ui_navigation_does_not_mutate_run_history() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open_tab(BottomPanelTab::Terminal);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.run.recent.is_empty());
}

#[tokio::test]
async fn leaving_run_view_does_not_cancel_running_run() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    let id = app.run.current.as_ref().unwrap().id.clone();

    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Run(id.clone())
    );

    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.id == id && record.state == RunState::Running));
}

#[tokio::test]
async fn run_start_updates_activity_without_navigating_from_conversation() {
    let (_dir, mut app) = focus_test_app().await;
    let before = app.workspace_navigation.clone();
    app.run.draft.command_input = "true".into();

    app.run_current_draft();

    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run && item.summary.contains("run started")));
    let summary = app.activity_summary().expect("run summary");
    assert_eq!(summary.label, "Running true");
    assert_eq!(summary.action_label, Some("View output"));

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Running true"), "{rendered}");
    assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("Running validation"),
        "run must not also render the old running tool card:\n{rendered}"
    );
}

#[tokio::test]
async fn run_start_while_in_file_does_not_hijack_workspace() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.run.draft.command_input = "true".into();
    app.run_current_draft();

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run));
}

#[tokio::test]
async fn run_failure_while_in_diff_updates_summary_without_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "false".into();
    app.run_current_draft();
    let run_id = app.run.current.as_ref().unwrap().id.clone();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_execution.execution.rx = Some(rx);

    tx.send(RunEvent::Finished {
        exit_code: Some(1),
        success: false,
    })
    .unwrap();
    app.poll_run();

    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.id == run_id && record.state == RunState::Failed));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run
            && item.severity == FeedbackSeverity::Error
            && item.summary.contains("run failed")));
    assert_eq!(
        app.activity_summary_command(),
        Some(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
    );
}

#[tokio::test]
async fn edge_run_cancel_preserves_output_and_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    let before = app.workspace_navigation.clone();
    app.run.draft.command_input = "long-running".into();
    app.run_current_draft();
    app.append_terminal_output(b"partial output\n");

    app.cancel_run();

    let record = app.run.current.as_ref().expect("current run");
    assert_eq!(record.state, RunState::Cancelled);
    assert_eq!(record.exit_status, None);
    assert!(app.terminal_capture.content.contains("partial output"));
    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .recent
        .iter()
        .any(|run| run.state == RunState::Cancelled));
}

#[tokio::test]
async fn edge_run_spawn_failure_shows_invocation_without_exit_code() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "definitely-missing-forge-command --flag".into();
    app.run_current_draft();
    let run_id = app.run.current.as_ref().unwrap().id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_execution.execution.rx = Some(rx);

    tx.send(RunEvent::SpawnFailed("No such file or directory".into()))
        .unwrap();
    app.poll_run();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
        .await
        .unwrap();

    let record = app.run.current.as_ref().expect("current run");
    assert_eq!(record.state, RunState::StartFailed);
    assert_eq!(record.exit_status, None);
    assert_eq!(
        record.invocation.executable,
        "definitely-missing-forge-command"
    );
    assert_eq!(record.invocation.arguments, vec!["--flag"]);
    assert!(record.spawn_error.as_deref().unwrap().contains("No such"));

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Could not start"), "{rendered}");
    assert!(
        rendered.contains("Executable: definitely-missing-forge-command"),
        "{rendered}"
    );
    assert!(rendered.contains("Arguments: [\"--flag\"]"), "{rendered}");
    assert!(rendered.contains("Directory:"), "{rendered}");
    assert!(
        rendered.contains("Cause: No such file or directory"),
        "{rendered}"
    );
    assert!(rendered.contains("e edit rerun"), "{rendered}");
    assert!(!rendered.contains("Exit status:"), "{rendered}");
}
