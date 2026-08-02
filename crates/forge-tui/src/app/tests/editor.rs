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
            mouse_capture: true,
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
            mouse_capture: true,
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
            mouse_capture: true,
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
            mouse_capture: true,
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
async fn external_editor_resume_draws_after_terminal_reinit() {
    let (_dir, mut app) = focus_test_app().await;
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
    app.external_editor.requested = true;

    let result = app.resume_after_external_editor(None);
    assert!(result.is_ok());
}
