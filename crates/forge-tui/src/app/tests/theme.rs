//! Theme persistence and light-palette rendering tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn theme_change_updates_active_palette_immediately() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_MIDNIGHT);
    assert_eq!(
        crate::theme::text().fg,
        Some(crate::theme::palette(forge_config::THEME_FORGE_MIDNIGHT).text)
    );

    app.handle_theme_command(Some("light"));
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_DAYLIGHT);
    assert_eq!(
        crate::theme::text().fg,
        Some(crate::theme::palette(forge_config::THEME_FORGE_DAYLIGHT).text)
    );
    assert!(app.conversation_cache.is_none());
}

#[tokio::test]
async fn theme_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    assert_eq!(app.runtime.theme_id, forge_config::THEME_FORGE_DAYLIGHT);
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_DAYLIGHT);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert_eq!(
        restored.runtime.theme_id,
        forge_config::THEME_FORGE_DAYLIGHT
    );
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_DAYLIGHT);
}

#[tokio::test]
async fn light_theme_paints_root_canvas_on_draw() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.handle_theme_command(Some("light"));
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
    let corner = term.backend().buffer()[(0, 0)].style().bg;
    assert_eq!(
        corner,
        Some(crate::theme::palette(forge_config::THEME_FORGE_DAYLIGHT).canvas)
    );
}

#[tokio::test]
async fn light_theme_resize_keeps_canvas_background() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.handle_theme_command(Some("light"));
    for (w, h) in [(80, 24), (160, 50), (120, 40)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert_buffer_fully_themed(term.backend().buffer());
    }
}

#[tokio::test]
async fn light_theme_representative_layout_snapshot() {
    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.files_visible = true;
    app.handle_theme_command(Some("light"));
    app.session.messages.push(Message {
        role: MessageRole::User,
        content: "Please review this change.\n\nIt spans multiple lines.".into(),
        tool_call_id: None,
        name: None,
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
    });
    app.session.messages.push(Message {
        role: MessageRole::Assistant,
        content: "Here is a concise review of your change.".into(),
        tool_call_id: None,
        name: None,
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
    });
    app.feedback = FeedbackModel::error("Model error: rate limited (HTTP 429).");
    app.conversation_cache = None;
    app.input.set_text("draft reply");
    app.input.cursor = app.input.text.len();

    let text = render_app_text(&mut app, 120, 40);
    assert!(text.contains("Forge"), "missing header:\n{text}");
    assert!(text.contains("FILES"), "missing sidebar:\n{text}");
    assert!(
        text.contains("Please review this change."),
        "missing user message:\n{text}"
    );
    assert!(
        text.contains("concise review"),
        "missing assistant response:\n{text}"
    );
    assert!(
        text.contains("Model error"),
        "missing model error feedback:\n{text}"
    );
    assert!(text.contains("draft reply"), "missing composer:\n{text}");

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
    let buf = term.backend().buffer();
    let mut saw_gutter = false;
    let mut saw_selection = false;
    let light = crate::theme::palette(forge_config::THEME_FORGE_DAYLIGHT);
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            if buf[(x, y)].style().fg == Some(light.user_message_gutter) {
                saw_gutter = true;
            }
            if buf[(x, y)].style().bg == Some(light.selection) {
                saw_selection = true;
            }
        }
    }
    assert!(saw_gutter, "expected light-theme user gutter colour");
    assert!(
        saw_selection || text.contains("draft reply"),
        "expected composer selection or typed text"
    );
}

#[tokio::test]
async fn light_theme_overlay_uses_themed_backdrop() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    app.overlay = Some(Overlay::Help);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
}

#[tokio::test]
async fn old_or_malformed_ui_state_migrates_safely_to_default() {
    let (dir, _app) = focus_test_app().await;
    let state_path = dir.path().join(".forge/ui-state.json");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(&state_path, r#"{"files_visible":true}"#).unwrap();

    let session = session_for_workspace(dir.path()).await;
    let app = TuiApp::new(
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

    assert!(!app.files_visible);
}

/// A theme switch changes the colours baked into each segment, so it *must*
/// recompute. This is the invalidation half of the contract: stale colours
/// after a theme change would be a visible bug.
#[tokio::test]
async fn theme_switch_recomputes_highlights() {
    let (_dir, mut app) = app_with_code("theme").await;
    let _guard = lock_highlight_cache();
    crate::theme::set_active(forge_config::THEME_FORGE_MIDNIGHT);
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    crate::theme::set_active(forge_config::THEME_FORGE_DAYLIGHT);
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    // Restore before asserting so a failure cannot leak a palette into others.
    crate::theme::set_active(forge_config::THEME_FORGE_MIDNIGHT);

    assert!(
        after.misses >= before.misses + CACHED_BLOCKS as u64,
        "a theme switch must recompute every block (misses {} -> {})",
        before.misses,
        after.misses
    );
}
