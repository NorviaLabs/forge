//! Review-changes Keep / Discard decisions.

use super::prelude::*;
use crossterm::event::{KeyCode, KeyModifiers};
use std::process::Command;

async fn review_app_with_dirty_file() -> (TempDir, TuiApp, PathBuf) {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let path = dir.path().join("file.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-qm", "base"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    fs::write(&path, "ONE\ntwo\nthree\n").unwrap();
    app.workspace_files.explorer.refresh_git_status();
    while app.workspace_files.explorer.git_status.loading {
        app.workspace_files.explorer.git_status.poll();
    }
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    (dir, app, path)
}

#[tokio::test]
async fn drawing_review_keeps_workspace_focus_so_hunk_keys_work() {
    let (_dir, mut app, _path) = review_app_with_dirty_file().await;
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    let _ = render_app_text(&mut app, 120, 40);
    assert_eq!(
        app.focus.block,
        FocusBlock::Workspace,
        "review must remain the workspace focus after draw"
    );
    app.handle_key(press(KeyCode::Char('k'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(!app.diff_view.kept.is_empty());
    assert_ne!(app.input.text, "k");
}

#[tokio::test]
async fn keep_marks_the_hunk_without_changing_bytes() {
    let (_dir, mut app, path) = review_app_with_dirty_file().await;
    let before = fs::read_to_string(&path).unwrap();
    app.execute_semantic_command(SemanticCommand::KeepHunk)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert!(!app.diff_view.kept.is_empty());
}

#[tokio::test]
async fn discard_hunk_restores_tracked_file_to_head() {
    let (_dir, mut app, path) = review_app_with_dirty_file().await;
    app.execute_semantic_command(SemanticCommand::DiscardHunk)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
    assert!(!app.diff_view.snapshot.stale);
}

#[tokio::test]
async fn stale_review_refuses_discard() {
    let (_dir, mut app, path) = review_app_with_dirty_file().await;
    app.diff_view.snapshot.stale = true;
    app.execute_semantic_command(SemanticCommand::DiscardHunk)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "ONE\ntwo\nthree\n");
}

#[tokio::test]
async fn untracked_discard_asks_before_deleting() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let extra = dir.path().join("extra.txt");
    fs::write(&extra, "scratch\n").unwrap();
    app.workspace_files.explorer.refresh_git_status();
    while app.workspace_files.explorer.git_status.loading {
        app.workspace_files.explorer.git_status.poll();
    }
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::DiscardHunk)
        .await
        .unwrap();
    assert!(extra.exists());
    assert_eq!(
        app.diff_view
            .pending_untracked_delete
            .as_ref()
            .and_then(|path| path.file_name()),
        extra.file_name()
    );
    app.execute_semantic_command(SemanticCommand::ConfirmReviewDelete)
        .await
        .unwrap();
    assert!(!extra.exists());
}
