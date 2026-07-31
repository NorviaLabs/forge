//! Filesystem watcher and git-status refresh tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::super::watch::path_is_under_dot_forge;
use super::prelude::*;

#[tokio::test]
async fn file_change_event_refreshes_git_status() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer.git_status = crate::git_status::GitStatusCache::new();
    assert!(!app.file_explorer.git_status.loading);

    app.file_change_tx
        .send(FileChangeEvent {
            path: app.session.workspace_root().join("changed.txt"),
        })
        .unwrap();
    app.poll_file_changes();

    assert!(app.file_explorer.git_status.loading);
}

#[tokio::test]
async fn file_change_does_not_reload_tree_while_files_sidebar_is_focused() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("crates")).unwrap();
    fs::create_dir(dir.path().join("crates/forge-tui")).unwrap();
    fs::write(dir.path().join("crates/forge-tui/Cargo.toml"), "").unwrap();
    app.file_explorer.refresh_selected();
    app.file_explorer.selected_path = Some(dir.path().join("crates").canonicalize().unwrap());
    app.file_explorer.expand_selected();
    app.file_explorer.selected_path =
        Some(dir.path().join("crates/forge-tui").canonicalize().unwrap());
    app.file_explorer.expand_selected();
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    app.file_explorer.git_status = crate::git_status::GitStatusCache::new();

    app.file_change_tx
        .send(FileChangeEvent {
            path: app.session.workspace_root().join("changed.txt"),
        })
        .unwrap();
    app.poll_file_changes();

    assert!(app.file_explorer.git_status.loading);
    assert!(app
        .file_explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "Cargo.toml"));
}

#[test]
fn forge_runtime_paths_are_ignored_by_file_watcher_filter() {
    assert!(path_is_under_dot_forge(Path::new(".forge/progress.json")));
    assert!(path_is_under_dot_forge(Path::new(
        "/tmp/repo/.forge/sessions/x.db"
    )));
    assert!(!path_is_under_dot_forge(Path::new("src/app.rs")));
    assert!(!path_is_under_dot_forge(Path::new("/tmp/repo/src/lib.rs")));
}
