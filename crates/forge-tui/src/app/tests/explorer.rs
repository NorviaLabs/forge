//! File explorer dialog and mutation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

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
    // What this test is for is a blowup — a redraw that starts rescanning the
    // whole tree, an accidental O(n^2) — which costs multiples, not percent.
    // 1500ms still catches that while leaving the slow runner ~3x of room.
    assert!(
        frame_ms < 1500.0,
        "30 down+draw steps in a 600-file tree took {frame_ms:.1}ms"
    );
}
