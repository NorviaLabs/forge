//! Opt-in review artifacts from the real draw path and mock session fixtures.
//! No palette, spacing, or screen arrangement is asserted by this harness.

use super::prelude::*;

struct RefreshMotionModel;

#[async_trait::async_trait]
impl forge_model::ModelClient for RefreshMotionModel {
    async fn complete(
        &self,
        request: forge_model::ModelRequest,
    ) -> Result<ModelResponse, forge_model::ModelError> {
        self.complete_with_stream(request, None).await
    }

    async fn complete_with_stream(
        &self,
        _request: forge_model::ModelRequest,
        tx: Option<forge_model::StreamEventTx>,
    ) -> Result<ModelResponse, forge_model::ModelError> {
        let mut text = String::new();
        // Leave the actual busy indicator visible for the manual recording.
        tokio::time::sleep(Duration::from_secs(3)).await;
        for index in 1..=60 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let chunk = format!("Mock provider chunk {index}: retained evidence λ/東京.rs.\n\n");
            if let Some(tx) = tx.as_ref() {
                let _ = tx.send(forge_types::ModelStreamEvent::TextDelta {
                    text: chunk.clone(),
                });
            }
            text.push_str(&chunk);
        }
        if let Some(tx) = tx {
            let _ = tx.send(forge_types::ModelStreamEvent::MessageEnd);
        }
        Ok(ModelResponse {
            text,
            tool_calls: vec![],
            usage: None,
            thinking: None,
        })
    }

    fn clear_provider_env(&self) {}
}

async fn seed_refresh_tasks(app: &mut TuiApp) {
    for position in 1..=6 {
        app.session_runtime
            .enqueue_task(&format!("Follow-up {position}: inspect retry handling in λ/東京.rs before changing the retained patch."))
            .await
            .unwrap();
    }
    let mut ids = Vec::new();
    for (command, label) in [
        ("printf 'Actual fixture output λ/東京\\n'", "Output fixture"),
        (
            "printf 'Actual failure output\\n'; exit 7",
            "Nonzero fixture",
        ),
        (
            "printf 'Captured before cancellation λ/東京\\n'; sleep 30",
            "Cancelled fixture",
        ),
    ] {
        ids.push(
            app.session_runtime
                .spawn_background_shell(command.into(), label.into())
                .await
                .unwrap(),
        );
    }
    let capture = app
        .session_runtime
        .background()
        .get(ids[2])
        .unwrap()
        .shell
        .as_ref()
        .unwrap()
        .output
        .clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        while capture.snapshot().stdout.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.session_runtime.cancel_background_task(ids[2]));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.poll_background_tasks().await.unwrap();
        if ids.iter().all(|id| {
            app.session_runtime
                .background()
                .get(*id)
                .unwrap()
                .status
                .is_terminal()
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native task fixtures did not finish"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    app.toast.clear();
}

async fn refresh_agents_fixture(
    theme: &str,
) -> (TempDir, TuiApp, Vec<forge_types::BackgroundTaskId>) {
    let request = |label: &str| {
        ModelResponse {
        text: format!("Mock provider finding from {label}: retry handling needs review. This is partial and unverified.\n\nThe child journal is read-only in this view."),
        tool_calls: vec![forge_types::ToolCall { id: "fixture-request".into(), name: "bash".into(), arguments: json!({"command": format!("printf 'Actual disposable child command: {label} λ/東京\\n'")}) }],
        usage: None, thinking: Some("Hidden fixture reasoning; exclude from result handoff.".into()),
    }
    };
    let model = Arc::new(MockModelClient::script(vec![request("writer"), request("reader"), request("sibling"), ModelResponse {
        text: "Mock provider conclusion after the actual disposable command. Not a validation result.".into(), tool_calls: vec![], usage: None, thinking: None,
    }]));
    let (fixture, mut app) = focus_test_app_with_model(model).await;
    init_repo(fixture.path());
    app.runtime.theme_id = theme.into();
    app.connect.preferences =
        forge_connect::PreferenceStore::new(fixture.path().join("preferences.json"));
    app.pane_resize = PaneResizeState::new(forge_config::PaneLayoutStore::new(
        fixture.path().join("pane-layout.toml"),
    ));
    app.session_runtime
        .set_governance(forge_governance::Governance::default().require_hitl_for_tool("bash"));
    app.session_runtime.messages.push(Message::new(
        MessageRole::User,
        "Subagent UI fixture; provider responses are mocked.",
    ));
    let mut ids = Vec::new();
    for (label, mode) in [
        ("Audit retries", forge_tools::AgentMode::Writer),
        ("Read-only reviewer", forge_tools::AgentMode::ReadOnly),
        ("Independent sibling", forge_tools::AgentMode::ReadOnly),
    ] {
        let id = app
            .session_runtime
            .spawn_subagent(forge_core::SubagentSpec {
                role: label.into(),
                prompt: "Inspect retry handling in λ/東京.rs. Fixture only; no validation claim."
                    .into(),
                mode,
                tool_allowlist: None,
            })
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.poll_background_tasks().await.unwrap();
            if matches!(
                app.session_runtime.background().get(id).unwrap().status,
                forge_core::BackgroundTaskStatus::WaitingForApproval { .. }
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "child fixture did not block");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        ids.push(id);
    }
    let checkout = app
        .session_runtime
        .background()
        .get(ids[0])
        .unwrap()
        .worktree_path
        .clone()
        .unwrap();
    // A real retained dirty checkout; this write is test setup, not model evidence.
    fs::write(checkout.join("a.txt"), "Retained dirty child edit λ/東京\n").unwrap();
    for position in 1..=6 {
        app.session_runtime
            .enqueue_task(&format!("Retained parent follow-up {position}"))
            .await
            .unwrap();
    }
    app.input.set_text("Retained parent draft λ/東京.rs.");
    app.task_selection
        .select_task(app.selected_session_id, ids[0]);
    app.toast.clear();
    crate::theme::install(crate::theme_registry::ThemeRegistry::builtin(), theme);
    (fixture, app, ids)
}

#[tokio::test]
#[ignore = "manual native terminal walkthrough; use --nocapture --test-threads=1"]
async fn walk_refresh_ui() {
    let theme = std::env::var("FORGE_NATIVE_THEME").unwrap_or_else(|_| "forge-dark".into());
    let (fixture, mut app) = if std::env::var_os("FORGE_NATIVE_AGENTS").is_some() {
        let (fixture, app, _) = refresh_agents_fixture(&theme).await;
        (fixture, app)
    } else if std::env::var_os("FORGE_NATIVE_MOTION").is_some() {
        focus_test_app_with_model(Arc::new(RefreshMotionModel)).await
    } else {
        focus_test_app_with_theme(&theme).await
    };
    crate::theme::install(crate::theme_registry::ThemeRegistry::builtin(), &theme);
    app.connect.preferences =
        forge_connect::PreferenceStore::new(fixture.path().join("preferences.json"));
    app.pane_resize = PaneResizeState::new(forge_config::PaneLayoutStore::new(
        fixture.path().join("pane-layout.toml"),
    ));
    app.runtime.reduced_motion = std::env::var_os("FORGE_REDUCED_MOTION").is_some();
    if std::env::var_os("FORGE_NATIVE_MOTION").is_some() {
        app.pending_turn.queue(
            "Show the mock provider stream. No tools or file changes.".into(),
            vec![],
        );
        app.input.set_text("Retained follow-up draft λ/東京.rs.");
    }
    if std::env::var_os("FORGE_NATIVE_TASKS").is_some()
        || std::env::var_os("FORGE_NATIVE_JOBS").is_some()
    {
        init_repo(fixture.path());
        app.session_runtime.messages.push(Message::new(
            MessageRole::User,
            "Native task inspection fixture; model responses are mocked.",
        ));
        seed_refresh_tasks(&mut app).await;
        app.input.set_text("Retained parent draft λ/東京.rs.");
        if std::env::var_os("FORGE_NATIVE_JOBS").is_some() {
            let id = app
                .session_runtime
                .spawn_background_shell(
                    "printf 'Live native output ✓\\n'; sleep 3600".into(),
                    "Native running job".into(),
                )
                .await
                .unwrap();
            app.task_selection.select_task(app.selected_session_id, id);
        }
    }
    super::super::shell::run_refresh_fixture(app).await.unwrap();
}

#[test]
#[ignore = "manual review capture; set FORGE_RENDER_DUMP_DIR"]
fn capture_refresh_frames() {
    let _guard = lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
    let out = PathBuf::from(std::env::var_os("FORGE_RENDER_DUMP_DIR").expect("capture directory"));
    fs::create_dir_all(&out).unwrap();
    let disabled = crossterm::style::Colored::ansi_color_disabled_memoized();
    let mut timings = Vec::new();
    let mut captured = 0;
    for presentation in ["dark", "light", "mono"] {
        crossterm::style::force_color_output(presentation != "mono");
        let theme = if presentation == "light" {
            "forge-light"
        } else {
            "forge-dark"
        };
        crate::theme::install(crate::theme_registry::ThemeRegistry::builtin(), theme);
        let sizes = std::env::var("FORGE_CAPTURE_SIZES").ok().map(|value| value.split(',').map(|size| { let (width, height) = size.split_once('x').expect("WIDTHxHEIGHT"); (width.parse().unwrap(), height.parse().unwrap()) }).collect::<Vec<_>>()).unwrap_or_else(|| vec![(80,18),(80,24),(120,40),(160,50)]);
        for (width, height) in sizes {
            for state in ["start", "draft", "caret", "plan", "working", "stillworking", "review", "source", "approval", "details", "recovery", "help", "commands", "files", "sessions", "models", "terminal", "dock", "queue", "jobs", "agents", "jobstop", "joboutput", "jobcontrols", "jobpartial", "jobinsert", "childlist", "childpeek", "childdecision", "childstop", "childpartial", "childinsert"] {
                if std::env::var("FORGE_CAPTURE_STATES").ok().is_some_and(|states| !states.split(',').any(|selected| selected == state)) { continue; }
                let (fixture, mut app) = if state.starts_with("child") {
                    let (fixture, app, _) = refresh_agents_fixture(theme).await;
                    (fixture, app)
                } else {
                    let (fixture, app) = focus_test_app_with_theme(theme).await;
                    init_repo(fixture.path());
                    (fixture, app)
                };
                app.focus_block(FocusBlock::Composer);
                if state != "start" && state != "draft" {
                    app.session_runtime.messages.push(Message::new(
                        MessageRole::User,
                        "Improve retry handling and validate the change.",
                    ));
                }
                match state {
                    "childlist" | "childpeek" | "childdecision" | "childstop" | "childpartial" | "childinsert" => {
                        app.focus_block(FocusBlock::Sidebar);
                        if state == "childlist" { app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Agents)); }
                        else { app.open_selected_child_session().await; }
                        if state == "childdecision" { app.open_selected_child_decision(); }
                        if ["childstop", "childpartial", "childinsert"].contains(&state) {
                            app.cancel_selected_task().await;
                            if state != "childstop" {
                                render_app_text(&mut app, width, height);
                                app.handle_key(press(KeyCode::Right, KeyModifiers::NONE)).await.unwrap();
                                app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE)).await.unwrap();
                                let id = app.child_view.as_ref().unwrap().task_id;
                                let deadline = Instant::now() + Duration::from_secs(3);
                                loop {
                                    app.poll_background_tasks().await.unwrap();
                                    if app.session_runtime.background().get(id).unwrap().status.is_terminal() { break; }
                                    assert!(Instant::now() < deadline, "child fixture did not stop");
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                                if state == "childinsert" { app.attach_selected_task().await; }
                            }
                        }
                    }
                    "dock" | "queue" | "jobs" | "agents" | "jobstop" | "joboutput" | "jobcontrols" | "jobpartial" | "jobinsert" => {
                        seed_refresh_tasks(&mut app).await;
                        app.input.set_text("Retained parent draft λ/東京.rs.");
                        if ["jobpartial", "jobinsert"].contains(&state) {
                            let label = if state == "jobpartial" { "Cancelled fixture" } else { "Output fixture" };
                            let id = app.session_runtime.background().list().find(|task| task.label == label).unwrap().id;
                            app.task_selection.select_task(app.selected_session_id, id);
                            app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Jobs));
                            if state == "jobinsert" {
                                app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE)).await.unwrap();
                            }
                        } else if ["jobstop", "joboutput", "jobcontrols"].contains(&state) {
                            let command = if state == "jobstop" { "printf 'Captured live output ✓\\n'; sleep 3600" } else if state == "jobcontrols" { "printf 'Actual control evidence λ/東京\\nESC=\\033[2J TAB=\\t CR=\\r BIDI=\\342\\200\\256end\\n'" } else { "printf 'Actual long output λ/東京\\n'; i=0; while [ $i -lt 100 ]; do printf 'line %s\\n' $i; i=$((i+1)); done; printf 'End of captured output\\n'; exit 7" };
                            let id = app.session_runtime.spawn_background_shell(command.into(), "Evidence fixture".into()).await.unwrap();
                            let deadline = Instant::now() + Duration::from_secs(3);
                            loop {
                                app.poll_background_tasks().await.unwrap();
                                let task = app.session_runtime.background().get(id).unwrap();
                                if (state == "jobstop" && !task.shell.as_ref().unwrap().output.snapshot().stdout.is_empty()) || (state != "jobstop" && task.status.is_terminal()) { break; }
                                assert!(Instant::now() < deadline, "job evidence did not arrive");
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            app.task_selection.select_task(app.selected_session_id, id);
                            app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Jobs));
                            if state == "jobstop" { app.cancel_selected_task().await; }
                            if state == "joboutput" { app.handle_key(press(KeyCode::End, KeyModifiers::NONE)).await.unwrap(); }
                        }
                        match state {
                            "queue" => app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Queue)),
                            "jobs" => app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Jobs)),
                            "agents" => app.open_tasks_view(Some(crate::tasks_strip::TaskFilter::Agents)),
                            _ => {},
                        }
                    }
                    "plan" => {
                        let mut message = Message::new(MessageRole::Assistant, "");
                        message.tool_calls.push(forge_types::ToolCall {
                            id: "capture-plan".into(), name: "update_plan".into(),
                            arguments: json!({"explanation":"Track the work; checklist status does not verify tests.","plan":[
                                {"step":"Inspect retry handling", "status":"completed"},
                                {"step":"Preserve the original error", "status":"in_progress"},
                                {"step":"Run focused validation", "status":"pending"}
                            ]}),
                        });
                        app.session_runtime.messages.push(message);
                        let mut result = Message::new(MessageRole::Tool, "Plan updated.");
                        result.tool_call_id = Some("capture-plan".into());
                        result.name = Some("update_plan".into());
                        app.session_runtime.messages.push(result);
                    }
                    "draft" => app.input.set_text(
                        "Preserve the original error and the Unicode path λ/東京.rs.\n".repeat(8),
                    ),
                    "caret" => {
                        app.input.set_text("Retained parent draft λ/東京.rs. █ stays literal.");
                        app.input.cursor = "Retaine".len();
                    }
                    "working" | "stillworking" => {
                        app.runtime.reduced_motion = state == "stillworking";
                        app.timing.turn_started = Some(Instant::now() - Duration::from_secs(5));
                        app.timing.started = app.timing.turn_started;
                        app.session_runtime.active_task.lifecycle =
                            forge_types::TaskLifecycle::Working;
                        app.stream_preview_for_tests("Checking the retry path.\n\nThe transient failure can be retried while preserving the original error.");
                    }
                    "review" => {
                        let path = fixture.path().join("retry.rs");
                        fs::write(&path, "fn retry_limit() -> usize { 1 }\n").unwrap();
                        for args in [
                            vec!["add", "retry.rs"],
                            vec!["commit", "-qm", "capture fixture"],
                        ] {
                            assert!(std::process::Command::new("git")
                                .args(args)
                                .current_dir(fixture.path())
                                .status()
                                .unwrap()
                                .success());
                        }
                        fs::write(&path, "fn retry_limit() -> usize { 3 }\n").unwrap();
                        app.session_runtime.messages.push(Message::new(
                            MessageRole::Assistant,
                            "Updated the retry limit. Review the patch before running validation.",
                        ));
                        app.open_diff_view(crate::diff_view::DiffSource::WorkingTree);
                        super::diff::settle_git(&mut app);
                        app.diff_view.select_path(std::path::Path::new("retry.rs"));
                        super::diff::settle_patch(&mut app);
                        assert!(matches!(
                            app.diff_view.patch,
                            crate::diff_view::PatchState::Ready(_)
                        ));
                    }
                    "approval" => {
                        set_pending_hitl(
                            &mut app,
                            HitlPayload {
                                call_id: "capture-exact-command".into(),
                                tool: "bash".into(),
                                args_redacted: json!({"command":"cargo test --package forge-tui --locked app::tests::workspace"}),
                                reason: "Run the targeted checks in this workspace.".into(),
                                failure: None,
                                sandbox_escalation: false,
                                denied_host: None,
                            },
                        );
                        app.sync_approval_focus();
                    }
                    "details" => {
                        set_pending_hitl(&mut app, HitlPayload {
                            call_id: "capture-literal-request".into(), tool: "bash".into(),
                            args_redacted: json!({"command":format!("printf  '%s\\n' '{}'", "longtoken-λ東京".repeat(40)), "cwd": fixture.path().join("workspace λ/東京").display().to_string()}),
                            reason: "Inspect the exact invocation before allowing it.".into(),
                            failure: Some("Retained sandbox failure; no retry has been approved.".into()),
                            sandbox_escalation: true, denied_host: None,
                        });
                        app.sync_approval_focus();
                        app.input.set_text("Retained draft λ/東京.rs.");
                    }
                    "source" => {
                        let path = fixture.path().join("retry.rs");
                        fs::write(&path, "// Preserve the original error.\nfn retry_limit() -> usize { 3 }\n").unwrap();
                        app.open_file_in_editor(&path);
                    }
                    "recovery" => {
                        app.session_runtime.active_task.lifecycle = forge_types::TaskLifecycle::Failed;
                        app.session_runtime.messages.push(Message::new(MessageRole::Assistant, "The patch is retained. Validation has not run."));
                        app.input.set_text("Review the retained patch before retrying.");
                        app.report_error("Provider disconnected during the response.\nReview the retained work, then use /continue to retry explicitly.");
                        app.toast.clear();
                    }
                    "help" => app.overlay = Some(Overlay::Help),
                    "commands" => app.input.set_text("/"),
                    "files" => app.overlay = Some(Overlay::FileExplorer {
                        cwd: fixture.path().display().to_string(), selected: 1, error: None,
                        items: vec![
                            FileExplorerItem { name: "src".into(), path: "src".into(), is_dir: true },
                            FileExplorerItem { name: "retry.rs".into(), path: "retry.rs".into(), is_dir: false },
                        ],
                    }),
                    "sessions" => app.overlay = Some(Overlay::SessionSwitcher {
                        selected: 0, filter: String::new(), items: vec![
                            SessionSwitcherItem { session_id: "capture-session-a".into(), label: "Retry handling".into(), branch: "feat/retry".into(), workspace: fixture.path().display().to_string(), state: "waiting".into(), attention: true, group: SessionSwitcherGroup::NeedsYou, managed: false, cleanup: SessionSwitcherCleanup::ReadOnly },
                            SessionSwitcherItem { session_id: "capture-session-b".into(), label: "Documentation".into(), branch: "feat/docs".into(), workspace: fixture.path().display().to_string(), state: "idle".into(), attention: false, group: SessionSwitcherGroup::Idle, managed: false, cleanup: SessionSwitcherCleanup::ReadOnly },
                        ],
                    }),
                    "models" => app.overlay = Some(Overlay::connect_model_open(
                        Vec::new(), vec![
                            crate::overlays::ModelItem { provider: "mock".into(), model: "review-model".into(), profile_id: None, source: forge_connect::CatalogSource::Configured, route_label: "Capture fixture".into() },
                            crate::overlays::ModelItem { provider: "mock".into(), model: "fast-model".into(), profile_id: None, source: forge_connect::CatalogSource::Configured, route_label: "Capture fixture".into() },
                        ], None, "review-model", ReasoningEffort::default(), ConnectModelColumn::Models,
                    )),
                    "terminal" => {
                        app.input.set_text("Retained model draft λ.");
                        app.open_bottom_panel();
                        let terminal = app.interactive_terminal.as_mut().expect("native PTY capture");
                        terminal.start_command("printf 'Terminal output λ/東京\\n'").unwrap();
                        let deadline = Instant::now() + Duration::from_secs(3);
                        loop {
                            let terminal = app.interactive_terminal.as_mut().unwrap();
                            terminal.poll();
                            if let Some(completed) = terminal.take_command_completion() {
                                assert_eq!(completed.exit_code, Some(0));
                                assert!(terminal.display_output().contains("Terminal output λ/東京"));
                                break;
                            }
                            assert!(Instant::now() < deadline, "terminal capture output did not arrive");
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        app.toast.clear();
                    }
                    _ => {}
                }
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let buffer = terminal.backend().buffer();
                let cells: Vec<_> = buffer
                    .content
                    .iter()
                    .map(|cell| {
                        json!({
                            "text":cell.symbol(), "fg":format!("{:?}",cell.fg),
                            "bg":format!("{:?}",cell.bg),
                            "bold":cell.modifier.contains(ratatui::style::Modifier::BOLD),
                            "inverse":cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                        })
                    })
                    .collect();
                let name = format!("{state}-{presentation}-{width}x{height}");
                fs::write(
                    out.join(format!("{name}.json")),
                    serde_json::to_vec(&json!({
                        "width":width,"height":height,"cells":cells,"fixture":true,
                    }))
                    .unwrap(),
                )
                .unwrap();
                if presentation == "dark" && width == 120 && ["start", "working"].contains(&state) {
                    app.focus_block(FocusBlock::Composer);
                    let mut samples = Vec::new();
                    for _ in 0..100 {
                        let began = Instant::now();
                        app.handle_key(press(KeyCode::Char('t'), KeyModifiers::NONE))
                            .await
                            .unwrap();
                        terminal.draw(|frame| app.draw(frame)).unwrap();
                        samples.push(began.elapsed().as_micros() as u64);
                    }
                    samples.sort_unstable();
                    timings.push(json!({"state":state,"samples":100,"median_us":samples[50],"p95_us":samples[95]}));
                }
                let active: Vec<_> = app.session_runtime.background().list().filter(|task| !task.status.is_terminal()).map(|task| task.id).collect();
                for id in active { app.session_runtime.cancel_background_task(id); }
                app.session_runtime.retire_resources().await.unwrap();
                captured += 1;
            }
        }
    }
    crossterm::style::force_color_output(!disabled);
    fs::write(
        out.join("timings.json"),
        serde_json::to_vec_pretty(&timings).unwrap(),
    )
    .unwrap();
    eprintln!(
        "Captured {captured} production frames with mock fixtures in {}",
        out.display()
    );
        });
}
