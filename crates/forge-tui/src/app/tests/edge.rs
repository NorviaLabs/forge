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
    assert!(!app.busy_state.active);
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
    // Regression: a provider/stream error must move the session lifecycle
    // out of `Working`, not just clear the TUI-local `busy` flag — otherwise
    // the header sticks on "Working" forever and the message queue's
    // dispatch gate (which only checks this lifecycle) never reopens.
    assert_eq!(
        app.session.active_task.lifecycle,
        forge_types::TaskLifecycle::Failed
    );
}

// Regression test for the "permanently stuck Working" bug found in the
// 2026-08-01 usability audit: a model/provider request that fails before
// producing any `ModelResponse` (no partial stream content this time, so the
// plain `report_error` display path runs rather than the interrupted-partial
// one exercised above) must still unstick the session — both the lifecycle
// itself and, critically, the ability to send the *next* message immediately
// rather than have it silently join a queue that can never dispatch.
#[tokio::test]
async fn edge_provider_error_unsticks_session_for_the_next_message() {
    let dir = TempDir::new().unwrap();
    let session = session_for_workspace_with_model(
        dir.path(),
        Arc::new(MockModelClient::stream_error(
            vec![],
            "HTTP 400 Bad Request",
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

    app.dispatch_line("first message").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    assert!(!app.busy_state.active);
    assert_eq!(
        app.session.active_task.lifecycle,
        forge_types::TaskLifecycle::Failed,
        "a request that errors before any ModelResponse must still fail the turn, \
         not leave the session stuck on Working"
    );

    // The mock client's scripted error was a one-shot; the next call falls
    // back to a plain successful response. This message must dispatch and
    // complete on its own — it must NOT still be sitting in the queue.
    app.dispatch_line("second message").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    assert!(
        app.session.queue().is_empty(),
        "the second message must have been sent, not queued behind the stuck turn"
    );
    assert_eq!(
        app.session.active_task.lifecycle,
        forge_types::TaskLifecycle::Completed
    );
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

    app.file_watch
        .change_tx
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

    app.file_watch
        .change_tx
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
    app.diff_view.selected = 0;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("two.rs"), GitStatusKind::Added);

    app.note_workspace_changed();
    assert!(app.diff_view.snapshot.stale);
    assert_eq!(app.diff_view.selected, 0);
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
    assert!(!app.diff_view.snapshot.stale);
    assert_eq!(app.diff_view.selected, 0);
}

/// Regression test for F-REVIEW-01/F-REVIEW-02: a raw filesystem-watch event
/// fires (and can even be replayed by the OS well after the write it
/// describes) *before* the async git-status refresh it triggers has landed.
/// Marking the review stale on that raw event alone meant the "stale" banner
/// could resurrect itself faster than a user's `r` (refresh) or Escape could
/// ever resolve it, making the panel feel permanently stuck. Staleness must
/// only be flagged once the changed-path set actually, verifiably differs
/// from what's under review.
#[tokio::test]
async fn edge_diff_stale_marking_waits_for_confirmed_status_change() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("one.rs"), GitStatusKind::Modified);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    assert!(!app.diff_view.snapshot.stale);

    // A raw watch event fires (e.g. unrelated churn, or a replayed historical
    // FSEvent) while `git_status` still holds the same data the review was
    // opened with. This must NOT flag staleness — there is nothing to refresh
    // to yet, so a refresh key press would have no visible effect.
    app.note_workspace_changed();
    assert!(
        !app.diff_view.snapshot.stale,
        "an fs event that doesn't change the known status set must not mark the review stale"
    );

    // Now the underlying status genuinely changes and a fresh event arrives.
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("two.rs"), GitStatusKind::Added);
    app.note_workspace_changed();
    assert!(
        app.diff_view.snapshot.stale,
        "a confirmed change to the tracked path set must mark the review stale"
    );

    // 'r' and 'R' both refresh and clear staleness.
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Char('R'), KeyModifiers::NONE)),
        Some(SemanticCommand::RefreshDiff)
    );
    app.execute_semantic_command(SemanticCommand::RefreshDiff)
        .await
        .unwrap();
    assert!(!app.diff_view.snapshot.stale);

    // Escape always leaves the diff workspace, stale or not.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

/// A git-status refresh resolving asynchronously (via `poll()`) after the
/// review was opened must re-check staleness against the now-current data,
/// even though no raw filesystem-watch event drove this specific tick.
#[tokio::test]
async fn edge_diff_reconciles_staleness_after_async_poll_resolves() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("one.rs"), GitStatusKind::Modified);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    assert!(!app.diff_view.snapshot.stale);

    // Simulate an async git-status refresh landing with genuinely different
    // data, without going through the fs-watch event path at all.
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("two.rs"), GitStatusKind::Added);
    app.reconcile_diff_staleness();
    assert!(app.diff_view.snapshot.stale);
}

/// F-HELP-01: `/help` used to return "unknown command" even though the
/// help overlay itself existed and was reachable via `?` on an empty
/// composer — `/help` just wasn't wired to it, and there was no other
/// visible hint that `?` did anything special.
#[tokio::test]
async fn slash_help_opens_the_help_overlay() {
    let (_dir, mut app) = focus_test_app().await;
    app.dispatch_line("/help").await.unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::Help)),
        "{:?}",
        app.overlay
    );
}

/// F-COMPOSER-01: Ctrl+U (standard readline "clear line") had no binding at
/// all, so a garbled or long composer buffer could only be cleared one
/// character at a time via repeated Backspace.
#[tokio::test]
async fn ctrl_u_clears_the_composer() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("some garbled /model text".to_string());
    assert!(!app.input.text.is_empty());
    app.handle_key(press(KeyCode::Char('u'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.input.text.is_empty(), "{:?}", app.input.text);
}
