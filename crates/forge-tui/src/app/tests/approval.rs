//! Inline HITL approval: focus-gated conversation prompt, session pattern
//! session remember, and per-theme render tests. Decisions are made only through
//! the prompt's menu (↑↓ Enter Esc) while it holds focus.

use super::super::approvals::ApprovalMenuKind;
use super::prelude::*;

fn bash_hitl_payload(call_id: &str, command: &str) -> HitlPayload {
    HitlPayload {
        call_id: call_id.into(),
        tool: "bash".into(),
        args_redacted: json!({"command": command}),
        reason: "test approval".into(),
        sandbox_escalation: false,
        denied_host: None,
    }
}

/// Install a pending approval AND claim focus on the card, mirroring what
/// `sync_approval_focus` does in the real event loop. Menu keys only route
/// while the card holds focus, so key-driven tests need this pairing.
fn set_pending_approval(app: &mut TuiApp, payload: HitlPayload) {
    set_pending_hitl(app, payload);
    app.sync_approval_focus();
}

/// Move the menu selection down to the row whose label matches `label`.
/// Robust to eligibility (rows differ per tool), unlike hard-coded indexes.
async fn press_down_to(app: &mut TuiApp, label: &str) {
    let rows = app.approval_menu_rows();
    let target = rows
        .iter()
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no approval row {label:?} in {rows:?}"));
    let current = app.approval_menu_selected();
    let steps = (target + rows.len() - current) % rows.len();
    for _ in 0..steps {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.approval_menu_selected(), target);
}

async fn flush_queued_hitl(app: &mut TuiApp) {
    app.drain_pending_hitl(None).await.unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.pending_approved_tool.is_some() {
        if std::time::Instant::now() > deadline {
            panic!("approved tool did not finish");
        }
        app.poll_approved_hitl().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn inline_approval_renders_full_payload_in_sidebar() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(
        &mut app,
        HitlPayload {
            call_id: "call-1".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "git push -u origin main"}),
            reason: "test approval".into(),
            sandbox_escalation: false,
            denied_host: None,
        },
    );

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        rendered.contains("Forge wants to run a shell command."),
        "{rendered}"
    );
    assert!(!rendered.contains("⏸ APPROVAL REQUIRED"), "{rendered}");
    assert!(rendered.contains("git push -u origin main"), "{rendered}");
    assert!(rendered.contains("\u{276f} Run once"), "{rendered}");
    assert!(
        rendered.contains("Remember similar commands this session"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Runs now. You will be asked again."),
        "{rendered}"
    );
    assert!(!rendered.contains("Would match: git push"), "{rendered}");
    assert!(rendered.contains("Esc"), "{rendered}");
    assert!(rendered.contains("don't run"), "{rendered}");
    // The prompt is a card now, not bare prose in the transcript flow.
    assert!(rendered.contains("Approval needed"), "{rendered}");
}

#[tokio::test]
async fn approval_leaves_underlying_workspace_untouched() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    set_pending_hitl(
        &mut app,
        forge_types::HitlPayload {
            call_id: "1".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "cargo test"}),
            reason: "test approval".into(),
            sandbox_escalation: false,
            denied_host: None,
        },
    );

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Forge wants to run"), "{rendered}");
    assert!(rendered.contains("Run once"), "{rendered}");
    assert_eq!(app.workspace_navigation, before);
    assert!(app.activity_summary().is_none());
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn menu_allow_once_approves_without_remembering() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("allowed.txt"), "ok").unwrap();
    set_pending_approval(&mut app, direct_hitl_payload("direct-once", "allowed.txt"));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    assert!(app.session.pending_hitl().is_none());
    assert!(app.remembered_approval_count() == 0);
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn menu_remember_approves_and_matches_the_suggested_family() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/foo.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("remember", "src/foo.txt");
    set_pending_approval(&mut app, payload.clone());

    press_down_to(&mut app, "Remember similar commands this session").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    assert!(app.is_approval_pattern_remembered(&payload));
    assert!(app.is_approval_pattern_remembered(&direct_hitl_payload("sib", "src/bar.txt")));
    assert!(!app.is_approval_pattern_remembered(&direct_hitl_payload("other", "other.txt")));

    let (other_dir, other_app) = focus_test_app().await;
    fs::create_dir_all(other_dir.path().join("src")).unwrap();
    fs::write(other_dir.path().join("src/foo.txt"), "ok").unwrap();
    assert!(!other_app.is_approval_pattern_remembered(&payload));
}

#[tokio::test]
async fn remembered_approval_expires_with_session() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("session.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("session", "session.txt");
    set_pending_approval(&mut app, payload.clone());

    press_down_to(&mut app, "Remember similar commands this session").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    let next_session = session_for_workspace(dir.path()).await;
    let next_app = TuiApp::new(
        next_session,
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

    assert!(next_app.remembered_approval_count() == 0);
    assert_ne!(
        app.approval_identity_for_payload(&payload),
        next_app.approval_identity_for_payload(&payload)
    );
}

#[tokio::test]
async fn shell_approval_offers_a_generalized_pattern() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("shell", "git push origin main"));

    let rows = app.approval_menu_rows();
    let labels: Vec<String> = rows.iter().map(|row| row.label.clone()).collect();
    assert!(
        labels
            .iter()
            .any(|label| label == "Remember similar commands this session"),
        "{labels:?}"
    );
    let pattern = rows
        .iter()
        .find(|row| row.label == "Remember similar commands this session")
        .and_then(|row| row.detail.as_deref());
    assert_eq!(pattern, Some("bash(git push *)"));
    assert!(app
        .approval_identity_for_payload(app.session.pending_hitl().unwrap())
        .is_some());
}

#[tokio::test]
async fn allow_pattern_row_shows_the_suggested_file_pattern() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/remember.txt"), "ok").unwrap();
    set_pending_approval(&mut app, direct_hitl_payload("file", "src/remember.txt"));

    let pattern = app
        .approval_menu_rows()
        .into_iter()
        .find(|row| row.label == "Remember similar commands this session")
        .and_then(|row| row.detail);
    assert_eq!(pattern.as_deref(), Some("read_file(src/**)"));

    press_down_to(&mut app, "Remember similar commands this session").await;
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        rendered.contains("Remember similar commands this session"),
        "{rendered}"
    );
    assert!(rendered.contains("files under src/"), "{rendered}");
}

#[tokio::test]
async fn unrecognized_line_preserves_text_and_keeps_pending() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("stale", "blocked.txt"));

    app.input.set_text("run the tests instead");
    app.submit_composer_message().await.unwrap();

    // The operator's text survives so they can edit it into a valid message.
    assert_eq!(app.input.text, "run the tests instead");
    assert!(app.session.pending_hitl().is_some());
    assert!(
        app.feedback.text.contains("Esc don't run"),
        "{}",
        app.feedback.text
    );
}

#[tokio::test]
async fn menu_esc_denies_and_records_tool_denial() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    set_pending_approval(&mut app, direct_hitl_payload("deny", "blocked.txt"));

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    assert!(app.session.pending_hitl().is_none());
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content.contains("HITL denied")));
    assert!(!app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn menu_allow_pattern_remembers_the_command_family_for_the_session() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(
        &mut app,
        bash_hitl_payload("call-1", "git push -u origin main"),
    );

    press_down_to(&mut app, "Remember similar commands this session").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    assert!(app.session.pending_hitl().is_none());
    assert_eq!(app.remembered_approval_count(), 1);

    // The same argv auto-approves without presenting another prompt.
    set_pending_hitl(
        &mut app,
        bash_hitl_payload("call-2", "git push -u origin main"),
    );
    app.drain_auto_hitl().await.unwrap();
    flush_queued_hitl(&mut app).await;
    assert!(app.session.pending_hitl().is_none());

    // A sibling in the same family also auto-approves.
    set_pending_hitl(
        &mut app,
        bash_hitl_payload("call-3", "git push origin feature"),
    );
    app.drain_auto_hitl().await.unwrap();
    flush_queued_hitl(&mut app).await;
    assert!(app.session.pending_hitl().is_none());

    // A different family still gates.
    set_pending_hitl(&mut app, bash_hitl_payload("call-4", "git status"));
    assert!(app.session.pending_hitl().is_some());
}

#[tokio::test]
async fn menu_enter_on_allow_once_approves() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("m1", "ls"));
    app.sync_approval_menu();
    assert_eq!(app.approval_menu_selected(), 0);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;
    assert!(app.session.pending_hitl().is_none());
    // Run once must not remember the invocation.
    assert!(app.remembered_approval_count() == 0);
}

#[tokio::test]
async fn menu_down_to_allow_pattern_and_enter() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("m2", "cargo test --all"));
    app.sync_approval_menu();
    // Run once (0) → Remember similar (1)
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.approval_menu_selected(), 1);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;
    assert!(app.session.pending_hitl().is_none());
    assert_eq!(app.remembered_approval_count(), 1);
}

#[tokio::test]
async fn approval_duplicate_confirmation_is_idempotent() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("dup.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("dup", "dup.txt"));

    app.resolve_approval_line(ApprovalMenuKind::AllowOnce)
        .await
        .unwrap();
    app.resolve_approval_line(ApprovalMenuKind::AllowOnce)
        .await
        .unwrap();
    flush_queued_hitl(&mut app).await;

    let successful_tool_messages = app
        .session
        .messages
        .iter()
        .filter(|message| message.content == "ok")
        .count();
    assert_eq!(successful_tool_messages, 1);
}

#[tokio::test]
async fn new_approval_resets_menu_selection_but_same_call_id_keeps_it() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("one", "cargo test"));
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.approval_menu_selected(), 1);

    // A render-tick sync for the same call_id must not reset the choice.
    app.sync_approval_menu();
    assert_eq!(app.approval_menu_selected(), 1);

    // A genuinely new approval resets to the top row.
    set_pending_hitl(&mut app, bash_hitl_payload("two", "cargo fmt"));
    app.sync_approval_menu();
    app.sync_approval_focus();
    assert_eq!(app.approval_menu_selected(), 0);
}

#[tokio::test]
async fn tab_away_from_approval_keeps_it_pending() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("t1", "ls"));
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.approval_menu_selected(), 1);

    // Tab off the card: approval stays pending and menu keys stop routing.
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(app.focus.block(), FocusBlock::Approval);
    assert!(app.session.pending_hitl().is_some());
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.approval_menu_selected(), 1);

    // Esc from the composer returns to the card; menu keys work again.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.approval_menu_selected(), 2);
}

#[tokio::test]
async fn approval_card_wraps_long_command() {
    let (_dir, mut app) = focus_test_app().await;
    let command = format!("git commit -m {}", "f".repeat(250));
    set_pending_hitl(&mut app, bash_hitl_payload("long", &command));

    let rendered = render_app_text(&mut app, 100, 40);
    assert!(
        rendered.contains("Forge wants to run a shell command."),
        "{rendered}"
    );
    assert!(rendered.contains("git commit -m"), "{rendered}");
    assert!(
        rendered.lines().all(|line| line.chars().count() <= 100),
        "{rendered}"
    );
}

#[tokio::test]
async fn approval_card_renders_in_every_shipped_theme() {
    let registry = crate::theme_registry::ThemeRegistry::load(None);
    for theme_id in [
        "catppuccin-mocha",
        "gruvbox-dark",
        "kanagawa-wave",
        "solarized-dark",
        "solarized-light",
    ] {
        assert!(
            registry.get(theme_id).is_some(),
            "built-in theme {theme_id} not registered"
        );
        crate::theme::install(registry.clone(), theme_id);
        for width in [100u16, 120u16] {
            let (_dir, mut app) = focus_test_app_with_theme(theme_id).await;
            set_pending_hitl(
                &mut app,
                bash_hitl_payload("th", "cargo build --release --locked"),
            );
            let rendered = render_app_text(&mut app, width, 30);
            assert!(
                rendered.contains("Forge wants to run a shell command."),
                "{theme_id} @ {width}:\n{rendered}"
            );
            assert!(
                rendered.contains("\u{276f} Run once"),
                "{theme_id} @ {width}:\n{rendered}"
            );
            assert!(
                rendered.contains("don't run"),
                "{theme_id} @ {width}:\n{rendered}"
            );
            // The card's border must close on every theme and width.
            assert!(
                rendered.contains("Approval needed") && rendered.contains('\u{256f}'),
                "{theme_id} @ {width}:\n{rendered}"
            );
            for line in rendered.lines() {
                assert!(
                    line.chars().count() <= width as usize,
                    "{theme_id} @ {width} overflow: {line:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn approving_a_slow_command_returns_before_the_command_finishes() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("slow", "sleep 5"));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.session.pending_hitl().is_some(),
        "the command should still be pending until the event loop drains it"
    );
    assert!(
        app.pending_interaction.has_hitl_decision(),
        "the event loop should have a queued HITL decision to drain"
    );

    let started = std::time::Instant::now();
    app.drain_pending_hitl(None).await.unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "draining approval must not wait for the approved command"
    );
    assert!(
        app.session.pending_hitl().is_none(),
        "the approval card must clear as soon as the operator decides"
    );
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        !rendered.contains("Forge wants to run a shell command."),
        "stale approval chrome must not linger after the decision:\n{rendered}"
    );
    assert!(
        app.pending_approved_tool.is_some(),
        "the approved command should keep running off the event loop"
    );
    assert!(
        !app.pending_turn.continue_requested(),
        "the follow-up model call must wait until the approved command finishes"
    );
}

#[tokio::test]
async fn interrupting_an_approved_command_clears_the_card_and_recovers() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("slow-cancel", "sleep 5"));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.drain_pending_hitl(None).await.unwrap();
    assert!(app.pending_approved_tool.is_some());

    app.cancellation.request();
    app.poll_approved_hitl().await.unwrap();

    assert!(
        app.pending_approved_tool.is_none(),
        "interrupt must abort the approved command"
    );
    assert!(
        app.session.pending_hitl().is_none(),
        "the approval card must not stay pending after interrupt"
    );
    assert!(
        !app.pending_turn.continue_requested(),
        "a cancelled command must not resume the model turn"
    );
    assert!(
        !app.busy_state.is_active(),
        "interrupt must return the composer to an idle state"
    );
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        !rendered.contains("Forge wants to run a shell command."),
        "{rendered}"
    );
}

#[tokio::test]
async fn a_denied_host_offers_a_persistent_grant_not_an_unconfined_run() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(
        &mut app,
        HitlPayload {
            call_id: "call-host".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "curl -I https://api.github.com"}),
            reason: "blocked by the sandbox: the destination host is not allowed".into(),
            sandbox_escalation: false,
            denied_host: Some("api.github.com".into()),
        },
    );

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(
        rendered.contains("Forge wants to allow network access to **.github.com."),
        "{rendered}"
    );
    assert!(
        rendered.contains("Always allow **.github.com"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Allow **.github.com this session"),
        "{rendered}"
    );
    assert!(!rendered.contains("Run once"), "{rendered}");
    assert!(
        !rendered.contains("Remember similar commands this session"),
        "{rendered}"
    );
}
