//! External editor handoff and precondition tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn external_editor_keybind_sets_flag() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(!app.external_editor.requested);
    let path = app.session.workspace_root().join("fake.txt");
    fs::write(&path, "hello").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.external_editor.requested);
}

#[tokio::test]
async fn edtui_editor_keeps_plain_e_and_uses_alt_e_for_external_editor() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let path = app.session.workspace_root().join("fake.txt");
    fs::write(&path, "hello").unwrap();
    app.open_file_in_editor(&path);
    app.editor_session = Some(crate::editor_session::EditorSession::new("hello"));

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.external_editor.requested);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.external_editor.requested);
}

#[tokio::test]
async fn save_editor_writes_atomically_and_clears_dirty_state() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("save.txt");
    fs::write(&path, "before\r\n").unwrap();
    app.open_file_in_editor(&path);
    app.editor_session = Some(crate::editor_session::EditorSession::new("after\r\n"));
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    // The session is deliberately made dirty through its public input path;
    // the save command must serialize CRLF and accept only after replacement.
    assert!(app.editor_session.as_ref().unwrap().is_dirty());

    app.execute_semantic_command(SemanticCommand::SaveEditor)
        .await
        .unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"xafter\r\n");
    assert!(!app.editor_session.as_ref().unwrap().is_dirty());
}

#[tokio::test]
async fn dirty_editor_exit_requires_an_explicit_discard_or_save_choice() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty.txt");
    fs::write(&path, "before").unwrap();
    app.open_file_in_editor(&path);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));

    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current,
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.current_workspace_is_file());
    assert_eq!(fs::read_to_string(&path).unwrap(), "before");
}

#[tokio::test]
async fn vim_command_line_q_bang_discards_and_exits() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("command.txt");
    fs::write(&path, "before").unwrap();
    app.open_file_in_editor(&path);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));

    for key in [
        press(KeyCode::Char(':'), KeyModifiers::NONE),
        press(KeyCode::Char('q'), KeyModifiers::NONE),
        press(KeyCode::Char('!'), KeyModifiers::SHIFT),
        press(KeyCode::Enter, KeyModifiers::NONE),
    ] {
        app.handle_key(key).await.unwrap();
    }

    assert!(!app.current_workspace_is_file());
    assert_eq!(fs::read_to_string(&path).unwrap(), "before");
}

#[tokio::test]
async fn dirty_editor_file_switch_requires_an_explicit_choice() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    app.open_file_in_editor(&first);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));

    app.open_file_in_editor(&second);
    assert!(matches!(
        app.explorer_dialog.current,
        Some(ExplorerDialog::DirtySwitch { .. })
    ));
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(first.canonicalize().unwrap().as_path())
    );

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(second.canonicalize().unwrap().as_path())
    );
    assert_eq!(fs::read_to_string(&first).unwrap(), "first");
}

#[tokio::test]
async fn vim_command_line_e_opens_a_workspace_file() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    fs::write(&first, "first").unwrap();
    fs::write(&second, "second").unwrap();
    app.open_file_in_editor(&first);

    for key in [
        press(KeyCode::Char(':'), KeyModifiers::NONE),
        press(KeyCode::Char('e'), KeyModifiers::NONE),
        press(KeyCode::Char(' '), KeyModifiers::NONE),
        press(KeyCode::Char('s'), KeyModifiers::NONE),
        press(KeyCode::Char('e'), KeyModifiers::NONE),
        press(KeyCode::Char('c'), KeyModifiers::NONE),
        press(KeyCode::Char('o'), KeyModifiers::NONE),
        press(KeyCode::Char('n'), KeyModifiers::NONE),
        press(KeyCode::Char('d'), KeyModifiers::NONE),
        press(KeyCode::Char('.'), KeyModifiers::NONE),
        press(KeyCode::Char('t'), KeyModifiers::NONE),
        press(KeyCode::Char('x'), KeyModifiers::NONE),
        press(KeyCode::Char('t'), KeyModifiers::NONE),
        press(KeyCode::Enter, KeyModifiers::NONE),
    ] {
        app.handle_key(key).await.unwrap();
    }

    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(second.canonicalize().unwrap().as_path())
    );
}

#[tokio::test]
async fn save_conflict_requires_explicit_force_or_reload_choice() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("conflict.txt");
    fs::write(&path, "before").unwrap();
    app.open_file_in_editor(&path);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    fs::write(&path, "outside").unwrap();

    app.execute_semantic_command(SemanticCommand::SaveEditor)
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current,
        Some(ExplorerDialog::SaveConflict)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "outside");

    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.explorer_dialog.current.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "xbefore");
}

#[tokio::test]
async fn external_editor_preconditions_no_file() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.external_editor.requested = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because no file is open.
    assert!(!app.external_editor.requested);
}

#[tokio::test]
async fn external_editor_preconditions_binary_file() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Binary;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.bin"));
    app.external_editor.requested = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because binary.
    assert!(!app.external_editor.requested);
}

#[tokio::test]
async fn external_editor_rejects_during_tool_execution() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.busy_state.phase = BusyPhase::Tool {
        name: "write".into(),
    };
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
    app.external_editor.requested = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because tool is active.
    assert!(!app.external_editor.requested);
}

#[tokio::test]
async fn source_viewer_mode_defaults_to_normal() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(
        app.source_viewer.mode,
        crate::source_viewer::ViewerMode::Normal
    );
}

#[tokio::test]
async fn edtui_editor_i_enters_insert_mode() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before_line = app.source_viewer.current_line;

    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Insert
    );
    assert_eq!(app.source_viewer.current_line, before_line);
    assert_eq!(app.source_viewer.lines, vec!["one", "two", "three"]);
}

#[tokio::test]
async fn source_viewer_esc_exits_insert_before_navigating_back() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "one\ntwo\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();

    // First Esc: drops back to NORMAL, stays on the file view.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Normal
    );
    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::File(path))
    );

    // Second Esc: now in NORMAL, so it navigates back as before.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(
        app.workspace_navigation.current,
        Some(WorkspaceView::File(
            dir.path().join("main.rs").canonicalize().unwrap()
        ))
    );
}

#[tokio::test]
async fn source_viewer_reopening_a_file_resets_mode_to_normal() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "one\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.source_viewer.enter_insert_mode();

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    assert_eq!(
        app.source_viewer.mode,
        crate::source_viewer::ViewerMode::Normal
    );
}

#[tokio::test]
async fn external_editor_resume_draws_after_terminal_reinit() {
    let (_dir, mut app) = focus_test_app().await;
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
    app.external_editor.requested = true;

    let result = app.resume_after_external_editor(None);
    assert!(result.is_ok());
}
