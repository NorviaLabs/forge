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
    assert_eq!(app.conversation_view.scroll, 3);
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
    let source = (1..=40)
        .map(|line| format!("line{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("source.rs"), source).unwrap();
    app.open_file_in_editor(&dir.path().join("source.rs"));
    app.focus_block(FocusBlock::Workspace);
    let start = app.source_viewer.current_line;

    app.handle_mouse(wheel_down()).await.unwrap();

    assert_eq!(app.source_viewer.current_line, start + 3);
}

#[tokio::test]
async fn wheel_over_terminal_is_a_noop() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = true;
    app.focus_block(FocusBlock::BottomPanel);
    app.conversation_view.scroll = 0;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
}

#[tokio::test]
async fn wheel_over_overlay_is_a_noop() {
    let (_dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::Help);
    app.conversation_view.scroll = 0;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
}

fn left_click(column: u16, row: u16) -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn left_release(column: u16, row: u16) -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn moved(column: u16, row: u16) -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[tokio::test]
async fn click_without_drag_clears_selection_instead_of_copying() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
    app.conversation_rows = vec!["hello world".to_string()];

    // Down and up at the same cell: no drag occurred.
    app.handle_mouse(left_click(5, 2)).await.unwrap();
    assert!(
        app.selection.is_active(),
        "mouse-down should start a selection"
    );

    app.handle_mouse(left_release(5, 2)).await.unwrap();

    assert!(
        !app.selection.is_active(),
        "a click without a drag must clear rather than leave a one-cell selection"
    );
    assert!(
        app.feedback.is_empty(),
        "a click without a drag must not trigger the auto-copy feedback toast"
    );
}

#[tokio::test]
async fn dragged_selection_auto_copies_and_reports_line_count() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
    app.conversation_rows = vec!["│ hello".to_string(), "│ world".to_string()];

    // Start the drag at the pane's left edge (col == area.x): the existing
    // rail-stripping in `visible_rows_selection_text` computes offsets
    // against the raw (pre-strip) row, so a start column mid-line would
    // land in the wrong place post-strip — an existing quirk, not something
    // this change touches.
    app.handle_mouse(left_click(0, 0)).await.unwrap();
    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 6,
        row: 1,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    app.handle_mouse(left_release(6, 1)).await.unwrap();

    assert!(
        app.selection.is_active(),
        "the highlight should persist after auto-copy, like a native text selection"
    );
    assert_eq!(app.selection.text, "hello\nworld");
    assert_eq!(app.feedback.severity, crate::widgets::FeedbackSeverity::Ok);
    assert_eq!(app.feedback.text, "Copied 2 lines");
}

#[tokio::test]
async fn selection_does_not_change_after_mouse_up() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
    app.conversation_rows = vec!["│ hello".to_string(), "│ world".to_string()];

    app.handle_mouse(left_click(0, 0)).await.unwrap();
    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 6,
        row: 1,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    app.handle_mouse(left_release(6, 1)).await.unwrap();
    let text_after_release = app.selection.text.clone();
    assert_eq!(text_after_release, "hello\nworld");

    // Stray pointer movement after mouse-up (some terminals send Moved or
    // even Drag events with no button held) must not keep extending a
    // selection that already finished — this is the "sticky selection" bug.
    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();

    assert_eq!(
        app.selection.text, text_after_release,
        "pointer movement after mouse-up must not change a finished selection"
    );
    // A duplicate/spurious Up event after the drag already finished must
    // also be a no-op, not re-derive and re-copy the same text again.
    app.feedback = crate::widgets::FeedbackModel::default();
    app.handle_mouse(left_release(0, 0)).await.unwrap();
    assert!(
        app.feedback.text.is_empty(),
        "a spurious duplicate mouse-up must not re-trigger the copy feedback toast"
    );
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

#[tokio::test]
async fn click_focuses_the_block_under_the_pointer() {
    let (_dir, mut app) = focus_test_app().await;
    render_app_text(&mut app, 120, 40);

    let footer = app.footer_area.expect("footer drawn");
    app.focus_block(FocusBlock::Workspace);
    app.handle_mouse(left_click(footer.x + 2, footer.y))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Footer);

    let composer = app.composer_area.expect("composer drawn");
    app.focus_block(FocusBlock::Workspace);
    app.handle_mouse(left_click(composer.x + 2, composer.y))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Composer);
}

#[tokio::test]
async fn click_navigator_tab_switches_between_sessions_and_files() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_tabs_area = Some(ratatui::layout::Rect::new(0, 0, 40, 1));

    // `▌Sessions` occupies x 0..9, then ` │ `, then the Files tab.
    app.handle_mouse(left_click(1, 0)).await.unwrap();
    assert_eq!(
        app.effective_navigator_tab(),
        crate::widgets::NavigatorTab::Sessions
    );
    app.handle_mouse(left_click(20, 0)).await.unwrap();
    assert_eq!(
        app.effective_navigator_tab(),
        crate::widgets::NavigatorTab::Files
    );
}

#[tokio::test]
async fn click_selects_a_navigator_session_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_tab = crate::widgets::NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.navigator_list_area = Some(ratatui::layout::Rect::new(0, 5, 40, 10));

    app.task_strip_selection = 99;
    app.handle_mouse(left_click(2, 5)).await.unwrap();

    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
    assert_eq!(app.task_strip_selection, 0);
}

#[tokio::test]
async fn click_opens_a_file_tree_row() {
    let (dir, mut app) = focus_test_app().await;
    std::fs::write(dir.path().join("clickme.txt"), "hi").unwrap();
    app.workspace_files.explorer.refresh_workspace();
    app.workspace_files.visible = true;
    app.navigator_tab = crate::widgets::NavigatorTab::Files;
    app.navigator_tab_explicit = true;
    render_app_text(&mut app, 140, 45);

    let index = app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .position(|node| node.display_name == "clickme.txt")
        .expect("test file should be visible");
    let list = app.navigator_list_area.expect("file list drawn");
    app.handle_mouse(left_click(list.x + 3, list.y + 2 + index as u16))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_files
            .explorer
            .selected_path
            .as_deref()
            .and_then(|path| path.file_name()),
        Some(std::ffi::OsStr::new("clickme.txt"))
    );
    assert!(
        app.current_workspace_is_file(),
        "clicking a file row should open it"
    );
}

#[tokio::test]
async fn motion_sets_file_hover_without_moving_focus() {
    let (dir, mut app) = focus_test_app().await;
    std::fs::write(dir.path().join("hoverme.txt"), "hi").unwrap();
    app.workspace_files.explorer.refresh_workspace();
    app.workspace_files.visible = true;
    app.navigator_tab = crate::widgets::NavigatorTab::Files;
    app.navigator_tab_explicit = true;
    render_app_text(&mut app, 140, 45);

    let index = app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .position(|node| node.display_name == "hoverme.txt")
        .expect("test file should be visible");
    let list = app.navigator_list_area.expect("file list drawn");
    let before = app.focus.block();

    app.handle_mouse(moved(list.x + 3, list.y + 2 + index as u16))
        .await
        .unwrap();

    assert_eq!(app.hover_file, Some(index));
    assert_eq!(app.focus.block(), before, "hover must never move focus");

    // Motion away clears the highlight.
    app.handle_mouse(moved(0, 200)).await.unwrap();
    assert_eq!(app.hover_file, None);
}

#[tokio::test]
async fn motion_hovers_a_session_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_tab = crate::widgets::NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.navigator_list_area = Some(ratatui::layout::Rect::new(0, 5, 40, 10));
    let before = app.focus.block();

    app.handle_mouse(moved(2, 5)).await.unwrap();

    assert_eq!(app.hover_session, Some(0));
    assert_eq!(app.focus.block(), before, "hover must never move focus");
}
