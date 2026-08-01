//! Slash commands, palette, queue, and input dispatch tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn source_search_is_transient_and_esc_restores_workspace_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.source_viewer.search.open);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn source_search_keeps_shift_arrows_inside_the_search_field() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.source_viewer.search.open);
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
}

#[tokio::test]
async fn jump_to_line_keeps_shift_arrows_inside_the_jump_field() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('g'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.source_viewer.jump.open);
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::JumpToLine)
    );
}

#[tokio::test]
async fn editor_reload_does_not_reach_chat_input() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "before\n").unwrap();
    app.input.set_text("draft");
    app.open_file_in_editor(&path);
    fs::write(&path, "after\n").unwrap();
    app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "draft");
    assert_eq!(app.source_viewer.lines, vec!["after"]);
}

#[tokio::test]
async fn resume_command_replaces_active_conversation_in_app() {
    let (dir, session) = test_session().await;
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut previous = AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    previous
        .append_user_message("restored conversation")
        .await
        .unwrap();
    let previous_id = previous.session_id;

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line(&format!("/resume {previous_id}"))
        .await
        .unwrap();

    assert_eq!(app.session.session_id, previous_id);
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "restored conversation"));
    assert!(app.status_message.contains("resumed"));
    assert!(app.notices.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.summary.contains("session resumed")));
}

#[tokio::test]
async fn compact_reports_context_handoff_in_chat_and_activity() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    app.dispatch_line("/compact").await.unwrap();

    assert!(app.notices.is_empty());
    assert!(app.ui_banners.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Context));
    assert_eq!(app.status_message, "Continuing in a fresh context");
    assert!(app
        .ui_banners
        .iter()
        .all(|item| !matches!(item, ChatItem::Banner { .. })));
}

#[tokio::test]
async fn enter_while_busy_enqueues_user_message() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Input routing keys off the authoritative session lifecycle, not the
    // UI `busy` flag — a real task must be Working for Enter to enqueue.
    app.session.append_user_message("first").await.unwrap();
    app.busy = true;
    for c in "queued later".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.session.queue.len(), 1);
    assert!(app.pending_prompt.is_none());
    assert_eq!(
        app.session.queue.visible().next().map(|q| q.text.as_str()),
        Some("queued later")
    );
}

#[tokio::test]
async fn typing_while_busy_updates_input_buffer() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.busy = true;
    for c in "next".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "next");
    assert_eq!(app.session.queue.len(), 0);
}

#[tokio::test]
async fn ctrl_p_toggles_bottom_panel_without_touching_input() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");

    app.handle_key(press(KeyCode::Char('`'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.input.text, "draft");

    app.handle_key(press(KeyCode::Char('`'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.bottom_panel.open);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn alt_number_opens_selected_bottom_panel_tab() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    app.handle_key(press(KeyCode::Char('4'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
}

#[tokio::test]
async fn focused_bottom_panel_cycles_without_typing_into_chat() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");
    app.bottom_panel.open_tab(BottomPanelTab::Terminal);
    app.focus_block(FocusBlock::BottomPanel);

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Terminal);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn editor_uppercase_g_does_not_reach_chat_input() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (dir, session) = test_session().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");
    app.open_file_in_editor(&path);

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.source_viewer.current_line, 2);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn question_mark_opens_help_overlay() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::F(1), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::Help)));
    assert!(app.input.text.is_empty());
    assert!(app.feedback.text.contains("Help"));
}

#[tokio::test]
async fn empty_enter_when_idle_dequeues_and_sends() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Simulate a message enqueued while processing.
    app.session.enqueue_task("from queue").await.unwrap();
    app.busy = false;
    assert!(app.pending_prompt.is_none());
    // Empty Enter = user action to dequeue + send
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.session.queue.is_empty());
    // Promotion already appended the message and started the task; the
    // turn continues via `pending_turn_continue`, not a re-appended
    // `pending_prompt` (that would double-append).
    assert!(app.pending_prompt.is_none());
    assert!(app.pending_turn_continue);
    assert!(app.busy);
    assert!(app
        .session
        .messages
        .iter()
        .any(|m| m.content == "from queue"));
}

#[tokio::test]
async fn ctrl_backspace_cancels_selected_queue_message() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.enqueue_user_message("a".into()).await;
    app.enqueue_user_message("b".into()).await;
    app.move_queue_selection(1);
    app.handle_key(press(KeyCode::Backspace, KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.session.queue.len(), 1);
    assert_eq!(
        app.session.queue.visible().next().map(|q| q.text.as_str()),
        Some("a")
    );
}

#[tokio::test]
async fn effort_selection_persists_across_tui_instances() {
    let (_dir, session) = test_session().await;
    let credential_dir = tempfile::tempdir().unwrap();
    let credential_path = credential_dir.path().join("credentials.toml");
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect.store = CredentialStore::new(credential_path.clone());

    app.reasoning_effort = ReasoningEffort::High;
    app.persist_selection();

    assert_eq!(
        app.connect.store.last_effort().unwrap().as_deref(),
        Some("high")
    );

    let (_dir, session) = test_session().await;
    let mut restarted = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    restarted.connect.store = CredentialStore::new(credential_path);
    restarted = restarted.restore_saved_auth();

    assert_eq!(restarted.reasoning_effort, ReasoningEffort::High);
}

#[tokio::test]
async fn switching_to_a_model_that_drops_the_current_effort_notifies_and_falls_back() {
    let (_dir, session) = test_session().await;
    let credential_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect.store = CredentialStore::new(credential_dir.path().join("credentials.toml"));

    // claude-sonnet-4-6 does not offer XHigh (effort.rs::options_for_model).
    app.reasoning_effort = ReasoningEffort::XHigh;
    app.resolve_effort_for_model("anthropic/claude-sonnet-4-6");

    assert_eq!(app.reasoning_effort, ReasoningEffort::Low);
    assert_eq!(
        app.status_message,
        "Extra High effort is not supported by this model. Using Low."
    );
}

#[tokio::test]
async fn model_command_applies_provider_id_to_session() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");
    app.apply_model_selection("native", "openai/gpt-4.1-mini", None);
    assert_eq!(app.runtime.model_label, "openai/gpt-4.1-mini");
    assert_eq!(app.session.active_model, "openai/gpt-4.1-mini");
    assert!(app.pending_prompt.is_none());
}

#[tokio::test]
async fn model_command_rejects_cross_provider_selection_without_matching_connection() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai-codex/gpt-5.6-sol".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect.store
        .set_oauth(
            "openai_codex",
            forge_connect::OauthTokens {
                access_token:
                    "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.sig"
                        .to_string(),
                refresh_token: None,
                expires_at: None,
            },
        )
        .unwrap();
    app.connect.profile = Some("openai_codex".into());
    app.runtime.model_label = "openai-codex/gpt-5.6-sol".into();
    app.session.set_active_model("openai-codex/gpt-5.6-sol");

    app.dispatch_line("/model not-connected claude-sonnet-4-5")
        .await
        .unwrap();

    assert_eq!(app.connect.profile.as_deref(), Some("openai_codex"));
    assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6-sol");
    assert!(
        app.status_message.contains("connect `not-connected` first")
            || app.notices.iter().any(|l| l.contains("not-connected")),
        "expected rejection notice, got status={} notices={:?}",
        app.status_message,
        app.notices
    );
}

#[tokio::test]
async fn app_dispatch_user_message() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("/tmp"),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    assert!(
        app.session
            .messages
            .iter()
            .any(|m| m.content.contains("hello tui") || m.content == "hi"),
        "messages={:?}",
        app.session
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn app_status_command() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("/status").await.unwrap();
    assert!(app.overlay.is_none());
    assert!(app.notices.is_empty());
}

#[tokio::test]
async fn clear_hides_existing_chat_without_deleting_context() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    let message_count = app.session.messages.len();
    let event_count = app.session.events.len();
    assert!(message_count > 0);

    app.dispatch_line("/clear").await.unwrap();

    assert_eq!(app.chat_message_start, message_count);
    assert_eq!(app.chat_event_start, event_count);
    assert_eq!(app.session.messages.len(), message_count);
    assert_eq!(app.session.events.len(), event_count);
    assert!(app.ui_banners.is_empty());
    assert!(app.notices.is_empty());
    assert_eq!(app.chat_scroll, 0);
    assert!(app.chat_follow);
}

#[tokio::test]
async fn app_quit_command() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "p".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("/quit").await.unwrap();
    assert!(app.should_quit);
}

#[tokio::test]
async fn history_records_submitted_lines_and_up_recalls() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.7.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.input.set_text("/status");
    app.handle_key(enter).await.unwrap();
    app.input.set_text("/model");
    app.handle_key(enter).await.unwrap();
    assert!(app.history.len() >= 2);
    let t = app.history.up(&app.input.text).unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/model");
    let t = app.history.up(&app.input.text).unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/status");
    let t = app.history.down().unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/model");
}

#[tokio::test]
async fn history_up_via_key_when_no_overlay() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.7.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.history.push("alpha");
    app.history.push("beta");
    app.input.clear();
    let up = KeyEvent {
        code: KeyCode::Up,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.handle_key(up).await.unwrap();
    assert_eq!(app.input.text, "beta");
    app.handle_key(up).await.unwrap();
    assert_eq!(app.input.text, "alpha");
    let down = KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.handle_key(down).await.unwrap();
    assert_eq!(app.input.text, "beta");
}

#[tokio::test]
async fn slash_stays_in_textbox_does_not_open_palette() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "/");
    assert!(app.overlay.is_none(), "Phase 8: / must not open palette");
    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('t'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "/st");
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn enter_runs_slash_from_main_textbox() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "/status");
    assert!(app.overlay.is_none());
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.notices.is_empty());
    assert!(app.history.entries().iter().any(|e| e == "/status"));
}

#[tokio::test]
async fn inspector_is_closed_by_default_and_opens_on_demand() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(!app.sidebar_visible);
    app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.sidebar_visible);
    assert!(split_areas_full(
        ratatui::layout::Rect::new(0, 0, 120, 30),
        0,
        3,
        app.sidebar_visible,
        0
    )
    .sidebar
    .is_some());
    app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.sidebar_visible);
    assert!(
        split_areas_full(ratatui::layout::Rect::new(0, 0, 80, 24), 0, 3, true, 0)
            .sidebar
            .is_none()
    );
}

#[tokio::test]
async fn inspector_view_shortcuts_cycle_without_opening_sidebar() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert_eq!(app.inspector_view, InspectorView::Task);
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Context);
    app.handle_key(press(KeyCode::Char('['), KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Task);
    assert!(!app.sidebar_visible);
}

#[tokio::test]
async fn multi_token_slash_connect_list_opens_picker() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/connect list".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    match &app.overlay {
        Some(Overlay::ConnectModel { providers, .. }) => {
            assert!(providers.iter().any(|p| p.vendor_id == "xai"));
            assert!(providers
                .iter()
                .any(|p| p.routes.iter().any(|r| r.profile_id == "opencode_go")));
        }
        other => panic!("expected ConnectModel, got {other:?}"),
    }
}

#[tokio::test]
async fn slash_tab_autocompletes_command() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/res".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert!(!app.slash_suggestions().is_empty());
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.input.text.starts_with("/resume"),
        "got {}",
        app.input.text
    );
}

#[tokio::test]
async fn startup_notices_seed_notice_panel() {
    let (_dir, session) = test_session().await;
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: vec!["mcp: failed".into()],
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    assert_eq!(app.notices, vec!["mcp: failed"]);
}

#[tokio::test]
async fn enter_on_highlighted_suggestion_runs_command() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Partial type; suggestions include /connect and /status.
    for c in "/con".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    let suggestions = app.slash_suggestions();
    assert!(!suggestions.is_empty(), "expected slash suggestions");
    // Move highlight onto /connect if it is not already first
    let connect_idx = suggestions
        .iter()
        .position(|s| s.cmd == "/connect")
        .expect("/connect in suggestions for /con");
    for _ in 0..connect_idx {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(
        app.slash_suggestions()[app.slash_suggest_idx].cmd,
        "/connect"
    );
    // One Enter should apply selection AND open the connect picker (not merely complete text)
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::ConnectModel { .. })),
        "Enter on highlighted /connect should open picker; overlay={:?} input={:?} status={}",
        app.overlay,
        app.input.text,
        app.status_message
    );
    assert!(
        app.input.text.is_empty(),
        "input should be cleared after run, got {:?}",
        app.input.text
    );
}

#[tokio::test]
async fn bare_slash_lists_all_palette_commands() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    let suggestions = app.slash_suggestions();
    let expected = crate::overlays::default_palette_items();
    assert_eq!(
        suggestions.len(),
        expected.len(),
        "bare / should list every palette command; got {:?}",
        suggestions.iter().map(|s| &s.cmd).collect::<Vec<_>>()
    );
    for cmd in [
        "/connect",
        "/model",
        "/compact",
        "/resume",
        "/clear",
        "/disconnect",
        "/quit",
    ] {
        assert!(
            suggestions.iter().any(|s| s.cmd == cmd),
            "missing {cmd} in suggestions"
        );
    }
}

#[tokio::test]
async fn enter_on_status_suggestion_runs_immediately() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/sta".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
}
