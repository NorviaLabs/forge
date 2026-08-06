//! Conversation transcript render-cache tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn typing_reuses_cached_conversation_lines() {
    use ratatui::backend::TestBackend;

    let (dir, session) = test_session().await;
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.conversation_view.splash_dismissed = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    Arc::get_mut(&mut app.render_cache.conversation.as_mut().unwrap().lines)
        .expect("cache handle is unshared between frames")
        .reserve(1_000);
    let cached_capacity = app
        .render_cache
        .conversation
        .as_ref()
        .unwrap()
        .lines
        .capacity();

    app.input.insert('x');
    terminal.draw(|frame| app.draw(frame)).unwrap();

    assert_eq!(
        app.render_cache
            .conversation
            .as_ref()
            .unwrap()
            .lines
            .capacity(),
        cached_capacity
    );
}

#[tokio::test]
async fn streaming_updates_reuse_cached_transcript_lines() {
    use ratatui::backend::TestBackend;

    let (dir, mut session) = test_session().await;
    session.messages.push(Message {
        outcome: Default::default(),
        role: MessageRole::Assistant,
        content: "historical answer".into(),
        tool_call_id: None,
        name: None,
        thinking: Some("historical completed thinking".into()),
        thinking_duration_secs: Some(1.0),
        tool_calls: vec![],
    });
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.conversation_view.splash_dismissed = true;
    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Model;
    app.stream.preview = "first chunk".into();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    Arc::get_mut(&mut app.render_cache.conversation.as_mut().unwrap().lines)
        .expect("cache handle is unshared between frames")
        .reserve(1_000);
    let cached_capacity = app
        .render_cache
        .conversation
        .as_ref()
        .unwrap()
        .lines
        .capacity();

    app.stream.preview.push_str(" and updated tail");
    terminal.draw(|frame| app.draw(frame)).unwrap();

    assert_eq!(
        app.render_cache
            .conversation
            .as_ref()
            .unwrap()
            .lines
            .capacity(),
        cached_capacity,
        "stream deltas must not rebuild historical transcript lines"
    );
    // The live preview is rebuilt on a cadence (not per token), so a frame that
    // lands inside the window renders the previous batch. Advance past it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    // The sidebar's narrower column can wrap "updated tail" across two
    // rendered rows — check both words independently rather than the exact
    // contiguous substring.
    assert!(rendered.contains("updated"), "{rendered}");
    assert!(rendered.contains("tail"), "{rendered}");
    assert!(rendered.contains("historical"), "{rendered}");
    assert!(rendered.contains("completed thinking"), "{rendered}");
}

/// A cache hit must share the cached line buffer, not copy it. Pointer
/// identity is a direct check: the previous code deep-copied every `Line` and
/// `Span` on every frame, so the allocation would differ here.
#[tokio::test]
async fn cache_hit_shares_transcript_lines_without_copying() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session.messages.push(forge_types::Message::new(
        forge_types::MessageRole::Assistant,
        "cached transcript body",
    ));
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    // Typing does not change the render key, so this is a cache hit.
    app.input.insert('x');
    draw_app(&mut app, 100, 30);
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    assert!(
        Arc::ptr_eq(&first, &second),
        "a cache hit must reuse the same line allocation, not clone it"
    );
}
