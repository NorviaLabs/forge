//! Mouse-wheel scrolling tests (v1: vertical wheel, focus-based routing).
//!
//! Synthetic `MouseEvent`s exercise the handler without a real mouse/terminal.

use super::prelude::*;

fn wheel_up() -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn wheel_down() -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn shift_wheel_down() -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::SHIFT,
    }
}

#[tokio::test]
async fn wheel_up_over_chat_unfollows_and_scrolls_conversation() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    app.conversation_view.follow = true;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert!(!app.conversation_view.follow);
    assert_eq!(app.conversation_view.scroll, 1);
}

#[tokio::test]
async fn wheel_down_to_bottom_refollows_conversation() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    app.conversation_view.follow = false;
    app.conversation_view.scroll = 2;

    // Two notches bring scroll to 0 (bottom) => follow is restored, matching
    // the keyboard PageDown path (scroll_conversation_down).
    app.handle_mouse(wheel_down()).await.unwrap();
    app.handle_mouse(wheel_down()).await.unwrap();

    assert!(app.conversation_view.follow);
    assert_eq!(app.conversation_view.scroll, 0);
}

#[tokio::test]
async fn shift_wheel_pages_conversation_like_pagedown() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    // Start scrolled up so a page-down notch has room to move.
    app.conversation_view.scroll = 10;

    app.handle_mouse(shift_wheel_down()).await.unwrap();

    // Shift+wheel pages by the same step keyboard PageUp/PageDown uses.
    assert_eq!(app.conversation_view.scroll, 5);
}

#[tokio::test]
async fn wheel_over_workspace_with_file_scrolls_source_viewer() {
    let (dir, mut app) = focus_test_app().await;
    std::fs::write(dir.path().join("source.rs"), "line1\nline2\nline3\n").unwrap();
    app.open_file_in_editor(&dir.path().join("source.rs"));
    app.focus_block(FocusBlock::Workspace);
    let start = app.source_viewer.current_line;

    app.handle_mouse(wheel_down()).await.unwrap();

    assert_eq!(app.source_viewer.current_line, start + 1);
}

#[tokio::test]
async fn wheel_over_terminal_is_a_noop() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = true;
    app.focus_block(FocusBlock::BottomPanel);
    app.conversation_view.scroll = 0;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
}

#[tokio::test]
async fn wheel_over_overlay_is_a_noop() {
    let (_dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::Help);
    app.conversation_view.scroll = 0;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
}

#[tokio::test]
async fn horizontal_wheel_is_ignored() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    app.conversation_view.scroll = 0;

    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollLeft,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
}
