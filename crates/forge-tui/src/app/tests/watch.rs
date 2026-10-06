//! Filesystem watcher and git-status refresh tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

use super::super::watch::{classify_watch_path, git_index_path, path_is_ignored_by_file_watcher};
use crate::file_explorer::RefreshTestGate;

#[test]
fn git_index_path_resolves_plain_checkouts_and_linked_worktrees() {
    // Plain checkout: `.git` is a directory, so the index sits inside it.
    let plain = TempDir::new().unwrap();
    std::fs::create_dir(plain.path().join(".git")).unwrap();
    assert_eq!(
        git_index_path(plain.path()).unwrap(),
        plain.path().join(".git/index")
    );

    // Linked worktree: `.git` is a `gitdir:` pointer file and the index lives
    // in the gitdir it names, outside the workspace root.
    let worktree = TempDir::new().unwrap();
    let gitdir = TempDir::new().unwrap();
    std::fs::write(
        worktree.path().join(".git"),
        format!("gitdir: {}\n", gitdir.path().display()),
    )
    .unwrap();
    assert_eq!(
        git_index_path(worktree.path()).unwrap(),
        gitdir.path().join("index")
    );

    // A directory that is not a repository has no index to watch.
    let none = TempDir::new().unwrap();
    assert_eq!(git_index_path(none.path()), None);
}

#[test]
fn classify_watch_path_keeps_the_index_but_drops_internal_churn() {
    let index = PathBuf::from("/repo/.git/index");

    // The index is enqueued as a status-only signal, never a tree change.
    assert_eq!(
        classify_watch_path(&index, Some(&index), true),
        Some(false),
        "the index must refresh Git without rebuilding the tree"
    );
    // Git internals other than the index stay ignored.
    assert_eq!(
        classify_watch_path(&PathBuf::from("/repo/.git/HEAD"), Some(&index), true),
        None
    );
    assert_eq!(
        classify_watch_path(
            &PathBuf::from("/repo/.forge/progress.json"),
            Some(&index),
            true
        ),
        None
    );
    // An ordinary source write keeps the caller's tree-change classification.
    assert_eq!(
        classify_watch_path(&PathBuf::from("/repo/src/lib.rs"), Some(&index), true),
        Some(true)
    );
    assert_eq!(
        classify_watch_path(&PathBuf::from("/repo/src/lib.rs"), Some(&index), false),
        Some(false)
    );
    // Without a resolved index (non-repository), `.git` content is ignored.
    assert_eq!(classify_watch_path(&index, None, true), None);
}

/// An index write refreshes the status cache, which is what surfaces an external
/// `git add` in the changed-file list.
#[tokio::test]
async fn git_index_change_refreshes_status() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    assert!(!app.workspace_files.explorer.git_status.loading);

    // `immediate` is a test-only short-circuit for the debounce; the real
    // classifier marks the index `tree_changed: false`.
    let index = git_index_path(app.session_view.workspace_root()).unwrap();
    app.file_watch.inject_test_change(index, false, true);
    app.poll_file_changes();

    assert!(
        app.workspace_files.explorer.git_status.loading,
        "an index change starts a status refresh"
    );
}

#[tokio::test]
async fn file_change_event_refreshes_git_status() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    assert!(!app.workspace_files.explorer.git_status.loading);

    app.file_watch
        .inject_change(app.session_runtime.workspace_root().join("changed.txt"));
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
async fn file_change_reloads_tree_while_files_sidebar_is_focused() {
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
    install_pending_explorer_refresh(&mut app).await;
    app.workspace_files.visible = true;
    app.focus_block(FocusBlock::Files);
    app.workspace_files.explorer.git_status = forge_workspace::git_status::GitStatusCache::new();
    fs::write(dir.path().join("added-while-focused.txt"), "new\n").unwrap();

    app.file_watch
        .inject_change(app.session_runtime.workspace_root().join("changed.txt"));
    app.poll_file_changes();
    install_pending_explorer_refresh(&mut app).await;

    assert!(app.workspace_files.explorer.git_status.loading);
    assert!(app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "added-while-focused.txt"));
}

#[test]
fn watcher_coalesces_duplicate_paths_and_waits_for_a_quiet_period() {
    let mut watch = FileWatchState::new();
    let first = PathBuf::from("src/lib.rs");
    let second = PathBuf::from("src/app.rs");
    watch.inject_test_change(first.clone(), false, false);
    watch.inject_test_change(first, true, false);
    watch.inject_test_change(second, false, false);

    assert!(watch.take_ready_batch().is_none());
    std::thread::sleep(FileWatchState::DEBOUNCE + Duration::from_millis(10));
    let batch = watch.take_ready_batch().expect("debounced batch");
    assert_eq!(batch.paths.len(), 2);
    assert!(batch.tree_changed, "path classifications should be merged");
    assert!(watch.take_ready_batch().is_none());
}

#[test]
fn watcher_overflow_requests_a_bounded_full_refresh() {
    let mut watch = FileWatchState::new();
    for index in 0..(FileWatchState::EVENT_QUEUE_CAPACITY + 64) {
        watch.inject_test_change(PathBuf::from(format!("generated/{index}.rs")), true, false);
    }

    let batch = watch.take_ready_batch().expect("overflow refresh");
    assert!(batch.overflowed);
    assert!(batch.tree_changed);
    assert!(batch.paths.len() <= FileWatchState::EVENT_QUEUE_CAPACITY);
    assert!(watch.take_ready_batch().is_none());
}

#[tokio::test]
async fn inactive_saved_watcher_is_drained_without_refreshing_selected_view() {
    let (_dir, mut app) = focus_test_app().await;
    let session_id = app.selected_session_id;
    app.save_session_view_state(session_id);
    let other_session_id = uuid::Uuid::new_v4();
    app.selected_session_id = other_session_id;
    app.restore_session_view_state(other_session_id);
    app.session_view_states
        .get_mut(&session_id)
        .expect("saved session view")
        .file_watch
        .inject_change(PathBuf::from("inactive.txt"));

    app.poll_file_changes();

    let batch = app
        .session_view_states
        .get_mut(&session_id)
        .and_then(|state| state.file_watch.take_ready_batch())
        .expect("saved watcher batch");
    assert_eq!(batch.paths, vec![PathBuf::from("inactive.txt")]);
    assert!(app.file_watch.take_ready_batch().is_none());
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

#[tokio::test]
async fn recent_mutating_tool_requests_workspace_refresh() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.explorer.load_root();
    app.transcript_view = forge_session::TranscriptSnapshot::from_messages(
        vec![forge_types::Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![forge_types::ToolCall {
                id: "write".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({}),
            }],
            outcome: Default::default(),
            attachments: Vec::new(),
        }],
        1,
    );

    app.maybe_note_workspace_changed_from_recent_tools();

    assert!(app.workspace_files.explorer.workspace_refresh_pending());
}

#[tokio::test]
async fn recent_non_mutating_tool_does_not_request_workspace_refresh() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.explorer.load_root();
    app.transcript_view = forge_session::TranscriptSnapshot::from_messages(
        vec![forge_types::Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![forge_types::ToolCall {
                id: "read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            }],
            outcome: Default::default(),
            attachments: Vec::new(),
        }],
        1,
    );

    app.maybe_note_workspace_changed_from_recent_tools();

    assert!(!app.workspace_files.explorer.workspace_refresh_pending());
}

/// Poll the terminal-thread side of the explorer until a background workspace
/// refresh has been installed (or dropped), then return.
async fn install_pending_explorer_refresh(app: &mut TuiApp) {
    for _ in 0..5_000 {
        app.poll_file_changes();
        if !app.workspace_files.explorer.workspace_refresh_pending() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("background explorer refresh never landed");
}

async fn wait_for_gate_started(gate: &RefreshTestGate, expected: usize) {
    for _ in 0..5_000 {
        if gate.started() >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("explorer refresh worker never reached the test gate");
}

/// The explorer refresh walks the filesystem on a blocking worker, so a tick
/// on the terminal thread must return immediately even while that walk is
/// parked — and the result must install exactly once.
#[tokio::test]
async fn workspace_refresh_keeps_terminal_thread_responsive() {
    use std::time::{Duration, Instant};

    let (dir, mut app) = focus_test_app().await;
    let gate = Arc::new(RefreshTestGate::new());
    app.workspace_files
        .explorer
        .set_refresh_test_gate(gate.clone());
    app.workspace_files.explorer.request_workspace_refresh();
    wait_for_gate_started(&gate, 1).await;

    // The worker is parked mid-refresh; a tick must not wait on it.
    let started = Instant::now();
    app.poll_file_changes();
    let tick = started.elapsed();
    assert!(
        tick < Duration::from_millis(250),
        "a terminal-thread tick blocked on the explorer refresh for {tick:?}"
    );

    fs::write(dir.path().join("appeared.txt"), "hi\n").unwrap();
    gate.release();
    install_pending_explorer_refresh(&mut app).await;

    assert!(app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "appeared.txt"));
    assert_eq!(app.workspace_files.explorer.refresh_install_count(), 1);
    // A later poll must not re-install the consumed result.
    app.poll_file_changes();
    assert_eq!(app.workspace_files.explorer.refresh_install_count(), 1);
}

/// A refresh superseded while its worker is in flight must have its result
/// dropped, with exactly the latest generation installing.
#[tokio::test]
async fn stale_workspace_refresh_result_is_dropped() {
    let (_dir, mut app) = focus_test_app().await;
    let gate = Arc::new(RefreshTestGate::new());
    app.workspace_files
        .explorer
        .set_refresh_test_gate(gate.clone());

    app.workspace_files.explorer.request_workspace_refresh();
    wait_for_gate_started(&gate, 1).await;
    // Supersede the parked worker; this coalesces into a follow-up.
    app.workspace_files.explorer.request_workspace_refresh();
    gate.release();

    install_pending_explorer_refresh(&mut app).await;

    assert_eq!(gate.started(), 2, "the coalesced follow-up should have run");
    assert_eq!(
        app.workspace_files.explorer.refresh_install_count(),
        1,
        "only the latest generation may install"
    );
}

/// Expanding a directory while a refresh worker is mid-walk must not be
/// overwritten by that worker's older snapshot.
#[tokio::test]
async fn expanding_during_refresh_drops_the_stale_snapshot() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("subdir")).unwrap();
    fs::write(dir.path().join("subdir").join("inside.txt"), "hi\n").unwrap();
    app.workspace_files.explorer.load_root();

    let gate = Arc::new(RefreshTestGate::new());
    app.workspace_files
        .explorer
        .set_refresh_test_gate(gate.clone());
    app.workspace_files.explorer.request_workspace_refresh();
    wait_for_gate_started(&gate, 1).await;

    let subdir_path = app
        .workspace_files
        .explorer
        .visible_nodes()
        .iter()
        .find(|node| node.display_name == "subdir")
        .map(|node| node.path.clone())
        .expect("subdir visible after load_root");
    app.workspace_files.explorer.selected_path = Some(subdir_path.clone());
    app.workspace_files.explorer.expand_selected();

    gate.release();
    install_pending_explorer_refresh(&mut app).await;

    assert_eq!(
        app.workspace_files.explorer.refresh_install_count(),
        0,
        "a refresh snapshot older than the expansion must be dropped"
    );
    assert!(
        app.workspace_files
            .explorer
            .visible_nodes()
            .iter()
            .any(|node| node.path == subdir_path.join("inside.txt")),
        "the expansion must survive the dropped refresh"
    );
}
