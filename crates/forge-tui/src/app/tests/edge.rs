//! Edge-case recovery flows (network, diff staleness, file externals).
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn edge_network_stream_interruption_preserves_partial_response() {
    let dir = TempDir::new().unwrap();
    let session = session_for_workspace_with_model(
        dir.path(),
        Arc::new(MockModelClient::stream_error(
            vec!["partial ".into(), "answer".into()],
            "network connection lost",
        )),
    )
    .await;
    let mut app = TuiApp::new(
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
    let file = dir.path().join("open.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(file))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.dispatch_line("hello").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();

    assert_eq!(app.workspace_navigation, before);
    assert!(!app.busy);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "x");
    assert!(app.feedback.text.contains("Retry or Continue"));
    assert!(app.session.messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.content.contains("partial answer")
            && message.content.contains("Interrupted")
    }));
}

#[tokio::test]
async fn edge_open_file_external_rename_updates_path_when_identity_matches() {
    let (dir, mut app) = focus_test_app().await;
    let old = dir.path().join("old.rs");
    let new = dir.path().join("new.rs");
    fs::write(&old, "fn main() {}\nline2\nline3\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(old.clone()))
        .await
        .unwrap();
    app.source_viewer.current_line = 2;
    app.source_viewer.top_line = 1;
    fs::rename(&old, &new).unwrap();

    app.file_change_tx
        .send(FileChangeEvent { path: new.clone() })
        .unwrap();
    app.poll_file_changes();

    let new = new.canonicalize().unwrap();
    assert_eq!(app.source_viewer.path.as_deref(), Some(new.as_path()));
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(new));
    assert_eq!(app.source_viewer.current_line, 2);
    assert_eq!(app.source_viewer.top_line, 1);
    assert_eq!(
        app.source_viewer.notice.as_deref(),
        Some("File renamed externally")
    );
}

#[tokio::test]
async fn edge_open_file_external_delete_keeps_file_view_and_buffer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("gone.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let opened = app.source_viewer.path.clone().unwrap();
    let lines = app.source_viewer.lines.clone();
    fs::remove_file(&path).unwrap();

    app.file_change_tx
        .send(FileChangeEvent {
            path: opened.clone(),
        })
        .unwrap();
    app.poll_file_changes();

    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert_eq!(app.source_viewer.path.as_deref(), Some(opened.as_path()));
    assert_eq!(
        app.source_viewer.status,
        crate::source_viewer::ViewerStatus::NotFound
    );
    assert_eq!(app.source_viewer.lines, lines);
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("File no longer exists"), "{rendered}");
    assert!(rendered.contains("Back"), "{rendered}");
    assert!(rendered.contains("Locate"), "{rendered}");
}

#[tokio::test]
async fn edge_diff_becomes_stale_and_refresh_clears_it() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("one.rs"), GitStatusKind::Modified);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.diff_selected = 0;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("two.rs"), GitStatusKind::Added);

    app.note_workspace_changed();
    assert!(app.diff_snapshot.stale);
    assert_eq!(app.diff_selected, 0);
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Stale review"), "{rendered}");
    assert!(
        rendered.contains("Apply disabled until refresh"),
        "{rendered}"
    );
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Char('r'), KeyModifiers::NONE)),
        Some(SemanticCommand::RefreshDiff)
    );

    app.execute_semantic_command(SemanticCommand::RefreshDiff)
        .await
        .unwrap();
    assert!(!app.diff_snapshot.stale);
    assert_eq!(app.diff_selected, 0);
}
