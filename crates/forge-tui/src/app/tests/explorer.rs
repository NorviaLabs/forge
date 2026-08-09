//! File explorer dialog and mutation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn explorer_search_accepts_shortcut_initials_without_opening_dialogs() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

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
    assert!(app.explorer_dialog.current.is_none());
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_tab_enters_tree_focus_and_n_opens_new_folder_dialog() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Files);
    assert!(!app.workspace_files.explorer.search_focused);
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.workspace_files.explorer.search_focused);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    app.handle_key(press(KeyCode::Char('N'), KeyModifiers::SHIFT))
        .await
        .unwrap();

    assert!(matches!(
        app.explorer_dialog.current,
        Some(ExplorerDialog::Name {
            action: ExplorerNameAction::CreateDirectory,
            ..
        })
    ));
}

#[tokio::test]
async fn explorer_new_file_dialog_owns_printable_input_and_selects_created_file() {
    let (dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.input.set_text("");

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
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
        app.explorer_dialog.current,
        Some(ExplorerDialog::Name { .. })
    ));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current,
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
    assert_eq!(app.focus.block, FocusBlock::Workspace);
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

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.explorer_dialog.current.is_none());
    assert_eq!(app.focus.block, FocusBlock::Files);
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

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('R'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    match app.explorer_dialog.current.as_mut() {
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
    assert_eq!(app.focus.block, FocusBlock::Workspace);
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
    match app.explorer_dialog.current.as_mut() {
        Some(ExplorerDialog::Name { input, .. }) => *input = "existing.rs".into(),
        other => panic!("unexpected dialog: {other:?}"),
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    match app.explorer_dialog.current {
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

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog.current,
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
    assert!(app.explorer_dialog.current.is_some());
}
