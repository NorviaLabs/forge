//! Slash commands, palette, queue, and input dispatch tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;
use forge_connect::PreferenceStore;

#[tokio::test]
async fn edtui_search_is_active_and_esc_returns_to_normal_mode() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Search
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Normal
    );
}

#[tokio::test]
async fn edtui_search_keeps_shift_arrows_inside_the_search_field() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Search
    );
}

#[tokio::test]
async fn ctrl_g_remains_editor_owned_with_edtui_active() {
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
    assert!(app.source_viewer.jump.input.is_empty());
    assert_eq!(
        app.editor_session.as_ref().unwrap().mode(),
        edtui::EditorMode::Normal
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
    assert_eq!(app.editor_session.as_ref().unwrap().text(), "before\n");
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
    assert!(app.status_state.message.contains("resumed"));
    assert!(app.notice_state.items.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.summary.contains("session resumed")));
}

#[tokio::test]
async fn resume_restores_input_history_for_up_down_recall() {
    use crossterm::event::{KeyCode, KeyModifiers};
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
    // A real submission records both — the unified ComposerLineSubmitted
    // event (for history) and the model-directed UserMessage (for the
    // transcript) — mirroring what `record_submitted_line` + `dispatch_line`
    // do together in the live TUI.
    previous
        .record_composer_line("first message")
        .await
        .unwrap();
    previous.append_user_message("first message").await.unwrap();
    let previous_id = previous.session_id;

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // A brand-new TuiApp starts with empty history — resuming should
    // populate it from the target session's own past input.
    assert!(app.history.is_empty());
    app.dispatch_line(&format!("/resume {previous_id}"))
        .await
        .unwrap();
    assert_eq!(app.history.len(), 1);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "first message");
}

#[tokio::test]
async fn resume_restores_local_only_slash_commands_too() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (dir, session) = test_session().await;
    let model = Arc::new(MockModelClient::script(vec![]));
    let previous = AgentSession::create(
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
    // "/status" never reaches the model (no `append_user_message`) — it's
    // exactly the kind of local-only command the old, UserMessage-only
    // history restoration used to drop.
    previous.record_composer_line("/status").await.unwrap();
    let previous_id = previous.session_id;

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line(&format!("/resume {previous_id}"))
        .await
        .unwrap();
    assert_eq!(app.history.len(), 1);

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "/status");
}

/// F-RESUME-01: the bare `/resume` list previously showed only a raw UUID
/// and timestamp per session, giving the user no way to tell sessions
/// apart without opening each one. It now shows a title hint derived from
/// the session's first user message.
#[tokio::test]
async fn bare_resume_list_shows_title_hint_from_first_user_message() {
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
        .append_user_message("fix the login bug please")
        .await
        .unwrap();

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("/resume").await.unwrap();

    let Some(Overlay::ResumePicker { items, .. }) = &app.overlay else {
        panic!("expected ResumePicker overlay, got {:?}", app.overlay);
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title.as_deref(), Some("fix the login bug please"));
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    app.dispatch_line("/compact").await.unwrap();

    assert!(app.notice_state.items.is_empty());
    assert!(app.banner_state.items.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Context));
    assert_eq!(app.status_state.message, "Continuing in a fresh context");
    assert!(app
        .banner_state
        .items
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // Input routing keys off the authoritative session lifecycle, not the
    // UI `busy` flag — a real task must be Working for Enter to enqueue.
    app.session.append_user_message("first").await.unwrap();
    app.busy_state.activate();
    for c in "queued later".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.session.queue().len(), 1);
    assert!(!app.pending_turn.has_prompt());
    assert_eq!(
        app.session
            .queue()
            .visible()
            .next()
            .map(|q| q.text.as_str()),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.busy_state.activate();
    for c in "next".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "next");
    assert_eq!(app.session.queue().len(), 0);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
async fn open_bottom_panel_sets_active_tab() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    app.open_bottom_panel();
    assert!(app.bottom_panel.open);
}

#[tokio::test]
async fn focused_bottom_panel_alt_arrows_do_not_type_into_chat() {
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.input.set_text("draft");
    app.open_bottom_panel();

    // The bottom panel is Terminal-only now — Alt+Left/Right has nothing
    // to cycle, but the key still shouldn't leak into the composer.
    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.input.set_text("draft");
    app.open_file_in_editor(&path);

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.editor_session.as_ref().unwrap().cursor_row(), 3);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
async fn slash_command_info_feedback_expires() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    app.dispatch_line("/help").await.unwrap();
    assert!(app.feedback.text.contains("Help"));
    app.feedback_until = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    app.tick_feedback();

    assert!(app.feedback.is_empty());
    assert!(app.status_state.message.is_empty());
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // Simulate a message enqueued while processing.
    app.session.enqueue_task("from queue").await.unwrap();
    app.busy_state.stop();
    assert!(!app.pending_turn.has_prompt());
    // Empty Enter = user action to dequeue + send
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.session.queue().is_empty());
    // Promotion already appended the message and started the task; the
    // turn continues via `PendingTurnState`, not a re-appended prompt
    // (that would double-append).
    assert!(!app.pending_turn.has_prompt());
    assert!(app.pending_turn.continue_requested());
    assert!(app.busy_state.is_active());
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.enqueue_user_message("a".into()).await;
    app.enqueue_user_message("b".into()).await;
    app.move_queue_selection(1);
    app.handle_key(press(KeyCode::Backspace, KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.session.queue().len(), 1);
    assert_eq!(
        app.session
            .queue()
            .visible()
            .next()
            .map(|q| q.text.as_str()),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(credential_path.clone());
    app.connect.preferences =
        PreferenceStore::new(credential_path.clone().with_file_name("preferences.toml"));

    app.reasoning_effort.value = ReasoningEffort::High;
    app.persist_selection();

    assert_eq!(
        app.connect.preferences.last_effort().unwrap().as_deref(),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    restarted.connect.preferences =
        PreferenceStore::new(credential_path.with_file_name("preferences.toml"));
    restarted.connect.store = CredentialStore::new(credential_path);
    restarted = restarted.restore_saved_auth();

    assert_eq!(restarted.reasoning_effort.value, ReasoningEffort::High);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(credential_dir.path().join("credentials.toml"));
    app.connect.preferences = PreferenceStore::new(credential_dir.path().join("preferences.toml"));

    // claude-sonnet-4-6 does not offer XHigh (effort.rs::options_for_model).
    app.reasoning_effort.value = ReasoningEffort::XHigh;
    app.resolve_effort_for_model("anthropic/claude-sonnet-4-6");

    assert_eq!(app.reasoning_effort.value, ReasoningEffort::Low);
    assert_eq!(
        app.status_state.message,
        "Extra High effort is not supported by this model. Using Low."
    );
}

#[tokio::test]
async fn drain_pending_prompt_sends_selected_effort_on_outbound_request() {
    let _home_guard = isolated_home_guard();
    let dir = TempDir::new().unwrap();
    let mock = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let session = session_for_workspace_with_model(dir.path(), mock.clone()).await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "anthropic/claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(dir.path().join("credentials.toml"));
    app.connect.preferences = PreferenceStore::new(dir.path().join("preferences.toml"));
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("anthropic".into());
    app.reasoning_effort.value = ReasoningEffort::High;

    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();

    let sent = mock
        .last_request()
        .expect("model client received a request");
    assert_eq!(sent.reasoning_effort.as_deref(), Some("high"));
}

#[tokio::test]
async fn drain_pending_prompt_omits_effort_for_model_that_does_not_support_it() {
    let _home_guard = isolated_home_guard();
    let dir = TempDir::new().unwrap();
    let mock = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let session = session_for_workspace_with_model(dir.path(), mock.clone()).await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(dir.path().join("credentials.toml"));
    app.connect.preferences = PreferenceStore::new(dir.path().join("preferences.toml"));
    // Stale effort left over from a previous model — must not leak onto a
    // model that doesn't support effort at all.
    app.reasoning_effort.value = ReasoningEffort::High;

    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();

    let sent = mock
        .last_request()
        .expect("model client received a request");
    assert_eq!(sent.reasoning_effort, None);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect.preferences = PreferenceStore::new(cred_dir.path().join("preferences.toml"));
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
    assert!(!app.pending_turn.has_prompt());
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect.preferences = PreferenceStore::new(cred_dir.path().join("preferences.toml"));
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

    app.dispatch_line("/model").await.unwrap();

    assert_eq!(app.connect.profile.as_deref(), Some("openai_codex"));
    assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6-sol");
    assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("/status").await.unwrap();
    assert!(matches!(app.overlay, Some(Overlay::StatusReport { .. })));
    assert!(app.notice_state.items.is_empty());
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    let message_count = app.session.messages.len();
    let event_count = app.session.events.len();
    assert!(message_count > 0);

    app.dispatch_line("/clear").await.unwrap();

    assert_eq!(app.conversation_view.message_start, message_count);
    assert_eq!(app.conversation_view.event_start, event_count);
    assert_eq!(app.session.messages.len(), message_count);
    assert_eq!(app.session.events.len(), event_count);
    assert!(app.banner_state.items.is_empty());
    assert!(app.notice_state.items.is_empty());
    assert_eq!(app.conversation_view.scroll, 0);
    assert!(app.conversation_view.follow);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("/quit").await.unwrap();
    assert!(app.exit.is_requested());
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
async fn up_down_navigate_multiline_draft_before_touching_history() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.9.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.history.push("alpha");
    app.history.push("beta");
    // Wide composer area so these short lines don't word-wrap — one visual
    // row per explicit line.
    app.composer_area = Some(ratatui::layout::Rect::new(0, 0, 60, 10));
    app.input.set_text("line one\nline two\nline three");
    // Cursor on the middle line — neither the first nor the last visual row.
    app.input.cursor = "line one\nline ".len();

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.input.text, "line one\nline two\nline three",
        "Up on a middle line should move the cursor, not touch history"
    );
    assert!(!app.history.browsing());

    // Now on the first line — the next Up should fall through to history.
    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.input.text, "beta",
        "Up on the first line should recall history"
    );
    assert!(app.history.browsing());
}

#[tokio::test]
async fn browsing_history_ignores_cursor_row_and_always_cycles() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.9.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.history.push("first\nentry\nhere");
    app.history.push("second entry");
    app.composer_area = Some(ratatui::layout::Rect::new(0, 0, 60, 10));

    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "second entry");
    assert!(app.history.browsing());

    // Still browsing — Up must cycle further into history regardless of
    // where the cursor sits in the currently-shown (multi-line) entry.
    app.handle_key(press(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "first\nentry\nhere");
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
    assert!(matches!(app.overlay, Some(Overlay::StatusReport { .. })));
    assert!(app.notice_state.items.is_empty());
    assert!(app.history.entries().iter().any(|e| e == "/status"));
}

#[tokio::test]
async fn status_slash_command_opens_overlay_on_enter() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.input.set_text("/status");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::StatusReport { .. })));
}

#[tokio::test]
async fn slash_connect_opens_picker() {
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    for c in "/connect".chars() {
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    assert_eq!(app.notice_state.items, vec!["mcp: failed"]);
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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
        app.slash_suggestions()[app.slash_suggestions.selected].cmd,
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
        app.status_state.message
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
    let workspace_root = session.workspace_root().to_path_buf();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    let suggestions = app.slash_suggestions();
    let expected = crate::overlays::default_palette_items();
    let skill_count = forge_context::discover_skills(&workspace_root).len();
    assert_eq!(
        suggestions.len(),
        expected.len() + skill_count,
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
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
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

#[tokio::test]
async fn attaching_clipboard_bytes_uses_application_storage_outside_a_repository() {
    let (_dir, mut app) = focus_test_app().await;
    app.attach_image_bytes(&forge_types::sample_png_bytes());
    assert_eq!(app.attachment.image_count(), 1);
    let image = &app.attachment.images()[0];
    assert!(
        std::path::Path::new(&image.path).is_absolute(),
        "application-data fallback must not be faked as a workspace path"
    );
    assert_eq!(image.mime, "image/png");
    assert!(std::path::Path::new(&image.path).is_file());
    assert!(app.pending_image_label().unwrap().starts_with("Image: "));
}

#[tokio::test]
async fn send_with_image_is_blocked_when_model_cannot_see() {
    let (_dir, mut app) = focus_test_app().await;
    app.runtime.provider = "mock".into();
    app.runtime.model_label = "mock".into();
    app.session.set_image_input_supported(false);
    app.attach_image_bytes(&forge_types::sample_png_bytes());
    app.input.set_text("compare this");
    app.submit_composer_message().await.unwrap();
    assert_eq!(app.input.text, "compare this");
    assert_eq!(app.attachment.image_count(), 1);
    assert!(!app.pending_turn.has_prompt());
}

#[tokio::test]
async fn send_with_image_is_allowed_when_model_can_see() {
    let (_dir, mut app) = focus_test_app().await;
    app.runtime.provider = "mock".into();
    app.runtime.model_label = "mock".into();
    app.session.set_image_input_supported(true);
    app.attach_image_bytes(&forge_types::sample_png_bytes());
    app.dispatch_line("compare this").await.unwrap();
    assert!(
        app.pending_turn.has_prompt(),
        "feedback={}",
        app.feedback.text
    );
    assert!(!app.attachment.has_images());
    assert_eq!(app.pending_turn.prompt(), Some("compare this"));
    assert_eq!(app.pending_turn.attachment_count(), 1);
}
