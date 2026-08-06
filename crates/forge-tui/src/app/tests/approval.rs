//! Inline HITL approval: focus-gated menu card, remember-session, and
//! per-theme render tests. Decisions are made only through the card's menu
//! (↑↓ Enter Esc) while it holds focus; typed answers were removed.

use super::prelude::*;

fn bash_hitl_payload(call_id: &str, command: &str) -> HitlPayload {
    HitlPayload {
        call_id: call_id.into(),
        tool: "bash".into(),
        args_redacted: json!({"command": command}),
        reason: "test approval".into(),
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
    let current = app.hitl_session.menu.selected;
    let steps = (target + rows.len() - current) % rows.len();
    for _ in 0..steps {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.hitl_session.menu.selected, target);
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
        },
    );

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("⏸ APPROVAL REQUIRED"), "{rendered}");
    assert!(rendered.contains("git push -u origin main"), "{rendered}");
    assert!(rendered.contains("cwd:"), "{rendered}");
    // The card now hugs its own content width (capped at prose width) rather
    // than always spanning the full pane, so a long cwd path can legitimately
    // wrap before reaching "env:" — check the two independently instead of
    // asserting they're contiguous.
    assert!(rendered.contains("env:"), "{rendered}");
    assert!(rendered.contains("inherited"), "{rendered}");
    assert!(rendered.contains("› Allow once"), "{rendered}");
    assert!(
        rendered.contains("Allow pattern going forward"),
        "{rendered}"
    );
    assert!(rendered.contains("bash(git push *)"), "{rendered}");
    assert!(
        rendered.contains("↑↓ select · Enter confirm · Esc cancel"),
        "{rendered}"
    );
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
        },
    );

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("⏸ APPROVAL REQUIRED"), "{rendered}");
    assert_eq!(app.workspace_navigation, before);
    assert!(app.activity_summary().is_none());
    assert_eq!(
        app.workspace_navigation.current,
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

    assert!(app.session.pending_hitl().is_none());
    assert!(app.hitl_session.allowed.is_empty());
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn menu_remember_approves_and_matches_exact_identity() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("remember.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("remember", "remember.txt");
    set_pending_approval(&mut app, payload.clone());

    press_down_to(&mut app, "Remember exact (session)").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let identity = app.approval_identity_for_payload(&payload).unwrap();
    assert!(app.hitl_session.allowed.contains(&identity));
    assert!(!app.hitl_session.allowed.contains(
        &app.approval_identity_for_payload(&direct_hitl_payload("arg", "other.txt"))
            .unwrap()
    ));

    let env_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "env": {"RUST_LOG": "debug"}}),
        ..direct_hitl_payload("env", "remember.txt")
    };
    assert!(!app
        .hitl_session
        .allowed
        .contains(&app.approval_identity_for_payload(&env_payload).unwrap()));

    let cwd_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "cwd": "nested"}),
        ..direct_hitl_payload("cwd", "remember.txt")
    };
    assert!(!app
        .hitl_session
        .allowed
        .contains(&app.approval_identity_for_payload(&cwd_payload).unwrap()));

    let (other_dir, other_app) = focus_test_app().await;
    fs::write(other_dir.path().join("remember.txt"), "ok").unwrap();
    assert!(!app
        .hitl_session
        .allowed
        .contains(&other_app.approval_identity_for_payload(&payload).unwrap()));
}

#[tokio::test]
async fn remembered_approval_expires_with_session() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("session.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("session", "session.txt");
    set_pending_approval(&mut app, payload.clone());

    press_down_to(&mut app, "Remember exact (session)").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

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

    assert!(next_app.hitl_session.allowed.is_empty());
    assert_ne!(
        app.approval_identity_for_payload(&payload),
        next_app.approval_identity_for_payload(&payload)
    );
}

#[tokio::test]
async fn shell_approval_cannot_be_remembered() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("shell", "git push origin main"));

    // Ineligible verb: the Remember row is not offered at all, and there is
    // no identity to persist even if it were.
    let labels: Vec<String> = app
        .approval_menu_rows()
        .iter()
        .map(|row| row.label.clone())
        .collect();
    assert!(
        !labels
            .iter()
            .any(|label| label == "Remember exact (session)"),
        "{labels:?}"
    );
    assert_eq!(
        app.approval_identity_for_payload(app.session.pending_hitl().unwrap()),
        None
    );
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
        app.feedback.text.contains("↑↓ select"),
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
async fn menu_allow_pattern_persists_and_auto_allows_matching_calls() {
    // Writes the pattern to the personal permissions file
    // (`forge_config::append_user_allow_rule`), which resolves via
    // `dirs::config_dir()`. Redirect that to a throwaway `HOME` so the test
    // never touches the developer's real config directory.
    let _env_guard = ScopedEnvGuard::new(&["HOME", "XDG_CONFIG_HOME"]);
    let home_dir = TempDir::new().unwrap();
    std::env::set_var("HOME", home_dir.path());

    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("call-1", "cargo test --all"));

    press_down_to(&mut app, "Allow pattern going forward").await;
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.session.pending_hitl().is_none());
    assert_eq!(app.hitl_session.pattern_allow.len(), 1);
    assert_eq!(app.hitl_session.pattern_allow[0].raw, "bash(cargo test *)");
    let persisted = forge_config::user_permissions_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .expect("pattern should be persisted to the redirected personal permissions file");
    assert!(persisted.contains("bash(cargo test *)"), "{persisted}");

    // A second, differently-worded but matching call auto-approves without
    // presenting an inline approval item — the pattern covers it, not just
    // the exact call.
    set_pending_hitl(
        &mut app,
        bash_hitl_payload("call-2", "cargo test --release"),
    );
    app.drain_auto_hitl().await.unwrap();
    assert!(app.session.pending_hitl().is_none());

    // But a non-matching command on the same tool still gates normally.
    set_pending_hitl(&mut app, bash_hitl_payload("call-3", "rm -rf /"));
    assert!(app.session.pending_hitl().is_some());
}

#[tokio::test]
async fn menu_enter_on_allow_once_approves() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("m1", "ls"));
    app.sync_approval_menu();
    assert_eq!(app.hitl_session.menu.selected, 0);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.session.pending_hitl().is_none());
    // Allow once on pattern-eligible shell offers a follow-up pattern nudge.
    assert!(app.hitl_session.pattern_nudge.is_some());
}

#[tokio::test]
async fn pattern_nudge_yes_persists_pattern() {
    let _env_guard = ScopedEnvGuard::new(&["HOME", "XDG_CONFIG_HOME"]);
    let home_dir = TempDir::new().unwrap();
    std::env::set_var("HOME", home_dir.path());

    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("n1", "cargo check --all"));
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let nudge = app.hitl_session.pattern_nudge.clone().expect("nudge");
    assert!(nudge.pattern.contains("cargo check"), "{}", nudge.pattern);
    assert_eq!(nudge.selected, 0); // Yes default
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.hitl_session.pattern_nudge.is_none());
    assert_eq!(app.hitl_session.pattern_allow.len(), 1);
    let persisted = forge_config::user_permissions_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .expect("persisted");
    assert!(persisted.contains("bash(cargo check"), "{persisted}");
}

#[tokio::test]
async fn pattern_nudge_esc_skips_without_write() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("n2", "cargo test -p x"));
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.hitl_session.pattern_nudge.is_some());
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.hitl_session.pattern_nudge.is_none());
    assert!(app.hitl_session.pattern_allow.is_empty());
}

#[tokio::test]
async fn menu_down_to_allow_pattern_and_enter() {
    let _env_guard = ScopedEnvGuard::new(&["HOME", "XDG_CONFIG_HOME"]);
    let home_dir = TempDir::new().unwrap();
    std::env::set_var("HOME", home_dir.path());

    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("m2", "cargo test --all"));
    app.sync_approval_menu();
    // Allow once (0) → Allow pattern (1)
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.hitl_session.menu.selected, 1);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.session.pending_hitl().is_none());
    assert_eq!(app.hitl_session.pattern_allow.len(), 1);
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
    assert_eq!(app.hitl_session.menu.selected, 1);

    // A render-tick sync for the same call_id must not reset the choice.
    app.sync_approval_menu();
    assert_eq!(app.hitl_session.menu.selected, 1);

    // A genuinely new approval resets to the top row.
    set_pending_hitl(&mut app, bash_hitl_payload("two", "cargo fmt"));
    app.sync_approval_menu();
    app.sync_approval_focus();
    assert_eq!(app.hitl_session.menu.selected, 0);
}

#[tokio::test]
async fn tab_away_from_approval_keeps_it_pending() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_approval(&mut app, bash_hitl_payload("t1", "ls"));
    assert_eq!(app.focus.block, FocusBlock::Approval);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.hitl_session.menu.selected, 1);

    // Tab off the card: approval stays pending and menu keys stop routing.
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(app.focus.block, FocusBlock::Approval);
    assert!(app.session.pending_hitl().is_some());
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.hitl_session.menu.selected, 1);

    // Esc from the composer returns to the card; menu keys work again.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Approval);
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.hitl_session.menu.selected, 2);
}

#[tokio::test]
async fn approval_card_wraps_long_command() {
    let (_dir, mut app) = focus_test_app().await;
    let command = format!("git commit -m {}", "f".repeat(250));
    set_pending_hitl(&mut app, bash_hitl_payload("long", &command));

    let rendered = render_app_text(&mut app, 100, 40);
    assert!(rendered.contains("⏸ APPROVAL REQUIRED"), "{rendered}");
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
                rendered.contains("⏸ APPROVAL REQUIRED"),
                "{theme_id} @ {width}:\n{rendered}"
            );
            assert!(
                rendered.contains("› Allow once"),
                "{theme_id} @ {width}:\n{rendered}"
            );
            assert!(
                rendered.contains("↑↓ select · Enter confirm · Esc cancel"),
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
