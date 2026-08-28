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
        attachments: Vec::new(),
    });
    let mut app = TuiApp::new(
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
    app.conversation_view.splash_dismissed = true;
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.stream.preview = "first chunk".into();
    // The event loop, not `draw`, lets the preview through; this test drives
    // `draw` directly, so it stands in for the loop.
    app.stream.reveal_everything_for_tests();
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
    app.stream.reveal_everything_for_tests();
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

/// Busy-phase flips used to sit on the conversation render key, so every
/// tool-call start rebuilt the whole transcript. Historical lines do not
/// depend on the current phase — live chrome is the separate preview buffer.
#[tokio::test]
async fn busy_phase_reuses_cached_transcript_lines() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session.messages.push(forge_types::Message::new(
        forge_types::MessageRole::Assistant,
        "cached transcript body",
    ));
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    app.busy_state.activate();
    app.busy_state.set_phase(crate::widgets::BusyPhase::Tool {
        name: "bash".into(),
    });
    draw_app(&mut app, 100, 30);
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    assert!(
        Arc::ptr_eq(&first, &second),
        "a busy-phase change must not rebuild historical transcript lines"
    );
}

/// Streamed reasoning is painted directly below the settled transcript, which
/// is a separate line list the preview never sees — the boundary blank every
/// other major block gets was missing at the seam, so thoughts hugged the row
/// above (usually the last railed tool row). The preview must open with a
/// blank line instead.
#[tokio::test]
async fn streamed_thinking_is_separated_from_the_settled_tool_trail() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.pending_turn.clear();
    app.session.messages.push(Message::new(MessageRole::User, "fix the failing test"));
    app.session.messages.push(Message {
        outcome: forge_types::ExecutionOutcome::Success,
        role: MessageRole::Tool,
        content: "src/lib.rs".into(),
        tool_call_id: Some("call_1".into()),
        name: Some("read_file".into()),
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
        attachments: Vec::new(),
    });
    app.busy_state.activate();
    app.busy_state.set_phase(crate::widgets::BusyPhase::Model);
    app.stream.thinking = "planning the fix".into();
    app.stream.reveal_everything_for_tests();
    let rendered = render_app_text(&mut app, 160, 50);

    let rows: Vec<&str> = rendered.lines().collect();
    let thinking_row = rows
        .iter()
        .position(|row| row.contains("planning the fix"))
        .expect("streamed thinking must be visible");
    assert!(
        thinking_row >= 2,
        "thinking should sit below a settled tool row, got {thinking_row}"
    );
    assert!(
        rows[thinking_row - 2].contains("Explored repository"),
        "the settled tool trail should be right above the seam:\n{}",
        rows[thinking_row - 2]
    );
    assert!(
        rows[thinking_row - 1].trim().is_empty(),
        "a blank line must separate the tool trail from streamed thinking:\n{}\n---\n{}",
        rows[thinking_row - 1],
        rows[thinking_row]
    );
}
