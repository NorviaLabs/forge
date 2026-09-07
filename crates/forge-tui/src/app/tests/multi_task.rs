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
    app.diff_explorer_was_visible = Some(true);
    app.pending_editor_path = Some(std::path::PathBuf::from("pending.rs"));
    app.pending_editor_home = true;
    app.external_editor.requested = true;
    assert!(app.cancellation.request());
    app.progress_state.description = Some("building index".into());

    app.save_session_view_state(first);

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
    assert!(app.diff_explorer_was_visible.is_none());
    assert!(app.pending_editor_path.is_none());
    assert!(!app.pending_editor_home);
    assert!(!app.external_editor.requested);
    assert!(!app.cancellation.is_requested());
    assert!(app.progress_state.description.is_none());

    app.restore_session_view_state(first);

    assert_eq!(app.input.text, "half-written prompt");
    assert_eq!(app.stream.preview, "streamed answer");
    assert_eq!(app.stream.thinking, "reasoning so far");
    assert_eq!(app.status_state.message, "first task status");
    assert_eq!(app.editor_command.as_deref(), Some("s/old/new/"));
    assert_eq!(app.editor_message.as_deref(), Some("written a.txt"));
    assert_eq!(app.banner_state.items.len(), 1);
    assert_eq!(app.conversation_view.scroll, 7);
    assert!(!app.conversation_view.follow);
    assert_eq!(app.diff_explorer_was_visible, Some(true));
    assert_eq!(
        app.pending_editor_path.as_deref(),
        Some(std::path::Path::new("pending.rs"))
    );
    assert!(app.pending_editor_home);
    assert!(app.external_editor.requested);
    assert!(app.cancellation.is_requested());
    assert_eq!(
        app.progress_state.description.as_deref(),
        Some("building index")
    );
}

#[tokio::test]
async fn terminal_and_explorer_follow_the_session_worktree() {
    if !crate::interactive_terminal::pty_allocation_available() {
        eprintln!("skipping: this host denies PTY allocation");
        return;
    }
    let (dir, mut app) = focus_test_app().await;
    let primary_id = uuid::Uuid::new_v4();
    let primary = dir.path().canonicalize().unwrap();

    app.session_view.workspace_root = primary.clone();
    app.sync_selected_workspace();
    app.open_bottom_panel();
    assert_eq!(
        app.interactive_terminal.as_ref().unwrap().cwd(),
        primary.as_path()
    );
    app.save_session_view_state(primary_id);

    let linked = dir.path().join("linked-terminal-worktree");
    std::fs::create_dir_all(&linked).unwrap();
    let linked = linked.canonicalize().unwrap();
    let sibling_id = uuid::Uuid::new_v4();
    app.session_view.workspace_root = linked.clone();
    app.restore_session_view_state(sibling_id);
    app.sync_selected_workspace();

    assert_eq!(
        app.workspace_files.explorer.root_path(),
        Some(linked.as_path())
    );
    assert!(app.interactive_terminal.is_none());
    app.open_bottom_panel();
    assert_eq!(
        app.interactive_terminal.as_ref().unwrap().cwd(),
        linked.as_path()
    );
    app.save_session_view_state(sibling_id);

    app.session_view.workspace_root = primary.clone();
    app.restore_session_view_state(primary_id);
    app.sync_selected_workspace();
    assert_eq!(
        app.workspace_files.explorer.root_path(),
        Some(primary.as_path())
    );
    assert_eq!(
        app.interactive_terminal.as_ref().unwrap().cwd(),
        primary.as_path()
    );
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
    app.save_session_view_state(app.session_runtime.session_id);

    app.restore_session_view_state(uuid::Uuid::new_v4());
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
    app.focus_block(FocusBlock::SessionStrip);
    let help = app.help_text();
    assert!(
        help.contains("Ctrl+Shift+T"),
        "task strip help should name the real switcher binding: {help}"
    );
    assert!(
        !help.contains("• Ctrl+T  Open session switcher"),
        "help must not advertise removed Ctrl+T turn expansion: {help}"
    );
    assert!(
        !help.contains("Alt+1"),
        "help must not advertise an unimplemented pinned-slot binding: {help}"
    );
}

/// An app wired to a real supervisor rooted in the same repository, with
/// trust redirected at a temporary store so granting it never touches the
/// developer's own. The supervisor starts with no registered sessions, so
/// the roster only ever contains what the test creates.
async fn app_with_supervisor() -> (TempDir, TuiApp, forge_session::SupervisorHandle) {
    isolate_global_skills();
    let dir = TempDir::new().unwrap();
    for args in [
        vec!["init", "-q", "--initial-branch=main"],
        vec!["config", "user.email", "forge@example.com"],
        vec!["config", "user.name", "Forge Test"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    for args in [vec!["add", "a.txt"], vec!["commit", "-q", "-m", "init"]] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
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
            theme_id: forge_config::DEFAULT_THEME_ID.into(),
        },
    );
    app.connect.profile = None;
    app.connect.store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );

    let storage = forge_session::RepositoryRuntimeStorage::new(dir.path()).unwrap();
    let control_dir =
        forge_storage::RuntimeStorage::path_for(&storage, forge_storage::RuntimeDataKind::Control)
            .unwrap();
    let control = Arc::new(
        forge_session::RepositoryControl::open(&control_dir)
            .await
            .unwrap(),
    );
    let lease = forge_session::RepositoryLease::acquire(&control_dir, dir.path()).unwrap();
    let mut cfg = forge_config::Config {
        resolved_workspace: dir.path().to_path_buf(),
        workspace_root: Some(dir.path().display().to_string()),
        ..Default::default()
    };
    cfg.journal.path = dir.path().join("j").display().to_string();
    let model: Arc<dyn forge_model::ModelClient> =
        Arc::new(forge_model::MockModelClient::script(vec![
            forge_types::ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
    let (_supervisor, handle) = forge_session::RepositorySupervisor::spawn_with_trust_store(
        control,
        lease,
        Vec::new(),
        2,
        cfg,
        model,
        Some(dir.path().join("trust.toml")),
    )
    .await
    .unwrap();
    app.supervisor = Some(SupervisorUiState {
        handle: handle.clone(),
        events: handle.subscribe(),
        current_session_id: app.session_runtime.session_id,
        snapshots: Default::default(),
    });
    (dir, app, handle)
}

async fn wait_for_chrome_session(
    app: &mut TuiApp,
    mut matches: impl FnMut(&SessionChromeItem) -> bool,
) -> SessionChromeItem {
    for _ in 0..300 {
        app.poll_supervisor_events();
        if let Some(task) = app.session_chrome.iter().find(|task| matches(task)) {
            return task.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("task never appeared in chrome");
}

#[tokio::test]
async fn strip_n_creates_an_unnamed_task_in_one_keypress() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.focus_block(FocusBlock::SessionStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();

    // One keypress: no form, no modal. The task is registered unnamed and
    // prompt-less, with the stable session-id branch/path naming.
    let task = wait_for_chrome_session(&mut app, |task| task.label.is_empty()).await;
    assert!(
        task.branch.starts_with("forge/session-"),
        "branch: {}",
        task.branch
    );
    assert!(
        app.overlay.is_none(),
        "instant create must not open a modal"
    );

    // The strip renders the positional fallback instead of a hole.
    let text = render_app_text(&mut app, 120, 30);
    assert!(
        text.contains("task 1"),
        "strip should fall back to `task 1`: {text}"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_first_prompt_names_an_unnamed_task() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.focus_block(FocusBlock::SessionStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    let task = wait_for_chrome_session(&mut app, |task| task.label.is_empty()).await;

    // The strip opens on the primary row; move right onto the new task,
    // then select it. Typing the first prompt both names it and runs it —
    // the strip label comes from the prompt's opening words.
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.input.set_text("rewrite the lexer".to_string());
    app.submit_composer_message().await.unwrap();

    let named = wait_for_chrome_session(&mut app, |task| task.label == "rewrite-the-lexer").await;
    assert_eq!(named.session_id, task.session_id);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}
