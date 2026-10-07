//! Opt-in review artifacts from the real draw path and mock session fixtures.
//! No palette, spacing, or screen arrangement is asserted by this harness.

use super::prelude::*;

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
            for state in ["start", "draft", "working", "review", "approval"] {
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
        "Captured 60 production frames with mock fixtures in {}",
        out.display()
    );
        });
}
