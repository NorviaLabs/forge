//! Provider connect/disconnect and credential overlay tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn connect_opencode_go_opens_api_key_overlay() {
    // Isolate HOME to prevent restore_saved_auth from discovering real credentials.
    let _home_guard = {
        let temp_home = tempfile::TempDir::new().unwrap();
        let cred_dir = temp_home.path().join("Library/Application Support/forge");
        std::fs::create_dir_all(&cred_dir).unwrap_or_default();
        let _ = std::fs::write(cred_dir.join("credentials.toml"), "");

        let guard = ScopedEnvGuard::new(&[
            "HOME",
            "XDG_CONFIG_HOME",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OLLAMA_API_KEY",
            "XAI_API_KEY",
        ]);
        std::env::set_var("HOME", temp_home.path());
        (temp_home, guard)
    };

    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let _store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
    app.dispatch_line("/connect opencode_go").await.unwrap();
    match &app.overlay {
        Some(Overlay::ConnectApiKey {
            profile_id, title, ..
        }) => {
            assert_eq!(profile_id, "opencode_go");
            assert!(title.contains("OpenCode"));
        }
        other => panic!("expected ConnectApiKey overlay, got {other:?}"),
    }
}

#[tokio::test]
async fn disconnect_clears_credentials_and_prompts_reauth() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");

    app.dispatch_line("/disconnect").await.unwrap();

    assert!(app.connect.auth_suspended);
    assert!(app.connect.profile.is_none());
    assert!(!app.is_provider_connected());
    assert!(!app.connect.store.is_connected("openai").unwrap());
    assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
    assert!(
        app.notice_state
            .items
            .iter()
            .any(|l| l.contains("disconnected"))
            || app.status_state.message.contains("disconnected")
    );
}

#[tokio::test]
async fn connect_picker_marks_saved_credentials_as_connected() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();

    app.open_connect_picker();
    let Some(Overlay::ConnectModel { providers, .. }) = &app.overlay else {
        panic!("expected connect picker");
    };
    assert!(
        providers
            .iter()
            .flat_map(|vendor| &vendor.routes)
            .any(|route| route.profile_id == "openai" && route.connected),
        "saved provider should be marked connected"
    );
}

#[tokio::test]
async fn successful_connect_hands_off_to_model_picker() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");

    app.open_model_picker_after_connect("openai");
    let Some(Overlay::ConnectModel {
        selected_route,
        focus,
        ..
    }) = &app.overlay
    else {
        panic!("expected model picker");
    };
    assert_eq!(selected_route.as_deref(), Some("openai"));
    assert_eq!(*focus, ConnectModelColumn::Models);
    assert!(app.feedback.text.contains("choose a model"));
}

#[tokio::test]
async fn model_selection_switches_to_the_matching_connected_provider() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());

    app.apply_model_selection("native", "anthropic/claude-sonnet-4-5", None);

    assert_eq!(app.connect.profile.as_deref(), Some("anthropic"));
    assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");
}

#[tokio::test]
async fn quick_switch_toggles_between_the_two_most_recent_deliberate_selections() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());

    // No deliberate selection recorded yet: nothing to switch to.
    app.quick_switch_model();
    assert_eq!(app.status_state.message, "no previous model to switch to");

    app.apply_model_selection("native", "openai/gpt-4.1-mini", Some("openai"));
    app.apply_model_selection("native", "anthropic/claude-sonnet-4-5", Some("anthropic"));
    assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");

    app.quick_switch_model();
    assert_eq!(app.runtime.model_label, "openai/gpt-4.1-mini");
    assert_eq!(app.connect.profile.as_deref(), Some("openai"));

    // A second Quick Switch toggles back — no picker opened at any point.
    app.quick_switch_model();
    assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");
    assert_eq!(app.connect.profile.as_deref(), Some("anthropic"));
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn invalid_api_key_error_stays_inside_key_modal() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    let mut overlay = Overlay::connect_api_key("openai", "OpenAI", None, None);
    if let Overlay::ConnectApiKey { key_input, .. } = &mut overlay {
        *key_input = "bad".into();
    }
    app.overlay = Some(overlay);

    app.try_connect_api_key("openai", Some("bad".into()));
    let Some(Overlay::ConnectApiKey {
        key_input, error, ..
    }) = &app.overlay
    else {
        panic!("expected API key modal to remain open");
    };
    assert_eq!(key_input, "bad");
    assert!(error.as_deref().is_some_and(|text| text.contains("short")));
    assert!(
        !app.banner_state.items.iter().any(|item| matches!(
            item,
            ChatItem::Banner {
                kind: BannerKind::Error,
                ..
            }
        )),
        "onboarding errors should stay in the modal"
    );
}

#[tokio::test]
async fn connect_xai_opens_oauth_overlay() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    // Isolate credentials + use stub device start (no network).
    std::env::set_var("FORGE_CONNECT_OAUTH_STUB", "1");
    std::env::remove_var("FORGE_CONNECT_OAUTH_FIXTURE");
    std::env::remove_var("FORGE_XAI_OAUTH_ACCESS_TOKEN");
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("c.toml"));
    app.dispatch_line("/connect xai").await.unwrap();
    std::env::remove_var("FORGE_CONNECT_OAUTH_STUB");
    match &app.overlay {
        Some(Overlay::ConnectOauth {
            profile_id, title, ..
        }) => {
            assert_eq!(profile_id, "xai");
            assert!(title.contains("Grok") || title.contains("xAI"));
        }
        other => panic!("expected ConnectOauth overlay, got {other:?}"),
    }
    assert!(app.connect.oauth_pending.is_some());
}

#[tokio::test]
async fn connect_alone_opens_profile_picker() {
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
    assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
}

#[tokio::test]
async fn blocks_chat_when_not_connected() {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Isolate HOME to a temp dir so CredentialStore::user_default() cannot
    // discover the real user's ~/.forge/credentials.toml.
    let _home_guard = {
        let temp_home = tempfile::TempDir::new().unwrap();
        // Create the credential directory structure that user_default() expects.
        // On macOS: {HOME}/Library/Application Support/forge/credentials.toml
        // On Linux: {HOME}/.config/forge/credentials.toml
        let cred_dir = temp_home.path().join("Library/Application Support/forge");
        std::fs::create_dir_all(&cred_dir).unwrap_or_default();
        let _ = std::fs::write(cred_dir.join("credentials.toml"), "");

        let guard = ScopedEnvGuard::new(&[
            "HOME",
            "XDG_CONFIG_HOME",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OLLAMA_API_KEY",
            "XAI_API_KEY",
        ]);
        std::env::set_var("HOME", temp_home.path());
        (temp_home, guard)
    };

    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    // Override credential store with empty temp file so connection check fails.
    let _store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
    app.connect.profile = None;
    app.refresh_connection_ui();
    assert!(!app.is_provider_connected());

    for c in "hello world".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(
        app.pending_turn.prompt.is_none(),
        "must not queue a model turn"
    );
    assert!(!app.busy_state.active);
    assert_eq!(app.input.text, "hello world");
    assert!(
        app.banner_state.items.iter().any(|b| matches!(
            b,
            ChatItem::Banner { text, .. } if text.to_ascii_lowercase().contains("not connected")
        )) || app
            .activity
            .recent(8)
            .iter()
            .any(|e| e.summary.to_ascii_lowercase().contains("not connected")),
        "expected not-connected feedback"
    );
}

#[tokio::test]
async fn bare_model_command_opens_on_models_column() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.dispatch_line("/model").await.unwrap();
    let Some(Overlay::ConnectModel { focus, .. }) = &app.overlay else {
        panic!("expected ConnectModel overlay, got {:?}", app.overlay);
    };
    assert_eq!(*focus, ConnectModelColumn::Models);
}

#[tokio::test]
async fn mock_provider_allows_chat_without_connect() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(app.is_provider_connected());
    app.dispatch_line("hi").await.unwrap();
    assert_eq!(app.pending_turn.prompt.as_deref(), Some("hi"));
}
