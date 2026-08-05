//! Repo header, footer, and status chrome tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

use super::super::util::footer_provider_id;

/// `repo_header()` must be a pure read of the cached field. If someone
/// reintroduces the `git` subprocess into it, the sentinel is overwritten by
/// real repo data and this fails — which is the point.
#[tokio::test]
async fn repo_header_reads_cache_without_shelling_out() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("sentinel-repo".into()),
        branch: Some("sentinel-branch".into()),
        dirty: true,
    };

    let header = app.repo_header();

    assert_eq!(header.repo_name.as_deref(), Some("sentinel-repo"));
    assert_eq!(header.branch.as_deref(), Some("sentinel-branch"));
    assert!(header.dirty);
}

/// Drawing must not derive the header either — several draws in a row leave
/// the cached sentinel untouched, proving the render path only reads it.
#[tokio::test]
async fn drawing_does_not_rederive_repo_header() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("sentinel-repo".into()),
        branch: Some("sentinel-branch".into()),
        dirty: true,
    };
    // Keep the TTL from firing a background refresh during the assertions.
    app.repo_header_state.refreshed_at = Instant::now();

    for _ in 0..3 {
        draw_app(&mut app, 120, 40);
    }

    assert_eq!(app.repo_header().branch.as_deref(), Some("sentinel-branch"));
}

/// FORGE-DESIGN 9.7: do not clear visible Git information during a refresh.
/// A dropped sender (failed refresh) must leave the last known header intact.
#[tokio::test]
async fn failed_repo_header_refresh_keeps_last_known_value() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("kept-repo".into()),
        branch: Some("kept-branch".into()),
        dirty: true,
    };
    let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
    drop(tx); // simulate a refresh worker that died
    app.repo_header_state.refresh_rx = Some(rx);

    app.poll_repo_header();

    assert_eq!(app.repo_header().branch.as_deref(), Some("kept-branch"));
    assert_eq!(app.repo_header().repo_name.as_deref(), Some("kept-repo"));
    assert!(app.repo_header().dirty);
    assert!(app.repo_header_state.refresh_rx.is_none());
}

/// Changing the working directory must invalidate the cached header on the
/// very next poll, so the header never describes the previous directory.
#[tokio::test]
async fn cwd_change_refreshes_repo_header_immediately() {
    let (dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("stale-repo".into()),
        branch: Some("stale-branch".into()),
        dirty: true,
    };

    let moved = dir.path().join("elsewhere");
    std::fs::create_dir_all(&moved).unwrap();
    app.runtime.cwd = moved.clone();
    app.poll_repo_header();

    assert_eq!(app.repo_header_state.cwd, moved);
    assert_eq!(app.repo_header().repo_name.as_deref(), Some("elsewhere"));
    // Plain directory, no git metadata: no branch, and not reported dirty.
    assert!(app.repo_header().branch.is_none());
    assert!(!app.repo_header().dirty);
}

/// An in-flight refresh that has not produced a value yet must be retained
/// rather than dropped, and must not disturb the current header.
#[tokio::test]
async fn pending_repo_header_refresh_is_retained() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header_state.cache = RepoHeaderCache {
        repo_name: Some("kept-repo".into()),
        branch: Some("kept-branch".into()),
        dirty: false,
    };
    let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
    app.repo_header_state.refresh_rx = Some(rx);

    app.poll_repo_header();

    assert!(
        app.repo_header_state.refresh_rx.is_some(),
        "pending refresh must survive"
    );
    assert_eq!(app.repo_header().branch.as_deref(), Some("kept-branch"));
    drop(tx);
}

#[tokio::test]
async fn final_shell_rendering_matrix_covers_v31_states_without_obsolete_chrome() {
    let sizes = [(80, 24), (120, 40), (160, 50), (240, 60)];
    let mut scenarios: Vec<(&str, TempDir, TuiApp, Vec<&str>)> = Vec::new();

    let (dir, app) = focus_test_app().await;
    scenarios.push(("conversation idle", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Model;
    app.timing.started = Some(Instant::now());
    scenarios.push(("agent thinking", dir, app, vec!["thinking"]));

    let (dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    scenarios.push(("files open", dir, app, vec!["FILES", "Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = false;
    scenarios.push(("files closed", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("src").join("matrix.rs");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "fn matrix() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(file.clone()))
        .await
        .unwrap();
    scenarios.push(("file open", dir, app, vec!["matrix.rs"]));

    let (dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    scenarios.push(("diff", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    scenarios.push((
        "background run",
        dir,
        app,
        vec!["Running cargo test", "View output"],
    ));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    scenarios.push(("run open", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    if let Some(record) = app.run.current.as_mut() {
        record.state = RunState::Failed;
        record.exit_status = Some(101);
    }
    let run_id = app.current_run_id().unwrap();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
        .await
        .unwrap();
    scenarios.push(("run failed", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    set_pending_hitl(
        &mut app,
        direct_hitl_payload("matrix-approval", "src/main.rs"),
    );
    scenarios.push((
        "approval",
        dir,
        app,
        vec!["approval · read_file", "type yes | no | remember"],
    ));

    let (dir, app) = focus_test_app().await;
    scenarios.push(("default shell", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.open_bottom_panel();
    scenarios.push(("bottom open", dir, app, vec!["Terminal", "Describe a task"]));

    for (name, _dir, mut app, expected) in scenarios {
        for (width, height) in sizes {
            let text = render_app_text(&mut app, width, height);
            assert!(
                !text.contains(" Chat  Editor  Diff "),
                "{name} at {width}x{height} restored permanent tabs:\n{text}"
            );
            assert!(
                !text.contains("BOTTOM"),
                "{name} at {width}x{height} restored BOTTOM label:\n{text}"
            );
            assert!(
                !text.contains("Ctrl+P close"),
                "{name} at {width}x{height} restored shortcut manual:\n{text}"
            );
            assert!(
                !text.to_ascii_lowercase().contains("mouse"),
                "{name} at {width}x{height} must not show mouse chrome:\n{text}"
            );
            let lower_text = text.to_ascii_lowercase();
            assert!(
                expected
                    .iter()
                    .any(|needle| lower_text.contains(&needle.to_ascii_lowercase())),
                "{name} at {width}x{height} missing expected state {:?}:\n{text}",
                expected
            );
            // New centered header still contains "Forge" and the lifecycle state
            assert!(
                text.contains("Forge"),
                "top bar should contain Forge brand: {text}"
            );
        }
    }
}

#[test]
fn footer_limits_parser_keeps_only_inline_limit_fields() {
    let limits = footer_limits_from_report(&[
        "Provider: OpenAI Codex".into(),
        "Session limit: 75% remaining".into(),
        "Weekly limit: 50% remaining".into(),
        "Credits: unlimited".into(),
    ]);

    assert_eq!(limits.usage, "Session limit: 75% remaining");
    assert_eq!(limits.weekly_limit, "Weekly limit: 50% remaining");
    assert_eq!(limits.credits, "Credits: unlimited");
}

#[test]
fn recent_resume_sessions_lists_previous_valid_journals() {
    let dir = TempDir::new().unwrap();
    let current = uuid::Uuid::new_v4();
    let previous = uuid::Uuid::new_v4();
    std::fs::write(dir.path().join(format!("{current}.db")), "").unwrap();
    std::fs::write(dir.path().join(format!("{previous}.db")), "").unwrap();
    std::fs::write(dir.path().join("not-a-session.db"), "").unwrap();
    std::fs::write(dir.path().join(format!("{}.txt", uuid::Uuid::new_v4())), "").unwrap();

    let sessions = recent_resume_sessions(dir.path(), current, 10).unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, previous);
}

#[test]
fn status_model_from_app_fields() {
    // layout + status integration smoke
    let m = StatusModel {
        status: forge_types::TaskLifecycle::Ready,
        session_short: "abc".into(),
        model: "m".into(),
        provider: "mock".into(),
        effort: "auto".into(),
        ctx_pct: 0.2,
        busy: false,
        busy_phase: BusyPhase::Idle,
        connect_profile: None,
        provider_connected: true,
        vendor_label: None,
        route_label: None,
        web_search_label: None,
        tools_visible: 0,
        prompt_cache_hits: 0,
        prompt_cache_writes: 0,
        repo_name: None,
        branch: None,
        dirty: false,
        resource: None,
        activity: None,
        progress_description: None,
        failure_category: None,
        waiting_detail: None,
    };
    assert_eq!(m.status_label().0, "Ready");
}

#[tokio::test]
async fn header_status_follows_session_lifecycle() {
    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    let ready = app.refresh_status_model();
    assert_eq!(ready.turn_lifecycle(), TurnLifecycle::Ready);
    assert!(ready.status_label().0.contains("Ready"));

    // A real task must actually be started for the authoritative lifecycle
    // to read Working — `busy` alone is UI activity detail, not lifecycle.
    app.session
        .append_user_message("do something")
        .await
        .unwrap();
    app.busy_state.active = true;
    app.busy_state.phase = BusyPhase::Tool {
        name: "read_file".into(),
    };
    let working = app.refresh_status_model();
    assert_eq!(working.turn_lifecycle(), TurnLifecycle::Working);
    assert!(working.status_label().0.contains("Working"));
    assert!(
        working.status_label().0.contains("Reading files"),
        "{:?}",
        working.status_label().0
    );

    app.busy_state.active = false;
    app.busy_state.phase = BusyPhase::Idle;
    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Completed;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Completed
    );

    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Failed;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Failed
    );

    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Cancelled;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Cancelled
    );

    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Interrupted;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Interrupted
    );

    set_pending_hitl(&mut app, direct_hitl_payload("h", "x.txt"));
    let waiting = app.refresh_status_model();
    assert_eq!(waiting.turn_lifecycle(), TurnLifecycle::Waiting);
    assert!(waiting.status_label().0.contains("Approval required"));
}

#[tokio::test]
async fn header_status_switches_with_selected_session() {
    let dir = TempDir::new().unwrap();
    let mut completed = session_for_workspace_with_model(
        dir.path(),
        Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "done-a".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }])),
    )
    .await;
    completed.run_user_message("a").await.unwrap();
    assert_eq!(
        completed.active_task.lifecycle,
        forge_types::TaskLifecycle::Completed
    );
    let id_a = completed.session_id;

    let running =
        session_for_workspace_with_model(dir.path(), Arc::new(MockModelClient::script(vec![])))
            .await;
    let id_b = running.session_id;

    let mut app = TuiApp::new(
        completed,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Completed"));

    app.session.resume_session(id_b).await.unwrap();
    assert_eq!(
        app.session.active_task.lifecycle,
        forge_types::TaskLifecycle::Interrupted
    );
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Interrupted"));

    app.session.resume_session(id_a).await.unwrap();
    assert_eq!(
        app.session.active_task.lifecycle,
        forge_types::TaskLifecycle::Completed
    );
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Completed"));
}

#[tokio::test]
async fn status_chrome_shows_not_connected_badge() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-test".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.profile = None;
    app.connect.store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );
    app.refresh_connection_ui();
    let backend = TestBackend::new(100, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.to_ascii_lowercase().contains("not connected") || text.contains("○"),
        "missing not-connected chrome:\n{text}"
    );
}

#[tokio::test]
async fn tui09_chrome_includes_model_on_frame() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-test".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // Isolate from any real, ambient connect credentials on the host so the
    // header chip this test guards against duplicating can't render.
    app.connect.profile = None;
    app.connect.store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );
    let chrome = app.refresh_status_model();
    assert_eq!(chrome.provider, "native");
    assert!(chrome.model.contains("gpt-test"));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    // Model lives on the composer chip row (not the status header).
    assert!(
        text.contains("gpt-test"),
        "expected model on composer chips:\n{text}"
    );
    assert!(
        !text.contains("in 0 · out 0 · total 0"),
        "usage spam must stay off the default chrome:\n{text}"
    );
}

#[tokio::test]
async fn tui09_narrow_frame_still_shows_model_or_ctx() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mymodel".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // Isolate from any real, ambient connect credentials on the host so the
    // header chip this test guards against duplicating can't render.
    app.connect.profile = None;
    app.connect.store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );
    // Width 60: no sidebar per layout MIN_WIDTH 80
    let backend = TestBackend::new(60, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("Forge"),
        "narrow frame missing app identity:\n{text}"
    );
    // Model/vendor live once, in the footer chip row — never duplicated in
    // the narrow header chrome.
    assert!(
        text.contains("[mymodel]"),
        "model chip should render in the footer:\n{text}"
    );
    assert!(
        !text.contains("ctx"),
        "narrow chrome must not duplicate ctx/usage metadata:\n{text}"
    );
}

#[tokio::test]
async fn tui09_status_renders_structured_session_card() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.notice_state.items.is_empty());
    let lines = match app.overlay.as_ref() {
        Some(Overlay::StatusReport { lines, .. }) => lines,
        other => panic!("expected status overlay, got {other:?}"),
    };
    assert!(lines.iter().any(|line| line.contains("provider=")));
    assert!(lines.iter().any(|line| line.contains("model=")));

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.contains("Status"),
        "status overlay should render:\n{text}"
    );
    assert!(
        text.contains("provider="),
        "status overlay should include session fields:\n{text}"
    );
}

#[tokio::test]
async fn tui08_report_error_writes_banner_feedback_and_activity() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.report_error("upstream returned 429 rate limit exceeded");
    assert_eq!(app.feedback.severity, FeedbackSeverity::Error);
    assert!(app.feedback.text.contains("429"));
    assert!(app.status_state.message.contains("429"));
    assert!(
        app.banner_state.items.iter().any(|b| matches!(
            b,
            ChatItem::Banner {
                kind: BannerKind::Error,
                ..
            }
        )),
        "expected error banner in ui_banners"
    );
    assert!(
        app.activity
            .all()
            .iter()
            .any(|i| i.kind == ActivityKind::Error),
        "expected error activity"
    );
}

#[tokio::test]
async fn tui08_feedback_strip_visible_on_frame() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.report_error("429 rate limit");
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("rate limited") || text.contains("429") || text.contains("Model error"),
        "frame missing feedback:\n{text}"
    );
}

#[tokio::test]
async fn tui10_activity_feed_records_model_and_error() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.push_activity(
        ActivityKind::Model,
        FeedbackSeverity::Info,
        "model call started",
    );
    app.report_error("429 rate limit");
    assert!(app.activity.len() >= 2);
    let recent: Vec<_> = app
        .activity
        .recent(10)
        .iter()
        .map(|i| i.summary.clone())
        .collect();
    assert!(
        recent
            .iter()
            .any(|s| s.contains("rate") || s.contains("429") || s.contains("Model")),
        "recent={recent:?}"
    );
    assert_eq!(app.busy_state.phase, BusyPhase::Idle);
}

#[tokio::test]
async fn elapsed_status_persists_during_answer_and_tool_processing() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.busy_state.active = true;
    app.timing.started = Some(Instant::now() - Duration::from_millis(1200));
    app.stream.preview = "partial answer".into();
    assert_eq!(app.busy_status_detail().as_deref(), Some("Working... 1.2s"));

    app.stream.preview.clear();
    app.busy_state.phase = BusyPhase::Tool {
        name: "read_file".into(),
    };
    assert!(app
        .busy_status_detail()
        .unwrap()
        .starts_with("Working... 1.2s"));
}

#[tokio::test]
async fn tui10_busy_phase_model_during_turn_clears_after() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("hello").await.unwrap();
    assert_eq!(app.busy_state.phase, BusyPhase::Model);
    assert!(app.pending_turn.prompt.is_some());
    app.drain_pending_prompt(None).await.unwrap();
    assert_eq!(app.busy_state.phase, BusyPhase::Idle);
    assert!(
        app.activity
            .all()
            .iter()
            .any(|i| i.kind == ActivityKind::Model),
        "expected model activity"
    );
}

#[tokio::test]
async fn tui08_context_sets_feedback_strip() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
}

#[test]
fn exit_summary_formats_token_usage() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 6_094,
            completion_tokens: 36,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 19,
            prompt_cache_hits: 5_504,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        format_exit_token_usage(&report),
        "Token usage: total=6,130 input=6,094 (+ 5,504 cached) output=36 (reasoning 19)"
    );
}

#[test]
fn footer_usage_formats_with_total_and_commas() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 6_094,
            completion_tokens: 36,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 19,
            prompt_cache_hits: 5_504,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        footer_usage_summary_with_cost(&report, None),
        "in 6,094 · out 36 · total 6,130"
    );
}

#[test]
fn footer_usage_includes_cached_cost() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        footer_usage_summary_with_cost(
            &report,
            Some(forge_connect::CatalogCost {
                input: 3.0,
                output: 15.0,
            })
        ),
        "in 1,000,000 · out 500,000 · total 1,500,000 · $10.5000"
    );
}

#[test]
fn footer_limits_use_connected_profile_instead_of_native_transport() {
    assert_eq!(
        footer_provider_id("native", Some("openai-codex")),
        "openai-codex"
    );
    assert_eq!(footer_provider_id("mock", None), "mock");
}
