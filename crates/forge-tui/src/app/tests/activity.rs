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

    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.pending_turn.clear();
    app.stream.preview = "partial answer".into();
    // The event loop lets the preview through; this test renders directly.
    let rendered = render_app_text(&mut app, 120, 40);

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path.clone()))
    );
    assert!(rendered.contains("fn main()"), "{rendered}");
    // Both panes remain available while streaming; a narrow inspector never
    // gets displaced by model activity. Switching back is the user's action.
    assert!(
        rendered.contains("partial answer"),
        "Streaming should stay visible beside inspection:\n{rendered}"
    );
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
        .await
        .unwrap();
    let rendered = render_app_text(&mut app, 80, 18);
    assert!(rendered.contains("partial answer"), "{rendered}");
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn agent_thinking_keeps_composer_usable() {
    let (_dir, mut app) = focus_test_app().await;
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.stream.thinking = "planning".into();
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "x");
    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.activity_summary().is_none());
}

#[tokio::test]
async fn alt_right_and_workspace_right_do_not_open_review() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files
        .explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
    app.focus_block(FocusBlock::Workspace);

    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Right, KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::NONE)),
        None
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
}
