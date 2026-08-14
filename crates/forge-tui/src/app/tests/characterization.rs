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
    assert_eq!(app.workspace_navigation.current, None);

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current, None);
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::File(path))
    );
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

    assert!(!app.workspace_files.explorer.search_focused);
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
