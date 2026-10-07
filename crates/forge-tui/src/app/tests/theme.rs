//! Theme persistence and light-palette rendering tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;
use crate::overlays::{handle_overlay_key, Key as OverlayKey, OverlayAction};

#[tokio::test]
async fn theme_picker_previews_on_navigate_confirms_on_enter_restores_on_esc() {
    let (_dir, mut app) = focus_test_app().await;
    let original = forge_config::DEFAULT_THEME_ID.to_string();
    assert_eq!(crate::theme::active(), original);

    app.handle_theme_command(None);
    assert!(matches!(app.overlay, Some(Overlay::Theme { .. })));

    let light = forge_config::THEME_FORGE_LIGHT.to_string();
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
    let (_fake_home, _home_guard) = fake_home_guard();
    let (dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    assert_eq!(app.runtime.theme_id, forge_config::THEME_FORGE_LIGHT);
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_LIGHT);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert_eq!(restored.runtime.theme_id, forge_config::THEME_FORGE_LIGHT);
    assert_eq!(crate::theme::active(), forge_config::THEME_FORGE_LIGHT);
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

    assert!(app.workspace_files.visible);
}

/// A theme switch changes the colours baked into each segment, so it *must*
/// recompute. This is the invalidation half of the contract: stale colours
/// after a theme change would be a visible bug.
#[tokio::test]
async fn theme_switch_recomputes_highlights() {
    let (_dir, mut app) = app_with_code("theme").await;
    let _guard = lock_highlight_cache();
    crate::theme::set_active(forge_config::THEME_FORGE_DARK);
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    crate::theme::set_active(forge_config::THEME_FORGE_LIGHT);
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    // Restore before asserting so a failure cannot leak a palette into others.
    crate::theme::set_active(forge_config::THEME_FORGE_DARK);

    assert!(
        after.misses >= before.misses + CACHED_BLOCKS as u64,
        "a theme switch must recompute every block (misses {} -> {})",
        before.misses,
        after.misses
    );
}
