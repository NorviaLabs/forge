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
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.timing.started = Some(Instant::now());
    scenarios.push(("agent thinking", dir, app, vec!["Describe a task"]));

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
    set_pending_hitl(
        &mut app,
        direct_hitl_payload("matrix-approval", "src/main.rs"),
    );
    scenarios.push((
        "approval",
        dir,
        app,
        vec!["Forge wants to run this tool.", "Run once"],
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
            // Top bar is identity-only now — directory + branch, always present.
            assert!(
                text.contains('⌂'),
                "top bar should contain the directory identity line: {text}"
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
        cwd_display: "~".to_string(),
        resource: None,
        activity: None,
        progress_description: None,
        failure_category: None,
        waiting_detail: None,
        incomplete_checks: None,
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
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Tool {
        name: "read_file".into(),
    });
    let working = app.refresh_status_model();
    assert_eq!(working.turn_lifecycle(), TurnLifecycle::Working);
    assert!(working.status_label().0.contains("Working"));
    assert!(
        working.status_label().0.contains("Reading files"),
        "{:?}",
        working.status_label().0
    );

    app.busy_state.stop();
    app.busy_state.set_phase(BusyPhase::Idle);
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

/// End-to-end for the false-failure fix, through every layer that decides
/// what the user is told: real tool execution -> completion evaluation ->
/// `turn_incomplete_checks` event -> transcript snapshot -> status model ->
/// rendered status bar.
///
/// The turn edits a file (verifies) and runs a command that exits non-zero
/// (does not). Both calls are in a single model response on purpose: that is
/// the shape that used to report "Failed / No file modifications were
/// successfully applied" while the edit sat on disk.
#[tokio::test]
async fn completed_turn_renders_unfinished_checks_without_claiming_failure() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("notes.txt"), "before\n").unwrap();

    let model = Arc::new(MockModelClient::script(vec![
        ModelResponse {
            text: String::new(),
            tool_calls: vec![
                forge_types::ToolCall {
                    id: "call-edit".into(),
                    name: "edit".into(),
                    arguments: json!({
                        "path": "notes.txt",
                        "old_string": "before",
                        "new_string": "after"
                    }),
                },
                forge_types::ToolCall {
                    id: "call-check".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "exit 127"}),
                },
            ],
            usage: None,
            thinking: None,
        },
        ModelResponse {
            text: "Edited the file; the check did not run.".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        },
    ]));

    // Governance off: this test is about how an outcome is *reported*, not
    // about approval gating, and a HITL pause would park the turn in Waiting.
    let mut session = AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("j"),
            enable_context_lifecycle: true,
            enable_governance: false,
            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    session.run_user_message("edit and check").await.unwrap();

    // Ground truth first: the edit really did land.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "after\n"
    );
    assert_eq!(
        session.active_task.lifecycle,
        forge_types::TaskLifecycle::Completed
    );

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            cwd: dir.path().to_path_buf(),
            ..test_runtime_config()
        },
    );
    let (label, style) = app.refresh_status_model().status_label();

    // The state stays Completed, and the unfinished check is named next to it
    // (short command names are named outright; see the degradation test below).
    assert!(label.contains("Completed"), "{label}");
    assert!(label.contains("exit 127 didn't finish"), "{label}");
    assert!(!label.contains("Failed"), "{label}");
    assert_eq!(style, TurnLifecycle::Completed.style());

    // ...and it survives the real render, not just the model.
    let rendered = render_app_text(&mut app, 160, 24);
    assert!(rendered.contains("didn't finish"), "{rendered}");
    assert!(
        !rendered.contains("No file modifications"),
        "a verified edit must never render as no modifications:\n{rendered}"
    );
}

/// The detail shares the footer row with the model/effort/mode identity, so a
/// long command name must degrade to a count rather than crowding out which
/// model the user is talking to.
#[tokio::test]
async fn a_long_unfinished_check_name_degrades_to_a_count() {
    let (_dir, mut app) = focus_test_app().await;
    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Completed;
    app.session.events.push(forge_core::TurnEvent {
        kind: "turn_incomplete_checks".into(),
        detail: "python -m pytest tests/test_help.py --verbose --tb=short".into(),
    });
    app.session_view = SessionSnapshot::capture(&app.session);

    let label = app.refresh_status_model().status_label().0;
    assert!(label.contains("1 check didn't finish"), "{label}");
    assert!(!label.contains("pytest"), "{label}");
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
    // Model lives on the footer's which-LLM control (not the status header).
    assert!(
        text.contains("gpt-test"),
        "expected model in the footer:\n{text}"
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
        text.contains('⌂'),
        "narrow frame missing app identity:\n{text}"
    );
    // The mode label's width changes what else fits on this row: "Manual" is
    // two columns wider than "Auto", and at width 60 that is enough to push
    // the model id out of the footer entirely. So which assertion holds
    // depends on what mode this host can reach.
    let mode_label = forge_core::permission_ceiling().0.label();
    assert!(
        text.contains(mode_label),
        "mode chip should render in the footer:\n{text}"
    );
    if mode_label == "Auto" {
        // The usage slot takes more of the row than the old flag, so the model
        // id middle-truncates harder at this width. Assert the ellipsis and
        // the tail survive. Header chrome must never duplicate
        // model/vendor/ctx.
        assert!(
            text.contains('…') && text.contains("el"),
            "model id should render in the footer:\n{text}"
        );
    }
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
    assert_eq!(app.busy_state.phase(), BusyPhase::Idle);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.busy_state.activate();
    app.timing.started = Some(Instant::now() - Duration::from_millis(1200));
    app.stream.preview = "partial answer".into();
    assert_eq!(app.busy_status_detail().as_deref(), Some("Working... 1.2s"));

    app.stream.preview.clear();
    app.busy_state.set_phase(BusyPhase::Tool {
        name: "read_file".into(),
    });
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("hello").await.unwrap();
    assert_eq!(app.busy_state.phase(), BusyPhase::Model);
    assert!(app.pending_turn.has_prompt());
    app.drain_pending_prompt(None).await.unwrap();
    assert_eq!(app.busy_state.phase(), BusyPhase::Idle);
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

/// The per-frame snapshot is the contract 1c rests on: render paths read
/// `session_view`, so `draw` must refresh it or the screen shows the previous
/// frame's session state.
#[tokio::test]
async fn draw_refreshes_the_session_snapshot() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 100, 30);
    assert_eq!(
        app.session_view.lifecycle,
        forge_types::TaskLifecycle::Ready
    );
    assert!(!app.session_view.is_awaiting_approval());

    // `set_pending_hitl` also moves the lifecycle to Waiting, which is the
    // pair of changes a real approval produces.
    set_pending_hitl(&mut app, direct_hitl_payload("c1", "src/main.rs"));

    // Still the previous frame's values until something draws.
    assert_eq!(
        app.session_view.lifecycle,
        forge_types::TaskLifecycle::Ready
    );
    assert!(!app.session_view.is_awaiting_approval());

    draw_app(&mut app, 100, 30);
    assert_eq!(
        app.session_view.lifecycle,
        forge_types::TaskLifecycle::Waiting
    );
    assert!(app.session_view.is_awaiting_approval());
}

/// `/status` runs between frames, so it must not report whatever was true when
/// the screen was last painted.
#[tokio::test]
async fn status_model_does_not_serve_a_stale_snapshot_between_frames() {
    let (_dir, mut app) = focus_test_app().await;
    draw_app(&mut app, 100, 30);

    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Failed;

    assert_eq!(
        app.refresh_status_model().status,
        forge_types::TaskLifecycle::Failed,
        "refresh_status_model must capture rather than reuse the last frame's snapshot"
    );
}

/// A frame must not read the credential file. Every read stats it on disk, and
/// `connected_profiles()` walks all seven builtin profiles, so routing the
/// header's "connected" chip through the live store makes a redraw cost a
/// handful of syscalls — on the render path the project otherwise keeps free
/// of filesystem work.
#[tokio::test]
async fn drawing_does_not_read_the_credential_store() {
    let (_dir, mut app) = focus_test_app().await;
    // The fixture runs the mock provider, and `is_provider_connected` short
    // circuits on that before it ever reaches the store. Name a real provider,
    // which is the case that actually draws in anger.
    app.runtime.provider = "anthropic".into();
    app.runtime.model_label = "claude-sonnet-4-5".into();
    app.connect.profile = Some("anthropic".into());
    draw_app(&mut app, 100, 30);

    let before = app.connect.store.read_count();
    for _ in 0..5 {
        draw_app(&mut app, 100, 30);
    }
    let after = app.connect.store.read_count();

    assert_eq!(
        after - before,
        0,
        "five draws performed {} credential-store reads",
        after - before
    );
}

/// Caching must not change the answer: the cheap path a frame uses has to
/// agree with the live one, or the header chip lies.
#[tokio::test]
async fn the_cached_connection_answer_matches_the_live_one() {
    let (_dir, mut app) = focus_test_app().await;
    app.runtime.provider = "anthropic".into();
    app.runtime.model_label = "claude-sonnet-4-5".into();

    // No profile selected: not connected, both ways.
    assert_eq!(app.connected_cached(), app.is_provider_connected());
    assert!(!app.connected_cached());

    // A profile with no stored credentials is still not connected.
    app.connect.profile = Some("anthropic".into());
    app.invalidate_connected();
    assert_eq!(app.connected_cached(), app.is_provider_connected());
}

/// The cache is keyed on time, so an in-app connect change has to invalidate
/// it explicitly or the chip stays wrong until the TTL lapses.
#[tokio::test]
async fn invalidating_forces_the_next_read_to_recompute() {
    let (_dir, mut app) = focus_test_app().await;
    app.runtime.provider = "anthropic".into();
    app.runtime.model_label = "claude-sonnet-4-5".into();
    app.connect.profile = Some("anthropic".into());

    app.connected_cached();
    let before = app.connect.store.read_count();
    app.connected_cached();
    assert_eq!(
        app.connect.store.read_count(),
        before,
        "a fresh cache must not re-read"
    );

    app.invalidate_connected();
    app.connected_cached();
    assert!(
        app.connect.store.read_count() > before,
        "invalidating must force a recompute"
    );
}
