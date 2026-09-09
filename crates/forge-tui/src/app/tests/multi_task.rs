//! Repository Session behaviour: actor ownership and per-Session view state.

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
async fn removed_roster_retires_saved_view_state_without_disturbing_selected_editor() {
    let (dir, app, handle) = app_with_supervisor().await;
    let mut app = Box::new(app);
    let primary_id = app.selected_session_id;

    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    let sibling = wait_for_chrome_session(&mut app, |item| item.label.is_empty()).await;
    let sibling_id = sibling.session_id;
    let sibling_workspace = app
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&sibling_id))
        .map(|snapshot| snapshot.task.workspace.clone())
        .expect("created session snapshot");

    // Visit the sibling so the saved state owns its real editor, watcher, and
    // (when this host permits PTYs) operator-terminal resources.
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling_id)
        .expect("sibling in task strip");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.poll_supervisor_events();
    let sibling_file = sibling_workspace.join("a.txt");
    app.open_file_in_editor(&sibling_file);
    if crate::interactive_terminal::pty_allocation_available() {
        app.open_bottom_panel();
        assert!(app.interactive_terminal.is_some());
    }

    // Move the fully populated sibling view into its per-session slot. Use a
    // supervisor selection event to install the primary view without routing
    // another task-strip switch through the stale roster under test.
    app.save_session_view_state(sibling_id);
    assert!(
        app.session_view_states.contains_key(&sibling_id),
        "the sibling view must be saved before retirement"
    );
    app.selected_session_id = primary_id;
    handle
        .command(forge_session::SupervisorCommand::SelectSession {
            session_id: Some(primary_id),
        })
        .await
        .unwrap();
    app.poll_supervisor_events();

    // Keep an unsaved editor live on the selected session. Retirement of an
    // unselected sibling must not replace or discard this state.
    let primary_file = dir.path().join("primary.txt");
    std::fs::write(&primary_file, "primary\n").unwrap();
    app.open_file_in_editor(&primary_file);
    app.editor_session
        .as_mut()
        .expect("primary editor")
        .substitute("primary", "edited", true, true);
    assert!(app
        .editor_session
        .as_ref()
        .is_some_and(|editor| editor.is_dirty()));

    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling_id)
        .expect("sibling in task strip");
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    for _ in 0..300 {
        app.poll_supervisor_events();
        if app.supervisor.as_ref().is_some_and(|supervisor| {
            supervisor
                .snapshots
                .get(&sibling_id)
                .is_some_and(|snapshot| {
                    snapshot.task.lifecycle == forge_session::SessionLifecycle::Archived
                })
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle
        .command(forge_session::SupervisorCommand::RemoveManagedWorktree {
            session_id: sibling_id,
        })
        .await
        .unwrap();
    for _ in 0..300 {
        app.poll_supervisor_events();
        if !app
            .session_chrome
            .iter()
            .any(|item| item.session_id == sibling_id)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        !app.session_view_states.contains_key(&sibling_id),
        "a Removed session must release its saved editor/watcher/terminal state"
    );
    assert_eq!(app.selected_session_id, primary_id);
    assert!(
        app.editor_session
            .as_ref()
            .is_some_and(|editor| editor.is_dirty()),
        "retiring an unselected session must preserve the selected dirty editor"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn worktree_cleanup_refuses_a_dirty_embedded_editor() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("dirty-before-cleanup.txt");
    std::fs::write(&path, "before\n").unwrap();
    app.open_file_in_editor(&path);
    app.editor_session
        .as_mut()
        .expect("editor")
        .substitute("before", "after", true, true);
    assert!(app
        .editor_session
        .as_ref()
        .is_some_and(|editor| editor.is_dirty()));

    let session_id = app.selected_session_id;
    assert!(!app.begin_session_view_retirement(session_id));
    assert!(app
        .editor_session
        .as_ref()
        .is_some_and(|editor| editor.is_dirty()));
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
async fn without_a_supervisor_the_session_is_direct_owned() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(app.selected_runtime(), SelectedRuntime::Direct);
    assert!(!app.selected_is_supervised());
    assert!(app.selected_snapshot().is_none());
}

#[tokio::test]
async fn supervisor_snapshot_makes_the_selected_session_supervised() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    let session = wait_for_chrome_session(&mut app, |item| item.label.is_empty()).await;

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.selected_runtime(),
        SelectedRuntime::Supervised(session.session_id)
    );
    assert!(app.selected_is_supervised());
    assert_eq!(
        app.selected_snapshot()
            .map(|snapshot| snapshot.task.session_id),
        Some(session.session_id)
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A supervisor update that carries no transcript change (a model switch) used
/// to invalidate the settled conversation cache unconditionally, so the
/// 200ms background-task poll forced a full O(transcript) rebuild every tick
/// while a session worked. The render key already detects real transcript
/// changes; identity-only updates must not throw the cached lines away.
#[tokio::test]
async fn unchanged_transcript_update_reuses_cached_conversation_lines() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.conversation_view.splash_dismissed = true;
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);

    handle
        .command(forge_session::SupervisorCommand::SetModel {
            session_id: app.selected_session_id,
            model_id: "mock-v2".into(),
            route_id: "native-v2".into(),
            reasoning_effort: None,
        })
        .await
        .unwrap();
    app.poll_supervisor_events();
    draw_app(&mut app, 100, 30);

    let second = Arc::clone(&app.render_cache.conversation.as_ref().unwrap().lines);
    assert!(
        Arc::ptr_eq(&first, &second),
        "a supervisor update with an unchanged transcript must not rebuild conversation lines"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn lagged_supervisor_events_resync_the_selected_snapshot() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let session_id = app.selected_session_id;
    let final_model = "mock-after-lag-599";

    // SetModel publishes a snapshot event for every update. Do not poll while
    // filling the broadcast channel so the TUI receiver is forced to report
    // Lagged on its next tick.
    for index in 0..600 {
        handle
            .command(forge_session::SupervisorCommand::SetModel {
                session_id,
                model_id: if index == 599 {
                    final_model.into()
                } else {
                    format!("mock-after-lag-{index}")
                },
                route_id: "native".into(),
                reasoning_effort: None,
            })
            .await
            .unwrap();
    }

    app.poll_supervisor_events();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_supervisor_events();
        let current = app
            .selected_snapshot()
            .and_then(|snapshot| snapshot.details.as_ref())
            .map(|details| details.active_model.as_str());
        if current == Some(final_model) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lagged supervisor events did not produce an authoritative refresh: {current:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(app.runtime.model_label, final_model);

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn supervised_primary_has_no_direct_runtime_and_runs_one_turn() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    assert!(app.session_runtime.as_ref().is_none());
    assert_eq!(
        app.selected_runtime(),
        SelectedRuntime::Supervised(primary_id)
    );

    app.input.set_text("run primary once");
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_supervisor_events();
        let users = app
            .selected_snapshot()
            .map(|snapshot| {
                snapshot
                    .transcript
                    .messages()
                    .iter()
                    .filter(|message| message.role == forge_types::MessageRole::User)
                    .count()
            })
            .unwrap_or_default();
        if users == 1 && app.session_view.lifecycle == forge_types::TaskLifecycle::Completed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "primary turn did not complete exactly once"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_task_strip_help_advertises_the_binding_that_is_actually_wired() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::TaskStrip);
    let help = app.help_text();
    assert!(
        help.contains("F3"),
        "session strip help should name the real switcher binding: {help}"
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
    let model: Arc<dyn forge_model::ModelClient> =
        Arc::new(forge_model::MockModelClient::script(vec![
            forge_types::ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
    app_with_supervisor_and_model(model).await
}

async fn app_with_supervisor_and_model(
    model: Arc<dyn forge_model::ModelClient>,
) -> (TempDir, TuiApp, forge_session::SupervisorHandle) {
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

    // The primary keeps its own model client while supervisor-created
    // sessions use the supervisor's, so seed both from the same client.
    let session = session_for_workspace_with_model(dir.path(), model.clone()).await;
    let session_id = session.session_id;
    let runtime = TuiRuntimeConfig {
        model_label: "mock".into(),
        provider: "mock".into(),
        cwd: dir.path().to_path_buf(),
        version: "test".into(),
        startup_notices: Vec::new(),
        file_icons: FileIconMode::Unicode,
        theme_id: forge_config::DEFAULT_THEME_ID.into(),
    };
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
    control
        .register_session(
            forge_session::NewRepositorySession {
                session_id,
                label: "main".into(),
                workspace: dir.path().to_path_buf(),
                branch: "main".into(),
                ownership: forge_session::WorktreeOwnership::Primary,
                slot: Some(1),
                model_id: "mock".into(),
                route_id: "native".into(),
                reasoning_effort: None,
            },
            None,
        )
        .await
        .unwrap();
    let record = control.session(session_id).await.unwrap();
    let (supervisor, handle) = forge_session::RepositorySupervisor::spawn_with_trust_store(
        control,
        lease,
        vec![(record, session)],
        2,
        cfg,
        model,
        Some(dir.path().join("trust.toml")),
    )
    .await
    .unwrap();
    let initial = supervisor.snapshot(session_id).await.unwrap();
    let mut app = TuiApp::new_supervised(initial, runtime, handle.clone());
    app.connect.profile = None;
    app.runtime.provider = "mock".into();
    app.connect.store = CredentialStore::new(dir.path().join("empty-creds.toml"));
    assert!(app.session_runtime.as_ref().is_none());
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
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();

    // One keypress: no form, no modal. The task is registered unnamed and
    // prompt-less, with UUID4-backed session branch/path naming.
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
        text.contains("session 1"),
        "strip should fall back to `session 1`: {text}"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_first_prompt_names_an_unnamed_task() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.focus_block(FocusBlock::TaskStrip);
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
    app.drain_pending_prompt(None).await.unwrap();

    let named = wait_for_chrome_session(&mut app, |task| task.label == "rewrite-the-lexer").await;
    assert_eq!(named.session_id, task.session_id);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A model client that parks turns whose prompt contains a registered marker
/// behind a notify, so a test can hold a session in `Running` across a
/// session switch. Keyed by prompt text (not call order): sibling sessions
/// race to the model, so an index-based gate can pin the wrong turn.
struct GateModel {
    gates: Vec<(String, std::sync::Arc<tokio::sync::Notify>)>,
}

impl GateModel {
    fn new(gates: Vec<(String, std::sync::Arc<tokio::sync::Notify>)>) -> Self {
        Self { gates }
    }

    async fn step(
        &self,
        req: &forge_model::ModelRequest,
        tx: Option<forge_model::StreamEventTx>,
    ) -> Result<forge_types::ModelResponse, forge_model::ModelError> {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|message| message.role == forge_types::MessageRole::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        if let Some((_, gate)) = self
            .gates
            .iter()
            .find(|(marker, _)| last_user.contains(marker))
        {
            gate.notified().await;
        }
        if let Some(tx) = tx {
            let _ = tx.send(forge_types::ModelStreamEvent::TextDelta {
                text: "done".into(),
            });
            let _ = tx.send(forge_types::ModelStreamEvent::MessageEnd);
        }
        Ok(forge_types::ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        })
    }
}

#[async_trait::async_trait]
impl forge_model::ModelClient for GateModel {
    async fn complete(
        &self,
        req: forge_model::ModelRequest,
    ) -> Result<forge_types::ModelResponse, forge_model::ModelError> {
        self.step(&req, None).await
    }

    async fn complete_with_stream(
        &self,
        req: forge_model::ModelRequest,
        tx: Option<forge_model::StreamEventTx>,
    ) -> Result<forge_types::ModelResponse, forge_model::ModelError> {
        self.step(&req, tx).await
    }

    fn clear_provider_env(&self) {}
}

async fn wait_for_turn_state(
    app: &mut TuiApp,
    session_id: uuid::Uuid,
    want: forge_session::SupervisorTurnState,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.poll_supervisor_events();
        let state = app
            .supervisor
            .as_ref()
            .and_then(|supervisor| supervisor.snapshots.get(&session_id))
            .map(|snapshot| snapshot.task.turn_state);
        if state == Some(want) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for turn {want:?} (got {state:?})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn wait_for_user_messages(app: &mut TuiApp, session_id: uuid::Uuid, want: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.poll_supervisor_events();
        let count = user_message_count(app, session_id);
        if count >= want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {want} user messages (got {count})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// A running and B completed; switching to B must show B's own idle
/// presentation and accept a prompt into B without disturbing A.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_session_accepts_prompt_while_another_runs() {
    let gate_b = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate_a = std::sync::Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn forge_model::ModelClient> = Arc::new(GateModel::new(vec![
        ("B work".to_string(), gate_b.clone()),
        ("A work".to_string(), gate_a.clone()),
    ]));
    let (_dir, app, handle) = app_with_supervisor_and_model(model).await;
    // Box the app so this test's future stays small: `TuiApp` is large and
    // deeply-nested key-handling futures already press on the test thread's
    // stack.
    let mut app = Box::new(app);
    let primary_id = app.selected_session_id;

    // Create B and switch to it.
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    let b = wait_for_chrome_session(&mut app, |item| item.label.is_empty()).await;
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.poll_supervisor_events();
    assert_eq!(app.selected_session_id, b.session_id);

    // Run B, held open on the gate.
    app.focus_block(FocusBlock::Composer);
    app.input.set_text("B work".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_turn_state(
        &mut app,
        b.session_id,
        forge_session::SupervisorTurnState::Running,
    )
    .await;
    assert!(app.busy_state.is_active());

    // Leave a draft on B and switch back to the primary.
    app.input.set_text("b leftover draft".to_string());
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.poll_supervisor_events();
    assert_eq!(app.selected_session_id, primary_id);

    // B completes while it is not selected; A starts and is held running.
    gate_b.notify_one();
    wait_for_turn_state(
        &mut app,
        b.session_id,
        forge_session::SupervisorTurnState::Completed,
    )
    .await;
    // B's saved view was reconciled by the completion event itself, not just
    // at the next switch.
    assert!(
        !app.session_view_states
            .get(&b.session_id)
            .is_some_and(|saved| saved.busy_state.is_active()),
        "a session that finishes unselected must not keep a busy view behind"
    );
    app.focus_block(FocusBlock::Composer);
    app.input.set_text("A work".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    )
    .await;

    // Switch to completed B while A still runs. Assert before the next
    // poll: the optimistic switch must already present B's own state, not
    // stale restored busy from before B finished unselected.
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.selected_session_id, b.session_id);
    // A strip switch keeps focus on the strip for keyboard navigation; the
    // operator moves to the composer to type.
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);
    app.focus_block(FocusBlock::Composer);
    assert!(
        !app.busy_state.is_active(),
        "completed B must not show A's (or stale) busy state"
    );
    assert!(
        app.contextual_hint().is_none(),
        "completed B must not show a busy queue hint: {:?}",
        app.contextual_hint()
    );
    assert!(app.selected_pending_hitl().is_none());
    assert!(app.selected_pending_question().is_none());
    assert_eq!(
        app.selected_input_route("follow-up for B"),
        input_route::InputRoute::StartNewTask
    );
    app.poll_supervisor_events();

    // B's draft survived and typing appends to it.
    assert_eq!(app.input.text, "b leftover draft");
    for c in ['h', 'i'] {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "b leftover drafthi");

    // Submitting on B's composer stages a prompt (not a TaskStrip re-select)
    // and it lands in B while A still runs.
    app.submit_composer_message().await.unwrap();
    assert!(
        app.pending_turn.has_prompt(),
        "submitting on B's composer must stage a prompt"
    );
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_user_messages(&mut app, b.session_id, 2).await;
    wait_for_turn_state(
        &mut app,
        b.session_id,
        forge_session::SupervisorTurnState::Completed,
    )
    .await;

    // A kept running its own turn, undisturbed by the switch and submit.
    wait_for_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    )
    .await;

    gate_a.notify_one();
    wait_for_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Completed,
    )
    .await;
    wait_for_user_messages(&mut app, primary_id, 1).await;
    assert_eq!(
        user_message_count(&app, primary_id),
        1,
        "switching and submitting on B must not disturb A"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

fn user_message_count(app: &TuiApp, session_id: uuid::Uuid) -> usize {
    app.supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&session_id))
        .map(|snapshot| {
            snapshot
                .transcript
                .messages()
                .iter()
                .filter(|message| message.role == forge_types::MessageRole::User)
                .count()
        })
        .unwrap_or(0)
}
