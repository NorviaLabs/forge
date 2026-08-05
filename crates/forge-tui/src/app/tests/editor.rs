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
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(!app.external_editor.requested);
    let path = app.session.workspace_root().join("fake.txt");
    fs::write(&path, "hello").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.external_editor.requested);
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
            validation_command: None,
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
            validation_command: None,
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
            validation_command: None,
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
async fn source_viewer_i_enters_insert_mode_without_changing_navigation() {
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
        app.source_viewer.mode,
        crate::source_viewer::ViewerMode::Insert
    );
    assert_eq!(app.source_viewer.current_line, before_line);
    assert_eq!(app.source_viewer.lines, vec!["one", "two", "three"]);

    // Navigation keys behave identically in both modes — INSERT doesn't gate them.
    app.handle_key(press(KeyCode::Char('j'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.source_viewer.current_line, before_line + 1);
}

#[tokio::test]
async fn source_viewer_esc_exits_insert_before_navigating_back() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "one\ntwo\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.source_viewer.enter_insert_mode();

    // First Esc: drops back to NORMAL, stays on the file view.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.source_viewer.mode,
        crate::source_viewer::ViewerMode::Normal
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
