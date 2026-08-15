//! Filesystem watcher and git-status refresh tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

use super::super::watch::path_is_ignored_by_file_watcher;

#[tokio::test]
async fn file_change_event_refreshes_git_status() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    assert!(!app.workspace_files.explorer.git_status.loading);

    app.file_watch
        .inject_change(app.session.workspace_root().join("changed.txt"));
    app.poll_file_changes();

    assert!(app.workspace_files.explorer.git_status.loading);
}

#[tokio::test]
async fn watcher_does_not_reload_active_edtui_buffer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("watched.txt");
    fs::write(&path, "inside").unwrap();
    app.open_file_in_editor(&path);
    let editor = app.editor_session.as_mut().unwrap();
    editor.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
    editor.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    let in_memory = editor.text();
    fs::write(&path, "outside").unwrap();

    app.file_watch.inject_change(path.clone());
    app.poll_file_changes();

    assert_eq!(app.editor_session.as_ref().unwrap().text(), in_memory);
    assert_eq!(app.source_viewer.document_text.as_deref(), Some("inside"));
    assert!(app
        .source_viewer
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("changed on disk")));
}

#[tokio::test]
async fn inspector_renders_settled_change_count_without_files_pane() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let status = std::process::Command::new("git")
        .args(["-C", dir.path().to_str().unwrap(), "add", "-A"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
        .args([
            "-C",
            dir.path().to_str().unwrap(),
            "commit",
            "-qm",
            "initial",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(dir.path().join("changed.txt"), "changed\n").unwrap();
    app.workspace_files.visible = false;
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    app.workspace_files.explorer.refresh_git_status();

    for _ in 0..20 {
        render_app_text(&mut app, 120, 40);
        if app.workspace_files.explorer.git_status.status.len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(app.workspace_files.explorer.git_status.status.len(), 1);
}

#[tokio::test]
async fn inspector_change_count_stays_stable_across_draws() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let status = std::process::Command::new("git")
        .args(["-C", dir.path().to_str().unwrap(), "add", "-A"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
        .args([
            "-C",
            dir.path().to_str().unwrap(),
            "commit",
            "-qm",
            "initial",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(dir.path().join("changed.txt"), "changed\n").unwrap();
    app.workspace_files.visible = false;
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    app.workspace_files.explorer.refresh_git_status();

    for _ in 0..20 {
        render_app_text(&mut app, 120, 40);
        if app.workspace_files.explorer.git_status.status.len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(app.workspace_files.explorer.git_status.status.len(), 1);

    for _ in 0..5 {
        render_app_text(&mut app, 120, 40);
        assert_eq!(app.workspace_files.explorer.git_status.status.len(), 1);
    }
}

#[tokio::test]
async fn file_change_does_not_reload_tree_while_files_sidebar_is_focused() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("crates")).unwrap();
    fs::create_dir(dir.path().join("crates/forge-tui")).unwrap();
    fs::write(dir.path().join("crates/forge-tui/Cargo.toml"), "").unwrap();
    app.workspace_files.explorer.refresh_selected();
    app.workspace_files.explorer.selected_path =
        Some(dir.path().join("crates").canonicalize().unwrap());
    app.workspace_files.explorer.expand_selected();
    app.workspace_files.explorer.selected_path =
        Some(dir.path().join("crates/forge-tui").canonicalize().unwrap());
    app.workspace_files.explorer.expand_selected();
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();

    app.file_watch
        .inject_change(app.session.workspace_root().join("changed.txt"));
    app.poll_file_changes();

    assert!(app.workspace_files.explorer.git_status.loading);
    assert!(app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "Cargo.toml"));
}

#[test]
fn forge_runtime_paths_are_ignored_by_file_watcher_filter() {
    assert!(path_is_ignored_by_file_watcher(Path::new(
        ".forge/progress.json"
    )));
    assert!(path_is_ignored_by_file_watcher(Path::new(
        "/tmp/repo/.forge/sessions/x.db"
    )));
    assert!(path_is_ignored_by_file_watcher(Path::new(".git/index")));
    assert!(path_is_ignored_by_file_watcher(Path::new(
        "/tmp/repo/.git/HEAD"
    )));
    assert!(!path_is_ignored_by_file_watcher(Path::new("src/app.rs")));
    assert!(!path_is_ignored_by_file_watcher(Path::new(
        "/tmp/repo/src/lib.rs"
    )));
}
