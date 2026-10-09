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

#[tokio::test]
async fn horizontal_session_chip_click_keeps_the_files_navbar_selected() {
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    render_app_text(&mut app, 140, 45);
    let (_, chip) = app.task_strip_chips[0];
    app.handle_mouse(left_click(chip.x + 3, chip.y))
        .await
        .unwrap();
    assert!(app.horizontal_session_strip_focused);
    assert_eq!(
        app.effective_navigator_tab(),
        crate::widgets::NavigatorTab::Files
    );
    render_app_text(&mut app, 140, 45);
    assert!(app.horizontal_session_strip_focused);
    let strip = app.task_strip_area.unwrap();
    let tabs = app.navigator_tabs_area.unwrap();
    assert_eq!(tabs.y, strip.bottom() + 1);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn clicking_content_match_opens_its_line_and_file_group_toggles() {
    let (dir, mut app) = focus_test_app().await;
    std::fs::write(
        dir.path().join("search.rs"),
        "first needle\nsecond needle\n",
    )
    .unwrap();
    app.handle_key(press(
        KeyCode::Char('f'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .await
    .unwrap();
    app.handle_paste("needle");
    for _ in 0..1_000 {
        app.workspace_files.explorer.poll_search_load();
        if app.workspace_files.explorer.search_result_count().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(
        app.workspace_files
            .explorer
            .search_result_count()
            .as_deref(),
        Some("2 matches")
    );
    render_app_text(&mut app, 140, 45);
    let list = app.navigator_list_area.unwrap();
    let y = list.y + crate::file_explorer::TREE_ROW_OFFSET;
    app.handle_mouse(left_click(list.x + 3, y + 2))
        .await
        .unwrap();
    assert_eq!(app.editor_session.as_ref().unwrap().cursor_row(), 1);
    assert_eq!(app.source_viewer.current_line, 1);

    app.handle_mouse(left_click(list.x + 3, y)).await.unwrap();
    assert_eq!(app.workspace_files.explorer.visible_nodes().len(), 1);
    app.handle_mouse(left_click(list.x + 3, y)).await.unwrap();
    assert_eq!(app.workspace_files.explorer.visible_nodes().len(), 3);
}

#[tokio::test]
async fn dragging_to_conversation_top_scrolls_and_extends_selection() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_area = Some(ratatui::layout::Rect::new(0, 5, 80, 10));
    app.conversation_rows = (0..10).map(|i| format!("row {i}")).collect();
    app.conversation_view.follow = true;

    app.handle_mouse(left_click(4, 12)).await.unwrap();
    app.handle_mouse(event::MouseEvent {
        kind: event::MouseEventKind::Drag(event::MouseButton::Left),
        column: 0,
        row: 5,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();

    assert_eq!(app.conversation_view.scroll, 3);
    assert!(!app.conversation_view.follow);
    let rect = app.selection.rect().unwrap();
    assert_eq!(rect.row_start, 5);
    assert_eq!(rect.row_end, 15);
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
async fn wheel_up_over_the_focused_transcript_scrolls_it() {
    let (_dir, mut app) = focus_test_app().await;
    // Sidebar is what a click on the transcript focuses.
    app.focus_block(FocusBlock::Sidebar);
    app.conversation_view.follow = true;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert!(!app.conversation_view.follow);
    assert_eq!(app.conversation_view.scroll, 3);
}

#[tokio::test]
async fn shift_wheel_pages_the_focused_transcript_by_the_measured_page() {
    let (_dir, mut app) = focus_test_app().await;
    // Drawing first is the point: an unrendered app only ever proves the
    // pre-draw fallback, which is what made this test pass for the wrong
    // reason before.
    draw_app(&mut app, 120, 40);
    let page = app.conversation_page_rows();
    assert!(page > 3, "fixture pane is worth paging: {page}");
    app.focus_block(FocusBlock::Sidebar);
    app.conversation_view.follow = false;
    // Start scrolled up so a page-down has room to move.
    app.conversation_view.scroll = 100;

    app.handle_mouse(shift_wheel_down()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 100 - page);
}

/// §8.6: the wheel matches the keyboard page/step size for the pane it
/// scrolls. Same pane, two inputs, identical distance.
#[tokio::test]
async fn shift_wheel_moves_the_transcript_by_the_same_page_as_pagedown() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 120, 40);
    let page = app.conversation_page_rows();
    assert!(page > 3, "fixture pane is worth paging: {page}");

    app.focus_block(FocusBlock::Composer);
    app.conversation_view.follow = false;
    app.conversation_view.scroll = 100;
    app.handle_mouse(shift_wheel_down()).await.unwrap();
    let wheel_target = app.conversation_view.scroll;

    app.conversation_view.scroll = 100;
    app.handle_key(press(KeyCode::PageDown, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.conversation_view.scroll, wheel_target);
    assert_eq!(wheel_target, 100 - page);
}

/// Scope pin: the session navigator is a deliberate wheel no-op, while the
/// file tree beside it moves its selection. Only the transcript was in scope.
#[tokio::test]
async fn wheel_over_the_session_navigator_is_a_noop() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::TaskStrip);
    app.conversation_view.scroll = 0;
    let selection = app.task_strip_selection;

    app.handle_mouse(wheel_up()).await.unwrap();
    app.handle_mouse(wheel_down()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
    assert_eq!(app.task_strip_selection, selection);
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
async fn wheel_in_terminal_does_not_scroll_conversation() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = true;
    app.focus_block(FocusBlock::BottomPanel);
    app.conversation_view.scroll = 0;

    app.handle_mouse(wheel_up()).await.unwrap();

    assert_eq!(app.conversation_view.scroll, 0);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
}

#[tokio::test]
async fn terminal_text_area_is_mouse_selectable() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = true;
    draw_app(&mut app, 120, 40);
    let area = app.terminal_area.expect("terminal text area was drawn");

    app.handle_mouse(left_click(area.x, area.y)).await.unwrap();

    assert_eq!(
        app.selection.pane,
        Some(crate::selection::CopyPane::Terminal)
    );
    assert!(app.selection.is_active());
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
async fn mouse_context_menu_is_pointer_owned_and_handles_keyboard_actions() {
    let (_dir, mut app) = focus_test_app().await;
    app.conversation_area = Some(ratatui::layout::Rect::new(0, 0, 80, 20));
    app.conversation_rows = vec!["│ hello".into()];

    app.handle_mouse(event::MouseEvent {
        kind: event::MouseEventKind::Down(event::MouseButton::Right),
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert!(app.context_menu.is_some());

    app.handle_context_menu_key(event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_context_menu_key(event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    app.handle_context_menu_key(event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.context_menu.is_none());
    assert!(app.feedback.text.contains("Nothing selected"));

    app.overlay = Some(Overlay::Help);
    app.handle_mouse(event::MouseEvent {
        kind: event::MouseEventKind::Down(event::MouseButton::Right),
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert!(app.context_menu.is_none());
    app.overlay = None;
}

fn left_release(column: u16, row: u16) -> event::MouseEvent {
    event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[tokio::test]
async fn context_menu_corner_and_resize_keep_both_actions_clickable() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 120, 40);
    app.handle_mouse(event::MouseEvent {
        kind: event::MouseEventKind::Down(event::MouseButton::Right),
        column: 118,
        row: 38,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    draw_app(&mut app, 80, 18);
    let rect = app.context_menu.as_ref().unwrap().rect();
    assert!(rect.right() <= 80 && rect.bottom() <= 18);
    app.handle_mouse(left_click(rect.x + 2, rect.y + 1))
        .await
        .unwrap();
    app.handle_mouse(left_release(rect.x + 2, rect.y + 1))
        .await
        .unwrap();
    assert!(
        app.context_menu.is_none(),
        "fitted Clear row uses fitted hit geometry"
    );
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
async fn start_prompt_hover_and_click_preserve_the_draft_without_submitting() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("Keep this draft.");
    render_app_text(&mut app, 120, 40);
    let (index, row) = app.start_prompt_rows[1];
    let focus = app.focus.block();
    app.handle_mouse(moved(row.x + 3, row.y)).await.unwrap();
    assert_eq!(app.hover_start_prompt, Some(index));
    assert_eq!(app.focus.block(), focus);
    app.handle_mouse(left_click(row.x + 3, row.y))
        .await
        .unwrap();
    assert!(app.input.text.starts_with("Keep this draft.\n"));
    assert!(app
        .input
        .text
        .contains(crate::widgets::start::STARTERS[index]));
    assert!(!app.pending_turn.has_prompt());
    assert_eq!(app.focus.block(), FocusBlock::Composer);
}

#[tokio::test]
async fn click_navigator_tab_switches_between_sessions_and_files() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_tabs_area = Some(ratatui::layout::Rect::new(0, 0, 40, 1));

    // The `Sessions` heading hands the keyboard to the session list.
    app.handle_mouse(left_click(1, 0)).await.unwrap();
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
    // Files and Git are main-workspace tabs, so a click past the heading is
    // inert rather than switching the navigator.
    app.handle_mouse(left_click(20, 0)).await.unwrap();
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
}

/// The `+` cell shares an edge with the tab boxes, so pointer routing has to
/// claim it before the tab branch — for hover and for the click alike.
#[tokio::test]
async fn hovering_the_new_session_cell_does_not_read_as_a_tab() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_tabs_area = Some(ratatui::layout::Rect::new(0, 0, 40, 2));
    let cell = crate::widgets::navigator::new_session_cell(ratatui::layout::Rect::new(0, 0, 40, 3))
        .expect("the row is wide enough");
    app.navigator_new_session_area = Some(cell);

    app.handle_mouse(moved(cell.x + 1, cell.y + 1))
        .await
        .unwrap();
    assert!(app.hover_navigator_new_session);
    assert_eq!(
        app.hover_navigator_tab, None,
        "the cell must not read as a hovered tab"
    );

    // Cells clear of the cell still belong to the tabs.
    app.handle_mouse(moved(cell.right() + 1, 1)).await.unwrap();
    assert!(!app.hover_navigator_new_session);
    assert_eq!(
        app.hover_navigator_tab,
        Some(crate::widgets::NavigatorTab::Files)
    );
}

#[tokio::test]
async fn click_selects_a_navigator_session_row() {
    let (_dir, mut app) = focus_test_app().await;
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
    app.select_workspace_tab(WorkspaceTab::Files);
    render_app_text(&mut app, 160, 50);

    let index = app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .position(|node| node.display_name == "clickme.txt")
        .expect("test file should be visible");
    let list = app.navigator_list_area.expect("file list drawn");
    app.handle_mouse(left_click(
        list.x + 3,
        list.y + crate::file_explorer::TREE_ROW_OFFSET + index as u16,
    ))
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
    app.select_workspace_tab(WorkspaceTab::Files);
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

    app.handle_mouse(moved(
        list.x + 3,
        list.y + crate::file_explorer::TREE_ROW_OFFSET + index as u16,
    ))
    .await
    .unwrap();

    assert_eq!(app.hover_file, Some(index));
    assert_eq!(app.focus.block(), before, "hover must never move focus");

    // Motion away clears the highlight.
    app.handle_mouse(moved(0, 200)).await.unwrap();
    assert_eq!(app.hover_file, None);
}

#[tokio::test]
async fn clicking_search_focuses_input_without_opening_a_tree_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    render_app_text(&mut app, 120, 40);
    let list = app.navigator_list_area.expect("file list drawn");
    let selected = app.workspace_files.explorer.selected_path.clone();
    // The surface is full-bleed, so even its leftmost column (one cell into
    // the shell inset) must hand the keyboard to Search.
    app.focus_block(FocusBlock::Workspace);
    app.handle_mouse(left_click(list.x, list.y + 2))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert_eq!(app.workspace_files.explorer.selected_path, selected);
    assert!(!app.current_workspace_is_file());
}

#[tokio::test]
async fn motion_hovers_a_session_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.navigator_list_area = Some(ratatui::layout::Rect::new(0, 5, 40, 10));
    let before = app.focus.block();

    app.handle_mouse(moved(2, 5)).await.unwrap();

    assert_eq!(app.hover_session, Some(0));
    assert_eq!(app.focus.block(), before, "hover must never move focus");
}

#[tokio::test]
async fn click_activates_a_footer_chip() {
    let (_dir, mut app) = focus_test_app().await;
    render_app_text(&mut app, 120, 40);
    let ranges = app.footer_chip_rects.expect("chips captured during paint");
    let y = app.footer_area.expect("footer drawn").y;

    app.handle_mouse(left_click(ranges[0].0 + 1, y))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Footer);
    assert_eq!(app.composer_chip_focus, Some(0));
    assert!(
        app.overlay.is_some(),
        "the model chip opens the connect/model control"
    );
}

#[tokio::test]
async fn motion_hovers_a_footer_chip_without_moving_focus() {
    let (_dir, mut app) = focus_test_app().await;
    render_app_text(&mut app, 120, 40);
    let ranges = app.footer_chip_rects.expect("chips captured during paint");
    let y = app.footer_area.expect("footer drawn").y;
    let before = app.focus.block();

    app.handle_mouse(moved(ranges[1].0 + 1, y)).await.unwrap();
    assert_eq!(app.hover_chip, Some(1));
    assert_eq!(app.focus.block(), before, "hover must never move focus");

    app.handle_mouse(moved(0, 200)).await.unwrap();
    assert_eq!(app.hover_chip, None);
}

#[tokio::test]
async fn motion_hovers_a_queued_message_without_moving_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.enqueue_user_message("queued alpha".into()).await;
    app.enqueue_user_message("queued beta".into()).await;
    render_app_text(&mut app, 120, 40);
    let area = app.queue_area.expect("queue strip drawn");
    let before = app.focus.block();

    // Row 0 is the strip title; row 2 is the second queued message, and the
    // strip's text starts at the shared sidebar inset.
    let x = area.x + crate::widgets::input::TEXT_INSET + 1;
    app.handle_mouse(moved(x, area.y + 2)).await.unwrap();
    assert_eq!(app.hover_queue, Some(1));
    assert_eq!(app.focus.block(), before, "hover must never move focus");

    app.handle_mouse(moved(0, 200)).await.unwrap();
    assert_eq!(app.hover_queue, None);
}

#[tokio::test]
async fn click_selects_an_overlay_row_and_hover_never_moves_it() {
    use crate::overlays::ModelItem;
    use forge_connect::CatalogSource;

    let (_dir, mut app) = focus_test_app().await;
    let item = |model: &str| ModelItem {
        provider: "native".into(),
        model: model.into(),
        profile_id: Some("native".into()),
        source: CatalogSource::Default,
        route_label: "native".into(),
    };
    app.overlay = Some(Overlay::connect_model_open(
        vec![],
        vec![item("alpha"), item("beta"), item("gamma")],
        Some("native"),
        "alpha",
        ReasoningEffort::default(),
        ConnectModelColumn::Models,
    ));
    render_app_text(&mut app, 120, 40);

    let rows = app.overlay_rows.borrow().clone();
    let (_, rect) = rows
        .iter()
        .find(|(row, _)| *row == crate::overlays::OverlayRow::Model(2))
        .expect("third model row captured");
    let block_before = app.focus.block();

    // Hover arms the pointer highlight but never the selection.
    app.handle_mouse(moved(rect.x + 2, rect.y)).await.unwrap();
    assert_eq!(
        app.hover_overlay,
        Some(crate::overlays::OverlayRow::Model(2))
    );
    match &app.overlay {
        Some(Overlay::ConnectModel { model_selected, .. }) => {
            assert_eq!(*model_selected, 0, "hover moved the model selection")
        }
        other => panic!("overlay lost: {other:?}"),
    }

    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();
    match &app.overlay {
        Some(Overlay::ConnectModel {
            model_selected,
            focus,
            ..
        }) => {
            assert_eq!(*model_selected, 2);
            assert_eq!(*focus, ConnectModelColumn::Models);
        }
        other => panic!("overlay lost: {other:?}"),
    }
    assert_eq!(
        app.focus.block(),
        block_before,
        "an overlay click must not move pane focus"
    );

    app.handle_mouse(moved(0, 200)).await.unwrap();
    assert_eq!(app.hover_overlay, None);
}

#[tokio::test]
async fn click_selects_an_approval_option_row() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(&mut app, direct_hitl_payload("call-1", "/tmp/x"));
    app.sync_approval_focus();
    render_app_text(&mut app, 120, 40);
    assert!(
        app.option_rects.len() >= 2,
        "approval option rects captured: {:?}",
        app.option_rects
    );

    let (index, rect) = app.option_rects[1];
    assert_eq!(index, 1);
    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert_eq!(app.approval_menu_selected(), 1);
}

#[tokio::test]
async fn motion_hovers_an_approval_option_without_moving_focus() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(&mut app, direct_hitl_payload("call-2", "/tmp/x"));
    app.sync_approval_focus();
    render_app_text(&mut app, 120, 40);
    let (_, rect) = app.option_rects[1];
    let before = app.focus.block();

    app.handle_mouse(moved(rect.x + 2, rect.y)).await.unwrap();

    assert_eq!(app.hover_option, Some(1));
    assert_eq!(app.focus.block(), before, "hover must never move focus");
}

#[tokio::test]
async fn click_selects_a_question_option_row() {
    use forge_types::{AskUserQuestionItem, AskUserQuestionOption, QuestionPayload};

    let (_dir, mut app) = focus_test_app().await;
    set_pending_question(
        &mut app,
        QuestionPayload {
            call_id: "q-1".into(),
            tool: "ask_user_question".into(),
            questions: vec![AskUserQuestionItem {
                id: "db".into(),
                question: "Which database?".into(),
                header: "Database".into(),
                options: vec![
                    AskUserQuestionOption {
                        label: "Postgres".into(),
                        description: String::new(),
                    },
                    AskUserQuestionOption {
                        label: "SQLite".into(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
            }],
        },
    );
    app.sync_question_focus();
    app.sync_question_menu();
    render_app_text(&mut app, 120, 40);
    assert!(
        app.option_rects.len() >= 2,
        "question option rects captured: {:?}",
        app.option_rects
    );

    let (index, rect) = app.option_rects[1];
    assert_eq!(index, 1);
    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert_eq!(app.question_menu_indexes().1, 1);
}

/// A click on a row in the navigator list hands the keyboard to the pane under
/// the tab row, exactly as a click on the tab itself does. Without this the
/// click's focus change is invisible: the pane paints unfocused and every bare
/// key still goes to the tab row, which swallows them on purpose.
#[tokio::test]
async fn click_on_a_navigator_row_drops_the_tab_row_focus() {
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.session_chrome.push(SessionChromeItem {
        session_id: uuid::Uuid::new_v4(),
        slot: None,
        label: "one".into(),
        branch: "main".into(),
        lifecycle: forge_types::TaskLifecycle::Ready,
        selected: true,
        secondary: None,
        attention: false,
        updated_at: chrono::Utc::now(),
    });
    app.focus_block(FocusBlock::TaskStrip);
    app.focus_navigator_tab_row();
    assert!(app.navigator_tab_row_focused, "the row holds the keyboard");

    app.navigator_list_area = Some(ratatui::layout::Rect::new(0, 5, 40, 10));
    app.handle_mouse(left_click(2, 5)).await.unwrap();

    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
    assert_eq!(app.task_strip_selection, 0);
    assert!(
        !app.navigator_tab_row_focused,
        "the clicked pane must own the keyboard, not the tab row"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A git-changes row selects on the first click and acts on the second, like a
/// session row: the patch lives in the Workspace block, so acting means
/// handing the keyboard there.
#[tokio::test]
async fn double_click_on_a_git_row_hands_the_keyboard_to_the_patch() {
    use crate::diff_view::{DiffEntry, DiffSide};
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);
    app.git_grouped_list = true;
    app.diff_view.entries = ["a.txt", "b.txt"]
        .into_iter()
        .map(|path| DiffEntry {
            path: path.into(),
            marker: "M",
            untracked: false,
            side: Some(DiffSide::Unstaged),
        })
        .collect();
    app.diff_view.selected = 1;
    // The list's first inner row is the `Unstaged` group heading, which is not
    // selectable, so file 0 sits one row below the list's origin.
    app.navigator_list_area = Some(ratatui::layout::Rect::new(0, 5, 40, 10));
    app.focus_block(FocusBlock::Files);

    app.handle_mouse(left_click(2, 6)).await.unwrap();
    assert_eq!(app.diff_view.selected, 0, "one click selects the row");
    assert_eq!(
        app.focus.block(),
        FocusBlock::Files,
        "one click does not jump to the patch"
    );

    app.handle_mouse(left_click(2, 6)).await.unwrap();
    assert_eq!(
        app.focus.block(),
        FocusBlock::Workspace,
        "the second click acts, the way `Enter` does"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A queued-message row is selectable by pointer. Before this the strip only
/// took hover, so `Ctrl+Backspace` could only ever cancel the row `Ctrl+↑` had
/// landed on — a mouse had no way to reach any other row.
#[tokio::test]
async fn click_on_a_queued_message_row_selects_it() {
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    // Written straight into the roster, so the fixture does not depend on when
    // a submitted prompt shows up in the supervisor's snapshot.
    app.supervisor
        .as_mut()
        .unwrap()
        .snapshots
        .get_mut(&app.selected_session_id)
        .unwrap()
        .queued_prompts = vec![(1, "first".into()), (2, "second".into())];
    assert_eq!(app.selected_queue_messages().len(), 2, "fixture");
    app.focus_block(FocusBlock::Composer);
    render_app_text(&mut app, 120, 40);
    let area = app.queue_area.expect("painted prompt queue");

    app.handle_mouse(left_click(area.x + 2, area.y + 2))
        .await
        .unwrap();

    assert_eq!(
        app.selected_queue_index(),
        Some(1),
        "the clicked row becomes the one Ctrl+Backspace acts on"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_queue_count_chip_opens_its_live_view_over_an_approval_and_keeps_the_draft() {
    let (_dir, mut app) = focus_test_app().await;
    app.session_runtime
        .enqueue_task("queued text")
        .await
        .unwrap();
    app.input.set_text("Retained draft λ/東京");
    set_pending_hitl(&mut app, direct_hitl_payload("pending", "file.txt"));
    app.sync_approval_focus();
    render_app_text(&mut app, 120, 40);
    let (_, area) = app
        .dock_paint
        .chips
        .iter()
        .find(|(filter, _)| *filter == crate::tasks_strip::TaskFilter::Queue)
        .copied()
        .expect("painted queue count");
    app.handle_mouse(left_click(area.x, area.y)).await.unwrap();
    assert!(matches!(
        app.overlay,
        Some(Overlay::Tasks {
            filter: crate::tasks_strip::TaskFilter::Queue,
            ..
        })
    ));
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "Retained draft λ/東京");
    assert!(app.selected_pending_hitl().is_some());
    assert_eq!(app.focus.block(), FocusBlock::Approval);
}

/// A click inside the background-activity strip is claimed by the strip and
/// hands the keyboard to the block it belongs to. The strip's own rows are
/// exercised in `widgets::background_strip`; this pins the routing, so the
/// click can never fall through to the transcript underneath it.
#[tokio::test]
async fn click_in_the_background_strip_takes_the_keyboard() {
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.background_area = Some(ratatui::layout::Rect::new(0, 40, 40, 4));
    app.focus_block(FocusBlock::Composer);

    app.handle_mouse(left_click(6, 42)).await.unwrap();

    assert_eq!(app.focus.block(), FocusBlock::Sidebar);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A slash-command suggestion row is selectable by pointer, and the click keeps
/// the keyboard in the composer. The palette floats over the transcript, so
/// before the renderer recorded its rows the click fell through to the
/// transcript and moved the keyboard out of the composer entirely.
#[tokio::test]
async fn click_on_a_slash_suggestion_row_keeps_the_keyboard_in_the_composer() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("/");
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 100, 40);
    let (index, rect) = app.slash_popup_rows[1];
    assert!(
        rect.width > 1 && rect.height == 1,
        "degenerate row {rect:?}"
    );

    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();

    assert_eq!(app.slash_suggestions.selected, index);
    assert_eq!(
        app.focus.block(),
        FocusBlock::Composer,
        "the popup is part of the composer, not the transcript beneath it"
    );
}

/// The second click on a suggestion row accepts it, the way `Tab` does.
#[tokio::test]
async fn double_click_on_a_slash_suggestion_row_accepts_it() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("/");
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 100, 40);
    let (index, rect) = app.slash_popup_rows[1];
    let expected = format!("{} ", app.slash_suggestions()[index].cmd);

    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();
    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();

    assert_eq!(app.input.text, expected);
}

/// Same contract for the `Ctrl+r` commands+history palette. Its section headers
/// are painted but not selectable, so only item rows are hittable.
#[tokio::test]
async fn clicking_the_inline_search_palette_selects_and_double_click_accepts() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_inline_search();
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 100, 40);
    assert!(
        !app.inline_search_rows.is_empty(),
        "the palette painted no hittable rows"
    );
    // Every recorded row must be a real line, not the palette's own border.
    for (_, rect) in &app.inline_search_rows {
        assert_eq!(rect.height, 1, "degenerate row {rect:?}");
    }
    let (index, rect) = app.inline_search_rows[2];
    let expected = app.inline_search_items()[index].clone();
    let expected = match expected {
        crate::app::InlineSearchItem::Command(item) => format!("{} ", item.display_cmd()),
        crate::app::InlineSearchItem::History(text) => text,
    };

    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();
    assert_eq!(app.inline_search.as_ref().unwrap().selected, index);
    assert_eq!(app.focus.block(), FocusBlock::Composer);

    app.handle_mouse(left_click(rect.x + 2, rect.y))
        .await
        .unwrap();
    assert_eq!(app.input.text, expected);
    assert!(app.inline_search.is_none(), "accepting closes the palette");
}

/// A click in the editor places the caret at the cell under the pointer, using
/// the same gutter math as drag-selection, so the two can never disagree about
/// where the text is.
#[tokio::test]
async fn click_in_the_editor_places_the_caret() {
    let (dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "alpha\nbravo\ncharlie\n").unwrap();
    app.open_file_in_editor(&path);
    draw_app(&mut app, 100, 40);
    let body = app.source_viewer.rendered_text.area;
    let content_x = body.x;

    app.handle_mouse(left_click(content_x + 3, body.y + 1))
        .await
        .unwrap();

    let editor = app.editor_session.as_ref().expect("a live editor");
    assert_eq!(editor.cursor_row(), 1, "the clicked line");
    assert_eq!(editor.cursor_col(), 3, "the clicked character");
    assert_eq!(app.focus.block(), FocusBlock::Workspace);

    // Clicking left of the line numbers means the start of the line.
    app.handle_mouse(left_click(body.x, body.y + 2))
        .await
        .unwrap();
    assert_eq!(app.editor_session.as_ref().unwrap().cursor_row(), 2);
    assert_eq!(app.editor_session.as_ref().unwrap().cursor_col(), 0);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// In the read-only preview there is no caret, so a click moves the current
/// line — the same thing every line-scrolling key moves, which keeps `j`/`k`
/// and the other navigation continuing from where the pointer left the cursor.
#[tokio::test]
async fn click_in_a_read_only_preview_moves_the_current_line() {
    let (dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "alpha\nbravo\ncharlie\n").unwrap();
    app.open_file_in_editor(&path);
    // The read-only viewer is what a file the editor cannot open gets.
    app.editor_session = None;
    app.source_viewer.lines = vec![
        "alpha".to_string(),
        "bravo".to_string(),
        "charlie".to_string(),
    ];
    draw_app(&mut app, 100, 40);
    let body = app.source_viewer.rendered_text.area;

    app.handle_mouse(left_click(body.x + 8, body.y + 2))
        .await
        .unwrap();

    assert_eq!(app.source_viewer.current_line, 2);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The GitHub issue list and its action menu are selectable by pointer. The
/// double click is the issue list's `Enter`, which opens the action menu — the
/// same split the keyboard has between moving and choosing.
#[tokio::test]
async fn click_selects_a_github_issue_row_and_its_action_row() {
    let (_dir, mut app) = focus_test_app().await;
    let issue = |number: u64, title: &str| forge_workspace::github::Issue {
        number,
        title: title.into(),
        url: String::new(),
        state: "open".into(),
        labels: Vec::new(),
    };
    app.overlay = Some(Overlay::GithubIssues {
        selected: 0,
        filter: String::new(),
        items: vec![issue(11, "crash on start"), issue(12, "stale docs")],
        error: None,
        action: 0,
        action_menu: true,
        pr_states: Default::default(),
    });
    render_app_text(&mut app, 120, 40);
    let rows = app.overlay_rows.borrow().clone();
    let rect_of = |row| {
        rows.iter()
            .find(|(recorded, _)| *recorded == row)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{row:?} captured: {rows:?}"))
    };
    let block_before = app.focus.block();

    app.handle_mouse(left_click(
        rect_of(crate::overlays::OverlayRow::Issue(1)).x + 2,
        rect_of(crate::overlays::OverlayRow::Issue(1)).y,
    ))
    .await
    .unwrap();
    app.handle_mouse(left_click(
        rect_of(crate::overlays::OverlayRow::IssueAction(2)).x + 2,
        rect_of(crate::overlays::OverlayRow::IssueAction(2)).y,
    ))
    .await
    .unwrap();

    match &app.overlay {
        Some(Overlay::GithubIssues {
            selected, action, ..
        }) => {
            assert_eq!(*selected, 1, "the clicked issue");
            assert_eq!(*action, 2, "the clicked action");
        }
        other => panic!("overlay lost: {other:?}"),
    }
    assert_eq!(app.focus.block(), block_before, "a click never moves focus");

    // Selecting an issue with the menu closed and double-clicking it runs the
    // issue list's own `Enter`: open the action menu.
    if let Some(Overlay::GithubIssues { action_menu, .. }) = &mut app.overlay {
        *action_menu = false;
    }
    let second = rect_of(crate::overlays::OverlayRow::Issue(1));
    app.handle_mouse(left_click(second.x + 2, second.y))
        .await
        .unwrap();
    app.handle_mouse(left_click(second.x + 2, second.y))
        .await
        .unwrap();
    match &app.overlay {
        Some(Overlay::GithubIssues { action_menu, .. }) => assert!(
            *action_menu,
            "the second click accepted, the way Enter does"
        ),
        other => panic!("overlay lost: {other:?}"),
    }
}

/// The commit-suggest choices are selectable, and the second click on `Cancel`
/// closes the menu through the same `Enter` path the keyboard uses.
#[tokio::test]
async fn click_selects_a_commit_suggest_row_and_double_click_cancels() {
    let (_dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::GitCommitSuggest { selected: 0 });
    render_app_text(&mut app, 120, 40);
    let rows = app.overlay_rows.borrow().clone();
    let rect_of = |row| {
        rows.iter()
            .find(|(recorded, _)| *recorded == row)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{row:?} captured: {rows:?}"))
    };
    let always = rect_of(crate::overlays::OverlayRow::CommitSuggest(1));

    app.handle_mouse(left_click(always.x + 2, always.y))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::GitCommitSuggest { selected: 1 })),
        "the clicked choice is the highlighted one"
    );

    let cancel = rect_of(crate::overlays::OverlayRow::CommitSuggest(2));
    app.handle_mouse(left_click(cancel.x + 2, cancel.y))
        .await
        .unwrap();
    app.handle_mouse(left_click(cancel.x + 2, cancel.y))
        .await
        .unwrap();
    assert!(app.overlay.is_none(), "Cancel accepted, so the menu closed");
}

/// A branch row records its index into the *filtered* matches — the space the
/// keyboard's `selected` lives in — not into the branch list. `main` is filtered
/// out here, so the first painted row is `feat/one` and must land on `0`.
#[tokio::test]
async fn click_selects_a_branch_row_in_the_filtered_index_space() {
    let (_dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::GitBranch {
        selected: 0,
        filter: "feat".into(),
        items: vec!["main".into(), "feat/one".into(), "feat/two".into()],
        current: Some("main".into()),
        merge: false,
        error: None,
    });
    render_app_text(&mut app, 120, 40);
    let rows = app.overlay_rows.borrow().clone();
    let row: Vec<_> = rows
        .iter()
        .filter(|(row, _)| matches!(row, crate::overlays::OverlayRow::Branch(_)))
        .cloned()
        .collect();
    assert_eq!(
        row.iter()
            .filter(|(kind, _)| *kind == crate::overlays::OverlayRow::Branch(0))
            .count(),
        1,
        "only the first match is recorded: {row:?}"
    );
    let (_, first) = row[0];
    let (_, second) = row[1];

    app.handle_mouse(left_click(first.x + 2, first.y))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::GitBranch { selected: 0, .. })),
        "feat/one is the first match, not item 1"
    );
    app.handle_mouse(left_click(second.x + 2, second.y))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::GitBranch { selected: 1, .. })),
        "feat/two is the second match"
    );
}

/// A file-explorer row is selectable, and the second click on a directory
/// descends into it — the same `Enter` path the keyboard uses.
#[tokio::test]
async fn click_selects_a_file_explorer_row_and_double_click_descends() {
    let (dir, mut app) = focus_test_app().await;
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    app.overlay = Some(Overlay::FileExplorer {
        cwd: dir.path().display().to_string(),
        selected: 0,
        items: vec![
            crate::overlays::FileExplorerItem {
                name: "sub".into(),
                path: sub.display().to_string(),
                is_dir: true,
            },
            crate::overlays::FileExplorerItem {
                name: "notes.md".into(),
                path: dir.path().join("notes.md").display().to_string(),
                is_dir: false,
            },
        ],
        error: None,
    });
    render_app_text(&mut app, 120, 40);
    let rows = app.overlay_rows.borrow().clone();
    let rect_of = |row| {
        rows.iter()
            .find(|(recorded, _)| *recorded == row)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{row:?} captured: {rows:?}"))
    };
    let file = rect_of(crate::overlays::OverlayRow::FileExplorer(1));

    app.handle_mouse(left_click(file.x + 2, file.y))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::FileExplorer { selected: 1, .. })),
        "the clicked entry is the highlighted one"
    );

    let directory = rect_of(crate::overlays::OverlayRow::FileExplorer(0));
    app.handle_mouse(left_click(directory.x + 2, directory.y))
        .await
        .unwrap();
    app.handle_mouse(left_click(directory.x + 2, directory.y))
        .await
        .unwrap();
    match &app.overlay {
        // The explorer canonicalizes what it opens, which is what makes its
        // `..` handling work, so the assertion compares canonical paths.
        Some(Overlay::FileExplorer { cwd, .. }) => assert_eq!(
            cwd,
            &sub.canonicalize().unwrap().display().to_string(),
            "the second click entered the directory, the way Enter does"
        ),
        other => panic!("overlay lost: {other:?}"),
    }
}

/// The search prompt takes every keystroke, so a click landing behind it must
/// not act on the pane it covers — a caret placed under a prompt the keyboard
/// can no longer reach is worse than no click at all.
#[tokio::test]
async fn a_click_behind_the_source_search_prompt_is_ignored() {
    let (dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "alpha\nbravo\ncharlie\n").unwrap();
    app.open_file_in_editor(&path);
    draw_app(&mut app, 100, 40);
    let body = app.source_viewer.rendered_text.area;
    app.source_viewer.start_search();
    app.source_viewer.append_search_char('a');
    app.normalize_focus();
    let mode = app.focus.mode();

    app.handle_mouse(left_click(body.x + 9, body.y + 1))
        .await
        .unwrap();

    assert_eq!(app.focus.mode(), mode, "the prompt keeps the keyboard");
    let editor = app.editor_session.as_ref().expect("a live editor");
    assert_eq!(
        (editor.cursor_row(), editor.cursor_col()),
        (0, 0),
        "a click behind the prompt must not move the caret"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn navigator_flush_tiles_and_file_footer_do_not_activate_rows() {
    let (_dir, mut app) = focus_test_app().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Files);
    render_app_text(&mut app, 160, 50);
    let tabs = app.navigator_tabs_area.unwrap();
    let plus = app.navigator_new_session_area.unwrap();
    let list = app.navigator_list_area.unwrap();
    let selected = app.workspace_files.explorer.selected_path.clone();
    // The footer row is not a tree row; clicking it changes nothing.
    app.handle_mouse(left_click(list.x + 1, list.bottom() - 1))
        .await
        .unwrap();
    assert_eq!(app.workspace_files.explorer.selected_path, selected);
    assert!(!app.current_workspace_is_file());
    // The `Sessions` heading hands the keyboard to the session list.
    app.handle_mouse(left_click(plus.x.saturating_sub(1), tabs.y))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
}

/// With the navigator column collapsed, the status-bar sessions chip is the
/// explicit entry into the session chooser.
#[tokio::test]
async fn clicking_the_collapsed_sessions_chip_opens_the_switcher() {
    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.sessions_chip_area = Some(ratatui::layout::Rect::new(0, 0, 12, 1));

    app.handle_mouse(left_click(2, 0)).await.unwrap();

    assert!(
        matches!(
            app.overlay,
            Some(crate::overlays::Overlay::SessionSwitcher { .. })
        ),
        "a chip click opens the session chooser"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}
