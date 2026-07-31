//! Pointer-input integration tests for [`TuiApp`].
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn mouse_click_pane_and_composer_focus() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 120, 30);

    let (x, y) = hit_point(&app, |target| {
        matches!(target, HitTarget::Pane(FocusBlock::Workspace))
    });
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    draw_app(&mut app, 120, 30);
    let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
}

#[tokio::test]
async fn mouse_click_file_row_selects_and_chevron_toggles() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 140, 30);

    let src = dir.path().join("src").canonicalize().unwrap();
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &src,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(src.as_path())
    );

    draw_app(&mut app, 140, 30);
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::DirectoryChevron(p) if p == path),
        &src,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert!(app
        .file_explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "lib.rs"));
}

#[tokio::test]
async fn mouse_click_bottom_tab_visible_control_emits_once() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel(Some(BottomPanelTab::Run));
    draw_app(&mut app, 120, 40);
    let (x, y) = hit_point(&app, |target| {
        matches!(
            target,
            HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(BottomPanelTab::Activity))
        )
    });
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
}

#[tokio::test]
async fn mouse_wheel_scrolls_hovered_pane_without_focus_change() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Composer);
    draw_app(&mut app, 120, 30);
    let (x, y) = hit_point(&app, |target| {
        matches!(target, HitTarget::Pane(FocusBlock::Workspace))
    });
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.chat_scroll, 3);
}

#[tokio::test]
async fn mouse_overlay_blocks_underlying_targets() {
    let (_dir, mut app) = focus_test_app().await;
    let payload = HitlPayload {
        call_id: "call-1".into(),
        tool: "write".into(),
        args_redacted: json!({"path": "src/main.rs"}),
        reason: "Edit requires approval".into(),
    };
    app.open_hitl_overlay(payload);
    draw_app(&mut app, 120, 30);
    let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert!(matches!(app.overlay, Some(Overlay::Hitl { .. })));
}

#[tokio::test]
async fn mouse_disabled_ignores_pointer_but_keeps_keyboard() {
    let (_dir, mut app) = focus_test_app().await;
    app.runtime.mouse_capture = false;
    draw_app(&mut app, 120, 30);
    let (x, y) = hit_point(&app, |target| {
        matches!(target, HitTarget::Pane(FocusBlock::Workspace))
    });
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
}

#[tokio::test]
async fn mouse_stale_regions_are_ignored_after_resize_or_list_mutation() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    app.file_explorer.selected_path = Some(dir.path().join("src").canonicalize().unwrap());
    app.file_explorer.expand_selected();
    draw_app(&mut app, 140, 30);

    let lib = dir.path().join("src/lib.rs").canonicalize().unwrap();
    let stale_lib_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &lib,
    );
    app.file_explorer.selected_path = Some(dir.path().join("src").canonicalize().unwrap());
    app.file_explorer.collapse_selected();
    app.file_explorer.selected_path = app.file_explorer.root_path().map(Path::to_path_buf);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        stale_lib_point.0,
        stale_lib_point.1,
    ))
    .await
    .unwrap();
    assert_ne!(
        app.file_explorer.selected_path.as_deref(),
        Some(lib.as_path())
    );

    draw_app(&mut app, 140, 30);
    let (x, y) = hit_point(&app, |target| {
        matches!(target, HitTarget::Pane(FocusBlock::Workspace))
    });
    app.invalidate_hit_regions();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_ne!(app.focus.block, FocusBlock::Workspace);
}

#[tokio::test]
async fn mouse_unsupported_buttons_and_80x24_regions_are_safe() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 80, 24);
    let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
    app.focus_block(FocusBlock::Workspace);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
}

#[tokio::test]
async fn mouse_double_click_same_file_opens_it_like_enter() {
    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 140, 30);

    let canonical = file.canonicalize().unwrap();
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &canonical,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(canonical.clone())
    );
    assert_eq!(app.source_viewer.path.as_deref(), Some(canonical.as_path()));

    let (enter_dir, mut enter_app) = focus_test_app().await;
    let enter_file = enter_dir.path().join("main.rs");
    fs::write(&enter_file, "fn main() {}\n").unwrap();
    enter_app.file_explorer.refresh_workspace();
    enter_app.files_visible = true;
    enter_app.file_explorer.selected_path = Some(enter_file.canonicalize().unwrap());
    assert_eq!(
        enter_app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::OpenSelectedEntry)
    );
    enter_app
        .execute_semantic_command(SemanticCommand::OpenSelectedEntry)
        .await
        .unwrap();
    assert!(matches!(
        enter_app.workspace_navigation.current,
        WorkspaceView::File(_)
    ));
    assert!(enter_app.source_viewer.path.is_some());
}

#[tokio::test]
async fn mouse_double_click_slow_or_different_rows_only_selects() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("first.rs");
    let second = dir.path().join("second.rs");
    fs::write(&first, "fn first() {}\n").unwrap();
    fs::write(&second, "fn second() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 140, 30);

    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    let first_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &first,
    );
    let second_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &second,
    );

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        first_point.0,
        first_point.1,
    ))
    .await
    .unwrap();
    app.pending_double_click.as_mut().unwrap().timestamp =
        Instant::now() - DOUBLE_CLICK_THRESHOLD - Duration::from_millis(1);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        first_point.0,
        first_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(first.as_path())
    );

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        second_point.0,
        second_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(second.as_path())
    );
}

#[tokio::test]
async fn mouse_double_click_cancels_on_scroll_resize_list_or_modal_change() {
    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 140, 30);

    let file = file.canonicalize().unwrap();
    let file_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &file,
    );
    let workspace_point = hit_point(&app, |target| {
        matches!(target, HitTarget::Pane(FocusBlock::Workspace))
    });

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    app.handle_mouse(mouse(
        MouseEventKind::ScrollUp,
        workspace_point.0,
        workspace_point.1,
    ))
    .await
    .unwrap();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.clear_pending_double_click();

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    app.invalidate_hit_regions();
    draw_app(&mut app, 140, 30);
    let file_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &file,
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.clear_pending_double_click();

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    app.note_workspace_changed();
    draw_app(&mut app, 140, 30);
    let file_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &file,
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.clear_pending_double_click();

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    app.overlay = Some(Overlay::welcome());
    app.invalidate_hit_regions();
    app.overlay = None;
    draw_app(&mut app, 140, 30);
    let file_point = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &file,
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        file_point.0,
        file_point.1,
    ))
    .await
    .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn mouse_double_click_uses_semantic_identity_for_truncated_names() {
    let (dir, mut app) = focus_test_app().await;
    let long_name = format!("{}-forge-mouse.rs", "very-long-name".repeat(8));
    let file = dir.path().join(long_name);
    fs::write(&file, "fn main() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 120, 30);

    let file = file.canonicalize().unwrap();
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &file,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();

    assert_eq!(app.source_viewer.path.as_deref(), Some(file.as_path()));
}

#[tokio::test]
async fn mouse_double_click_folder_row_toggles_once() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 140, 30);

    let src = dir.path().join("src").canonicalize().unwrap();
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &src,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();

    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(src.as_path())
    );
    assert!(app
        .file_explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "lib.rs"));
}

#[tokio::test]
async fn mouse_double_click_controls_do_not_gain_row_activation() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel(Some(BottomPanelTab::Run));
    draw_app(&mut app, 120, 40);
    let (x, y) = hit_point(&app, |target| {
        matches!(
            target,
            HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(BottomPanelTab::Activity))
        )
    });
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();

    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    assert!(app.pending_double_click.is_none());
}

#[tokio::test]
async fn mouse_double_click_cannot_bypass_delete_confirmation() {
    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("delete-me.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    app.file_explorer.selected_path = Some(file.canonicalize().unwrap());
    app.open_explorer_delete_dialog();
    draw_app(&mut app, 120, 30);

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 10))
        .await
        .unwrap();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 10))
        .await
        .unwrap();

    assert!(matches!(
        app.explorer_dialog,
        Some(ExplorerDialog::ConfirmDelete { .. })
    ));
    assert!(file.exists());
}

#[tokio::test]
async fn edge_mouse_disabled_keeps_keyboard_workflow_and_no_mouse_hint() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.runtime.mouse_capture = false;
    app.files_visible = true;
    app.file_explorer.refresh_workspace();
    let canonical = path.canonicalize().unwrap();
    app.file_explorer.selected_path = Some(canonical.clone());
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(canonical)
    );
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        !rendered.to_ascii_lowercase().contains("mouse"),
        "mouse-disabled mode should not reserve mouse-specific hints:\n{rendered}"
    );
}

#[tokio::test]
async fn edge_hit_target_invalidated_cancels_double_click_state() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.files_visible = true;
    draw_app(&mut app, 120, 30);
    let canonical = path.canonicalize().unwrap();
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &canonical,
    );

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();
    assert!(app.pending_double_click.is_some());
    app.invalidate_hit_regions();
    assert!(app.pending_double_click.is_none());
    draw_app(&mut app, 120, 30);
    let (x, y) = hit_point_for_path(
        &app,
        |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
        &canonical,
    );
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn edge_end_to_end_recovery_flow_mouse_enabled_and_disabled() {
    for mouse_capture in [true, false] {
        let (dir, mut app) = focus_test_app().await;
        app.runtime.mouse_capture = mouse_capture;
        let path = dir.path().join("flow.rs");
        fs::write(&path, "fn flow() {}\n").unwrap();

        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("flow.rs"), GitStatusKind::Modified);
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );

        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(path.clone())
        );

        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );

        app.run.draft.command_input = "cargo test".into();
        app.run_current_draft();
        let run_id = app.run.current.as_ref().unwrap().id.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_exec.rx = Some(rx);
        tx.send(RunEvent::Finished {
            exit_code: Some(101),
            success: false,
        })
        .unwrap();
        app.poll_run();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );

        app.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Run(run_id.clone())
        );
        app.execute_semantic_command(SemanticCommand::GoBack)
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );

        fs::write(&path, "fn flow() {}\nfn changed() {}\n").unwrap();
        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("extra.rs"), GitStatusKind::Added);
        app.file_change_tx
            .send(FileChangeEvent { path: path.clone() })
            .unwrap();
        app.poll_file_changes();
        assert!(app.diff_snapshot.stale);

        app.execute_semantic_command(SemanticCommand::RefreshDiff)
            .await
            .unwrap();
        assert!(!app.diff_snapshot.stale);
        app.execute_semantic_command(SemanticCommand::GoHome)
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }
}
