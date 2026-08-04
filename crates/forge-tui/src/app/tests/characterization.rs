//! Broad layout/behavior characterization tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn characterization_contextual_views_are_reachable_with_current_controls() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Left, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn characterization_files_selection_and_expansion_survive_focus_roundtrip() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    app.workspace_files.explorer.refresh_workspace();
    let src = dir.path().join("src").canonicalize().unwrap();
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.workspace_files.explorer.selected_path = Some(src.clone());
    app.workspace_files.explorer.expand_selected();

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(app.focus.block, FocusBlock::Files);
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block, FocusBlock::Files);
    assert_eq!(
        app.workspace_files.explorer.selected_path.as_deref(),
        Some(src.as_path())
    );
    assert!(app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "lib.rs"));
}

#[tokio::test]
async fn characterization_80x24_draws_without_panic() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn characterization_run_completion_preserves_bottom_panel_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel(Some(BottomPanelTab::Tasks));
    app.run.draft.command_input = "/usr/bin/true".into();
    app.run_current_draft();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.run_execution.execution.pending_validation);

    app.drain_pending_validation(None).await.unwrap();
    for _ in 0..50 {
        app.poll_run();
        if app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state != RunState::Running)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.run.current.as_ref().is_some_and(|record| matches!(
        record.state,
        RunState::Succeeded | RunState::Failed | RunState::StartFailed
    )));
}
