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

    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Model;
    app.pending_turn.prompt = None;
    app.stream.preview = "partial answer".into();
    let rendered = render_app_text(&mut app, 100, 30);

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::File(path))
    );
    assert!(rendered.contains("fn main()"), "{rendered}");
    // The sidebar always shows the conversation now, regardless of what the
    // center pane displays — streaming preview text is expected here.
    assert!(
        rendered.contains("partial answer"),
        "Sidebar should keep streaming visible while File view is primary:\n{rendered}"
    );
}

#[tokio::test]
async fn agent_thinking_keeps_composer_usable() {
    let (_dir, mut app) = focus_test_app().await;
    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Model;
    app.stream.thinking = "planning".into();
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "x");
    assert_eq!(app.workspace_navigation.current, None);
    assert!(app.activity_summary().is_none());
}

#[tokio::test]
async fn activity_summary_priority_renders_one_actionable_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files
        .explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Model;

    let summary = app.activity_summary().expect("changes summary");
    assert_eq!(summary.label, "1 file changed");
    assert_eq!(summary.action_label, Some("Review"));
}

#[tokio::test]
async fn changes_summary_action_uses_review_changes_command() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files
        .explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::Diff(DiffCommandContext::Current))
    );
}

#[tokio::test]
async fn alt_right_activity_review_requires_a_dirty_editor_decision() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-review.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();

    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    app.workspace_files
        .explorer
        .git_status
        .status
        .insert(PathBuf::from("dirty-review.rs"), GitStatusKind::Modified);

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();

    assert!(matches!(
        app.explorer_dialog.current,
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());
    assert!(app.editor_session.as_ref().unwrap().is_dirty());
    assert_eq!(fs::read_to_string(path).unwrap(), "fn main() {}\n");
}

#[tokio::test]
async fn alt_right_without_summary_still_opens_review_changes() {
    let (_dir, mut app) = focus_test_app().await;
    // No git changes, no run — summary has no action.
    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::Diff(DiffCommandContext::Current))
    );
}
