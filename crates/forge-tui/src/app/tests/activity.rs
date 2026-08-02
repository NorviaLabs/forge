//! Activity feed and agent-turn presentation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn agent_streaming_while_viewing_file_does_not_navigate() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.pending_turn.prompt = None;
    app.stream_preview = "partial answer".into();
    let rendered = render_app_text(&mut app, 100, 30);

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert!(rendered.contains("fn main()"), "{rendered}");
    assert!(
        !rendered.contains("partial answer"),
        "File view should remain primary while streaming:\n{rendered}"
    );
}

#[tokio::test]
async fn agent_thinking_keeps_composer_usable() {
    let (_dir, mut app) = focus_test_app().await;
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.stream_thinking = "planning".into();
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "x");
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    let summary = app.activity_summary().expect("thinking summary");
    assert_eq!(summary.label, "Forge is thinking");
}

#[tokio::test]
async fn activity_summary_priority_renders_one_actionable_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Running cargo test"), "{rendered}");
    assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("files changed · Review"),
        "Run summary must outrank changes:\n{rendered}"
    );
    assert!(
        !rendered.contains("Forge is thinking"),
        "Run summary must outrank thinking:\n{rendered}"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    app.run_exec.rx = Some(rx);
    tx.send(RunEvent::Finished {
        exit_code: Some(1),
        success: false,
    })
    .unwrap();
    app.poll_run();

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Run failed: cargo test"), "{rendered}");
    assert_eq!(rendered.matches("Inspect").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("Running cargo test"),
        "Failure summary must replace active-run summary:\n{rendered}"
    );
}

#[tokio::test]
async fn summary_action_opens_expected_workspace_view() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    let id = app.run.current.as_ref().unwrap().id.clone();
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current, WorkspaceView::Run(id));
}

#[tokio::test]
async fn changes_summary_action_uses_review_changes_command() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);

    assert_eq!(
        app.activity_summary_command(),
        Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
    );
    app.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
}
