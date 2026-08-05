//! Provider connect/disconnect and credential overlay tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;
use crate::overlays::{handle_overlay_key, Key as OverlayKey, ModelItem, OverlayAction};

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
async fn model_selection_with_explicit_route_never_crosses_wires_between_openai_and_openai_codex() {
    // `openai` (generic API key) and `openai_codex` (subscription/OAuth)
    // share a vendor but are distinct accounts with distinct entitlements —
    // a picker selection naming one explicitly must never land on the
    // other, even though both are connected simultaneously.
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
        .set_api_key("openai_codex", "sk-test-openai-codex-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());

    // The picker-selection path always supplies an explicit `profile_id`
    // (the resolved route) — this is what `finish_connect_flow`/the Models
    // column Enter handler do, never the free-text `/model <arg>` path.
    app.apply_model_selection("native", "openai-codex/gpt-5.6", Some("openai_codex"));

    assert_eq!(app.connect.profile.as_deref(), Some("openai_codex"));
    assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6");

    // And the reverse: switching back to the generic API-key route must not
    // stick on the subscription route either.
    app.apply_model_selection("native", "openai/gpt-5.6", Some("openai"));

    assert_eq!(app.connect.profile.as_deref(), Some("openai"));
    assert_eq!(app.runtime.model_label, "openai/gpt-5.6");
}

/// A model picker overlay open on the Models column with a single,
/// deterministic catalog row — avoids depending on `model_picker_items`'
/// live/cached catalog fetch for these tests.
fn model_picker_overlay_with(model: &str, profile_id: &str) -> Overlay {
    Overlay::connect_model_open(
        vec![],
        vec![ModelItem {
            provider: "native".into(),
            model: model.into(),
            profile_id: Some(profile_id.into()),
            source: forge_connect::CatalogSource::Default,
            route_label: profile_id.into(),
        }],
        Some(profile_id),
        model,
        ReasoningEffort::default(),
        ConnectModelColumn::Models,
    )
}

async fn model_switch_test_app(cred_dir: &tempfile::TempDir) -> TuiApp {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-5.6".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());
    app.session.set_active_model("openai/gpt-5.6");
    app
}

#[tokio::test]
async fn selecting_the_current_model_is_a_no_op_and_keeps_the_same_route() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;

    app.overlay = Some(model_picker_overlay_with("openai/gpt-5.6", "openai"));
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter);
    app.apply_overlay_action(action).await.unwrap();

    assert_eq!(app.connect.profile.as_deref(), Some("openai"));
    assert_eq!(app.runtime.model_label, "openai/gpt-5.6");
    assert_eq!(app.session.active_model, "openai/gpt-5.6");
}

#[tokio::test]
async fn switching_to_another_model_updates_label_route_and_session_together() {
    // Regression for "openai-codex/luna is not found": reach the row via a
    // *partial* filter (a substring of the full id), exactly as the picker's
    // search box is used in practice, and confirm the complete id survives.
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;

    app.overlay = Some(Overlay::connect_model_open(
        vec![],
        vec![ModelItem {
            provider: "native".into(),
            model: "openai/gpt-5.6-luna".into(),
            profile_id: Some("openai".into()),
            source: forge_connect::CatalogSource::Default,
            route_label: "openai".into(),
        }],
        Some("openai"),
        "openai/gpt-5.6",
        ReasoningEffort::default(),
        ConnectModelColumn::Models,
    ));
    for c in "luna".chars() {
        handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Char(c));
    }
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter);
    assert!(
        matches!(&action, OverlayAction::SelectModel { model, .. } if model == "openai/gpt-5.6-luna"),
        "expected a resolved SelectModel action, got {action:?}"
    );
    app.apply_overlay_action(action).await.unwrap();

    assert_eq!(app.connect.profile.as_deref(), Some("openai"));
    assert_eq!(app.runtime.model_label, "openai/gpt-5.6-luna");
    assert_eq!(app.session.active_model, "openai/gpt-5.6-luna");
}

#[tokio::test]
async fn changing_effort_after_a_model_switch_persists_the_new_value() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;

    app.overlay = Some(model_picker_overlay_with("openai/gpt-5.6-luna", "openai"));
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter);
    app.apply_overlay_action(action).await.unwrap();

    app.apply_overlay_action(OverlayAction::SelectEffort(ReasoningEffort::High))
        .await
        .unwrap();

    assert_eq!(app.reasoning_effort.value, ReasoningEffort::High);
    app.persist_selection();
    assert_eq!(
        app.connect.store.last_effort().unwrap().as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn cancelling_at_the_models_column_leaves_active_selection_untouched() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;
    let (before_label, before_profile) =
        (app.runtime.model_label.clone(), app.connect.profile.clone());

    app.overlay = Some(model_picker_overlay_with("openai/gpt-5.6-luna", "openai"));
    for c in "luna".chars() {
        handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Char(c));
    }
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Esc);
    assert_eq!(action, OverlayAction::Close);
    app.apply_overlay_action(action).await.unwrap();

    assert!(app.overlay.is_none());
    assert_eq!(app.runtime.model_label, before_label);
    assert_eq!(app.connect.profile, before_profile);
}

#[tokio::test]
async fn cancelling_at_the_effort_column_leaves_active_selection_untouched() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;
    let before_effort = app.reasoning_effort.value;

    app.overlay = Some(Overlay::connect_model_open(
        vec![],
        vec![],
        Some("openai"),
        "openai/gpt-5.6",
        ReasoningEffort::default(),
        ConnectModelColumn::Effort,
    ));
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Esc);
    assert_eq!(action, OverlayAction::Close);
    app.apply_overlay_action(action).await.unwrap();

    assert!(app.overlay.is_none());
    assert_eq!(app.reasoning_effort.value, before_effort);
}

#[tokio::test]
async fn restart_restores_the_persisted_selection_via_restore_saved_auth() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;

    app.overlay = Some(model_picker_overlay_with("openai/gpt-5.6-luna", "openai"));
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter);
    app.apply_overlay_action(action).await.unwrap();
    app.apply_overlay_action(OverlayAction::SelectEffort(ReasoningEffort::High))
        .await
        .unwrap();
    app.persist_selection();

    let (_dir2, session2) = test_session().await;
    let restarted = TuiApp::new(
        session2,
        TuiRuntimeConfig {
            model_label: String::new(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let mut restarted = restarted;
    restarted.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    restarted
        .connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    let restarted = restarted.restore_saved_auth();

    assert_eq!(restarted.connect.profile.as_deref(), Some("openai"));
    assert_eq!(restarted.runtime.model_label, "openai/gpt-5.6-luna");
    assert_eq!(restarted.reasoning_effort.value, ReasoningEffort::High);
    assert_eq!(restarted.session.active_model, "openai/gpt-5.6-luna");
}

#[tokio::test]
async fn first_request_after_switching_models_uses_the_new_complete_id() {
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = model_switch_test_app(&cred_dir).await;

    app.overlay = Some(model_picker_overlay_with("openai/gpt-5.6-luna", "openai"));
    let action = handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter);
    app.apply_overlay_action(action).await.unwrap();

    let request = app.session.build_model_request();
    assert_eq!(request.model, "openai/gpt-5.6-luna");
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
async fn first_connection_auto_selects_a_default_model_and_lands_in_steady_state() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    assert_eq!(
        app.connect.profile, None,
        "must start from a genuinely zero-state, not a leaked real profile"
    );
    // Simulate what the real connect flow already guarantees by this point:
    // the credential is persisted and `connect.profile` points at it.
    app.connect
        .store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());

    app.finish_connect_flow("openai");

    assert!(
        app.overlay.is_none(),
        "first connection must land in steady state, not force the model picker open"
    );
    assert_eq!(app.connect.profile.as_deref(), Some("openai"));
    assert!(!app.runtime.model_label.is_empty());
    assert!(app.runtime.model_label.starts_with("openai/"));
    assert_eq!(
        app.reasoning_effort.value,
        ReasoningEffort::default_for_model(&app.runtime.model_label)
    );
}

#[tokio::test]
async fn second_connection_still_opens_the_model_picker() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());
    app.finish_connect_flow("openai");
    assert!(
        app.overlay.is_none(),
        "first connection lands in steady state"
    );

    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("anthropic".into());
    app.finish_connect_flow("anthropic");

    let Some(Overlay::ConnectModel {
        selected_route,
        focus,
        ..
    }) = &app.overlay
    else {
        panic!(
            "expected the routine second connect to open the model picker, got {:?}",
            app.overlay
        );
    };
    assert_eq!(selected_route.as_deref(), Some("anthropic"));
    assert_eq!(*focus, ConnectModelColumn::Models);
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
async fn oauth_cancel_via_escape_stops_the_background_poll() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    app.connect.store = CredentialStore::new(cred_dir.path().join("c.toml"));
    app.dispatch_line("/connect xai").await.unwrap();
    std::env::remove_var("FORGE_CONNECT_OAUTH_STUB");
    assert!(
        app.connect.oauth_pending.is_some(),
        "flow must be pending first"
    );

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.overlay.is_none());
    assert!(
        app.connect.oauth_pending.is_none(),
        "cancelling must stop the background poll, not just close the overlay"
    );
    assert!(app.connect.oauth_last_poll.is_none());

    // Even if the device code were to complete after this point, there is no
    // pending flow left to advance — the cancelled attempt can never
    // silently connect.
    app.poll_oauth_tick();
    assert!(app.connect.profile.is_none());
    assert_eq!(app.runtime.model_label, "m");
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
async fn open_connect_picker_opens_immediately_and_starts_background_refresh() {
    let _home_guard = isolated_home_guard();
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));

    // Opening must not block on network I/O: the overlay is populated
    // synchronously from cache, and a refresh is kicked off in the
    // background rather than run inline.
    app.open_connect_picker();

    assert!(
        matches!(app.overlay, Some(Overlay::ConnectModel { .. })),
        "expected ConnectModel overlay, got {:?}",
        app.overlay
    );
    assert!(
        app.catalog_fetch.refresh_rx.is_some(),
        "expected a background catalog refresh to be in flight"
    );
}

#[tokio::test]
async fn poll_catalog_refresh_is_a_noop_when_nothing_in_flight() {
    let _home_guard = isolated_home_guard();
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(app.catalog_fetch.refresh_rx.is_none());
    app.poll_catalog_refresh();
    assert!(app.overlay.is_none());
    assert!(app.catalog_fetch.refresh_rx.is_none());
}

#[tokio::test]
async fn warm_catalog_once_connected_is_a_noop_for_the_mock_provider() {
    let _home_guard = isolated_home_guard();
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );

    app.warm_catalog_once_connected();

    assert!(app.catalog_fetch.refresh_rx.is_none());
    assert!(!app.catalog_fetch.warmed);
}

#[tokio::test]
async fn warm_catalog_once_connected_starts_exactly_one_background_refresh() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "anthropic/claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("anthropic".into());

    app.warm_catalog_once_connected();
    assert!(app.catalog_fetch.warmed);
    assert!(
        app.catalog_fetch.refresh_rx.is_some(),
        "expected the first connected tick to start a background refresh"
    );

    // Simulate the refresh completing, then tick again — a second connected
    // tick must not start another one.
    let (tx, rx) = std::sync::mpsc::channel();
    app.catalog_fetch.refresh_rx = Some(rx);
    tx.send(Ok(())).unwrap();
    app.poll_catalog_refresh();
    assert!(app.catalog_fetch.refresh_rx.is_none());

    app.warm_catalog_once_connected();
    assert!(
        app.catalog_fetch.refresh_rx.is_none(),
        "warm-once must not fire a second background refresh after the first completes"
    );
}

#[tokio::test]
async fn background_catalog_refresh_updates_open_picker_rows_once_complete() {
    let _home_guard = isolated_home_guard();
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));

    app.open_connect_picker();
    assert!(app.catalog_fetch.refresh_rx.is_some());

    // The real worker thread does credential-less (and, in a sandboxed test
    // environment, possibly unreachable) network I/O, so its completion time
    // is not something a test should race against. Swap in a synthetic
    // channel we control to exercise the completion/refresh-in-place
    // transition deterministically, matching how `poll_catalog_refresh`
    // reacts to any completed worker regardless of what it fetched.
    let (tx, rx) = std::sync::mpsc::channel();
    app.catalog_fetch.refresh_rx = Some(rx);
    tx.send(Ok(())).unwrap();

    app.poll_catalog_refresh();

    assert!(app.catalog_fetch.refresh_rx.is_none());
    assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
}

#[tokio::test]
async fn alt_c_opens_compact_model_control() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "anthropic/claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("anthropic".into());

    app.handle_key(press(KeyCode::Char('c'), KeyModifiers::ALT))
        .await
        .unwrap();

    match &app.overlay {
        Some(Overlay::ConnectModel { compact, focus, .. }) => {
            assert!(*compact);
            assert_eq!(*focus, ConnectModelColumn::Models);
        }
        other => panic!("expected compact ConnectModel overlay, got {other:?}"),
    }
}

#[tokio::test]
async fn footer_shows_na_effort_for_a_model_that_does_not_support_it() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    // gpt-4.1-mini isn't in ReasoningEffort::model_supports_effort's
    // openai allow-list (only gpt-5/o1/o3/o4 prefixes are) — a real,
    // connected model with no adjustable effort.
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));
    app.connect
        .store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect.profile = Some("openai".into());

    let text = render_app_text(&mut app, 120, 40);

    assert!(
        text.contains("N/A"),
        "expected the footer to show an explicit N/A effort segment:\n{text}"
    );
    assert!(
        !text.contains("[Auto]") && !text.contains("[Low]"),
        "must not display a level word for a model with no adjustable effort:\n{text}"
    );
}

#[tokio::test]
async fn compact_control_escape_cancels_without_state_change() {
    let _home_guard = isolated_home_guard();
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "anthropic/claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    let store_dir = tempfile::TempDir::new().unwrap();
    app.connect.store = CredentialStore::new(store_dir.path().join("empty-creds.toml"));
    app.connect
        .store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect.profile = Some("anthropic".into());
    app.reasoning_effort.value = ReasoningEffort::Low;
    let model_before = app.runtime.model_label.clone();
    let effort_before = app.reasoning_effort.value;

    app.open_connect_picker_compact(ConnectModelColumn::Models);
    let focus_at = |app: &TuiApp| match &app.overlay {
        Some(Overlay::ConnectModel { focus, .. }) => *focus,
        other => panic!("expected ConnectModel overlay, got {other:?}"),
    };
    assert_eq!(focus_at(&app), ConnectModelColumn::Models);

    // Each view is standalone (no Tab cycling) — Esc closes the picker
    // from the view it was opened on, with no partial commit.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.overlay.is_none());
    assert_eq!(app.runtime.model_label, model_before);
    assert_eq!(app.reasoning_effort.value, effort_before);
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
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    assert!(app.is_provider_connected());
    app.dispatch_line("hi").await.unwrap();
    assert_eq!(app.pending_turn.prompt.as_deref(), Some("hi"));
}
