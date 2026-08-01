//! Workspace navigation and files-sidebar visibility tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn workspace_navigation_starts_at_conversation_home() {
    let (_dir, app) = focus_test_app().await;

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.workspace_navigation.history.is_empty());
}

#[tokio::test]
async fn workspace_navigation_pushes_file_and_replaces_file_resource() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    fs::write(&first, "fn a() {}\n").unwrap();
    fs::write(&second, "fn b() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(first.clone())
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![WorkspaceView::Conversation]
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(second)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![WorkspaceView::Conversation]
    );
}

#[tokio::test]
async fn workspace_navigation_pushes_between_file_diff_and_file() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    fs::write(&first, "fn a() {}\n").unwrap();
    fs::write(&second, "fn b() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![
            WorkspaceView::Conversation,
            WorkspaceView::File(first.clone())
        ]
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(second)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![
            WorkspaceView::Conversation,
            WorkspaceView::File(first),
            WorkspaceView::Diff(DiffCommandContext::Current)
        ]
    );
}

#[tokio::test]
async fn workspace_back_skips_invalid_file_entries() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("stale.rs");
    fs::write(&path, "fn stale() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    fs::remove_file(&path).unwrap();
    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn workspace_home_returns_to_conversation_and_clears_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.workspace_navigation.history.is_empty());
}

#[tokio::test]
async fn overlay_open_and_close_do_not_mutate_workspace_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.overlay = Some(Overlay::welcome());
    app.execute_semantic_command(SemanticCommand::CloseOverlay)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation, before);
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn files_visibility_is_independent_of_workspace_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.files_visible = true;

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert!(app.files_visible);
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn files_visibility_renders_independently_in_each_workspace_view() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    for view in [
        WorkspaceView::Conversation,
        WorkspaceView::File(path.clone()),
        WorkspaceView::Diff(DiffCommandContext::Current),
    ] {
        app.files_visible = true;
        app.navigate_to_workspace_view(view.clone());
        let rendered = render_app_text(&mut app, 160, 50);
        assert!(
            rendered.contains("FILES"),
            "Files should render for {view:?} when preference is open:\n{rendered}"
        );

        app.files_visible = false;
        let rendered = render_app_text(&mut app, 160, 50);
        assert!(
            !rendered.contains("FILES"),
            "Files should not render for {view:?} when preference is closed:\n{rendered}"
        );
    }
}

#[tokio::test]
async fn files_visibility_auto_collapses_and_restores_without_mutating_preference() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);

    let narrow = render_app_text(&mut app, 80, 24);
    assert!(!narrow.contains("FILES"), "{narrow}");
    assert!(app.files_visible, "auto-collapse must not persist close");
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    let wide = render_app_text(&mut app, 160, 50);
    assert!(wide.contains("FILES"), "{wide}");
    assert!(app.files_visible);
}

#[tokio::test]
async fn files_explicit_close_remains_closed_after_resizing() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();

    assert!(!app.files_visible);
    let narrow = render_app_text(&mut app, 80, 24);
    let wide = render_app_text(&mut app, 160, 50);
    assert!(!narrow.contains("FILES"), "{narrow}");
    assert!(!wide.contains("FILES"), "{wide}");
}

#[tokio::test]
async fn files_visibility_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert!(app.files_visible);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert!(restored.files_visible);

    let (_other_dir, other) = focus_test_app().await;
    assert!(
        !other.files_visible,
        "Files preference must not leak across repositories"
    );
}

#[tokio::test]
async fn opening_file_does_not_open_closed_files_preference() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.files_visible = false;

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();

    assert!(!app.files_visible);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn responsive_sizes_render_without_panic_and_follow_files_policy() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.splash_dismissed = true;
    for (width, height, expect_files) in [
        (80, 24, false),
        (120, 40, true),
        (160, 50, true),
        (240, 60, true),
    ] {
        let rendered = render_app_text(&mut app, width, height);
        assert!(
            rendered.contains("Describe a task"),
            "composer should remain reachable at {width}x{height}:\n{rendered}"
        );
        assert_eq!(
            rendered.contains("FILES"),
            expect_files,
            "unexpected Files visibility at {width}x{height}:\n{rendered}"
        );
    }
}
