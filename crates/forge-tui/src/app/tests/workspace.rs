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

    // DESIGN-017: the conflict dialog carries the shared `> Title` modal
    // grammar, not a padded bare title.
    let rendered = render_app_text(&mut app, 120, 35);
    assert!(
        rendered.contains("> Unsaved Changes"),
        "conflict dialog title:\n{rendered}"
    );
    // DESIGN-018 matrix: the title and choices survive the 80×24 minimum
    // frame too.
    let narrow = render_app_text(&mut app, 80, 24);
    assert!(
        narrow.contains("> Unsaved Changes"),
        "conflict dialog title at 80x24:\n{narrow}"
    );
    assert!(
        narrow.contains("d discard"),
        "conflict dialog choices at 80x24:\n{narrow}"
    );

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
async fn narrow_files_navigation_reveals_the_pane_without_mutating_preference() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    let _narrow = render_app_text(&mut app, 80, 24);
    assert!(
        app.workspace_files.visible,
        "auto-collapse must not persist close"
    );
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(
        app.navigator_list_area.is_some(),
        "the key owner must be painted"
    );

    let _wide = render_app_text(&mut app, 160, 50);
    assert!(app.workspace_files.visible);
}

#[tokio::test]
async fn switching_workspace_panes_preserves_unsaved_work_and_reading_position() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("retry.rs");
    fs::write(&path, "fn retry() {}\n").unwrap();
    app.session_runtime.messages.push(Message::new(
        MessageRole::Assistant,
        (0..100)
            .map(|i| format!("History line {i}\n"))
            .collect::<String>(),
    ));
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    let buffer = app.editor_session.as_ref().unwrap().serialized_text();
    let cursor = app.editor_session.as_ref().unwrap().cursor_col();
    app.input.set_text("Keep this unsent draft");
    app.input.move_left();
    let draft_cursor = app.input.cursor;
    app.scroll_conversation_up(8);

    for (width, height) in [(80, 18), (120, 40), (160, 50), (80, 18)] {
        render_app_text(&mut app, width, height);
        app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
            .await
            .unwrap();
        render_app_text(&mut app, width, height);
        assert_eq!(app.focus.block(), FocusBlock::Sidebar);
        assert!(
            app.conversation_area.is_some(),
            "conversation input must have a visible owner"
        );
        let reading_position = app.conversation_view.scroll;
        app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
            .await
            .unwrap();
        render_app_text(&mut app, width, height);
        assert_eq!(app.focus.block(), FocusBlock::Workspace);
        assert!(
            app.editor_area.is_some(),
            "editor input must have a visible owner"
        );
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::File(path.clone()))
        );
        // Pointer tabs use the same retained state as the keyboard switch.
        for block in [FocusBlock::Sidebar, FocusBlock::Workspace] {
            let (_, rect) = app
                .workspace_tab_areas
                .iter()
                .find(|(candidate, _)| *candidate == block)
                .copied()
                .unwrap();
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: rect.x + 3,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
            .await
            .unwrap();
            render_app_text(&mut app, width, height);
            assert_eq!(app.focus.block(), block);
        }
        assert!(app.workspace_navigation.history().is_empty());
        assert_eq!(app.conversation_view.scroll, reading_position);
        assert_eq!(app.input.text, "Keep this unsent draft");
        assert_eq!(app.input.cursor, draft_cursor);
        let editor = app.editor_session.as_ref().unwrap();
        assert!(editor.is_dirty());
        assert_eq!(editor.serialized_text(), buffer);
        assert_eq!(editor.cursor_col(), cursor);
        assert!(
            !app.explorer_dialog.is_open(),
            "view switching is not a destructive exit"
        );
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "fn retry() {}\n");
}

#[tokio::test]
async fn a_pending_approval_remains_reachable_over_narrow_inspection_and_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.input.set_text("Keep this unsent draft");
    app.bottom_panel.open = true;
    set_pending_hitl(&mut app, direct_hitl_payload("pending", "/tmp/x"));
    app.sync_approval_focus();

    for block in [FocusBlock::Workspace, FocusBlock::Search] {
        app.focus_block(block);
        render_app_text(&mut app, 80, 18);
        assert_eq!(app.focus.block(), FocusBlock::Approval);
        assert!(app.conversation_area.is_some());
        assert!(
            !app.option_rects.is_empty(),
            "the decision must be actionable"
        );
        assert!(app.session_view.pending_hitl.is_some());
        assert_eq!(app.input.text, "Keep this unsent draft");
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::File(path.clone()))
        );
    }
}

#[tokio::test]
async fn narrow_file_search_is_reachable_and_returns_to_the_retained_editor() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.navigator_list_area.is_some());
    assert!(app.workspace_files.explorer.search_focused);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path.clone()))
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.editor_area.is_some());
    assert!(!app.explorer_dialog.is_open());
}

#[tokio::test]
async fn compact_navigation_and_inspection_can_reclaim_a_retained_terminals_rows() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.bottom_panel.open = true;
    app.input.set_text("Keep this draft");

    let rendered = render_app_text(&mut app, 80, 18);
    assert!(
        rendered.contains("fn main()"),
        "inspection must remain usable"
    );
    assert!(app.terminal_area.is_none());
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert!(app.navigator_list_area.is_some_and(|area| !area.is_empty()));
    assert!(app.workspace_files.explorer.search_focused);

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.terminal_area.is_some_and(|area| !area.is_empty()));
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.editor_area.is_some());
    assert!(app.terminal_area.is_none());
    assert!(app.bottom_panel.open, "presentation must retain the shell");
    assert_eq!(app.input.text, "Keep this draft");
    app.toggle_bottom_panel();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.terminal_area.is_some());
    app.toggle_bottom_panel();
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn workspace_switch_does_not_interrupt_a_modal_or_an_editor_command() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    app.editor_command = Some(":w".into());
    app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.editor_command.as_deref(), Some(":w"));
    assert_eq!(app.workspace_navigation, before);
    app.editor_command = None;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.overlay.is_some());
    assert_eq!(app.workspace_navigation, before);
}

#[tokio::test]
async fn resizing_conversation_moves_its_boundary_and_cancel_restores_the_layout() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);
    app.focus_block(FocusBlock::Sidebar);
    render_app_text(&mut app, 160, 50);
    let initial = app.pane_resize.preferences;
    let original_boundary = app.pane_resize.conversation_separator.unwrap();
    app.input.set_text("Keep this draft during resizing");

    app.begin_resize_mode();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 160, 50);
    assert!(app.pane_resize.conversation_separator.unwrap().x > original_boundary.x);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.pane_resize.preferences, initial);
    render_app_text(&mut app, 160, 50);

    assert!(app.start_mouse_resize(original_boundary.x, original_boundary.y));
    assert!(app.drag_mouse_resize(original_boundary.x + 4, original_boundary.y));
    render_app_text(&mut app, 160, 50);
    assert!(app.pane_resize.conversation_separator.unwrap().x > original_boundary.x);
    assert!(app.finish_mouse_resize());
    assert_eq!(app.input.text, "Keep this draft during resizing");
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::Diff)
    );
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
    let (_fake_home, _home_guard) = fake_home_guard();
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
