//! HITL approval overlay and remember-session tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn edge_approval_at_80x24_keeps_required_fields_and_actions() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_hitl_overlay(direct_hitl_payload("call-1", "src/main.rs"));

    let rendered = render_app_text(&mut app, 80, 24);
    assert!(rendered.contains("Approval required"), "{rendered}");
    assert!(rendered.contains("Direct"), "{rendered}");
    assert!(rendered.contains("read_file"), "{rendered}");
    assert!(rendered.contains("Working directory"), "{rendered}");
    assert!(rendered.contains("test approval"), "{rendered}");
    assert!(rendered.contains("Allow once"), "{rendered}");
    assert!(rendered.contains("Deny"), "{rendered}");
    assert!(
        rendered.contains("Remember this exact Direct invocation"),
        "{rendered}"
    );
}

#[tokio::test]
async fn approval_overlay_preserves_underlying_workspace() {
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

    app.maybe_open_hitl();

    assert!(matches!(app.overlay, Some(Overlay::Hitl { .. })));
    assert_eq!(app.workspace_navigation, before);
    assert!(app.activity_summary().is_none());
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn approval_direct_allow_once_resolves_without_remembering() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("allowed.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("direct-once", "allowed.txt"));
    app.maybe_open_hitl();

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.session.pending_hitl().is_none());
    assert!(app.hitl_session_allow.is_empty());
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn approval_remembered_direct_invocation_matches_exact_identity() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("remember.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("remember", "remember.txt");
    set_pending_hitl(&mut app, payload.clone());
    app.maybe_open_hitl();

    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();

    let identity = app.approval_identity_for_payload(&payload).unwrap();
    assert!(app.hitl_session_allow.contains(&identity));
    assert!(!app.hitl_session_allow.contains(
        &app.approval_identity_for_payload(&direct_hitl_payload("arg", "other.txt"))
            .unwrap()
    ));

    let env_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "env": {"RUST_LOG": "debug"}}),
        ..direct_hitl_payload("env", "remember.txt")
    };
    assert!(!app
        .hitl_session_allow
        .contains(&app.approval_identity_for_payload(&env_payload).unwrap()));

    let cwd_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "cwd": "nested"}),
        ..direct_hitl_payload("cwd", "remember.txt")
    };
    assert!(!app
        .hitl_session_allow
        .contains(&app.approval_identity_for_payload(&cwd_payload).unwrap()));

    let (other_dir, other_app) = focus_test_app().await;
    fs::write(other_dir.path().join("remember.txt"), "ok").unwrap();
    assert!(!app
        .hitl_session_allow
        .contains(&other_app.approval_identity_for_payload(&payload).unwrap()));
}

#[tokio::test]
async fn approval_remembered_direct_expires_with_session() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("session.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("session", "session.txt");
    set_pending_hitl(&mut app, payload.clone());
    app.maybe_open_hitl();
    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
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
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    assert!(next_app.hitl_session_allow.is_empty());
    assert_ne!(
        app.approval_identity_for_payload(&payload),
        next_app.approval_identity_for_payload(&payload)
    );
}

#[tokio::test]
async fn approval_shell_mode_cannot_be_remembered() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(
        &mut app,
        HitlPayload {
            call_id: "shell".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "git push origin main"}),
            reason: "test approval".into(),
        },
    );
    app.maybe_open_hitl();

    let Some(Overlay::Hitl { approval, .. }) = &app.overlay else {
        panic!("expected approval overlay");
    };
    assert_eq!(approval.mode, ApprovalExecutionMode::Shell);
    assert!(!approval.remember_eligible);
    assert_eq!(
        app.approval_identity_for_payload(app.session.pending_hitl().unwrap()),
        None
    );
}

#[tokio::test]
async fn approval_escape_denies_and_underlying_commands_are_blocked() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("esc", "blocked.txt"));
    app.maybe_open_hitl();
    let before_history = app.workspace_navigation.clone();

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
    assert!(app.session.pending_hitl().is_some());

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.session.pending_hitl().is_none());
    assert_eq!(app.workspace_navigation, before_history);
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
async fn approval_duplicate_confirmation_is_idempotent() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("dup.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("dup", "dup.txt"));
    app.maybe_open_hitl();

    app.resolve_hitl_overlay(HitlDecision::Approve, false)
        .await
        .unwrap();
    app.resolve_hitl_overlay(HitlDecision::Approve, false)
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
async fn approval_overlay_80x24_renders_actions_and_redacts_secrets() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(
        &mut app,
        HitlPayload {
            call_id: "secret".into(),
            tool: "read_file".into(),
            args_redacted: json!({"path": "config.txt", "api_key": "[REDACTED]"}),
            reason: "secret test".into(),
        },
    );
    app.maybe_open_hitl();

    let rendered = render_app_text(&mut app, 80, 24);

    assert!(rendered.contains("Approval required"), "{rendered}");
    assert!(rendered.contains("Mode: Direct"), "{rendered}");
    assert!(rendered.contains("Executable: read_file"), "{rendered}");
    assert!(rendered.contains("Working directory:"), "{rendered}");
    assert!(rendered.contains("[Allow once]"), "{rendered}");
    assert!(rendered.contains("[Deny]"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
    assert!(
        !rendered.contains("Remember this exact Direct invocation"),
        "{rendered}"
    );
}
