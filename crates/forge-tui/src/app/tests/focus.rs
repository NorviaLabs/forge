//! Focus, tab order, and semantic keybinding integration tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn focus_starts_on_composer_block() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn tab_cycles_visible_blocks_and_skips_hidden_ones() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.files_visible = true;
    app.bottom_panel.open = true;
    app.normalize_focus();

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Files);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.sidebar_visible = false;
    app.normalize_focus();
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Files);
}

#[tokio::test]
async fn tab_and_shift_tab_reach_composer() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
}

#[tokio::test]
async fn opening_and_closing_bottom_panel_transfers_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.bottom_panel.open);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn shift_arrow_tabs_only_apply_to_the_active_navigation_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );

    app.sidebar_visible = true;
    app.focus_block(FocusBlock::Inspector);
    assert_eq!(app.focus.block, FocusBlock::Inspector);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Context);

    app.open_bottom_panel(None);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
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
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.mode, FocusMode::Navigation);
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.input.text, "[]x");
}

#[tokio::test]
async fn esc_from_composer_returns_to_previous_block_and_keeps_draft() {
    let (_dir, mut app) = focus_test_app().await;
    for block in [
        FocusBlock::Files,
        FocusBlock::Workspace,
        FocusBlock::Inspector,
        FocusBlock::BottomPanel,
    ] {
        app.files_visible = true;
        app.sidebar_visible = true;
        app.bottom_panel.open = true;
        app.focus_block(block);
        app.enter_chat_composer();
        app.input.set_text("draft");
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, block);
        assert_eq!(app.input.text, "draft");
    }
}

#[tokio::test]
async fn type_to_compose_keeps_first_unbound_printable() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.input.text, "x");
}

#[tokio::test]
async fn semantic_key_paths_emit_existing_commands() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        Some(SemanticCommand::ToggleFiles)
    );
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        Some(SemanticCommand::OpenGlobalCommandPalette)
    );

    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::SHIFT)),
        Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::SubmitMessage)
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::SHIFT)),
        Some(SemanticCommand::InsertComposerNewline)
    );

    app.files_visible = true;
    assert_eq!(
        app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::OpenSelectedEntry)
    );
}

#[tokio::test]
async fn semantic_commands_dispatch_without_rendering_a_frame() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert!(app.files_visible);
    assert_eq!(app.focus.block, FocusBlock::Files);

    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(path.clone())
    );
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(path.canonicalize().unwrap().as_path())
    );

    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Run);
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
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(
        "missing-run".into(),
    )))
    .await
    .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn modal_and_transient_precedence_still_wins_over_semantic_bindings() {
    let (dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.files_visible);
    assert!(app.overlay.is_some());

    app.overlay = None;
    let path = dir.path().join("source.txt");
    fs::write(&path, "alpha\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
    assert_eq!(app.source_viewer.search.query, "z");
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

    assert_eq!(app.focus.block, FocusBlock::Composer);
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

    assert_eq!(app.status_message, "Refreshing git status...");
}

#[tokio::test]
async fn switching_to_diff_focuses_workspace_for_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.file_explorer
        .git_status
        .status
        .insert(std::path::PathBuf::from("a.txt"), GitStatusKind::Modified);
    app.file_explorer
        .git_status
        .status
        .insert(std::path::PathBuf::from("b.txt"), GitStatusKind::Modified);

    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.diff_selected, 1);
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
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(app.input.text.is_empty());

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
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
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert!(app.input.text.is_empty());
    }
}

#[tokio::test]
async fn overlay_precedes_block_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus.mode = FocusMode::Navigation;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.overlay.is_some());
}

#[tokio::test]
async fn resize_drops_focus_from_a_zero_width_files_block() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn helper_labels_reflect_focus_mode() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(app.help_text().contains("Conversation"));
    app.workspace_navigation
        .replace_view(WorkspaceView::Diff(DiffCommandContext::Current));
    assert!(app.help_text().contains("Review changes"));
}

#[tokio::test]
async fn tab_nav_command_recognizes_shifted_plain_arrows_only() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Left, KeyModifiers::SHIFT)),
        Some(TabNavCommand::PreviousTab)
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::SHIFT)),
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
}

#[tokio::test]
async fn focus_availability_and_restore_skip_hidden_blocks() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.sidebar_visible = false;
    app.bottom_panel.open = false;
    let availability = app.focus_availability();
    assert!(availability.contains(FocusBlock::Files));
    assert!(!availability.contains(FocusBlock::Inspector));
    assert!(!availability.contains(FocusBlock::BottomPanel));

    app.focus.previous_block = Some(FocusBlock::Inspector);
    app.restore_focus_after_closing(FocusBlock::Files);
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.focus.return_block, Some(FocusBlock::Workspace));
}

#[tokio::test]
async fn contextual_hint_appears_only_for_transient_or_blocking_state() {
    let (_dir, mut app) = focus_test_app().await;
    assert!(app.contextual_hint().is_none());

    app.focus_block(FocusBlock::Workspace);
    assert!(app.contextual_hint().is_none());

    app.focus.mode = FocusMode::Transient(TransientOwner::SourceSearch);
    assert!(app
        .contextual_hint()
        .is_some_and(|hint| hint.contains("Esc cancel")));

    app.focus.mode = FocusMode::Navigation;
    app.overlay = Some(Overlay::turn_limit(4));
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("Enter confirm · Esc cancel")
    );
}
