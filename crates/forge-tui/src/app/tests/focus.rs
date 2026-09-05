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
async fn tab_cycles_visible_blocks_and_skips_hidden_ones() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.workspace_files.visible = true;
    app.bottom_panel.open = true;
    app.normalize_focus();

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Footer);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_focused);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(!app.workspace_files.explorer.search_focused);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);

    app.normalize_focus();
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(!app.workspace_files.explorer.search_focused);
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_focused);
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
    app.focus_block(FocusBlock::Workspace);

    // Sidebar sits between Workspace and Composer — they're the same
    // physical column post-sidebar layout (composer docked below it).
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Composer);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
}

#[tokio::test]
async fn open_modal_suppresses_background_marker_and_close_restores_it() {
    // DESIGN-004: exactly one effective keyboard owner is visible. While a
    // modal is open the background pane loses its `>` marker (paint only —
    // `FocusState` is untouched); closing the modal brings the marker back
    // on the still-valid owner.
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel();
    app.focus_block(FocusBlock::BottomPanel);

    let plain = render_app_text(&mut app, 120, 40);
    assert!(plain.contains("> Terminal"), "{plain}");
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);

    app.overlay = Some(Overlay::welcome());
    let modal = render_app_text(&mut app, 120, 40);
    assert!(
        !modal.contains("> Terminal"),
        "background marker must suppress under a modal:\n{modal}"
    );
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);

    app.overlay = None;
    let restored = render_app_text(&mut app, 120, 40);
    assert!(restored.contains("> Terminal"), "{restored}");
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
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
    for block in [
        FocusBlock::Files,
        FocusBlock::Workspace,
        FocusBlock::BottomPanel,
    ] {
        app.workspace_files.visible = true;
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
    app.workspace_files.visible = false;
    app.workspace_files.explorer.refresh_workspace();

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.workspace_files.visible);
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
    assert!(app.workspace_files.visible);
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
async fn global_palette_selection_uses_semantic_dispatch() {
    let (_dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::DispatchSlash {
        origin: SlashCommandOrigin::GlobalPalette,
        line: "/refresh".into(),
    })
    .await
    .unwrap();

    assert_eq!(app.status_state.message, "Refreshing git status...");
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
async fn resize_drops_focus_from_a_zero_width_files_block() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
    assert_eq!(app.focus.mode(), FocusMode::Navigation);
}

#[tokio::test]
async fn helper_labels_reflect_focus_mode() {
    let (_dir, session) = test_session().await;
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(app.help_text().contains("No file open"));
    assert!(!app.help_text().contains("Review changes"));
    assert!(!app.help_text().contains("Alt+→"));
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
    app.workspace_files.visible = true;
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

#[tokio::test]
async fn contextual_hint_appears_only_for_transient_or_blocking_state() {
    let (_dir, mut app) = focus_test_app().await;
    assert!(app.contextual_hint().is_none());

    app.focus_block(FocusBlock::Workspace);
    assert!(app.contextual_hint().is_none());

    app.focus.set_transient(TransientOwner::SourceSearch);
    assert!(app
        .contextual_hint()
        .is_some_and(|hint| hint.contains("Esc cancel")));

    app.focus.set_navigation(app.focus.block());
    app.overlay = Some(Overlay::turn_limit(4));
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("Enter confirm · Esc cancel")
    );
}

#[tokio::test]
async fn contextual_hint_omits_queue_information_while_waiting() {
    let (_dir, mut app) = focus_test_app().await;
    app.session.enqueue_task("next task").await.unwrap();
    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Waiting;
    app.session.active_task.wait_reason = Some(forge_types::WaitReason::Approval {
        request_id: "request".into(),
        payload: forge_types::HitlPayload {
            call_id: "request".into(),
            tool: "tool".into(),
            args_redacted: serde_json::json!({"command": "command"}),
            reason: "test approval".into(),
            failure: None,
            sandbox_escalation: false,
            denied_host: None,
        },
    });
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("Waiting for approval")
    );
}

#[tokio::test]
async fn footer_focus_hint_is_relevant_to_the_selected_chip() {
    // Chips stay visible when the footer is focused; the hint names the
    // action of the currently selected chip, and follows focus.
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));
    let llm_hint = app.contextual_hint().expect("footer focus should hint");
    assert!(llm_hint.contains("Hit Enter ⏎ to open model"), "{llm_hint}");

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    let effort_hint = app.contextual_hint().expect("footer focus should hint");
    assert!(
        effort_hint.contains("Hit Enter ⏎ to change effort"),
        "{effort_hint}"
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    let wrap_hint = app.contextual_hint().expect("footer focus should hint");
    assert!(
        wrap_hint.contains("Hit Enter ⏎ to open model"),
        "two chips wrap back to model: {wrap_hint}"
    );
}

#[tokio::test]
async fn navigation_hints_match_the_active_chrome_surface() {
    let (_dir, mut app) = focus_test_app().await;

    app.focus_block(FocusBlock::TaskStrip);
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("←→ select · Enter switch · s stop · c continue · p pin · x archive")
    );

    app.focus_block(FocusBlock::Files);
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("↑↓ navigate · Enter open · ⇧Tab search · Esc cancel")
    );

    app.focus_block(FocusBlock::Search);
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("Type to filter · Enter open · Esc cancel")
    );
}
