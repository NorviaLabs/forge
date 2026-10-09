//! Workspace navigation and files-sidebar visibility tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[test]
fn workspace_selection_tracks_resource_navigation_without_dropping_it() {
    let file = WorkspaceView::File(PathBuf::from("retained.rs"));
    let mut navigation = WorkspaceNavigation::default();
    assert_eq!(navigation.selected_tab(), WorkspaceTab::Agent);
    assert!(!navigation.resource_selected());
    navigation.navigate_to(file.clone());
    assert_eq!(navigation.selected_tab(), WorkspaceTab::Files);
    assert_eq!(navigation.current().unwrap().tab(), WorkspaceTab::Files);
    assert!(navigation.resource_selected());
    navigation.select_conversation();
    assert_eq!(navigation.selected_tab(), WorkspaceTab::Agent);
    assert_eq!(navigation.current(), None);
    assert!(!navigation.resource_selected());
    navigation.select_resource();
    assert_eq!(navigation.current(), Some(file.clone()));
    assert!(navigation.resource_selected());
    navigation.navigate_to(WorkspaceView::GithubIssues);
    assert_eq!(navigation.selected_tab(), WorkspaceTab::Git);
    assert_eq!(navigation.current().unwrap().tab(), WorkspaceTab::Git);
    assert_eq!(navigation.pop_previous_valid(|_| true), None);
    navigation.select_tab(WorkspaceTab::Files);
    assert_eq!(navigation.current(), Some(file));
    assert_eq!(navigation.selected_tab(), WorkspaceTab::Files);
    assert!(navigation.resource_selected());
    navigation.home();
    navigation.select_resource();
    assert_eq!(navigation.current(), None);
    assert!(!navigation.resource_selected());
}

#[test]
fn workspace_tabs_keep_independent_bounded_histories() {
    let mut navigation = WorkspaceNavigation::default();
    for i in 0..WORKSPACE_HISTORY_LIMIT + 4 {
        navigation.push_view(WorkspaceView::File(PathBuf::from(format!("{i}.rs"))));
    }
    let files = navigation.clone();
    navigation.navigate_to(WorkspaceView::Diff);
    navigation.navigate_to(WorkspaceView::GithubIssues);
    assert_eq!(navigation.history(), &[WorkspaceView::Diff]);
    navigation.select_tab(WorkspaceTab::Files);
    assert_eq!(navigation.current(), files.current());
    assert_eq!(navigation.history(), files.history());
    assert_eq!(navigation.history().len(), WORKSPACE_HISTORY_LIMIT);
    navigation.select_tab(WorkspaceTab::Git);
    assert_eq!(
        navigation.pop_previous_valid(|view| *view == WorkspaceView::Diff),
        Some(WorkspaceView::Diff)
    );
    navigation.select_tab(WorkspaceTab::Files);
    assert_eq!(navigation.history(), files.history());
    navigation.home();
    for tab in [WorkspaceTab::Files, WorkspaceTab::Git] {
        navigation.select_tab(tab);
        assert!(navigation.current().is_none());
        assert!(navigation.history().is_empty());
    }
}

#[tokio::test]
async fn git_tab_round_trip_preserves_dirty_editor_and_diff_state() {
    use crate::widgets::NavigatorTab;

    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("retained.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.open_file_in_editor(&path);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    let contents = editor.serialized_text();
    let cursor = editor.cursor_col();
    app.input.set_text("unsent draft");
    app.input.move_left();
    let draft_cursor = app.input.cursor;
    app.select_navigator_tab_from_row(NavigatorTab::Git);
    super::diff::settle_git(&mut app);
    app.diff_view.scroll = 7;
    let selected = app.diff_view.selected_path().map(Path::to_path_buf);

    for _ in 0..3 {
        app.select_navigator_tab_from_row(NavigatorTab::Files);
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::File(path.clone()))
        );
        let editor = app.editor_session.as_ref().unwrap();
        assert!(editor.is_dirty());
        assert_eq!(editor.serialized_text(), contents);
        assert_eq!(editor.cursor_col(), cursor);
        assert!(!app.explorer_dialog.is_open());
        app.enter_chat_composer();
        assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Agent);
        assert_eq!(app.input.text, "unsent draft");
        assert_eq!(app.input.cursor, draft_cursor);
        app.select_navigator_tab_from_row(NavigatorTab::Git);
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::Diff)
        );
        assert_eq!(app.diff_view.scroll, 7);
        assert_eq!(app.diff_view.selected_path(), selected.as_deref());
    }
    // Opening the same dirty file from Git must reveal it without reloading.
    app.open_file_in_editor(&path);
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Files);
    assert_eq!(
        app.editor_session.as_ref().unwrap().serialized_text(),
        contents
    );
    // Pointer navigation shares the same non-destructive transition.
    app.select_navigator_tab_from_row(NavigatorTab::Git);
    draw_app(&mut app, 160, 50);
    let (_, rect) = app
        .workspace_tab_areas
        .iter()
        .find(|(tab, _)| *tab == WorkspaceTab::Files)
        .copied()
        .unwrap();
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Files);
    assert_eq!(
        app.editor_session.as_ref().unwrap().serialized_text(),
        contents
    );
    assert!(app.editor_session.as_ref().unwrap().is_dirty());
    assert_eq!(fs::read_to_string(path).unwrap(), "fn main() {}\n");
}

#[tokio::test]
async fn workspace_tab_switch_keeps_github_subview_and_git_back_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("retained.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.open_file_in_editor(&path);
    app.select_workspace_tab(WorkspaceTab::Git);
    app.workspace_navigation
        .navigate_to(WorkspaceView::GithubIssues);
    app.github_view.scroll = 5;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.select_workspace_tab(WorkspaceTab::Git);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::GithubIssues)
    );
    assert_eq!(app.github_view.scroll, 5);
    app.close_github_issues();
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::Diff)
    );
    app.select_workspace_tab(WorkspaceTab::Files);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn retained_workspace_resources_move_with_each_session() {
    let (dir, mut app) = focus_test_app().await;
    let first = app.session_runtime.session_id;
    let second = uuid::Uuid::new_v4();
    for (id, name, scroll) in [(first, "first.rs", 7), (second, "second.rs", 11)] {
        let path = dir.path().join(name);
        fs::write(&path, "fn main() {}\n").unwrap();
        app.open_file_in_editor(&path);
        app.select_workspace_tab(WorkspaceTab::Git);
        app.diff_view.scroll = scroll;
        app.save_session_view_state(id);
    }
    for (id, name, scroll) in [(first, "first.rs", 7), (second, "second.rs", 11)] {
        app.restore_session_view_state(id);
        assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Git);
        assert_eq!(app.diff_view.scroll, scroll);
        app.select_workspace_tab(WorkspaceTab::Files);
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::File(dir.path().join(name)))
        );
        assert_eq!(
            app.source_viewer.path.as_ref().unwrap(),
            &dir.path().join(name).canonicalize().unwrap()
        );
        app.save_session_view_state(id);
    }
}

#[tokio::test]
async fn workspace_selection_and_draft_restore_with_session_state() {
    let (_dir, mut app) = focus_test_app().await;
    let first = app.session_runtime.session_id;
    let second = uuid::Uuid::new_v4();
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);
    app.focus_block(FocusBlock::Workspace);
    app.input.set_text("first draft");
    app.save_session_view_state(first);

    app.restore_session_view_state(second);
    assert!(!app.workspace_navigation.resource_selected());
    assert_eq!(app.workspace_navigation.current(), None);
    app.input.set_text("second draft");
    app.save_session_view_state(second);

    app.restore_session_view_state(first);
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Git);
    assert!(app.workspace_navigation.resource_selected());
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::Diff)
    );
    assert_eq!(app.input.text, "first draft");
    app.save_session_view_state(first);
    app.restore_session_view_state(second);
    assert!(!app.workspace_navigation.resource_selected());
    assert_eq!(app.input.text, "second draft");
}

#[tokio::test]
async fn file_browser_keeps_navigation_tabs_without_supervisor() {
    let (_dir, mut app) = focus_test_app().await;
    assert!(app.supervisor.is_none());
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "Inspect this project."));
    let rendered = render_app_text(&mut app, 120, 40);
    assert!(rendered.contains("Sessions"), "{rendered}");
    assert!(rendered.contains("Files"), "{rendered}");
    assert!(app.navigator_tabs_area.is_some());
    assert!(app.navigator_new_session_area.is_some());
}

#[tokio::test]
async fn header_and_tabs_keep_selection_distinct_from_keyboard_focus() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    let (_dir, mut app) = focus_test_app().await;
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "Inspect this project."));
    for (tab, focus) in [
        (WorkspaceTab::Agent, FocusBlock::Composer),
        (WorkspaceTab::Agent, FocusBlock::Sidebar),
        (WorkspaceTab::Files, FocusBlock::Files),
        (WorkspaceTab::Files, FocusBlock::Search),
    ] {
        app.select_workspace_tab(tab);
        app.focus_block(focus);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let status_bg = theme::status_bar().bg.unwrap();
        for x in crate::design::FRAME_INSET_X..120 - crate::design::FRAME_INSET_X {
            assert_eq!(buffer[(x, 0)].bg, status_bg, "header column {x}");
        }
        let (_, agent_tab) = app
            .workspace_tab_areas
            .iter()
            .find(|(candidate, _)| *candidate == WorkspaceTab::Agent)
            .copied()
            .unwrap();
        // The tab tile is a thin label row, not a filled block.
        assert_eq!(agent_tab.height, 1);
        let row: String = (agent_tab.x..agent_tab.right())
            .map(|x| buffer[(x, agent_tab.y)].symbol())
            .collect();
        assert!(
            row.starts_with(if focus == FocusBlock::Sidebar {
                "> Agent"
            } else {
                "  Agent"
            }),
            "{row:?}"
        );
        // Selected-session identity sits on the row above the tabs.
        let header: String = (agent_tab.x..agent_tab.right())
            .map(|x| buffer[(x, agent_tab.y.saturating_sub(1))].symbol())
            .collect();
        assert!(!header.trim().is_empty(), "header identity: {header:?}");
        for x in agent_tab.x..agent_tab.right() {
            assert_eq!(buffer[(x, agent_tab.y)].bg, theme::panel().bg.unwrap());
        }
        // Selection underlines the selected tab's label and only that one.
        let underlined = |tab: ratatui::layout::Rect| {
            (tab.x + 2..tab.x + 7)
                .all(|x| buffer[(x, tab.y)].modifier.contains(Modifier::UNDERLINED))
        };
        assert_eq!(
            underlined(agent_tab),
            tab == WorkspaceTab::Agent,
            "Agent underline for {tab:?}"
        );
        if tab == WorkspaceTab::Files {
            let (_, files_tab) = app
                .workspace_tab_areas
                .iter()
                .find(|(candidate, _)| *candidate == WorkspaceTab::Files)
                .copied()
                .unwrap();
            assert!(underlined(files_tab), "Files tab is selected");
            let area = app.navigator_list_area.unwrap();
            let title: String = (area.x..area.right())
                .map(|x| buffer[(x, area.y + 2)].symbol())
                .collect();
            assert!(title.contains("Search files..."), "{title:?}");
            assert!(!title.contains("> Files") && !title.contains("> Search"));
        }
    }
}

#[tokio::test]
async fn workspace_tab_selection_remains_visible_without_keyboard_focus() {
    use ratatui::style::Modifier;

    let (_dir, mut app) = focus_test_app().await;
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);
    app.bottom_panel.open = true;
    app.focus_block(FocusBlock::BottomPanel);
    for (width, height) in [(80, 18), (120, 40), (160, 50)] {
        let buffer = render_app_buffer(&mut app, width, height);
        assert_eq!(app.workspace_tab_areas.len(), 3);
        for (tab, area) in &app.workspace_tab_areas {
            let y = area.y;
            // Selection survives even if NO_COLOR strips every RGB value.
            assert_eq!(
                buffer[(area.x + 2, y)]
                    .modifier
                    .contains(Modifier::UNDERLINED),
                *tab == WorkspaceTab::Git,
                "{width}x{height}: {tab:?}"
            );
        }
        assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Git);
        assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    }
}

#[tokio::test]
async fn selected_navigator_tab_background_stays_inside_shared_frame() {
    use crate::widgets::NavigatorTab;
    use ratatui::backend::TestBackend;

    let (_dir, mut app, handle) = super::multi_task::app_with_supervisor().await;
    app.focus_block(FocusBlock::TaskStrip);
    for (width, height) in [(120, 40), (160, 50)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let area = app.navigator_tabs_area.expect("visible navigator tabs");
        assert_eq!(area.height, crate::design::NAVIGATOR_TAB_H);
        let rect =
            crate::widgets::navigator::navigator_tab_rects(area, app.navigator_git_available())
                .into_iter()
                .find_map(|(candidate, rect)| (candidate == NavigatorTab::Sessions).then_some(rect))
                .unwrap();
        let buffer = terminal.backend().buffer();
        // Flush tile: the active ground fills its whole tile edge to edge.
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                assert_eq!(
                    buffer[(x, y)].bg,
                    theme::accent_soft_bg(),
                    "{width}x{height} x={x} rect={rect:?}"
                );
            }
        }
        // The active ground never spills past the tile's own frame.
        for x in area.x..area.right() {
            if x < rect.x || x >= rect.right() {
                assert_ne!(buffer[(x, area.y)].bg, theme::accent_soft_bg());
            }
        }
        let label_row: String = (area.x..area.right())
            .map(|x| buffer[(x, area.y)].symbol())
            .collect();
        assert!(label_row.contains("Sessions"), "{label_row}");
    }
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn top_bar_keeps_one_row_at_all_frame_heights() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    for height in [40, 28, 18] {
        let rendered = render_app_text(&mut app, 120, height);
        let lines: Vec<_> = rendered.lines().collect();
        assert!(lines[0].contains("FORGE"), "{rendered}");
        assert_eq!(
            lines.iter().filter(|line| line.contains("FORGE")).count(),
            1
        );
        assert!(!lines[0].contains('╭'), "{rendered}");
    }
}

#[tokio::test]
async fn top_bar_centers_brand_and_workspace_identity() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    for (width, height) in [(120, 40), (100, 28), (80, 18)] {
        let rendered = render_app_text(&mut app, width, height);
        let row = rendered.lines().next().expect("status row");
        assert!(row.contains("FORGE"), "{width}x{height}: {row:?}");
        let leading = row.chars().take_while(|c| *c == ' ').count();
        let trailing = row.chars().rev().take_while(|c| *c == ' ').count();
        // Centered: the identity leaves an even gutter on both sides. The
        // one-column tolerance comes from an odd leftover split.
        assert!(
            leading.abs_diff(trailing) <= 1,
            "{width}x{height}: leading {leading} vs trailing {trailing} in {row:?}"
        );
    }
}

#[tokio::test]
async fn composer_centers_placeholder_and_short_draft_at_comfortable_heights() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    for started in [false, true] {
        if started {
            app.session_runtime
                .messages
                .push(Message::new(MessageRole::User, "Inspect this project."));
        }
        for text in ["", "A short task"] {
            app.input.set_text(text);
            for (width, height) in [(120, 40), (80, 28), (80, 18)] {
                draw_app(&mut app, width, height);
                let area = app.composer_area.expect("visible composer");
                let cursor =
                    crate::widgets::input::composer_cursor_position(&app.input, area, None)
                        .expect("visible caret");
                let compact = height < crate::design::COMPACT_FRAME_H;
                assert_eq!(area.height, if compact { 2 } else { 4 });
                assert_eq!(cursor.1, area.y + if compact { 1 } else { 2 });
            }
        }
    }
}

#[tokio::test]
async fn start_keeps_enabled_file_navigation_visible_without_a_turn() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 120, 40);
    assert!(app.navigator_list_area.is_some());
    assert!(!app.start_prompt_rows.is_empty());
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    assert!(!app.pending_turn.has_prompt());

    app.workspace_files.visible = false;
    draw_app(&mut app, 120, 40);
    assert!(app.navigator_list_area.is_none());
    assert!(!app.start_prompt_rows.is_empty());

    app.workspace_files.visible = true;
    draw_app(&mut app, 80, 18);
    assert!(app.navigator_list_area.is_none());
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Files);
    draw_app(&mut app, 80, 18);
    assert!(app.navigator_list_area.is_some());
}

#[tokio::test]
async fn first_task_remains_reachable_beside_requested_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("Retain the first requirement.");
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Files);
    draw_app(&mut app, 120, 40);
    assert!(app.navigator_list_area.is_some());
    assert!(
        app.start_prompt_rows.is_empty(),
        "the start prompt belongs to Agent"
    );
    draw_app(&mut app, 80, 18);
    assert!(app.navigator_list_area.is_some());
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert_eq!(app.input.text, "Retain the first requirement.");
    // Returning to Agent restores the home prompt with the draft intact.
    app.select_workspace_tab(WorkspaceTab::Agent);
    draw_app(&mut app, 120, 40);
    assert!(!app.start_prompt_rows.is_empty());
    assert_eq!(app.input.text, "Retain the first requirement.");
}

#[tokio::test]
async fn system_context_keeps_the_start_placeholder_until_a_visible_turn() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.hint = COMPOSER_OPENER.into();
    app.sync_composer_placeholder();
    assert_eq!(app.input.hint, COMPOSER_OPENER);
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "Inspect this project."));
    draw_app(&mut app, 80, 18);
    app.sync_composer_placeholder();
    assert_eq!(app.input.hint, COMPOSER_WORKING);
}

#[tokio::test]
async fn starter_choice_preserves_the_draft_without_submitting() {
    let (_dir, mut app) = focus_test_app().await;
    app.input.set_text("Keep the original requirement.");
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 120, 40);
    app.focus_block(FocusBlock::Sidebar);
    draw_app(&mut app, 120, 40);
    assert!(
        !app.start_prompt_rows.is_empty(),
        "starter actions must be reachable before the first turn"
    );
    app.handle_key(event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.input.text,
        "Keep the original requirement.\nReview the changes in this workspace"
    );
    assert!(!app.pending_turn.has_prompt());
    assert_eq!(app.focus.block(), FocusBlock::Composer);
}

#[tokio::test]
async fn resized_start_draft_retains_text_and_a_reachable_caret() {
    let (_dir, mut app) = focus_test_app().await;
    let draft = "Keep this complete requirement and the Unicode path λ/東京.rs.\n".repeat(8);
    app.input.set_text(draft.clone());
    app.focus_block(FocusBlock::Composer);
    for (width, height) in [(160, 50), (80, 18), (120, 40), (80, 24)] {
        draw_app(&mut app, width, height);
        let area = app.composer_area.expect("visible prompt");
        let cursor = crate::widgets::input::composer_cursor_position(&app.input, area, None)
            .expect("retained caret remains visible");
        assert!(area.contains(cursor.into()));
        assert_eq!(app.input.text, draft);
        assert!(app.focus_availability().contains(app.focus.block()));
    }
}

#[tokio::test]
async fn workspace_navigation_starts_empty_at_home() {
    let (_dir, app) = focus_test_app().await;

    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
}

#[tokio::test]
async fn workspace_navigation_pushes_file_and_replaces_file_resource() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    fs::write(&first, "fn a() {}\n").unwrap();
    fs::write(&second, "fn b() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(first.clone()))
    );
    // Nothing to push onto history yet — the home state (`None`) isn't a
    // concrete view, so opening the first file from home leaves history empty.
    assert_eq!(app.workspace_navigation.history(), Vec::new());

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(second))
    );
    assert_eq!(app.workspace_navigation.history(), Vec::new());
}

#[tokio::test]
async fn workspace_back_from_a_file_returns_home() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("stale.rs");
    fs::write(&path, "fn stale() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
}

#[tokio::test]
async fn workspace_home_returns_to_empty_and_clears_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
}

#[tokio::test]
async fn workspace_home_requires_a_dirty_editor_decision() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-home.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());

    // DESIGN-017: the conflict dialog carries the shared `> Title` modal
    // grammar, not a padded bare title.
    let rendered = render_app_text(&mut app, 120, 35);
    assert!(
        rendered.contains("> Unsaved Changes"),
        "conflict dialog title:\n{rendered}"
    );
    // DESIGN-018 matrix: the title and choices survive the 80×24 minimum
    // frame too.
    let narrow = render_app_text(&mut app, 80, 24);
    assert!(
        narrow.contains("> Unsaved Changes"),
        "conflict dialog title at 80x24:\n{narrow}"
    );
    assert!(
        narrow.contains("d discard"),
        "conflict dialog choices at 80x24:\n{narrow}"
    );

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.workspace_navigation.history().is_empty());
}

#[tokio::test]
async fn alt_navigation_requires_a_dirty_editor_decision() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-nav.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));

    app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::DirtyExit)
    ));
    assert!(app.current_workspace_is_file());
}

#[tokio::test]
async fn overlay_open_and_close_do_not_mutate_workspace_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.overlay = Some(Overlay::welcome());
    app.execute_semantic_command(SemanticCommand::CloseOverlay)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation, before);
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn files_visibility_is_independent_of_workspace_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.workspace_files.visible = true;

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert!(app.workspace_files.visible);
    assert_eq!(app.workspace_navigation.current(), None);
}

#[tokio::test]
async fn files_panel_is_open_by_default() {
    let (_dir, app) = focus_test_app().await;

    assert!(app.workspace_files.visible);
    assert_eq!(app.focus.block(), FocusBlock::Composer);
}

#[tokio::test]
async fn narrow_files_navigation_reveals_the_pane_without_mutating_preference() {
    let (_dir, mut app) = focus_test_app().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Files);

    let _narrow = render_app_text(&mut app, 80, 24);
    assert!(
        app.workspace_files.visible,
        "auto-collapse must not persist close"
    );
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(
        app.navigator_list_area.is_some(),
        "the key owner must be painted"
    );

    let _wide = render_app_text(&mut app, 160, 50);
    assert!(app.workspace_files.visible);
}

#[tokio::test]
async fn switching_workspace_panes_preserves_unsaved_work_and_reading_position() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("retry.rs");
    fs::write(&path, "fn retry() {}\n").unwrap();
    app.session_runtime.messages.push(Message::new(
        MessageRole::Assistant,
        (0..100)
            .map(|i| format!("History line {i}\n"))
            .collect::<String>(),
    ));
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    app.editor_session
        .as_mut()
        .unwrap()
        .handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    let buffer = app.editor_session.as_ref().unwrap().serialized_text();
    let cursor = app.editor_session.as_ref().unwrap().cursor_col();
    app.input.set_text("Keep this unsent draft");
    app.input.move_left();
    let draft_cursor = app.input.cursor;
    app.scroll_conversation_up(8);

    for (width, height) in [(80, 18), (120, 40), (160, 50), (80, 18)] {
        render_app_text(&mut app, width, height);
        app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
            .await
            .unwrap();
        render_app_text(&mut app, width, height);
        assert_eq!(app.focus.block(), FocusBlock::Sidebar);
        assert!(
            app.conversation_area.is_some(),
            "conversation input must have a visible owner"
        );
        let reading_position = app.conversation_view.scroll;
        app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
            .await
            .unwrap();
        render_app_text(&mut app, width, height);
        assert_eq!(app.focus.block(), FocusBlock::Workspace);
        assert!(
            app.editor_area.is_some(),
            "editor input must have a visible owner"
        );
        assert_eq!(
            app.workspace_navigation.current(),
            Some(WorkspaceView::File(path.clone()))
        );
        // Pointer tabs use the same retained state as the keyboard switch.
        for (tab, expect_focus) in [
            (WorkspaceTab::Agent, FocusBlock::Composer),
            (WorkspaceTab::Files, FocusBlock::Search),
        ] {
            let (_, rect) = app
                .workspace_tab_areas
                .iter()
                .find(|(candidate, _)| *candidate == tab)
                .copied()
                .unwrap();
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: rect.x + 3,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            })
            .await
            .unwrap();
            render_app_text(&mut app, width, height);
            assert_eq!(app.workspace_navigation.selected_tab(), tab);
            assert_eq!(app.focus.block(), expect_focus);
        }
        assert!(app.workspace_navigation.history().is_empty());
        assert_eq!(app.conversation_view.scroll, reading_position);
        assert_eq!(app.input.text, "Keep this unsent draft");
        assert_eq!(app.input.cursor, draft_cursor);
        let editor = app.editor_session.as_ref().unwrap();
        assert!(editor.is_dirty());
        assert_eq!(editor.serialized_text(), buffer);
        assert_eq!(editor.cursor_col(), cursor);
        assert!(
            !app.explorer_dialog.is_open(),
            "view switching is not a destructive exit"
        );
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "fn retry() {}\n");
}

#[tokio::test]
async fn a_pending_approval_remains_reachable_over_narrow_inspection_and_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.input.set_text("Keep this unsent draft");
    app.bottom_panel.open = true;
    set_pending_hitl(&mut app, direct_hitl_payload("pending", "/tmp/x"));
    app.sync_approval_focus();

    for block in [FocusBlock::Workspace, FocusBlock::Search] {
        app.focus_block(block);
        render_app_text(&mut app, 80, 18);
        assert_eq!(app.focus.block(), FocusBlock::Approval);
        assert!(app.conversation_area.is_some());
        assert!(
            !app.option_rects.is_empty(),
            "the decision must be actionable"
        );
        assert!(app.session_view.pending_hitl.is_some());
        assert_eq!(app.input.text, "Keep this unsent draft");
        assert!(app.workspace_navigation.has_retained_resource());
        assert_eq!(
            app.source_viewer.path.as_ref(),
            Some(&path.canonicalize().unwrap())
        );
    }
}

#[tokio::test]
async fn narrow_file_search_is_reachable_and_returns_to_the_retained_editor() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.navigator_list_area.is_some());
    assert!(app.workspace_files.explorer.search_focused);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path.clone()))
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.editor_area.is_some());
    assert!(!app.explorer_dialog.is_open());
}

#[tokio::test]
async fn compact_navigation_and_inspection_can_reclaim_a_retained_terminals_rows() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.bottom_panel.open = true;
    app.input.set_text("Keep this draft");

    let rendered = render_app_text(&mut app, 80, 18);
    assert!(
        rendered.contains("fn main()"),
        "inspection must remain usable"
    );
    assert!(app.terminal_area.is_none());
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert!(app.navigator_list_area.is_some_and(|area| !area.is_empty()));
    assert!(app.workspace_files.explorer.search_focused);

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.terminal_area.is_some_and(|area| !area.is_empty()));
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.editor_area.is_some());
    assert!(app.terminal_area.is_none());
    assert!(app.bottom_panel.open, "presentation must retain the shell");
    assert_eq!(app.input.text, "Keep this draft");
    app.toggle_bottom_panel();
    render_app_text(&mut app, 80, 18);
    assert_eq!(app.focus.block(), FocusBlock::BottomPanel);
    assert!(app.terminal_area.is_some());
    app.toggle_bottom_panel();
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn workspace_switch_does_not_interrupt_a_modal_or_an_editor_command() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    app.editor_command = Some(":w".into());
    app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.editor_command.as_deref(), Some(":w"));
    assert_eq!(app.workspace_navigation, before);
    app.editor_command = None;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::F(6), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.overlay.is_some());
    assert_eq!(app.workspace_navigation, before);
}

#[tokio::test]
async fn resizing_conversation_moves_its_boundary_and_cancel_restores_the_layout() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);
    app.focus_block(FocusBlock::Files);
    render_app_text(&mut app, 160, 50);
    let initial = app.pane_resize.preferences;
    let original_boundary = app.pane_resize.conversation_separator.unwrap();
    app.input.set_text("Keep this draft during resizing");

    app.begin_resize_mode();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    render_app_text(&mut app, 160, 50);
    assert!(app.pane_resize.conversation_separator.unwrap().x > original_boundary.x);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.pane_resize.preferences, initial);
    render_app_text(&mut app, 160, 50);

    assert!(app.start_mouse_resize(original_boundary.x, original_boundary.y));
    assert!(app.drag_mouse_resize(original_boundary.x + 4, original_boundary.y));
    render_app_text(&mut app, 160, 50);
    assert!(app.pane_resize.conversation_separator.unwrap().x > original_boundary.x);
    assert!(app.finish_mouse_resize());
    assert_eq!(app.input.text, "Keep this draft during resizing");
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::Diff)
    );
}

#[tokio::test]
async fn files_explicit_close_remains_closed_after_resizing() {
    let (_dir, mut app) = focus_test_app().await;
    // Ctrl+E from the explorer returns to Agent; resizing must not reopen Files.
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Search);
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Agent);
    let _narrow = render_app_text(&mut app, 80, 24);
    let _wide = render_app_text(&mut app, 160, 50);
    assert_eq!(app.workspace_navigation.selected_tab(), WorkspaceTab::Agent);
}

#[tokio::test]
async fn files_visibility_persists_per_repository() {
    let (_fake_home, _home_guard) = fake_home_guard();
    let (dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = false;
    app.save_ui_state();

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
    assert!(!restored.workspace_files.visible);

    let (_other_dir, other) = focus_test_app().await;
    assert!(
        other.workspace_files.visible,
        "Files preference must not leak across repositories"
    );
}

#[tokio::test]
async fn opening_file_does_not_open_closed_files_preference() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.workspace_files.visible = false;

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();

    assert!(!app.workspace_files.visible);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

// ---------------------------------------------------------------------------
// Ctrl+E must never hand keystrokes to the editor.
//
// The original failure: with a file open and the Files pane already visible
// but unfocused, Ctrl+E closed the pane and returned focus to the editor.
// The editor is modal, so typing `index.rs` ran `i` (INSERT) and wrote
// "ndex.rs" into the buffer. Nothing on screen indicated focus had moved, and
// the Unsaved Changes dialog defaults to Save.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ctrl_e_reaches_files_instead_of_closing_when_focus_is_elsewhere() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert!(
        app.workspace_files.visible,
        "Ctrl+E must take you to Files, not close the pane you were heading for"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::Search,
        "focus must land in the file search, not the editor"
    );
}

/// Pressing it again, from the explorer, returns to Agent — the toggle is
/// intact, it is just no longer reachable by accident.
#[tokio::test]
async fn ctrl_e_from_the_explorer_still_closes_the_pane() {
    let (_dir, mut app) = focus_test_app().await;
    app.select_workspace_tab(WorkspaceTab::Files);
    app.focus_block(FocusBlock::Search);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.selected_tab(),
        WorkspaceTab::Agent,
        "the toggle must still work"
    );
    assert_ne!(
        app.focus.block(),
        FocusBlock::Workspace,
        "closing must not drop focus into the modal editor"
    );
}

/// The end-to-end regression: the exact keystrokes that used to corrupt a
/// buffer must now filter files instead.
#[tokio::test]
async fn typing_after_ctrl_e_filters_files_and_never_edits_the_buffer() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    for ch in "index.rs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }

    // If any keystroke had reached the editor the filter would be empty, and
    // `i` would have switched the buffer into INSERT.
    assert_eq!(
        app.workspace_files.explorer.search_query, "index.rs",
        "every keystroke must reach the file filter"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::Search,
        "focus must not drift into the editor mid-typing"
    );
}

/// A frame wide enough to hold all three resizable boundaries at once: the
/// files column, the conversation/resource seam, and the terminal band.
async fn resizable_pane_fixture() -> (TempDir, TuiApp) {
    let (dir, mut app) = focus_test_app().await;
    // A message dismisses the start splash, so the real workspace layout (and
    // not the onboarding group) is what gets measured.
    app.session_runtime
        .messages
        .push(Message::new(MessageRole::User, "layout fixture"));
    app.workspace_files.visible = true;
    app.navigate_to_workspace_view(WorkspaceView::Diff);
    app.bottom_panel.open = true;
    (dir, app)
}

/// Positions along `boundary`'s seam, from the seam's own origin, that carry a
/// resize grip. A vertical seam answers in rows, a horizontal one in columns.
fn grip_offsets(
    app: &TuiApp,
    buffer: &ratatui::buffer::Buffer,
    boundary: ResizeBoundary,
    glyph: &str,
) -> Vec<u16> {
    let seam = app.resize_seam(boundary).expect("pane is resizable");
    let vertical = seam.width == 1;
    let cell = |offset: u16| {
        if vertical {
            (seam.x, seam.y + offset)
        } else {
            (seam.x + offset, seam.y)
        }
    };
    (0..if vertical { seam.height } else { seam.width })
        .filter(|offset| {
            let (x, y) = cell(*offset);
            buffer[(x, y)].symbol() == glyph
        })
        .collect()
}

#[tokio::test]
async fn resizable_seams_show_a_centred_grip_marker() {
    let (_dir, mut app) = resizable_pane_fixture().await;
    let buffer = render_app_buffer(&mut app, 160, 40);

    for (boundary, glyph) in [
        (ResizeBoundary::Files, "\u{250a}"),
        (ResizeBoundary::Conversation, "\u{250a}"),
        (ResizeBoundary::BottomPanel, "\u{2508}"),
    ] {
        let seam = app.resize_seam(boundary).unwrap();
        let extent = seam.width.max(seam.height);
        let grip = grip_offsets(&app, &buffer, boundary, glyph);
        assert_eq!(grip.len(), 5, "{boundary:?} grip length");
        // Margins on both sides: the grip names the seam without turning into
        // the full-length rule that would double the pane's own border.
        assert!(grip[0] >= 1, "{boundary:?} grip starts inside the seam");
        assert!(
            grip[4] <= extent - 2,
            "{boundary:?} grip ends inside the seam"
        );
        // Centred, within the odd/even rounding of the seam's extent.
        let centre = (grip[0] + grip[4]) / 2;
        assert!(
            centre.abs_diff((extent - 1) / 2) <= 1,
            "{boundary:?} grip is centred (at {centre} of {extent})"
        );
    }

    // At rest the grip carries the pane frames' own border weight, so the
    // marker is never the faintest thing in the seam.
    let files_seam = app.resize_seam(ResizeBoundary::Files).unwrap();
    let cell = &buffer[(files_seam.x, files_seam.y + files_seam.height / 2)];
    assert_eq!(cell.symbol(), "\u{250a}");
    assert_eq!(cell.style().fg, crate::theme::border().fg);
}

#[tokio::test]
async fn resizing_a_boundary_emphasises_its_grip() {
    let (_dir, mut app) = resizable_pane_fixture().await;
    // The seams are measured during a draw, and key routing reads them.
    render_app_text(&mut app, 160, 40);
    app.begin_resize_mode();
    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.pane_resize.interaction.unwrap().boundary,
        Some(ResizeBoundary::Conversation)
    );

    let buffer = render_app_buffer(&mut app, 160, 40);
    let seam = app.resize_seam(ResizeBoundary::Conversation).unwrap();
    let cell = &buffer[(seam.x, seam.y + seam.height / 2)];
    assert_eq!(cell.style().fg, Some(crate::theme::accent_color()));
    assert!(cell
        .style()
        .add_modifier
        .contains(ratatui::style::Modifier::BOLD));
}

/// Hover is the pointer's own preview of the drag, so pointing at a seam
/// brightens its grip without entering resize mode or moving focus.
#[tokio::test]
async fn hovering_a_seam_emphasises_its_grip() {
    let (_dir, mut app) = resizable_pane_fixture().await;
    render_app_text(&mut app, 160, 40);
    let seam = app.resize_seam(ResizeBoundary::Conversation).unwrap();
    let focus_before = app.focus.block();

    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: seam.x,
        row: seam.y + seam.height / 2,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert_eq!(app.hover_resize, Some(ResizeBoundary::Conversation));
    assert!(app.pane_resize.interaction.is_none(), "hover never drags");
    assert_eq!(app.focus.block(), focus_before, "hover never moves focus");

    let buffer = render_app_buffer(&mut app, 160, 40);
    let cell = &buffer[(seam.x, seam.y + seam.height / 2)];
    assert_eq!(cell.style().fg, Some(crate::theme::accent_color()));

    // Moving off the seam restores the resting marker.
    app.handle_mouse(event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: seam.x.saturating_sub(20),
        row: seam.y,
        modifiers: KeyModifiers::NONE,
    })
    .await
    .unwrap();
    assert_eq!(app.hover_resize, None);
    let buffer = render_app_buffer(&mut app, 160, 40);
    assert_eq!(
        buffer[(seam.x, seam.y + seam.height / 2)].style().fg,
        crate::theme::border().fg
    );
}
