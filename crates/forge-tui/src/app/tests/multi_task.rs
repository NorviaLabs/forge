//! Repository multi-task behaviour: per-task view state and the guards that
//! keep a primary-only action from running against a sibling.

use super::prelude::*;

#[tokio::test]
async fn switching_tasks_carries_the_whole_view_and_leaves_a_clean_slate() {
    let (_dir, mut app) = focus_test_app().await;
    let first = uuid::Uuid::new_v4();

    app.input.set_text("half-written prompt".to_string());
    app.stream.preview.push_str("streamed answer");
    app.stream.thinking.push_str("reasoning so far");
    app.status_state.message = "first task status".into();
    app.editor_command = Some("s/old/new/".into());
    app.editor_message = Some("written a.txt".into());
    app.banner_state.items.push(ChatItem::Assistant {
        text: "banner".into(),
    });
    app.conversation_view.scroll = 7;
    app.conversation_view.follow = false;

    app.save_task_view_state(first);

    // The app is left blank for whoever is selected next — nothing of the
    // saved task may bleed through.
    assert!(app.input.text.is_empty());
    assert!(app.stream.preview.is_empty());
    assert!(app.stream.thinking.is_empty());
    assert!(app.status_state.message.is_empty());
    assert!(app.editor_command.is_none());
    assert!(app.editor_message.is_none());
    assert!(app.banner_state.items.is_empty());
    assert_eq!(app.conversation_view.scroll, 0);
    assert!(app.conversation_view.follow);

    app.restore_task_view_state(first);

    assert_eq!(app.input.text, "half-written prompt");
    assert_eq!(app.stream.preview, "streamed answer");
    assert_eq!(app.stream.thinking, "reasoning so far");
    assert_eq!(app.status_state.message, "first task status");
    assert_eq!(app.editor_command.as_deref(), Some("s/old/new/"));
    assert_eq!(app.editor_message.as_deref(), Some("written a.txt"));
    assert_eq!(app.banner_state.items.len(), 1);
    assert_eq!(app.conversation_view.scroll, 7);
    assert!(!app.conversation_view.follow);
}

#[tokio::test]
async fn selecting_a_task_rebinds_workspace_owned_views() {
    let (dir, mut app) = focus_test_app().await;
    let linked = dir.path().join("linked-worktree");
    std::fs::create_dir_all(&linked).unwrap();
    app.session_view.workspace_root = linked.canonicalize().unwrap();

    app.sync_selected_workspace();

    let linked = linked.canonicalize().unwrap();
    assert_eq!(app.runtime.cwd, linked);
    assert_eq!(
        app.workspace_files.explorer.root_path(),
        Some(app.runtime.cwd.as_path())
    );
    assert_eq!(app.repo_header_state.cwd, app.runtime.cwd);
}

#[tokio::test]
async fn switching_tasks_discards_the_previous_worktree_diff_cache() {
    let (dir, mut app) = focus_test_app().await;
    app.diff_view = crate::diff_view::DiffView::new(crate::diff_view::DiffSource::WorkingTree);
    app.diff_view.entries.push(crate::diff_view::DiffEntry {
        path: "old.txt".into(),
        marker: "M",
        untracked: false,
    });
    app.workspace_navigation.navigate_to(WorkspaceView::Diff);

    let linked = dir.path().join("linked-worktree");
    std::fs::create_dir_all(&linked).unwrap();
    app.session_view.workspace_root = linked.canonicalize().unwrap();
    app.sync_selected_workspace();

    assert!(app.diff_view.entries.is_empty());
    assert!(app.diff_view.loaded_for.is_none());
    assert!(matches!(
        app.diff_view.patch,
        crate::diff_view::PatchState::Loading
    ));
}

#[tokio::test]
async fn a_task_never_visited_before_starts_from_a_clean_view() {
    let (_dir, mut app) = focus_test_app().await;
    // Whatever model the host's restored auth put in the footer — the point
    // is that a first switch does not blank it.
    let model_before = app.runtime.model_label.clone();
    app.input.set_text("primary draft".to_string());
    app.save_task_view_state(app.session.session_id);

    app.restore_task_view_state(uuid::Uuid::new_v4());
    assert!(app.input.text.is_empty());
    assert_eq!(app.runtime.model_label, model_before);
}

#[tokio::test]
async fn without_a_supervisor_the_primary_is_always_the_selected_runtime() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(app.selected_runtime(), SelectedRuntime::Primary);
    assert!(!app.selected_is_sibling());
    assert!(app.selected_snapshot().is_none());
    // Primary-only actions must stay reachable in single-task mode.
    assert!(app.require_primary_task("/clear"));
}

#[tokio::test]
async fn the_task_strip_help_advertises_the_binding_that_is_actually_wired() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::TaskStrip);
    let help = app.help_text();
    assert!(
        help.contains("Ctrl+Shift+T"),
        "task strip help should name the real switcher binding: {help}"
    );
    assert!(
        !help.contains("• Ctrl+T  Open task switcher"),
        "help must not advertise removed Ctrl+T turn expansion: {help}"
    );
    assert!(
        !help.contains("Alt+1"),
        "help must not advertise an unimplemented pinned-slot binding: {help}"
    );
}
