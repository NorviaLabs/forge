//! Inline HITL approval (transcript item + typed composer resolution) and
//! remember-session tests.

use super::prelude::*;

fn bash_hitl_payload(call_id: &str, command: &str) -> HitlPayload {
    HitlPayload {
        call_id: call_id.into(),
        tool: "bash".into(),
        args_redacted: json!({"command": command}),
        reason: "test approval".into(),
    }
}

async fn submit_line(app: &mut TuiApp, line: &str) {
    app.input.set_text(line);
    app.submit_composer_message().await.unwrap();
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
    assert!(rendered.contains("approval · bash"), "{rendered}");
    assert!(rendered.contains("git push -u origin main"), "{rendered}");
    assert!(rendered.contains("cwd:"), "{rendered}");
    assert!(rendered.contains("env: inherited"), "{rendered}");
    assert!(rendered.contains("› Allow once"), "{rendered}");
    assert!(
        rendered.contains("Allow pattern going forward"),
        "{rendered}"
    );
    assert!(rendered.contains("bash(git push *)"), "{rendered}");
    assert!(
        rendered.contains("↑↓ select · Enter confirm · Esc deny") || rendered.contains("↑↓ select"),
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
    assert!(rendered.contains("approval · bash"), "{rendered}");
    assert_eq!(app.workspace_navigation, before);
    assert!(app.activity_summary().is_none());
    assert_eq!(
        app.workspace_navigation.current,
        Some(WorkspaceView::File(path))
    );
}

#[tokio::test]
async fn typing_yes_approves_once_without_remembering() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("allowed.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("direct-once", "allowed.txt"));

    submit_line(&mut app, "yes").await;

    assert!(app.session.pending_hitl().is_none());
    assert!(app.hitl_session.allowed.is_empty());
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn typing_remember_approves_and_matches_exact_identity() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("remember.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("remember", "remember.txt");
    set_pending_hitl(&mut app, payload.clone());

    submit_line(&mut app, "remember").await;

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
    set_pending_hitl(&mut app, payload.clone());

    submit_line(&mut app, "remember").await;

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
    set_pending_hitl(&mut app, bash_hitl_payload("shell", "git push origin main"));

    submit_line(&mut app, "remember").await;

    // Ineligible verb: warned, nothing resolved.
    assert!(app.session.pending_hitl().is_some());
    assert!(app.hitl_session.allowed.is_empty());
    assert!(
        app.feedback.text.contains("cannot be remembered"),
        "{}",
        app.feedback.text
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

    // The operator's text survives so they can edit it into a valid answer.
    assert_eq!(app.input.text, "run the tests instead");
    assert!(app.session.pending_hitl().is_some());
    assert!(
        app.feedback.text.contains("↑↓ select") || app.feedback.text.contains("yes/no"),
        "{}",
        app.feedback.text
    );
}

#[tokio::test]
async fn typing_no_denies() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("deny", "blocked.txt"));

    submit_line(&mut app, "no").await;

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
async fn typing_no_with_feedback_reaches_the_agent_as_tool_result_content() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("call-1", "blocked.txt"));

    submit_line(&mut app, "no use --dry-run instead").await;

    assert!(app.session.pending_hitl().is_none());
    let tool_message = app
        .session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool result should record the denial");
    assert!(tool_message.content.contains("HITL denied by tui"));
    assert!(tool_message.content.contains("use --dry-run instead"));
}

#[tokio::test]
async fn typing_always_persists_pattern_and_auto_allows_matching_calls() {
    // Writes the pattern to the personal permissions file
    // (`forge_config::append_user_allow_rule`), which resolves via
    // `dirs::config_dir()`. Redirect that to a throwaway `HOME` so the test
    // never touches the developer's real config directory.
    let _env_guard = ScopedEnvGuard::new(&["HOME", "XDG_CONFIG_HOME"]);
    let home_dir = TempDir::new().unwrap();
    std::env::set_var("HOME", home_dir.path());

    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(&mut app, bash_hitl_payload("call-1", "cargo test --all"));

    submit_line(&mut app, "always").await;

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
    set_pending_hitl(&mut app, bash_hitl_payload("m1", "ls"));
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
    set_pending_hitl(&mut app, bash_hitl_payload("n1", "cargo check --all"));
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
    set_pending_hitl(&mut app, bash_hitl_payload("n2", "cargo test -p x"));
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
    set_pending_hitl(&mut app, bash_hitl_payload("m2", "cargo test --all"));
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
async fn menu_esc_denies() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_hitl(&mut app, bash_hitl_payload("m3", "rm -rf /"));
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.session.pending_hitl().is_none());
}

#[tokio::test]
async fn approval_duplicate_confirmation_is_idempotent() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("dup.txt"), "ok").unwrap();
    set_pending_hitl(&mut app, direct_hitl_payload("dup", "dup.txt"));

    app.resolve_approval_line(input_route::ApprovalAction::Approve)
        .await
        .unwrap();
    app.resolve_approval_line(input_route::ApprovalAction::Approve)
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
