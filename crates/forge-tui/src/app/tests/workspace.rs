//! Workspace navigation and files-sidebar visibility tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn workspace_navigation_starts_empty_at_home() {
    let (_dir, app) = focus_test_app().await;

    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
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
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(first.clone()))
    );
    // Nothing to push onto history yet — the home state (`None`) isn't a
    // concrete view, so opening the first file from home leaves history empty.
    assert_eq!(app.workspace_navigation.history(), Vec::new());

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(second))
    );
    assert_eq!(app.workspace_navigation.history(), Vec::new());
}

#[tokio::test]
async fn workspace_back_from_a_file_returns_home() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("stale.rs");
    fs::write(&path, "fn stale() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
}

#[tokio::test]
async fn workspace_home_returns_to_empty_and_clears_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
}

#[tokio::test]
async fn workspace_home_requires_a_dirty_editor_decision() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-home.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
}

#[tokio::test]
async fn alt_navigation_requires_a_dirty_editor_decision() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-nav.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));

    app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());
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
    app.workspace_files.visible = true;

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert!(app.workspace_files.visible);
    assert_eq!(app.workspace_navigation.current(), None);
}

#[tokio::test]
async fn files_panel_is_open_by_default() {
    let (_dir, app) = focus_test_app().await;

    assert!(app.workspace_files.visible);
    assert_eq!(app.focus.block(), FocusBlock::Composer);
}

#[tokio::test]
async fn files_visibility_renders_independently_in_each_workspace_view() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.workspace_files.visible = true;
    app.navigate_to_workspace_view(WorkspaceView::File(path.clone()));
    assert!(app.workspace_files.visible);

    app.workspace_files.visible = false;
    let _rendered = render_app_text(&mut app, 160, 50);
    assert!(!app.workspace_files.visible);

    // The empty/home state (`current == None`) is its own case now —
    // conversation isn't a navigable `WorkspaceView` to loop over above.
    app.go_home_workspace();
    app.workspace_files.visible = true;
    let _rendered = render_app_text(&mut app, 160, 50);
    assert!(app.workspace_files.visible);
    app.workspace_files.visible = false;
    let _rendered = render_app_text(&mut app, 160, 50);
    assert!(!app.workspace_files.visible);
}

#[tokio::test]
async fn files_visibility_auto_collapses_and_restores_without_mutating_preference() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    let _narrow = render_app_text(&mut app, 80, 24);
    assert!(
        app.workspace_files.visible,
        "auto-collapse must not persist close"
    );
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);

    let _wide = render_app_text(&mut app, 160, 50);
    assert!(app.workspace_files.visible);
}

#[tokio::test]
async fn files_explicit_close_remains_closed_after_resizing() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    // Closing is now only reachable from the explorer itself: from anywhere
    // else Ctrl+E means "take me to Files". That is what stops an accidental
    // close from handing keystrokes to the modal editor, and it costs one
    // extra press to close from elsewhere.
    app.focus_block(FocusBlock::Search);
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();

    assert!(!app.workspace_files.visible);
    let _narrow = render_app_text(&mut app, 80, 24);
    let _wide = render_app_text(&mut app, 160, 50);
    assert!(!app.workspace_files.visible);
}

#[tokio::test]
async fn files_visibility_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    // Ctrl+E closes only from the explorer; from anywhere else it means "take
    // me to Files". Focus there first so this exercises an explicit close.
    app.focus_block(FocusBlock::Search);
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert!(!app.workspace_files.visible);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert!(!restored.workspace_files.visible);

    let (_other_dir, other) = focus_test_app().await;
    assert!(
        other.workspace_files.visible,
        "Files preference must not leak across repositories"
    );
}

#[tokio::test]
async fn opening_file_does_not_open_closed_files_preference() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.workspace_files.visible = false;

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();

    assert!(!app.workspace_files.visible);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn responsive_sizes_render_without_panic_and_follow_files_policy() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.conversation_view.splash_dismissed = true;
    for (width, height, _expect_files) in [
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
        assert!(app.workspace_files.visible);
    }
}

// ---------------------------------------------------------------------------
// Ctrl+E must never hand keystrokes to the editor.
//
// The original failure: with a file open and the Files pane already visible
// but unfocused, Ctrl+E closed the pane and returned focus to the editor.
// The editor is modal, so typing `index.rs` ran `i` (INSERT) and wrote
// "ndex.rs" into the buffer. Nothing on screen indicated focus had moved, and
// the Unsaved Changes dialog defaults to Save.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ctrl_e_reaches_files_instead_of_closing_when_focus_is_elsewhere() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(
        app.workspace_files.visible,
        "Ctrl+E must take you to Files, not close the pane you were heading for"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::Search,
        "focus must land in the file search, not the editor"
    );
}

/// Pressing it again, from the explorer, still closes — the toggle is intact,
/// it is just no longer reachable by accident.
#[tokio::test]
async fn ctrl_e_from_the_explorer_still_closes_the_pane() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Search);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(!app.workspace_files.visible, "the toggle must still work");
    assert_ne!(
        app.focus.block(),
        FocusBlock::Workspace,
        "closing must not drop focus into the modal editor"
    );
}

/// The end-to-end regression: the exact keystrokes that used to corrupt a
/// buffer must now filter files instead.
#[tokio::test]
async fn typing_after_ctrl_e_filters_files_and_never_edits_the_buffer() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    for ch in "index.rs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }

    // If any keystroke had reached the editor the filter would be empty, and
    // `i` would have switched the buffer into INSERT.
    assert_eq!(
        app.workspace_files.explorer.search_query, "index.rs",
        "every keystroke must reach the file filter"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::Search,
        "focus must not drift into the editor mid-typing"
    );
}
