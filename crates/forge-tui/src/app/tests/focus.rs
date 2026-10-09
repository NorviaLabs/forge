//! Focus, tab order, and semantic keybinding integration tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn focus_starts_on_composer_block() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.focus.mode(), FocusMode::Navigation);
}

#[tokio::test]
async fn tab_stays_in_terminal_and_shift_tab_leaves() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = true;
    app.focus_block(FocusBlock::BottomPanel);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
}

#[tokio::test]
async fn tab_cycles_visible_blocks_and_skips_hidden_ones() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_navigation.push_view(WorkspaceView::Diff);
    app.workspace_files.visible = true;
    app.bottom_panel.open = true;

    // A resource tab hides the conversation, so Tab never reaches Sidebar.
    app.focus_block(FocusBlock::Workspace);
    for _ in 0..6 {
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_ne!(app.focus.block(), FocusBlock::Sidebar);
    }

    // Agent hides the resource editor, so Tab never reaches Workspace.
    app.focus_block(FocusBlock::Sidebar);
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
    for _ in 0..6 {
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_ne!(app.focus.block(), FocusBlock::Workspace);
    }
}

#[tokio::test]
async fn reopening_terminal_restarts_an_exited_shell() {
    if !crate::interactive_terminal::pty_allocation_available() {
        return;
    }
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel();
    app.interactive_terminal
        .as_mut()
        .unwrap()
        .consume_input(b"exit 0\r")
        .unwrap();
    for _ in 0..100 {
        let terminal = app.interactive_terminal.as_mut().unwrap();
        terminal.poll();
        if !terminal.running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!app.interactive_terminal.as_ref().unwrap().running);
    app.toggle_bottom_panel();
    app.open_bottom_panel();
    assert!(app.interactive_terminal.as_ref().unwrap().running);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
}

#[tokio::test]
async fn busy_tab_navigation_preserves_the_composer_draft() {
    let (_dir, mut app) = focus_test_app().await;
    app.session_runtime
        .append_user_message("working")
        .await
        .unwrap();
    render_app_text(&mut app, 120, 40);
    for phase in [
        BusyPhase::Model,
        BusyPhase::Tool {
            name: "bash".into(),
        },
    ] {
        app.busy_state.start(phase);
        for draft in ["", "next task"] {
            app.focus_block(FocusBlock::Composer);
            app.input.set_text(draft);
            app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
                .await
                .unwrap();
            assert_eq!(app.focus.block(), FocusBlock::Footer);
            app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
                .await
                .unwrap();
            assert_eq!(app.focus.block(), FocusBlock::Composer);
            app.handle_key(press(KeyCode::Tab, KeyModifiers::SHIFT))
                .await
                .unwrap();
            assert_eq!(app.focus.block(), FocusBlock::Sidebar);
            assert_eq!(app.input.text, draft);
            assert!(app.selected_queue_messages().is_empty());
        }
    }
}

#[tokio::test]
async fn tabbing_into_footer_selects_which_llm_first() {
    // Entering FocusBlock::Footer (an ordinary Tab stop, not a separate
    // F3 side-channel) selects the first control (which-LLM, index 0).
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    app.composer_chip_focus = None;

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));
}

#[tokio::test]
async fn left_right_move_between_footer_controls() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer_chip_focus, Some(1));

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer_chip_focus, Some(0), "wraps back to which-LLM");

    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.composer_chip_focus, Some(1), "wraps the other way too");
}

#[tokio::test]
async fn enter_on_which_llm_chip_opens_the_connect_picker() {
    // With nothing connected, Enter on the which-LLM chip opens the same
    // provider/model picker (see activate_composer_chip).
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::ConnectModel { .. })),
        "Enter on which-LLM chip should open the model picker, got {:?}",
        app.overlay
    );
}

#[tokio::test]
async fn leaving_footer_clears_its_sub_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_ne!(app.focus.block(), FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, None);
}

#[tokio::test]
async fn esc_leaves_footer_back_to_previous_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.focus_block(FocusBlock::Footer);

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_ne!(app.focus.block(), FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, None);
}

#[tokio::test]
async fn f3_no_longer_focuses_the_footer() {
    // F3 used to be a standalone side-channel into chip navigation;
    // reaching the footer is an ordinary Tab stop now.
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::F(3), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.composer_chip_focus, None);
}

#[tokio::test]
async fn tab_and_shift_tab_traverse_sidebar_and_composer() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_navigation.push_view(WorkspaceView::Diff);
    app.focus_block(FocusBlock::Sidebar);

    // Sidebar focus selects Agent; the shared composer follows.
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Agent);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Composer);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
}

#[tokio::test]
async fn opening_and_closing_bottom_panel_transfers_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('`'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.bottom_panel.open);
    app.handle_key(press(KeyCode::Char('`'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn esc_closes_the_focused_terminal_panel_like_ctrl_backtick() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('`'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.bottom_panel.open);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn arrows_switch_tabs_only_in_the_active_navigation_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);

    // The bottom panel is Terminal-only now (Tasks moved to the sidebar),
    // so left/right has nothing to cycle while it's focused — workspace
    // navigation stays exactly where it was.
    app.open_bottom_panel();
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);
}

#[tokio::test]
async fn chat_input_keeps_literal_brackets_and_shift_arrows_do_not_switch_tabs() {
    let (_dir, mut app) = focus_test_app().await;
    app.handle_key(press(KeyCode::Char('['), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "[]");
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.mode(), FocusMode::Navigation);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.input.text, "[]x");
}

#[tokio::test]
async fn esc_from_composer_returns_to_previous_block_and_keeps_draft() {
    let (_dir, mut app) = focus_test_app().await;
    for block in [FocusBlock::Sidebar, FocusBlock::BottomPanel] {
        app.bottom_panel.open = true;
        app.focus_block(block);
        app.enter_chat_composer();
        app.input.set_text("draft");
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block(), block);
        assert_eq!(app.input.text, "draft");
    }
}

#[tokio::test]
async fn type_to_compose_keeps_first_unbound_printable() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.input.text, "x");
}

#[tokio::test]
async fn semantic_key_paths_emit_existing_commands() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        Some(SemanticCommand::ToggleFiles)
    );

    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Right, KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::SubmitMessage)
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::SHIFT)),
        Some(SemanticCommand::InsertComposerNewline)
    );

    app.workspace_files.visible = true;
    assert_eq!(
        app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::OpenSelectedEntry)
    );
}

#[tokio::test]
async fn ctrl_e_then_enter_opens_file_from_explorer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("open_me.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    let path = path.canonicalize().unwrap();
    app.workspace_files.explorer.refresh_workspace();

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Files);
    assert_eq!(app.focus.block(), FocusBlock::Search);

    app.workspace_files.explorer.selected_path = Some(path.clone());
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn semantic_commands_dispatch_without_rendering_a_frame() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.workspace_files.visible = false;
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Files);
    assert_eq!(app.focus.block(), FocusBlock::Search);

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path.clone()))
    );
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(path.canonicalize().unwrap().as_path())
    );
}

#[tokio::test]
async fn semantic_dispatch_handles_invalid_or_stale_identifiers_without_panic() {
    let (_dir, mut app) = focus_test_app().await;
    let missing = PathBuf::from("/definitely/missing/forge-file.rs");

    app.execute_semantic_command(SemanticCommand::SelectEntry(missing.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ToggleDirectory(missing.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(missing))
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn modal_and_transient_precedence_still_wins_over_semantic_bindings() {
    let (dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.workspace_files.visible);
    assert!(app.overlay.is_some());

    app.overlay = None;
    let path = dir.path().join("source.txt");
    fs::write(&path, "alpha\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Search
    );
    assert_eq!(app.editor_session.as_ref().unwrap().search_pattern(), "z");
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn printable_globals_remain_available_to_type_to_compose() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('x'), KeyModifiers::NONE)),
        None
    );

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.input.text, "x");
}

#[tokio::test]
async fn registered_printable_editor_commands_do_not_enter_composer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "a\nb\nc\n").unwrap();
    app.open_file_in_editor(&path);
    app.input.set_text("");

    app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.input.text.is_empty());

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn non_printable_keys_do_not_type_to_compose() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);

    for key in [
        press(KeyCode::Enter, KeyModifiers::NONE),
        press(KeyCode::Left, KeyModifiers::NONE),
        press(KeyCode::Right, KeyModifiers::SHIFT),
        press(KeyCode::Char('c'), KeyModifiers::CONTROL),
        press(KeyCode::Char('x'), KeyModifiers::ALT),
    ] {
        app.handle_key(key).await.unwrap();
        assert_eq!(app.focus.block(), FocusBlock::Workspace);
        assert!(app.input.text.is_empty());
    }
}

#[tokio::test]
async fn overlay_precedes_block_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus.set_navigation(app.focus.block());
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.overlay.is_some());
}

#[tokio::test]
async fn narrow_files_focus_has_a_visible_navigation_surface() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Files);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(app.navigator_list_area.is_some());
    assert_eq!(app.focus.mode(), FocusMode::Navigation);
}

/// The terminal panel is reachable from anywhere via `Ctrl+\``, so help must
/// advertise it from every block. Listing it only under `FocusBlock::BottomPanel`
/// tells you how to close a panel you had no way to discover.
#[tokio::test]
async fn help_advertises_the_terminal_shortcut_from_every_block() {
    let (_dir, mut app) = focus_test_app().await;
    for block in [
        FocusBlock::Workspace,
        FocusBlock::Files,
        FocusBlock::Composer,
        FocusBlock::Footer,
    ] {
        app.focus_block(block);
        let help = app.help_text();
        assert!(
            help.contains("• Ctrl+`  Toggle terminal panel"),
            "help for {block:?} must advertise the terminal shortcut, got:\n{help}"
        );
    }
}

#[tokio::test]
async fn tab_nav_command_recognizes_plain_arrows_only() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Left, KeyModifiers::NONE)),
        Some(TabNavCommand::PreviousTab)
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::NONE)),
        Some(TabNavCommand::NextTab)
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Left, KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::SHIFT)),
        None
    );
}

#[tokio::test]
async fn focus_availability_and_restore_skip_hidden_blocks() {
    let (_dir, mut app) = focus_test_app().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.bottom_panel.open = false;
    let availability = app.focus_availability();
    assert!(availability.contains(FocusBlock::Search));
    assert!(availability.contains(FocusBlock::Files));
    assert!(!availability.contains(FocusBlock::BottomPanel));

    app.focus
        .set_previous_block_for_test(Some(FocusBlock::BottomPanel));
    app.restore_focus_after_closing(FocusBlock::Files);
    // Falls back to the composer, not the workspace. `Workspace` is the modal
    // editor, so defaulting there means the next keystroke edits a file.
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert_eq!(app.focus.return_block(), Some(FocusBlock::Composer));
}
