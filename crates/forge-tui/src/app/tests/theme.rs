//! Theme persistence and light-palette rendering tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;
use crate::overlays::{handle_overlay_key, Key as OverlayKey, OverlayAction};

#[tokio::test]
async fn theme_change_updates_active_palette_immediately() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(crate::theme::active(), forge_config::THEME_SOLARIZED_DARK);
    assert_eq!(
        crate::theme::text().fg,
        Some(crate::theme::palette(forge_config::THEME_SOLARIZED_DARK).text)
    );

    app.handle_theme_command(Some("light"));
    assert_eq!(crate::theme::active(), forge_config::THEME_SOLARIZED_LIGHT);
    assert_eq!(
        crate::theme::text().fg,
        Some(crate::theme::palette(forge_config::THEME_SOLARIZED_LIGHT).text)
    );
    assert!(app.render_cache.conversation.is_none());
}

#[tokio::test]
async fn theme_picker_previews_on_navigate_confirms_on_enter_restores_on_esc() {
    let (_dir, mut app) = focus_test_app().await;
    let original = forge_config::THEME_SOLARIZED_DARK.to_string();
    assert_eq!(crate::theme::active(), original);

    app.handle_theme_command(None);
    assert!(matches!(app.overlay, Some(Overlay::Theme { .. })));

    let light = forge_config::THEME_SOLARIZED_LIGHT.to_string();
    app.apply_overlay_action(OverlayAction::PreviewTheme(light.clone()))
        .await
        .unwrap();
    assert_eq!(crate::theme::active(), light);
    assert_eq!(app.runtime.theme_id, light);
    assert!(
        matches!(app.overlay, Some(Overlay::Theme { .. })),
        "preview must keep the picker open"
    );
    assert!(
        app.feedback.is_empty(),
        "preview must stay silent (no status/toast)"
    );

    // Esc restores the theme from open and closes without persisting.
    app.apply_overlay_action(OverlayAction::Close)
        .await
        .unwrap();
    assert!(app.overlay.is_none());
    assert_eq!(crate::theme::active(), original);
    assert_eq!(app.runtime.theme_id, original);

    // Confirm persists and closes.
    app.handle_theme_command(None);
    app.apply_overlay_action(OverlayAction::SelectTheme(light.clone()))
        .await
        .unwrap();
    assert!(app.overlay.is_none());
    assert_eq!(crate::theme::active(), light);
    assert_eq!(app.runtime.theme_id, light);
    assert!(
        !app.feedback.is_empty(),
        "confirm should acknowledge the theme change"
    );
}

#[tokio::test]
async fn theme_picker_dock_keeps_conversation_visible() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.session
        .messages
        .push(Message::new(MessageRole::User, "visible under theme dock"));
    app.handle_theme_command(None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let buf = term.backend().buffer();
    let mut text = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("Theme · ↑↓ preview"),
        "expected theme dock:\n{text}"
    );
    assert!(
        text.contains("visible under theme dock"),
        "conversation must stay visible behind the dock:\n{text}"
    );
}

#[tokio::test]
async fn theme_picker_down_key_emits_preview_action() {
    let (_dir, mut app) = focus_test_app().await;
    app.handle_theme_command(None);
    let overlay = app.overlay.as_mut().expect("theme overlay");
    let action = handle_overlay_key(overlay, OverlayKey::Down);
    assert!(
        matches!(action, OverlayAction::PreviewTheme(_)),
        "↓ should live-preview, got {action:?}"
    );
}

#[tokio::test]
async fn theme_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    assert_eq!(app.runtime.theme_id, forge_config::THEME_SOLARIZED_LIGHT);
    assert_eq!(crate::theme::active(), forge_config::THEME_SOLARIZED_LIGHT);

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
        forge_config::THEME_SOLARIZED_LIGHT
    );
    assert_eq!(crate::theme::active(), forge_config::THEME_SOLARIZED_LIGHT);
}

#[tokio::test]
async fn light_theme_paints_root_canvas_on_draw() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    app.handle_theme_command(Some("light"));
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
    let corner = term.backend().buffer()[(0, 0)].style().bg;
    assert_eq!(
        corner,
        Some(crate::theme::palette(forge_config::THEME_SOLARIZED_LIGHT).canvas)
    );
}

#[tokio::test]
async fn light_theme_resize_keeps_canvas_background() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
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
    app.conversation_view.splash_dismissed = true;
    app.workspace_files.visible = true;
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
    app.render_cache.conversation = None;
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
    let mut saw_active_gutter = false;
    let mut saw_selection = false;
    let light = crate::theme::palette(forge_config::THEME_SOLARIZED_LIGHT);
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            if buf[(x, y)].style().fg == Some(light.user_gutter_active) {
                saw_active_gutter = true;
            }
            if buf[(x, y)].style().bg == Some(light.selection) {
                saw_selection = true;
            }
        }
    }
    assert!(
        saw_active_gutter,
        "expected light-theme active gutter colour"
    );
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

    assert!(app.workspace_files.visible);
}

/// A theme switch changes the colours baked into each segment, so it *must*
/// recompute. This is the invalidation half of the contract: stale colours
/// after a theme change would be a visible bug.
#[tokio::test]
async fn theme_switch_recomputes_highlights() {
    let (_dir, mut app) = app_with_code("theme").await;
    let _guard = lock_highlight_cache();
    crate::theme::set_active(forge_config::THEME_SOLARIZED_DARK);
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    crate::theme::set_active(forge_config::THEME_SOLARIZED_LIGHT);
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    // Restore before asserting so a failure cannot leak a palette into others.
    crate::theme::set_active(forge_config::THEME_SOLARIZED_DARK);

    assert!(
        after.misses >= before.misses + CACHED_BLOCKS as u64,
        "a theme switch must recompute every block (misses {} -> {})",
        before.misses,
        after.misses
    );
}
