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
async fn saved_terminal_is_serviced_while_its_session_is_not_selected() {
    if !crate::interactive_terminal::pty_allocation_available() {
        eprintln!("skipping: this host denies PTY allocation");
        return;
    }
    let (_dir, mut app) = focus_test_app().await;
    let session_id = app.selected_session_id;
    let other_session_id = uuid::Uuid::new_v4();
    app.open_bottom_panel();
    app.interactive_terminal
        .as_mut()
        .expect("terminal")
        // Use the shell's printf builtin so the high-volume producer does not
        // leave a pipeline child behind if the test times out or the PTY is
        // torn down while output is still buffered.
        .start_command("printf '%0500000d\\n' 0")
        .unwrap();
    app.save_session_view_state(session_id);
    app.selected_session_id = other_session_id;
    app.restore_session_view_state(other_session_id);
    assert!(app.interactive_terminal.is_none());
    assert_ne!(app.selected_session_id, session_id);
    assert!(app.any_interactive_terminal_running());

    for _ in 0..400 {
        app.poll_interactive_terminals();
        let completed = app
            .session_view_states
            .get_mut(&session_id)
            .and_then(|state| state.interactive_terminal.as_mut())
            .and_then(|terminal| terminal.take_command_completion());
        if completed.is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("saved terminal did not drain its high-volume command");
}

#[tokio::test]
async fn removed_roster_retires_saved_view_state_without_disturbing_selected_editor() {
    let (dir, app, handle) = app_with_supervisor().await;
    let mut app = Box::new(app);
    let primary_id = app.selected_session_id;

    let sibling = create_promptless_session(&mut app).await;
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
    // `x` confirms before it does anything; the archive and the checkout
    // removal are then dispatched together from that confirmation.
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        matches!(
            app.overlay,
            Some(crate::overlays::Overlay::SessionConfirm { .. })
        ),
        "`x` must confirm before archiving and removing"
    );
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
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
async fn quit_closes_selected_session_before_exiting_on_last_session() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;

    let sibling = create_promptless_session(&mut app).await;
    let sibling_id = sibling.session_id;
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling_id)
        .expect("sibling in task strip");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.poll_supervisor_events();

    app.dispatch_line("/quit").await.unwrap();
    // The close is queued rather than awaited, so its follow-up lands on a
    // later application tick instead of blocking the terminal owner.
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        let closed = !app
            .session_chrome
            .iter()
            .any(|item| item.session_id == sibling_id);
        if closed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The roster echo may lag the close reply: the selection moves to the
    // surviving sibling on the completion tick, so the commandline is back
    // on a usable session even before the navigator drops the closed row.
    assert_eq!(
        app.selected_session_id, primary_id,
        "the commandline must return to a usable session once the close lands"
    );
    assert!(!app.exit.is_requested());
    assert_eq!(app.selected_session_id, primary_id);
    assert!(!app
        .session_chrome
        .iter()
        .any(|item| item.session_id == sibling_id));

    app.dispatch_line("/quit").await.unwrap();
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        if app.exit.is_requested() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(app.exit.is_requested());
    // Quitting the last session leaves no runtime selected; the shutdown-path
    // token report must not panic on the missing runtime.
    assert_eq!(app.selected_token_usage_report().api.total_api_tokens(), 0);

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Overwrite the tracked close's completion with a failure, the way a
/// retirement that blew its deadline reports one.
fn fail_pending_close(app: &mut TuiApp, message: &str) {
    assert_eq!(
        app.pending_command_completions.len(),
        1,
        "expected exactly one pending close to fail"
    );
    let (reply, response) = tokio::sync::oneshot::channel::<Result<(), String>>();
    reply
        .send(Err(message.to_string()))
        .expect("the pending completion still holds the receiver");
    app.pending_command_completions[0].reply = response;
    app.poll_pending_commands();
}

/// `/quit` on the last session must exit even when its close fails.
///
/// Retirement is best-effort and time-boxed in the supervisor, so a workspace
/// that takes longer than that deadline to release makes the close fail.
/// Because the exit used to be gated on that success, the follow-up did
/// nothing and the app loop kept running — an unquittable session. The leftover
/// actor is reconciled on the next launch, so exiting is always safe; the
/// failure rides out in the exit summary instead of dying on the last frame.
#[tokio::test]
async fn quit_exits_even_when_the_last_session_close_fails() {
    let (_dir, mut app, handle) = app_with_supervisor().await;

    app.dispatch_line("/quit").await.unwrap();
    fail_pending_close(
        &mut app,
        "session resources did not stop before workspace retirement",
    );

    assert!(
        app.exit.is_requested(),
        "a failed close on the last session must still quit"
    );
    assert_eq!(
        app.quit_failure.as_deref(),
        Some("session resources did not stop before workspace retirement"),
        "the failure must reach the exit summary, not die on the last frame"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// With a sibling still active, `/quit` closes the selected session and stays.
/// A close that fails must leave the operator in the app — only the
/// last-session case trades cleanup for an exit.
#[tokio::test]
async fn quit_with_a_sibling_stays_running_when_the_close_fails() {
    let (_dir, mut app, handle) = app_with_supervisor().await;

    let sibling = create_promptless_session(&mut app).await;
    let sibling_id = sibling.session_id;
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling_id)
        .expect("sibling in task strip");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.poll_supervisor_events();

    app.dispatch_line("/quit").await.unwrap();
    fail_pending_close(
        &mut app,
        "session resources did not stop before workspace retirement",
    );

    assert!(
        !app.exit.is_requested(),
        "a failed close must not take the whole app down while a sibling is live"
    );
    assert_eq!(app.selected_session_id, sibling_id);

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A quit-all summary names only the categories that actually lose something:
/// a `0` line is noise, and the dialog exists to be read.
#[test]
fn quit_all_loss_lines_only_name_what_is_actually_lost() {
    let quiet = QuitAllSummary {
        sessions: 3,
        ..Default::default()
    };
    assert!(quiet.loss_lines().is_empty());
    assert!(
        !quiet.needs_confirmation(),
        "sessions that are idle are not worth a dialog"
    );

    let one_working = QuitAllSummary {
        in_flight: 1,
        ..quiet.clone()
    };
    assert!(
        !one_working.needs_confirmation(),
        "one busy session the operator is watching is the ordinary case"
    );

    let busy = QuitAllSummary {
        sessions: 4,
        in_flight: 2,
        queued_prompts: 1,
        pending_requests: 1,
        dirty_sessions: vec!["api-refactor".into(), "docs".into()],
    };
    assert!(busy.needs_confirmation());
    assert_eq!(
        busy.loss_lines(),
        vec![
            "2 sessions with a turn running — cancelled".to_string(),
            "1 queued prompt never dispatched".to_string(),
            "1 request waiting on you — dismissed".to_string(),
            "unsaved changes in api-refactor, docs".to_string(),
        ]
    );
}

/// The counters come from the roster, not from the focused view, and archived
/// rows are history rather than sessions the quit will stop.
#[tokio::test]
async fn quit_all_summary_reads_the_whole_roster() {
    let (dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    let template = app
        .supervisor
        .as_ref()
        .unwrap()
        .snapshots
        .get(&primary_id)
        .unwrap()
        .clone();

    let running_id = uuid::Uuid::new_v4();
    let mut running = template.clone();
    running.task.session_id = running_id;
    running.task.label = "api-refactor".into();
    running.task.ownership = forge_session::WorktreeOwnership::Managed;
    running.task.turn_state = forge_session::SupervisorTurnState::Running;
    running.queued_prompts = vec![(1, "queued behind it".into())];

    let archived_id = uuid::Uuid::new_v4();
    let mut archived = template.clone();
    archived.task.session_id = archived_id;
    archived.task.lifecycle = forge_session::SessionLifecycle::Archived;
    archived.task.turn_state = forge_session::SupervisorTurnState::Running;

    {
        let snapshots = &mut app.supervisor.as_mut().unwrap().snapshots;
        snapshots.insert(primary_id, {
            let mut primary = template.clone();
            primary.task.turn_state = forge_session::SupervisorTurnState::Running;
            primary
        });
        snapshots.insert(running_id, running);
        snapshots.insert(archived_id, archived);
    }

    // An unsaved buffer on the selected session counts like any other.
    let path = dir.path().join("a.txt");
    app.open_file_in_editor(&path);
    app.editor_session
        .as_mut()
        .expect("primary editor")
        .substitute("one", "edited", true, true);
    assert!(app
        .editor_session
        .as_ref()
        .is_some_and(|editor| editor.is_dirty()));

    let summary = app.quit_all_summary();
    assert_eq!(summary.sessions, 2, "archived sessions are not stopped");
    assert_eq!(summary.in_flight, 2);
    assert_eq!(summary.queued_prompts, 1);
    assert_eq!(summary.dirty_sessions, vec!["main".to_string()]);
    assert!(summary.needs_confirmation());

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// One busy session is not worth interrupting the operator; quitting is the
/// ordinary way to leave a session that is still working.
#[tokio::test]
async fn one_working_session_quits_without_a_confirm() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    set_primary_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    );

    app.begin_quit();
    assert!(
        app.explorer_dialog.current().is_none(),
        "one running session must quit without a dialog"
    );
    assert!(app.quitting, "the quit itself must still be under way");
    assert!(
        app.status_state.message.contains("closing"),
        "the sweep reports progress: {}",
        app.status_state.message
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The second one is work that dies without anyone watching it, so the confirm
/// appears — and defaults to leaving it alone.
#[tokio::test]
async fn the_quit_all_confirm_defaults_to_cancel() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    set_primary_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    );
    inject_synthetic_session(&mut app, "docs", forge_session::SupervisorTurnState::Queued);

    app.begin_quit();
    match app.explorer_dialog.current() {
        Some(ExplorerDialog::QuitAll { summary, choice }) => {
            assert_eq!(summary.sessions, 2);
            assert_eq!(summary.in_flight, 2);
            assert_eq!(
                *choice,
                QuitAllChoice::Cancel,
                "the destructive row must never be the default"
            );
        }
        other => panic!("expected the quit-all confirm, got {other:?}"),
    }
    assert!(!app.exit.is_requested());
    assert!(!app.quitting, "nothing is swept until the row is confirmed");

    // Enter on an untouched dialog confirms Cancel.
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.explorer_dialog.current().is_none());
    assert!(!app.exit.is_requested());
    assert!(!app.quitting);

    // Esc dismisses it too.
    app.begin_quit();
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.explorer_dialog.current().is_none());
    assert!(!app.quitting);

    // Only the explicit Quit-all row starts the sweep.
    app.begin_quit();
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.explorer_dialog.current().is_none());
    assert!(app.quitting, "confirming starts the sweep");
    assert!(
        app.status_state.message.contains("closing"),
        "the sweep reports progress: {}",
        app.status_state.message
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Confirming runs the real sweep against the live supervisor and exits once
/// it reports, however many sessions the roster held.
#[tokio::test]
async fn confirming_quit_all_sweeps_the_sessions_and_exits() {
    // The sweep closes the primary itself, so there is nothing left to shut
    // down: the handle's channel closing is the supervisor's exit.
    let (_dir, mut app, _handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    set_primary_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    );
    inject_synthetic_session(
        &mut app,
        "docs",
        forge_session::SupervisorTurnState::Running,
    );

    app.begin_quit();
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        if app.exit.is_requested() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(app.exit.is_requested(), "quit-all must end in exit");
    assert!(
        app.supervisor
            .as_ref()
            .is_none_or(|supervisor| supervisor.snapshots.is_empty()
                || !supervisor.snapshots.contains_key(&primary_id)),
        "the primary session must have been closed, not just left behind"
    );
}

/// `/quit` on the primary session is a request to leave Forge, so it takes the
/// same gate as `Ctrl+D` instead of closing the session the operator is
/// looking at. The same command in a managed session view still closes only
/// that session.
#[tokio::test]
async fn slash_quit_on_the_primary_session_takes_the_quit_gate() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    set_primary_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    );
    inject_synthetic_session(&mut app, "docs", forge_session::SupervisorTurnState::Queued);

    app.dispatch_line("/quit").await.unwrap();

    assert!(
        matches!(
            app.explorer_dialog.current(),
            Some(ExplorerDialog::QuitAll { .. })
        ),
        "the primary session's /quit must ask before stopping every session"
    );
    assert!(!app.quitting, "nothing is swept until the row is confirmed");

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The dialog body is the whole point: the operator sees the bill before
/// agreeing to it.
#[tokio::test]
async fn the_quit_all_confirm_names_what_is_lost() {
    let (dir, mut app, handle) = app_with_supervisor().await;
    let primary_id = app.selected_session_id;
    set_primary_turn_state(
        &mut app,
        primary_id,
        forge_session::SupervisorTurnState::Running,
    );
    let sibling_id = inject_synthetic_session(
        &mut app,
        "docs",
        forge_session::SupervisorTurnState::Running,
    );
    app.supervisor
        .as_mut()
        .unwrap()
        .snapshots
        .get_mut(&sibling_id)
        .unwrap()
        .queued_prompts = vec![(1, "never dispatched".into())];

    let path = dir.path().join("a.txt");
    app.open_file_in_editor(&path);
    app.editor_session
        .as_mut()
        .expect("primary editor")
        .substitute("one", "edited", true, true);

    app.begin_quit();
    let text = render_app_text(&mut app, 100, 40);

    assert!(text.contains("Quit All Sessions"), "missing title: {text}");
    assert!(text.contains("Quitting stops every session."));
    assert!(text.contains("2 sessions with a turn running — cancelled"));
    assert!(text.contains("1 queued prompt never dispatched"));
    assert!(
        text.contains("unsaved changes in main"),
        "the unsaved buffer must be named: {text}"
    );
    assert!(text.contains("Quit all 2 sessions"));
    assert!(text.contains("Cancel"));

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Put the selected session into a turn state the roster would report, so the
/// gate can be exercised without racing a real turn.
fn set_primary_turn_state(
    app: &mut TuiApp,
    session_id: uuid::Uuid,
    state: forge_session::SupervisorTurnState,
) {
    app.supervisor
        .as_mut()
        .unwrap()
        .snapshots
        .get_mut(&session_id)
        .expect("selected session in the roster")
        .task
        .turn_state = state;
}

/// Add a roster entry that no actor backs. Quit-all reads the roster, so this
/// is enough to pose the "work elsewhere" question the gate answers.
fn inject_synthetic_session(
    app: &mut TuiApp,
    label: &str,
    state: forge_session::SupervisorTurnState,
) -> uuid::Uuid {
    let template = app
        .supervisor
        .as_ref()
        .unwrap()
        .snapshots
        .values()
        .next()
        .expect("a roster entry to copy")
        .clone();
    let session_id = uuid::Uuid::new_v4();
    let mut snapshot = template;
    snapshot.task.session_id = session_id;
    snapshot.task.label = label.into();
    snapshot.task.ownership = forge_session::WorktreeOwnership::Managed;
    snapshot.task.turn_state = state;
    app.supervisor
        .as_mut()
        .unwrap()
        .snapshots
        .insert(session_id, snapshot);
    session_id
}

/// Supervised turns are driven by supervisor events, not the local submit
/// path, so the turn clock must be anchored when the actor reports Running.
/// Before this, the live line counted from `timing.started` (app uptime) and
/// showed values like `Waiting for the model · 642s` seconds after Enter.
#[tokio::test]
async fn a_running_supervised_turn_anchors_the_turn_clock() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let session_id = app.selected_session_id;
    let mut snapshot = app
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&session_id))
        .expect("primary snapshot")
        .clone();

    snapshot.task.turn_state = forge_session::SupervisorTurnState::Idle;
    app.sync_supervised_presentation(&snapshot);
    assert!(
        app.timing.turn_started.is_none(),
        "an idle session must not carry a turn clock"
    );

    snapshot.task.turn_state = forge_session::SupervisorTurnState::Running;
    app.sync_supervised_presentation(&snapshot);
    assert!(app.busy_state.is_active(), "a running turn is busy");
    assert!(
        app.timing.turn_started.is_some(),
        "a running supervised turn must anchor the turn clock"
    );

    snapshot.task.turn_state = forge_session::SupervisorTurnState::Completed;
    app.sync_supervised_presentation(&snapshot);
    assert!(
        app.timing.turn_started.is_none(),
        "an ended turn must clear the turn clock"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A completed supervised turn closes with the same `Response finished · …`
/// line a direct turn gets. Regression: the summary was only recorded on the
/// direct path, so actor-owned sessions lost their exitline entirely.
#[tokio::test]
async fn a_completed_supervised_turn_gets_a_summary_line() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.focus_block(FocusBlock::Composer);
    app.input.set_text("say hi".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    let session_id = app.selected_session_id;
    wait_for_turn_state(
        &mut app,
        session_id,
        forge_session::SupervisorTurnState::Completed,
    )
    .await;

    assert!(
        app.turn_summaries
            .iter()
            .any(|record| record.key.session == session_id.to_string()),
        "a supervised turn must record a summary: {:?}",
        app.turn_summaries
    );
    let rendered = render_app_text(&mut app, 120, 40);
    assert!(
        rendered.contains("Response finished"),
        "the exitline is missing:\n{rendered}"
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

/// Regression for #592: switching sessions rebinds the workspace, and rebinding
/// used to shell out to `git status` inline, stalling the UI. The header read
/// must be dispatched to a worker and the stale value dropped, never computed
/// on the switch path.
#[tokio::test]
async fn switching_workspace_defers_repo_header_to_worker() {
    let (dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("stale-repo".into()),
        branch: Some("stale-branch".into()),
        dirty: true,
    };
    let linked = dir.path().join("linked-worktree-592");
    std::fs::create_dir_all(&linked).unwrap();
    app.session_view.workspace_root = linked.canonicalize().unwrap();

    app.sync_selected_workspace();

    let linked = linked.canonicalize().unwrap();
    assert_eq!(app.runtime.cwd, linked);
    assert_eq!(app.repo_header_state.cwd, app.runtime.cwd);
    assert!(
        app.repo_header().repo_name.is_none(),
        "the switch path must not synchronously populate the repo header"
    );
    assert!(
        app.repo_header_state.refresh_rx.is_some(),
        "the repo header refresh must be dispatched to a worker"
    );
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
    let session = create_promptless_session(&mut app).await;

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
async fn supervisor_stream_batches_leave_time_for_input_without_losing_events() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let (tx, rx) = tokio::sync::broadcast::channel(1024);
    app.supervisor.as_mut().unwrap().events = rx;
    let session_id = app.selected_session_id;
    let mut expected = String::new();
    for index in 0..300 {
        let text = format!("{index},");
        expected.push_str(&text);
        tx.send(forge_session::SupervisorEvent::Stream {
            session_id,
            event: forge_types::ModelStreamEvent::TextDelta { text },
        })
        .unwrap();
    }
    app.poll_supervisor_events();
    assert!(!app.stream.preview.is_empty());
    assert!(
        app.stream.preview.len() < expected.len(),
        "one tick drained the entire burst"
    );
    assert!(expected.starts_with(&app.stream.preview));
    app.focus_block(FocusBlock::Composer);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "x");
    assert!(!app.supervisor.as_ref().unwrap().events.is_empty());
    for _ in 0..300 {
        if app.supervisor.as_ref().unwrap().events.is_empty() {
            break;
        }
        app.poll_supervisor_events();
    }
    assert_eq!(app.stream.preview, expected);
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

/// Regression: the operator's own message must reach the conversation when
/// the turn starts, not when the answer lands. The actor appended the prompt
/// to its session but published no snapshot until the turn's closing refresh,
/// so the reply streamed in below a blank where the question should have been.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_message_shows_before_the_answer_lands() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn forge_model::ModelClient> = Arc::new(GateModel::new(vec![(
        "hold open".to_string(),
        gate.clone(),
    )]));
    let (_dir, mut app, handle) = app_with_supervisor_and_model(model).await;
    let session_id = app.selected_session_id;

    app.focus_block(FocusBlock::Composer);
    app.input.set_text("hold open".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();

    // The gate holds the model, so anything on screen now is the operator's
    // own line. Publish it when the session records it, not a turn later.
    wait_for_user_messages(&mut app, session_id, 1).await;
    assert_eq!(
        app.selected_snapshot()
            .map(|snapshot| snapshot.task.turn_state),
        Some(forge_session::SupervisorTurnState::Running),
        "the answer landed before the model was released"
    );
    let rendered = render_app_text(&mut app, 120, 40);
    assert!(
        rendered.contains("hold open"),
        "conversation dropped the operator's message: {rendered}"
    );
    assert!(
        app.selected_snapshot().is_some_and(|snapshot| snapshot
            .transcript
            .messages()
            .iter()
            .all(|message| message.role != forge_types::MessageRole::Assistant)),
        "no answer should exist while the model is still gated"
    );

    gate.notify_one();
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
async fn n_allocates_an_unnamed_session_and_hands_the_cursor_to_its_composer() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let known: Vec<uuid::Uuid> = app
        .session_chrome
        .iter()
        .map(|task| task.session_id)
        .collect();
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();

    // `n` creates with no prompt at all, so nothing can park the creation on
    // the trust modal.
    assert!(app.overlay.is_none(), "prompt-less creation never parks");

    let created = wait_for_chrome_session(&mut app, |task| !known.contains(&task.session_id)).await;
    assert!(!created.session_id.is_nil());
    assert!(
        created.label.is_empty(),
        "the session is named later, from its first prompt"
    );
    let snapshot = app
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&created.session_id))
        .expect("created session snapshot");
    assert!(
        snapshot.queued_prompts.is_empty(),
        "creation must not carry a prompt into the queue"
    );
    assert_eq!(
        snapshot.task.turn_state,
        forge_session::SupervisorTurnState::Idle
    );

    // The created session is selected and its composer holds the cursor, so
    // the first prompt can be typed straight away.
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        if app.selected_session_id == created.session_id {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(app.selected_session_id, created.session_id);
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// `/new` is the composer-reachable twin of `n` in the Sessions tab: same
/// prompt-less creation, same cursor hand-off, so the first prompt can be
/// typed without visiting the navigator at all.
#[tokio::test]
async fn slash_new_allocates_the_same_unnamed_session_as_the_strip_key() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let known: Vec<uuid::Uuid> = app
        .session_chrome
        .iter()
        .map(|task| task.session_id)
        .collect();

    app.dispatch_line("/new").await.unwrap();

    assert!(app.overlay.is_none(), "prompt-less creation never parks");
    assert!(
        app.pending_turn.prompt().is_none(),
        "the command itself must not become the first prompt"
    );

    let created = wait_for_chrome_session(&mut app, |task| !known.contains(&task.session_id)).await;
    assert!(!created.session_id.is_nil());
    assert!(
        created.label.is_empty(),
        "the session is named later, from its first prompt"
    );
    let snapshot = app
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&created.session_id))
        .expect("created session snapshot");
    assert!(
        snapshot.queued_prompts.is_empty(),
        "creation must not carry a prompt into the queue"
    );

    // The created session is selected and its composer holds the cursor.
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        if app.selected_session_id == created.session_id {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(app.selected_session_id, created.session_id);
    assert_eq!(app.focus.block(), FocusBlock::Composer);
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
    let b = create_promptless_session(&mut app).await;
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

/// The navigator defaults to `Sessions` only once there is more than one
/// session; a single session keeps the file tree (`FORGE-DESIGN §7.7`).
#[tokio::test]
async fn navigator_defaults_to_sessions_once_a_second_session_exists() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    assert_eq!(
        app.effective_navigator_tab(),
        NavigatorTab::Files,
        "one session keeps the file tree"
    );

    let _ = create_promptless_session(&mut app).await;
    assert_eq!(
        app.effective_navigator_tab(),
        NavigatorTab::Sessions,
        "two sessions default to the list"
    );

    let rendered = render_app_text(&mut app, 120, 40);
    assert!(rendered.contains("Sessions"), "{rendered}");
    assert!(rendered.contains("Files"), "{rendered}");
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Ctrl+1 / Ctrl+2 flip the navigator tab and stick.
#[tokio::test]
async fn ctrl_tab_switches_the_navigator() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.handle_key(press(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.navigator_tab_explicit);
    assert_eq!(app.effective_navigator_tab(), NavigatorTab::Sessions);

    app.handle_key(press(KeyCode::Char('2'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.effective_navigator_tab(), NavigatorTab::Files);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// `↑` at the top of the session list reaches the navigator's tab row; below the
/// first row it keeps its cursor meaning (`FORGE-DESIGN §8.3`).
#[tokio::test]
async fn up_at_the_top_of_the_session_list_reaches_the_tab_row() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let _ = create_promptless_session(&mut app).await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);

    app.task_strip_selection = 1;
    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.task_strip_selection, 0, "the cursor moves first");
    assert!(
        !app.navigator_tab_row_focused,
        "a `↑` that still has somewhere to go must not jump into the row"
    );

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.navigator_tab_row_focused,
        "the second `↑` reaches the row"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::TaskStrip,
        "the pane keeps focus.block(), so `Tab` still cycles from it"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// From the file tree the same `↑` reaches the row, `←`/`→` walk it without
/// leaving it, and `↓` drops back into the pane the tab shows.
#[tokio::test]
async fn the_tab_row_switches_tabs_without_leaving_the_row() {
    use crate::widgets::{NavigatorRowStop, NavigatorTab};
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Files;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::Search);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.navigator_tab_row_focused,
        "an empty tree is already at the top, so one `↑` reaches the row"
    );

    // The row's stops run `Sessions · + · Files`, so `←` reaches the `+` cell
    // before the `Sessions` tab — and resting there leaves the pane alone.
    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::NewSession);
    assert_eq!(app.effective_navigator_tab(), NavigatorTab::Files);

    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.effective_navigator_tab(), NavigatorTab::Sessions);
    assert_eq!(
        app.focus.block(),
        FocusBlock::TaskStrip,
        "the pane under the row follows the tab, so no invisible block owns keys"
    );
    assert!(
        app.navigator_tab_row_focused,
        "`←`/`→` keep the keyboard on the row"
    );

    // Stepping back the other way walks the `+` cell on the way through.
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::NewSession);
    assert_eq!(
        app.effective_navigator_tab(),
        NavigatorTab::Sessions,
        "the cell never carries the active tab with it"
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.effective_navigator_tab(), NavigatorTab::Files);
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(
        !app.workspace_files.explorer.search_focused,
        "the pane under the row paints unfocused while the row holds the keys"
    );
    assert!(app.navigator_tab_row_focused);

    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        !app.navigator_tab_row_focused,
        "`↓` steps back into the pane"
    );
    assert_eq!(app.focus.block(), FocusBlock::Search);
    assert!(
        app.workspace_files.explorer.search_focused,
        "…which takes the keyboard and its caret back"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A chord can switch tabs while the row holds the keyboard. The cursor has to
/// move with the tab, or `←`/`→` would step from a tab the cursor is not drawn
/// on; the `+` stop is tab-independent and stays put.
#[tokio::test]
async fn a_tab_chord_moves_the_rows_cursor_with_the_tab() {
    use crate::widgets::{NavigatorRowStop, NavigatorTab};
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::Sessions);

    app.handle_key(press(KeyCode::Char('2'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::Files);

    // Resting on `+` and then switching tabs keeps the cursor on `+`.
    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::NewSession);
    app.handle_key(press(KeyCode::Char('1'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::NewSession);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The drawn row and the pointer's hit target are one geometry: three segments
/// sharing their edges, the `+` between the two tabs, and its joints surviving
/// the list's top border, which repaints that whole row (`§9.6`).
#[tokio::test]
async fn the_drawn_tab_row_carries_the_plus_cell() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app, handle) = app_with_supervisor().await;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    app.tick_render_state();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();

    let cell = app
        .navigator_new_session_area
        .expect("a repository frame draws the cell");
    let (x, y) = (cell.x, cell.y);
    let right = cell.x + cell.width - 1;
    assert_eq!(buffer[(x, y)].symbol(), "┬", "top joint with `Sessions`");
    assert_eq!(buffer[(right, y)].symbol(), "┬", "top joint with `Files`");
    assert_eq!(buffer[(x + 1, y + 1)].symbol(), "+", "the glyph");
    assert_eq!(
        buffer[(x, y + 2)].symbol(),
        "┴",
        "the bottom joint with `Sessions` survives the list's top border"
    );
    assert_eq!(
        buffer[(right, y + 2)].symbol(),
        "┴",
        "so does the one with `Files`"
    );
    assert_eq!(buffer[(x, y + 1)].symbol(), "│");
    assert_eq!(buffer[(right, y + 1)].symbol(), "│");
    assert_ne!(
        buffer[(x + 1, y + 1)].style().bg,
        Some(theme::accent_soft_bg()),
        "the cell never takes a tab's active ground"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The row's stops run `Sessions · + · Files` left to right: `←`/`→` walk them,
/// stop at each end, and `Enter` on the `+` cell creates a session — the same
/// prompt-less create the Sessions list's `n` runs (`FORGE-DESIGN §7.7`).
#[tokio::test]
async fn the_tab_rows_plus_cell_creates_a_session() {
    use crate::widgets::{NavigatorRowStop, NavigatorTab};
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Files;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::Search);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    // `↑` lands on the tab on screen, never on the `+` cell.
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::Files);

    // The right end of the row does not wrap back to the `+` cell.
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::Files);

    app.handle_key(press(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_row_stop, NavigatorRowStop::NewSession);
    assert_eq!(
        app.effective_navigator_tab(),
        NavigatorTab::Files,
        "the cell leaves the active tab where it was"
    );
    assert_eq!(
        app.focus.block(),
        FocusBlock::Search,
        "so the pane under the row keeps the keys it had"
    );
    assert!(app.navigator_tab_row_focused, "the row keeps the keyboard");

    let known: Vec<uuid::Uuid> = app
        .session_chrome
        .iter()
        .map(|task| task.session_id)
        .collect();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let created = wait_for_chrome_session(&mut app, |task| !known.contains(&task.session_id)).await;
    assert!(
        created.label.is_empty(),
        "the cell creates unnamed, like `n` and `/new`"
    );
    for _ in 0..300 {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        if app.selected_session_id == created.session_id {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(app.selected_session_id, created.session_id);
    assert_eq!(app.focus.block(), FocusBlock::Composer);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A click on the `+` cell is the same verb the row's `Enter` runs there, and it
/// has to beat the tab branch: the cell shares an edge with each tab, so a
/// mis-route would read as a tab switch.
#[tokio::test]
async fn clicking_the_plus_cell_creates_a_session_instead_of_switching_tabs() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    // `Files` is the stricter case: the cell sits beside the `Sessions` tab,
    // so a mis-route there would switch tabs instead of creating.
    app.navigator_tab = NavigatorTab::Files;
    app.navigator_tab_explicit = true;
    draw_app(&mut app, 120, 40);
    let cell = app
        .navigator_new_session_area
        .expect("a repository frame draws the cell");
    let known: Vec<uuid::Uuid> = app
        .session_chrome
        .iter()
        .map(|task| task.session_id)
        .collect();

    app.handle_mouse(left_click(cell.x + 1, cell.y + 1))
        .await
        .unwrap();

    assert_eq!(
        app.effective_navigator_tab(),
        NavigatorTab::Files,
        "the click must not switch to the tab the cell sits beside"
    );
    let created = wait_for_chrome_session(&mut app, |task| !known.contains(&task.session_id)).await;
    assert!(created.label.is_empty());
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// `Tab` still cycles blocks from the row: it is never a Tab stop of its own
/// (`FORGE-DESIGN §8.3`).
#[tokio::test]
async fn tab_still_cycles_blocks_from_the_tab_row() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.navigator_tab_row_focused);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.navigator_tab_row_focused, "the cycle clears the row");
    assert_ne!(
        app.focus.block(),
        FocusBlock::TaskStrip,
        "`Tab` moves to the next block"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Session verbs are inert on the row: they would act on the list it covers.
#[tokio::test]
async fn session_verbs_do_not_fire_from_the_tab_row() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.navigator_tab_row_focused);

    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    // The row swallows every bare key, so `n` is never a session verb there.
    // A leaked printable would surface as a chat draft.
    assert!(
        app.input.text.is_empty(),
        "`n` is inert on the row and must not start a draft"
    );
    assert!(app.navigator_tab_row_focused, "the row keeps the keyboard");
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.navigator_done_pending.is_empty(),
        "`x` is inert on the row"
    );
    assert!(app.navigator_tab_row_focused, "the row keeps the keyboard");
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Leaving the navigator drops the row with it: the row is a sub-focus of the
/// navigator column, not a block that can linger off-screen.
#[tokio::test]
async fn leaving_the_navigator_clears_the_tab_row() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.navigator_tab_row_focused);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        !app.navigator_tab_row_focused,
        "`Esc` steps back into the pane"
    );
    assert_eq!(app.focus.block(), FocusBlock::TaskStrip);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.navigator_tab_row_focused);
    app.focus_block(FocusBlock::Composer);
    assert!(!app.navigator_tab_row_focused);
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A single session has no tab bar, so there is no row to reach.
#[tokio::test]
async fn a_single_session_has_no_tab_row_to_reach() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Files);
    app.focus_navigator_tab_row();
    assert!(!app.navigator_tab_row_focused, "no repository, no tab bar");
}

/// `Space` opens the inline peek; typing fills the reply; `Esc` collapses.
#[tokio::test]
async fn space_peeks_and_esc_collapses_in_the_navigator() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.focus_block(FocusBlock::TaskStrip);
    let id = app.session_chrome[0].session_id;

    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_peek, Some(id));

    app.handle_key(press(KeyCode::Char('h'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_reply, "hi");

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.navigator_peek, None);
    assert!(app.navigator_reply.is_empty());
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// With peek set, the navigator renders the last answer and the reply box.
#[tokio::test]
async fn the_peek_renders_the_last_answer_and_reply() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let id = app.selected_session_id;
    app.focus_block(FocusBlock::Composer);
    app.input.set_text("hello".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_turn_state(&mut app, id, forge_session::SupervisorTurnState::Completed).await;

    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.navigator_peek = Some(id);
    app.navigator_reply = "push it".to_string();
    app.focus_block(FocusBlock::TaskStrip);

    let rendered = render_app_text(&mut app, 120, 40);
    assert!(rendered.contains("done"), "last answer missing: {rendered}");
    assert!(
        rendered.contains("push it"),
        "reply buffer missing: {rendered}"
    );
    assert!(
        rendered.contains("Enter send"),
        "peek hint missing: {rendered}"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// `x` on an idle managed session archives and cleans it; the primary is refused.
#[tokio::test]
async fn x_archives_and_cleans_an_idle_managed_session() {
    use crate::widgets::NavigatorTab;
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let sibling = create_promptless_session(&mut app).await;
    app.navigator_tab = NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling in list");

    // Select the sibling so archiving it exercises the selected-session path:
    // the navigator cursor and the prompt target must agree afterwards.
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.selected_session_id, sibling.session_id);

    app.focus_block(FocusBlock::TaskStrip);
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling still in list");
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let archived = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            app.poll_supervisor_events();
            let done = app
                .supervisor
                .as_ref()
                .and_then(|supervisor| supervisor.snapshots.get(&sibling.session_id))
                .is_none_or(|snapshot| {
                    snapshot.task.lifecycle == forge_session::SessionLifecycle::Archived
                });
            if done || std::time::Instant::now() >= deadline {
                break done;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    assert!(archived, "the idle session should have been archived");

    // The navigator drops the archived row rather than leaving it listed.
    let removed = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            app.poll_supervisor_events();
            let gone = !app
                .session_chrome
                .iter()
                .any(|item| item.session_id == sibling.session_id);
            if gone || std::time::Instant::now() >= deadline {
                break gone;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    assert!(removed, "the archived session should leave the navigator");

    // Regression: archiving the selected session left `selected_session_id`
    // pointed at the archived row while the navigator cursor fell back to
    // index 0, so context and navigation disagreed.
    let reselected = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            app.poll_supervisor_events();
            let moved = app.selected_session_id != sibling.session_id
                && app
                    .session_chrome
                    .iter()
                    .any(|item| item.session_id == app.selected_session_id);
            if moved || std::time::Instant::now() >= deadline {
                break moved;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    assert!(
        reselected,
        "archiving the selected session should move selection to a live session"
    );
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A clean archive must report the checkout removal, not the blocked-cleanup
/// warning. A removed Session leaves the roster entirely — `snapshots` drops
/// the row once its checkout is gone — so "still listed" is the signal that a
/// checkout is on disk, never a `removed` row the roster does not carry.
#[tokio::test]
async fn a_clean_archive_reports_the_checkout_removed_not_uncommitted_work() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let sibling = create_promptless_session(&mut app).await;
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling in list");
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling still in list");
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        // The removal's roster echo drops the row; the tracked command's
        // reply can land one tick later, so wait for both before reading the
        // feedback the retirement follow-up sets.
        let settled = app.pending_command_completions.is_empty()
            && app
                .supervisor
                .as_ref()
                .is_some_and(|supervisor| !supervisor.snapshots.contains_key(&sibling.session_id));
        if settled || std::time::Instant::now() >= deadline {
            assert!(settled, "the archived session's checkout should be gone");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(
        app.feedback.text, "worktree removed · branch kept",
        "a clean archive removed the checkout; the row must not claim uncommitted work"
    );
    assert_eq!(app.feedback.severity, FeedbackSeverity::Ok);

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The other side of the same branch: a checkout holding real uncommitted work
/// is kept, so the row stays in the roster as `retained` and the warning is
/// the honest report.
#[tokio::test]
async fn a_blocked_cleanup_reports_the_kept_checkout() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    let sibling = create_promptless_session(&mut app).await;
    let workspace = app
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.snapshots.get(&sibling.session_id))
        .map(|snapshot| snapshot.task.workspace.clone())
        .expect("the sibling's worktree");
    std::fs::write(workspace.join("uncommitted.txt"), "keep me").unwrap();

    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling in list");
    app.focus_block(FocusBlock::TaskStrip);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.task_strip_selection = app
        .session_chrome
        .iter()
        .position(|item| item.session_id == sibling.session_id)
        .expect("sibling still in list");
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        let settled = app.pending_command_completions.is_empty()
            && app
                .supervisor
                .as_ref()
                .and_then(|supervisor| supervisor.snapshots.get(&sibling.session_id))
                .is_some_and(|snapshot| {
                    snapshot.task.lifecycle == forge_session::SessionLifecycle::Retained
                });
        if settled || std::time::Instant::now() >= deadline {
            assert!(settled, "the blocked cleanup should settle as retained");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(app.feedback.severity, FeedbackSeverity::Warn);
    assert_eq!(app.feedback.text, "worktree kept — it has uncommitted work");
    assert!(
        app.session_chrome
            .iter()
            .any(|item| item.session_id == sibling.session_id),
        "a retained Session still owns its checkout, so the row stays listed"
    );
    assert!(workspace.join("uncommitted.txt").is_file());

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Below the navigator width the session state collapses to a status chip.
/// Regression for #643: `attention` was sticky, so a session that once stopped
/// for input kept showing `● needs you` while its turn was actually running.
/// A published snapshot must re-derive the row from turn state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_turn_clears_stale_sidebar_attention() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn forge_model::ModelClient> =
        Arc::new(GateModel::new(vec![("hold".to_string(), gate.clone())]));
    let (_dir, mut app, handle) = app_with_supervisor_and_model(model).await;
    let session_id = app.selected_session_id;

    app.focus_block(FocusBlock::Composer);
    app.input.set_text("hold".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_turn_state(
        &mut app,
        session_id,
        forge_session::SupervisorTurnState::Running,
    )
    .await;

    // Simulate the pre-fix sticky flag: attention left true on a live turn.
    if let Some(task) = app
        .session_chrome
        .iter_mut()
        .find(|task| task.session_id == session_id)
    {
        task.attention = true;
    }

    // Any subsequent roster publication must derive attention from truth.
    handle
        .command(forge_session::SupervisorCommand::Refresh)
        .await
        .unwrap();
    app.poll_supervisor_events();

    let task = app
        .session_chrome
        .iter()
        .find(|task| task.session_id == session_id)
        .expect("primary row");
    assert!(
        !task.attention,
        "a running turn must not read as needs-you: {task:?}"
    );
    assert!(
        task.is_working(),
        "a running turn must read as working: {task:?}"
    );

    app.navigator_tab = crate::widgets::NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    let rendered = render_app_text(&mut app, 120, 40);
    assert!(
        rendered.contains("running"),
        "sidebar missing running: {rendered}"
    );
    assert!(
        !rendered.contains("needs you"),
        "sidebar wrongly reads needs-you: {rendered}"
    );

    gate.notify_one();
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_narrow_navigator_falls_back_to_a_status_chip() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.session_chrome[0].attention = true;
    let rendered = render_app_text(&mut app, 100, 40);
    assert!(rendered.contains("need"), "chip missing: {rendered}");
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// The chip is one row: its spinner is the only way a collapsed column still
/// separates "work in flight" from "work waiting on you" (`§482`), and it steps
/// on the same clock as the rows it replaces.
#[tokio::test]
async fn the_collapsed_chip_keeps_the_working_spinner_moving() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.session_chrome[0].lifecycle = forge_types::TaskLifecycle::Working;
    app.session_chrome[0].secondary = Some("running".to_string());

    app.session_row_step = 0;
    let first = render_app_text(&mut app, 100, 40);
    app.session_row_step = 3;
    let second = render_app_text(&mut app, 100, 40);

    assert!(first.contains("working"), "chip missing: {first}");
    assert!(
        crate::widgets::turn_line::SPINNER_FRAMES
            .iter()
            .any(|frame| first.contains(frame)),
        "the chip carries no spinner frame: {first}"
    );
    assert_ne!(first, second, "the chip's spinner did not step");
    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A row's marker comes from what the session did, not from whether a turn is
/// live. Reading it from the live-turn flags alone rendered every finished
/// session `○`, so a turn that failed and a turn that finished cleanly looked
/// identical in the column the operator scans.
#[tokio::test]
async fn a_finished_session_keeps_the_outcome_its_marker_reports() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = crate::widgets::NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.session_chrome[0].label = "Fix login redirect".into();
    app.session_chrome[0].attention = false;

    let row_of = |rendered: &str| {
        rendered
            .lines()
            .find(|line| line.contains("Fix login redirect"))
            .expect("session row")
            .to_string()
    };

    app.session_chrome[0].lifecycle = forge_types::TaskLifecycle::Failed;
    app.session_chrome[0].secondary = Some("failed".into());
    let failed = row_of(&render_app_text(&mut app, 120, 40));
    assert!(failed.contains('✗'), "a failed session: {failed:?}");
    assert!(
        !failed.contains('○'),
        "a failed session still renders the idle ring: {failed:?}"
    );

    app.session_chrome[0].lifecycle = forge_types::TaskLifecycle::Completed;
    app.session_chrome[0].secondary = Some("completed".into());
    let completed = row_of(&render_app_text(&mut app, 120, 40));
    assert!(
        completed.contains('✓'),
        "a completed session: {completed:?}"
    );
    assert!(
        !completed.contains('✗'),
        "a completed session still renders the failure mark: {completed:?}"
    );

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// A row that says "running" has to look like it. The marker steps one frame
/// per event-loop tick, so two frames of the same app show two different
/// glyphs — and it steps off the tick, not the wall clock, so the motion stops
/// with the work.
#[tokio::test]
async fn a_running_session_row_turns_its_spinner() {
    let (_dir, mut app, handle) = app_with_supervisor().await;
    app.navigator_tab = crate::widgets::NavigatorTab::Sessions;
    app.navigator_tab_explicit = true;
    app.session_chrome[0].label = "Fix login redirect".into();
    app.session_chrome[0].lifecycle = forge_types::TaskLifecycle::Working;
    app.session_chrome[0].secondary = Some("running".into());
    app.session_chrome[0].attention = false;

    // The marker cell is the char before the space that precedes the label.
    let glyph_of = |rendered: &str| {
        let line = rendered
            .lines()
            .find(|line| line.contains("Fix login redirect"))
            .expect("session row");
        let label = line.find("Fix login redirect").expect("label");
        line[..label].chars().rev().nth(1).expect("marker cell")
    };

    let first = glyph_of(&render_app_text(&mut app, 120, 40));
    let second = glyph_of(&render_app_text(&mut app, 120, 40));
    for glyph in [first, second] {
        assert!(
            crate::widgets::turn_line::SPINNER_FRAMES
                .iter()
                .any(|frame| frame.starts_with(glyph)),
            "{glyph:?} is not a frame the running marker speaks"
        );
    }
    assert_ne!(first, second, "the running row is frozen");

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Regression: a UI action that mutates a busy Session must not block the
/// terminal owner. Model selection used to await command completion, so a
/// running turn deferred it while the TUI stopped reading keys and painting;
/// it now queues immediately and its follow-up lands when the actor frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tracked_command_applies_after_a_busy_turn_without_blocking_input() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let model: Arc<dyn forge_model::ModelClient> = Arc::new(GateModel::new(vec![(
        "hold open".to_string(),
        gate.clone(),
    )]));
    let (_dir, app, handle) = app_with_supervisor_and_model(model).await;
    let mut app = Box::new(app);
    let session_id = app.selected_session_id;

    app.focus_block(FocusBlock::Composer);
    app.input.set_text("hold open".to_string());
    app.submit_composer_message().await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    wait_for_turn_state(
        &mut app,
        session_id,
        forge_session::SupervisorTurnState::Running,
    )
    .await;

    // The actor holds its session mutex for the whole turn. Queueing the model
    // change must return immediately and leave the command pending.
    let started = std::time::Instant::now();
    assert!(app.submit_session_command_tracked(
        forge_session::SupervisorCommand::SetModel {
            session_id,
            model_id: "queued-model".into(),
            route_id: String::new(),
            reasoning_effort: None,
        },
        CommandFollowUp::Toast("model applied".into()),
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_millis(250),
        "queueing the command blocked the terminal owner for {:?}",
        started.elapsed()
    );
    assert_eq!(app.pending_command_completions.len(), 1);

    // The TUI stays live: a key lands in the composer while the command waits.
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.contains('x'));
    app.poll_pending_commands();
    assert_eq!(
        app.pending_command_completions.len(),
        1,
        "a command deferred by a busy actor must stay pending, not be dropped"
    );

    // Release the turn; the deferred command executes and its follow-up lands.
    gate.notify_one();
    wait_for_turn_state(
        &mut app,
        session_id,
        forge_session::SupervisorTurnState::Completed,
    )
    .await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.poll_supervisor_events();
        app.poll_pending_commands();
        let applied = app
            .supervisor
            .as_ref()
            .and_then(|supervisor| supervisor.snapshots.get(&session_id))
            .and_then(|snapshot| snapshot.details.as_ref())
            .is_some_and(|details| details.active_model == "queued-model");
        if applied && app.pending_command_completions.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the deferred model change never applied"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    handle
        .command(forge_session::SupervisorCommand::Shutdown)
        .await
        .unwrap();
}

/// Create a prompt-less managed session directly, bypassing the inline
/// composer — for tests that only need a sibling to exist (the composer's
/// first prompt would otherwise start a model turn).
async fn create_promptless_session(app: &mut TuiApp) -> SessionChromeItem {
    let before: std::collections::HashSet<uuid::Uuid> = app
        .session_chrome
        .iter()
        .map(|item| item.session_id)
        .collect();
    app.focus_block(FocusBlock::TaskStrip);
    app.submit_session_command(forge_session::SupervisorCommand::CreateSession {
        label: String::new(),
        first_prompt: None,
    });
    wait_for_chrome_session(app, |item| !before.contains(&item.session_id)).await
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
