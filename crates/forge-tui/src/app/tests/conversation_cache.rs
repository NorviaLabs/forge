//! Conversation transcript render-cache tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn selection_copies_actual_transcript_cells_with_bottom_padding_and_unicode() {
    use crate::selection::{Cell, CopyPane};
    use ratatui::backend::TestBackend;
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime.messages.push(Message::new(
        MessageRole::User,
        (1..=80)
            .map(|i| format!("ROW{i:02}: A界BC🙂D e\u{301}Z"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let area = app.conversation_area.unwrap();
    let buf = terminal.backend().buffer();
    let (row, col, expected) = (area.y..area.bottom())
        .find_map(|row| {
            (area.x..area.right().saturating_sub(6)).find_map(|col| {
                let text = (0..6)
                    .map(|dx| buf[(col + dx, row)].symbol())
                    .collect::<String>();
                (text.starts_with("ROW") && text.ends_with(':')).then_some((row, col, text))
            })
        })
        .expect("numbered rows must be visible");
    app.selection
        .start_in(CopyPane::Conversation, Cell { row, col });
    app.selection.update(Cell { row, col: col + 5 });
    assert_eq!(app.conversation_selection_text(area), expected);
    let bc = (col..area.right().saturating_sub(1))
        .find(|x| buf[(*x, row)].symbol() == "B" && buf[(*x + 1, row)].symbol() == "C")
        .unwrap();
    app.selection
        .start_in(CopyPane::Conversation, Cell { row, col: bc });
    app.selection.update(Cell { row, col: bc + 1 });
    assert_eq!(app.conversation_selection_text(area), "BC");
    app.selection.finish("BC".into());
    app.scroll_conversation_up(3);
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(
        !app.selection.is_active(),
        "scroll must clear frozen screen selection"
    );
}

#[tokio::test]
async fn selection_copies_live_tail_and_invalidates_on_content_and_resize() {
    use crate::selection::{Cell, CopyPane};
    use ratatui::backend::TestBackend;
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "history"));
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.timing.started = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    app.timing.turn_started = app.timing.started;
    app.stream.preview = "LIVE ABCDEF".into();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let area = app.conversation_area.unwrap();
    let buf = terminal.backend().buffer();
    let (status_row, status_col) = (area.y..area.bottom())
        .find_map(|row| {
            (area.x..area.right().saturating_sub(2)).find_map(|col| {
                ((0..3)
                    .map(|dx| buf[(col + dx, row)].symbol())
                    .collect::<String>()
                    == "esc")
                    .then_some((row, col))
            })
        })
        .expect("busy status row must be painted after debounce");
    app.selection.start_in(
        CopyPane::Conversation,
        Cell {
            row: status_row,
            col: status_col,
        },
    );
    app.selection.update(Cell {
        row: status_row,
        col: status_col + 2,
    });
    assert_eq!(
        app.conversation_selection_text(area),
        "esc",
        "status row shares copy layout"
    );
    let (row, col) = (area.y..area.bottom())
        .find_map(|row| {
            (area.x..area.right().saturating_sub(5)).find_map(|col| {
                ((0..6)
                    .map(|dx| buf[(col + dx, row)].symbol())
                    .collect::<String>()
                    == "ABCDEF")
                    .then_some((row, col))
            })
        })
        .expect("live tail must be rendered");
    app.selection
        .start_in(CopyPane::Conversation, Cell { row, col });
    app.selection.update(Cell { row, col: col + 5 });
    assert_eq!(app.conversation_selection_text(area), "ABCDEF");
    app.selection.finish("ABCDEF".into());
    app.stream.preview.push_str(" changed");
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(
        !app.selection.is_active(),
        "content changes invalidate finished selection"
    );
    app.selection
        .start_in(CopyPane::Conversation, Cell { row, col });
    app.selection.update(Cell { row, col: col + 5 });
    draw_app(&mut app, 100, 30);
    assert!(
        !app.selection.is_active(),
        "resize invalidates active selection too"
    );
}

#[tokio::test]
async fn rendered_transcript_drag_keeps_visited_rows_when_autoscrolling() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime.messages.push(Message::new(
        MessageRole::User,
        (1..=80)
            .map(|i| format!("ROW{i:02}: text"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let area = app.conversation_area.unwrap();
    let row = area.y + 4;
    let buf = terminal.backend().buffer();
    let col = (area.x..area.right())
        .find(|col| buf[(*col, row)].symbol() == "R")
        .unwrap();
    let anchor_text = (0..6)
        .map(|dx| buf[(col + dx, row)].symbol())
        .collect::<String>();
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col + 5,
        row,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: col,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(app.selection.is_dragging());
    let text = app.conversation_selection_text(area);
    assert!(
        text.contains(&anchor_text),
        "drag lost its original rendered anchor: {text:?}"
    );
    assert!(
        text.lines().count() > 4,
        "drag must extend into scrolled history"
    );
}

#[tokio::test]
async fn presentation_misses_reuse_projection_and_content_changes_match_full_projection() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    for index in 0..500 {
        app.session_runtime
            .messages
            .push(Message::new(MessageRole::User, format!("question {index}")));
        app.session_runtime
            .messages
            .push(Message::new(MessageRole::Assistant, "answer".repeat(100)));
    }
    draw_app(&mut app, 100, 30);
    let cached = &mut app.render_cache.projection.as_mut().unwrap().model;
    cached.items.reserve(10_000);
    let allocation = cached.items.as_ptr();
    let text_allocation = match &cached.items[1] {
        crate::conversation::ChatItem::Assistant { text } => text.as_ptr(),
        item => panic!("unexpected item: {item:?}"),
    };
    for width in [80, 120, 100] {
        app.render_cache.conversation = None;
        draw_app(&mut app, width, 30);
        let cached = &app.render_cache.projection.as_ref().unwrap().model;
        assert_eq!(allocation, cached.items.as_ptr());
        assert!(
            matches!(&cached.items[1], crate::conversation::ChatItem::Assistant { text } if text.as_ptr() == text_allocation)
        );
    }
    for change in 0..4 {
        match change {
            0 => app
                .session_runtime
                .messages
                .push(Message::new(MessageRole::Assistant, "appended")),
            1 => app.session_runtime.messages.last_mut().unwrap().content = "replaced".into(),
            2 => app.conversation_view.message_start = 4,
            _ => app.session_runtime.messages.clear(),
        }
        draw_app(&mut app, 100, 30);
        let cached = &app.render_cache.projection.as_ref().unwrap().model;
        let messages = app.transcript_view.messages();
        let start = app.conversation_view.message_start.min(messages.len());
        let full = crate::conversation::ConversationModel::from_messages(
            &messages[start..],
            &[],
            app.session_view.lifecycle,
            cached.opts.clone(),
        );
        assert_eq!(cached.items, full.items);
    }
}

fn numbered_lines(count: usize) -> String {
    (0..count)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::test]
async fn typing_reuses_cached_conversation_lines() {
    use ratatui::backend::TestBackend;

    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            reduced_motion: false,
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
            reduced_motion: false,
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
    app.session_runtime.messages.push(forge_types::Message::new(
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

#[tokio::test]
async fn scrolling_within_a_render_bucket_reuses_transcript_lines() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "show the history"));
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::Assistant, numbered_lines(400)));

    draw_app(&mut app, 120, 40);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);
    let first_key = app
        .render_cache
        .conversation
        .as_ref()
        .unwrap()
        .key
        .keep_from_end;
    assert!(!app.render_cache.conversation.as_ref().unwrap().complete);

    app.conversation_view.follow = false;
    app.conversation_view.scroll = 1;
    draw_app(&mut app, 120, 40);
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    assert_eq!(
        app.render_cache
            .conversation
            .as_ref()
            .unwrap()
            .key
            .keep_from_end,
        first_key,
        "one-row scroll must stay inside its render bucket"
    );
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn scrolling_past_a_render_bucket_rebuilds_transcript_lines() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "show the history"));
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::Assistant, numbered_lines(400)));

    draw_app(&mut app, 120, 40);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);
    let first_key = app
        .render_cache
        .conversation
        .as_ref()
        .unwrap()
        .key
        .keep_from_end;

    app.conversation_view.follow = false;
    app.conversation_view.scroll = 64;
    draw_app(&mut app, 120, 40);
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);
    let second_key = app
        .render_cache
        .conversation
        .as_ref()
        .unwrap()
        .key
        .keep_from_end;

    assert_ne!(first_key, second_key);
    assert!(!Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn complete_transcript_clamps_overscroll_and_allows_downward_scroll() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "show the history"));
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::Assistant, numbered_lines(50)));

    draw_app(&mut app, 120, 40);
    assert!(app.render_cache.conversation.as_ref().unwrap().complete);

    app.conversation_view.follow = false;
    app.conversation_view.scroll = u16::MAX;
    draw_app(&mut app, 120, 40);
    let top = app.conversation_view.scroll;
    assert!(
        top > 0,
        "clamped scroll should reach non-zero history: top={top}, lines={}, area={:?}",
        app.render_cache.conversation.as_ref().unwrap().lines.len(),
        app.conversation_area
    );
    assert!(top < u16::MAX);

    app.scroll_conversation_up(1);
    draw_app(&mut app, 120, 40);
    assert_eq!(app.conversation_view.scroll, top);

    app.scroll_conversation_down(1);
    draw_app(&mut app, 120, 40);
    assert_eq!(app.conversation_view.scroll, top - 1);
}

#[tokio::test]
async fn same_length_message_changes_invalidate_transcript_cache() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime.messages.push(forge_types::Message::new(
        forge_types::MessageRole::Assistant,
        "old text",
    ));
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    app.session_runtime.messages.last_mut().unwrap().content = "new text".into();
    draw_app(&mut app, 100, 30);
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    assert!(!Arc::ptr_eq(&first, &second));
    let projection = &app.render_cache.projection.as_ref().unwrap().model;
    assert!(projection.items.iter().any(|item| matches!(item, crate::conversation::ChatItem::Assistant { text } if text == "new text")));
}

/// Busy-phase flips used to sit on the conversation render key, so every
/// tool-call start rebuilt the whole transcript. Historical lines do not
/// depend on the current phase — live chrome is the separate preview buffer.
#[tokio::test]
async fn busy_phase_reuses_cached_transcript_lines() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime.messages.push(forge_types::Message::new(
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

/// The throbber tick drives the footer running dot, and used to sit on the
/// conversation render key as the plan-marker pulse. Every other tick
/// therefore rebuilt the whole transcript — an O(transcript) synchronous
/// walk on the UI thread every ~400ms while any turn ran, which froze the
/// TUI on large sessions. The settled transcript does not depend on the
/// animation phase any more than it depends on the busy phase; the pulse is
/// baked into the cached lines and the live chrome animates separately.
#[tokio::test]
async fn throbber_pulse_reuses_cached_transcript_lines() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session_runtime.messages.push(forge_types::Message::new(
        forge_types::MessageRole::Assistant,
        "cached transcript body",
    ));
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    app.busy_state.activate();
    app.busy_state.set_phase(crate::widgets::BusyPhase::Model);
    for _ in 0..8 {
        app.busy_state.tick();
        draw_app(&mut app, 100, 30);
    }
    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    assert!(
        Arc::ptr_eq(&first, &second),
        "a throbber tick must not rebuild historical transcript lines"
    );
}
