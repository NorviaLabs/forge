//! File explorer dialog and mutation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn explorer_dialog_rendering_covers_all_file_modal_variants() {
    let (dir, app) = focus_test_app().await;
    let root = dir.path().canonicalize().unwrap();
    let file = root.join("src.rs");
    let child = root.join("src").join("lib.rs");
    let summary = QuitAllSummary {
        sessions: 3,
        in_flight: 2,
        queued_prompts: 1,
        pending_requests: 1,
        dirty_sessions: vec!["editor".into()],
    };
    let dialogs = vec![
        ExplorerDialog::Name {
            action: ExplorerNameAction::CreateFile,
            parent: root.clone(),
            source: None,
            input: "src.rs".into(),
            error: Some("bad name".into()),
        },
        ExplorerDialog::Name {
            action: ExplorerNameAction::CreateDirectory,
            parent: root.clone(),
            source: None,
            input: "src".into(),
            error: None,
        },
        ExplorerDialog::Name {
            action: ExplorerNameAction::Rename,
            parent: root.clone(),
            source: Some(file.clone()),
            input: "renamed.rs".into(),
            error: None,
        },
        ExplorerDialog::ConfirmCreate {
            action: ExplorerNameAction::CreateFile,
            parent: root.clone(),
            name: "src.rs".into(),
            path: file.clone(),
        },
        ExplorerDialog::ConfirmCreate {
            action: ExplorerNameAction::CreateDirectory,
            parent: root.clone(),
            name: "src".into(),
            path: root.join("src"),
        },
        ExplorerDialog::ConfirmRename {
            source: file.clone(),
            path: root.join("renamed.rs"),
            name: "renamed.rs".into(),
        },
        ExplorerDialog::ConfirmDelete {
            source: root.join("src"),
            name: "src".into(),
            kind: forge_workspace::file_ops::EntryKind::Directory,
            non_empty: true,
            permanent: false,
            error: None,
        },
        ExplorerDialog::ConfirmDelete {
            source: file.clone(),
            name: "src.rs".into(),
            kind: forge_workspace::file_ops::EntryKind::File,
            non_empty: false,
            permanent: false,
            error: Some("Trash is unavailable".into()),
        },
        ExplorerDialog::ConfirmDelete {
            source: file.clone(),
            name: "src.rs".into(),
            kind: forge_workspace::file_ops::EntryKind::File,
            non_empty: false,
            permanent: true,
            error: None,
        },
        ExplorerDialog::DirtyExit,
        ExplorerDialog::DirtySwitch {
            path: child.clone(),
        },
        ExplorerDialog::SaveConflict,
        ExplorerDialog::QuitAll {
            summary: QuitAllSummary {
                sessions: 2,
                ..Default::default()
            },
            choice: QuitAllChoice::Cancel,
        },
        ExplorerDialog::QuitAll {
            summary,
            choice: QuitAllChoice::QuitAll,
        },
    ];

    let mut rendered = String::new();
    for dialog in &dialogs {
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        app.render_explorer_dialog(dialog, area, &mut buffer);
        for row in 0..area.height {
            for column in 0..area.width {
                rendered.push_str(buffer[(column, row)].symbol());
            }
        }
    }

    for expected in [
        "New File",
        "New Folder",
        "Rename",
        "Confirm Create",
        "Confirm Rename",
        "Permanent Delete",
        "Unsaved Changes",
        "File Changed on Disk",
        "Quit All Sessions",
        "bad name",
        "2 sessions with a turn running",
        "unsaved changes in editor",
    ] {
        assert!(rendered.contains(expected), "missing {expected:?}");
    }
}

#[tokio::test]
async fn file_editor_save_reload_and_attachment_paths_are_reconciled() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("notes.rs");
    fs::write(&path, "one\ntwo\n").unwrap();
    let path = path.canonicalize().unwrap();

    app.show_file_in_editor(&path);
    assert!(app.editor_session.is_some());
    app.editor_session
        .as_mut()
        .unwrap()
        .replace_text("changed\n");
    app.save_active_editor();
    assert_eq!(fs::read_to_string(&path).unwrap(), "changed\n");
    assert!(app.feedback.text.contains("saved"));

    fs::write(&path, "from disk\n").unwrap();
    app.reload_active_editor_from_disk();
    assert_eq!(app.editor_session.as_ref().unwrap().text(), "from disk\n");
    assert!(app.feedback.text.contains("reloaded file from disk"));

    let workspace_path = app.session_view.workspace_root().join("notes.rs");
    app.source_viewer.path = Some(workspace_path);
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.toggle_file_attachment();
    assert_eq!(app.attachment.file().unwrap().rel_path, "notes.rs");
    app.toggle_file_attachment();
    assert!(app.attachment.file().is_none());

    app.open_file_viewer("notes.rs");
    assert!(app.overlay.is_some());
    app.open_file_viewer("missing.rs");
    assert!(app.overlay.is_some());
    assert!(app.status_state.message.contains("File explorer"));
}

#[tokio::test]
async fn file_editor_conflicts_and_workspace_path_boundaries_are_safe() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("notes.rs");
    fs::write(&path, "original\n").unwrap();
    let path = path.canonicalize().unwrap();
    app.show_file_in_editor(&path);
    app.editor_session.as_mut().unwrap().replace_text("local\n");
    fs::write(&path, "external\n").unwrap();
    app.save_active_editor();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::SaveConflict)
    ));
    app.save_active_editor_with_force(true);
    assert_eq!(fs::read_to_string(&path).unwrap(), "local\n");

    assert!(app.resolve_workspace_path("notes.rs").is_ok());
    assert!(app.resolve_workspace_path("../outside.rs").is_err());
    app.open_file_explorer(Some("notes.rs"), Some("shown error".into()));
    assert!(app.overlay.is_some());
}

#[tokio::test]
async fn explorer_guards_handle_missing_selection_and_non_file_targets() {
    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("entry.txt");
    fs::write(&file, "entry\n").unwrap();

    app.save_active_editor();
    assert!(app.feedback.text.contains("No file open"));
    app.reload_active_editor_from_disk();
    app.complete_pending_editor_switch(false);

    app.open_file_explorer(Some("entry.txt"), None);
    assert!(app.overlay.is_some());
    app.open_file_viewer(".");
    assert!(app.status_state.message.contains("File explorer"));

    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.open_explorer_name_dialog(ExplorerNameAction::CreateFile);
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::Name { .. })
    ));
    app.workspace_files.explorer.selected_path = None;
    app.open_explorer_delete_dialog();
    assert!(app.feedback.text.contains("No file or folder selected"));
}

#[tokio::test]
async fn explorer_search_accepts_shortcut_initials_without_opening_dialogs() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Search);

    for ch in "docs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    for ch in ['N', 'R', 'X'] {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::SHIFT))
            .await
            .unwrap();
    }

    assert_eq!(app.workspace_files.explorer.search_query, "docsNRX");
    assert!(!app.explorer_dialog.is_open());
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_search_esc_clears_the_filter_before_leaving_the_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Search);

    for ch in "docs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.workspace_files.explorer.search_query, "docs");

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.workspace_files.explorer.search_query.is_empty());
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_focused);

    // Printable keys after Esc stay in the filter, never the composer draft.
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.workspace_files.explorer.search_query, "x");
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_search_esc_with_empty_query_leaves_the_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_query.is_empty());

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_ne!(app.focus.block(), FocusBlock::Search);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_files_focus_treats_shortcut_keys_as_tree_commands() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    assert!(!app.workspace_files.explorer.search_focused);

    app.handle_key(press(KeyCode::Char('N'), KeyModifiers::SHIFT))
        .await
        .unwrap();

    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::Name {
            action: ExplorerNameAction::CreateDirectory,
            ..
        })
    ));
}

#[tokio::test]
async fn explorer_tab_from_search_moves_to_files_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_focused);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(!app.workspace_files.explorer.search_focused);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(app.workspace_files.explorer.search_focused);
}

#[tokio::test]
async fn explorer_new_file_dialog_owns_printable_input_and_selects_created_file() {
    let (dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.input.set_text("");

    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    for ch in "new.rs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert!(app.input.text.is_empty());
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::Name { .. })
    ));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::ConfirmCreate { .. })
    ));
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let created = dir.path().join("new.rs").canonicalize().unwrap();
    assert!(created.is_file());
    assert_eq!(
        app.workspace_files.explorer.selected_path.as_deref(),
        Some(created.as_path())
    );
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert_eq!(app.editor_session.as_ref().unwrap().text(), "");
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Normal
    );
}

#[tokio::test]
async fn explorer_name_escape_cancels_without_focus_change_or_composer_input() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(!app.explorer_dialog.is_open());
    assert_eq!(app.focus.block(), FocusBlock::Files);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_rename_prepopulates_and_updates_open_child_file() {
    let (dir, mut app) = focus_test_app().await;
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    let src = src.canonicalize().unwrap();
    let child = src.join("lib.rs");
    fs::write(&child, "pub fn old() {}\n").unwrap();
    app.workspace_files.explorer.refresh_workspace();
    app.workspace_files.explorer.selected_path = Some(src.clone());
    app.open_file_in_editor(&child);
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.workspace_files.explorer.selected_path = Some(src.clone());

    app.handle_key(press(KeyCode::Char('R'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    match app.explorer_dialog.current_mut() {
        Some(ExplorerDialog::Name { input, .. }) => {
            assert_eq!(input, "src");
            *input = "Source".into();
        }
        other => panic!("unexpected dialog: {other:?}"),
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let renamed_child = dir.path().join("Source/lib.rs").canonicalize().unwrap();
    assert!(renamed_child.is_file());
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(renamed_child.as_path())
    );
    let renamed_dir = dir.path().join("Source").canonicalize().unwrap();
    assert_eq!(
        app.workspace_files.explorer.selected_path.as_deref(),
        Some(renamed_dir.as_path())
    );
    assert_eq!(app.focus.block(), FocusBlock::Workspace);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_rename_collision_keeps_name_dialog_with_error() {
    let (dir, mut app) = focus_test_app().await;
    let old = dir.path().join("old.rs");
    let existing = dir.path().join("existing.rs");
    fs::write(&old, "").unwrap();
    fs::write(&existing, "").unwrap();
    app.workspace_files.explorer.refresh_workspace();
    app.workspace_files.explorer.selected_path = Some(old.canonicalize().unwrap());
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    app.open_explorer_name_dialog(ExplorerNameAction::Rename);
    match app.explorer_dialog.current_mut() {
        Some(ExplorerDialog::Name { input, .. }) => *input = "existing.rs".into(),
        other => panic!("unexpected dialog: {other:?}"),
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    match app.explorer_dialog.current() {
        Some(ExplorerDialog::Name {
            error: Some(error), ..
        }) => {
            assert!(error.contains("Destination already exists"));
        }
        other => panic!("unexpected dialog: {other:?}"),
    }
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_delete_non_empty_folder_requires_stronger_confirmation() {
    let (dir, mut app) = focus_test_app().await;
    let folder = dir.path().join("generated");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("out.txt"), "").unwrap();
    let folder = folder.canonicalize().unwrap();
    app.workspace_files.explorer.refresh_workspace();
    app.workspace_files.explorer.selected_path = Some(folder.clone());
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current(),
        Some(ExplorerDialog::ConfirmDelete {
            non_empty: true,
            permanent: false,
            ..
        })
    ));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(folder.exists());
    assert!(app.explorer_dialog.is_open());
}

#[tokio::test]
async fn holding_arrows_in_a_large_tree_stays_on_a_frame_budget() {
    use std::time::Instant;

    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    for pkg in 0..20 {
        let pkg_dir = dir.path().join(format!("pkg_{pkg:02}"));
        std::fs::create_dir(&pkg_dir).unwrap();
        for file in 0..30 {
            std::fs::write(pkg_dir.join(format!("f_{file:02}.rs")), "").unwrap();
        }
    }
    let session = session_for_workspace(dir.path()).await;
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
    app.connect.profile = None;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    draw_app(&mut app, 120, 40);

    let dirs: Vec<_> = app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .filter(|node| {
            node.depth == 1
                && app
                    .workspace_files
                    .explorer
                    .is_visible_directory(&node.path)
        })
        .map(|node| node.path.clone())
        .collect();
    for path in dirs {
        app.workspace_files.explorer.selected_path = Some(path);
        app.workspace_files.explorer.expand_selected();
    }
    draw_app(&mut app, 120, 40);

    let started = Instant::now();
    for _ in 0..200 {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
    }
    let move_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(
        move_ms < 50.0,
        "200 down keys in a 600-file tree took {move_ms:.1}ms"
    );

    let started = Instant::now();
    for _ in 0..30 {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
        draw_app(&mut app, 120, 40);
    }
    let frame_ms = started.elapsed().as_secs_f64() * 1000.0;
    // Sized for the slowest machine that runs it, not the fastest. This gates
    // releases (`release.yml` verifies on a 4-vCPU runner, well below CI's),
    // and at 500ms it had no headroom there: ~520-540ms on a tree that costs
    // ~83ms here, so it blocked the beta.6 cut twice over 3% of honest growth.
    //
    // 600ms clears the worst observed run by ~11%. That is deliberately tight:
    // this stays a real ceiling on redraw cost rather than a formality, at the
    // price of tripping again if that runner has a bad day.
    assert!(
        frame_ms < 600.0,
        "30 down+draw steps in a 600-file tree took {frame_ms:.1}ms"
    );
}
