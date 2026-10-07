//! Opt-in review artifacts from the real draw path and mock session fixtures.
//! No palette, spacing, or screen arrangement is asserted by this harness.

use super::prelude::*;

#[tokio::test]
#[ignore = "manual native terminal walkthrough; use --nocapture --test-threads=1"]
async fn walk_refresh_ui() {
    let theme = std::env::var("FORGE_NATIVE_THEME").unwrap_or_else(|_| "forge-dark".into());
    let (fixture, mut app) = focus_test_app_with_theme(&theme).await;
    crate::theme::install(crate::theme_registry::ThemeRegistry::builtin(), &theme);
    app.connect.preferences =
        forge_connect::PreferenceStore::new(fixture.path().join("preferences.json"));
    app.pane_resize = PaneResizeState::new(forge_config::PaneLayoutStore::new(
        fixture.path().join("pane-layout.toml"),
    ));
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
    for presentation in ["dark", "light", "mono"] {
        crossterm::style::force_color_output(presentation != "mono");
        let theme = if presentation == "light" {
            "forge-light"
        } else {
            "forge-dark"
        };
        crate::theme::install(crate::theme_registry::ThemeRegistry::builtin(), theme);
        for (width, height) in [(80, 18), (80, 24), (120, 40), (160, 50)] {
            for state in ["start", "draft", "plan", "working", "review", "source", "approval", "details", "recovery", "help", "commands", "files", "sessions", "models", "terminal"] {
                let (fixture, mut app) = focus_test_app_with_theme(theme).await;
                init_repo(fixture.path());
                app.focus_block(FocusBlock::Composer);
                if state != "start" && state != "draft" {
                    app.session_runtime.messages.push(Message::new(
                        MessageRole::User,
                        "Improve retry handling and validate the change.",
                    ));
                }
                match state {
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
                    "working" => {
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
        "Captured 180 production frames with mock fixtures in {}",
        out.display()
    );
        });
}
