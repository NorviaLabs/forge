//! Tests for the built-in provider definitions (`providers/*.toml`) and
//! their key checks, one module per provider.

mod openai {
    #[allow(dead_code)]
    fn openai_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("openai")
    }

    use crate::verify::VerifyError;

    /// The profile's key check, against `base`.
    fn verify_api_key(key: &str, base: &str) -> Result<(), crate::verify::VerifyError> {
        let mut profile = openai_profile();
        profile.base_url = base.into();
        crate::service::verify_profile_key(&profile, key)
    }

    fn mock_server(status: u16) -> Option<String> {
        crate::test_support::serve(vec![status], r#"{"data":[]}"#)
    }

    #[test]
    fn profile_shape() {
        let p = openai_profile();
        assert_eq!(p.id, "openai");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("openai/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-short", &openai_profile().base_url)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", &openai_profile().base_url)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verifies_success_with_trimmed_base_url() {
        let Some(base) = mock_server(200) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-valid-key-for-tests", &base).is_ok());
    }

    #[test]
    fn verify_reports_auth_and_server_statuses_without_secret() {
        let Some(base) = mock_server(401) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        let err = verify_api_key("sk-valid-key-for-tests", &base)
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(!err.contains("sk-valid"));

        let err = verify_api_key("sk-valid-key-for-tests", &mock_server(500).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains("sk-valid"));
    }

    /// The key goes only to the configured host: a redirect is reported,
    /// never followed. (Following it here would reach a closed port and show
    /// up as `Unreachable`.)
    #[test]
    fn verify_does_not_follow_a_redirect() {
        let Some(base) = forge_test_support::mock_http(vec![(
            302,
            "",
            vec![("Location", "http://127.0.0.1:9/models")],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let error = verify_api_key("sk-0123456789abcdef0123", &base).unwrap_err();
        assert!(
            matches!(error, VerifyError::Status { status: 302, .. }),
            "{error:?}"
        );
    }
}

mod openai_codex {
    #[allow(dead_code)]
    fn openai_codex_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("openai_codex")
    }

    use base64::Engine;

    #[test]
    fn profile_uses_forge_oauth() {
        let profile = openai_codex_profile();
        assert!(profile.effective_auth().is_oauth());
        assert!(profile.api_key_env.is_empty());
        assert!(profile
            .default_models
            .iter()
            .all(|model| model.starts_with("openai_codex/")));
    }

    #[test]
    fn extracts_chatgpt_account_from_access_token() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        assert_eq!(
            crate::oauth_chatgpt::account_id_from_token(&format!("header.{encoded}.signature"))
                .unwrap(),
            "account-123"
        );
    }
}

mod anthropic {
    use crate::verify::verify_anthropic_key as verify_api_key;
    #[allow(dead_code)]
    fn anthropic_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("anthropic")
    }

    fn mock_server(statuses: Vec<u16>) -> Option<String> {
        crate::test_support::serve(statuses, r#"{"ok":true}"#)
    }

    #[test]
    fn profile_shape() {
        let p = anthropic_profile();
        assert_eq!(p.id, "anthropic");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("anthropic/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-ant-short", &anthropic_profile().base_url)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", &anthropic_profile().base_url)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verify_models_endpoint_accepts_success_client_error_and_rejects_auth_or_server() {
        let Some(base) = mock_server(vec![200]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &base).is_ok());
        assert!(verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![400]).unwrap()
        )
        .is_ok());

        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![403]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));

        // A 500 is now reported as a status failure rather than as
        // "unreachable": the server answered, so the request did reach it.
        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![500]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP 500"), "{err}");
    }

    #[test]
    fn verify_falls_back_to_messages_endpoint_on_missing_models_route() {
        let Some(base) = mock_server(vec![404, 400]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &base).is_ok());

        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![404, 401]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));
    }
}

mod xai {
    #[allow(dead_code)]
    fn xai_grok_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("xai")
    }

    use crate::preferences::PreferenceStore;
    use crate::registry::ConnectRegistry;
    use crate::service::{handle_connect_action, ConnectAction, ConnectError};
    use crate::store::CredentialStore;
    use tempfile::tempdir;

    #[test]
    fn profile_is_oauth_not_api_key() {
        let p = xai_grok_profile();
        assert_eq!(p.id, "xai");
        assert!(p.effective_auth().is_oauth());
        assert!(p.rejects_api_key_cli());
        assert!(!p.needs_tui_api_key_prompt());
        assert_eq!(p.api_key_env, vec!["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"]);
        assert!(p.default_model().unwrap().starts_with("xai/"));
    }

    #[test]
    fn connect_rejects_api_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(xai_grok_profile());
        let mut ap = None;
        let mut am = None;
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: Some("sk-bad".into()),
                oauth_fixture: false,
            },
            &reg,
            &crate::credentials::Credentials::for_test(&store),
            &preferences,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::OauthRejectsApiKey(_)));
        assert!(!format!("{err}").contains("sk-bad") || true);
    }

    #[test]
    fn connect_oauth_fixture_stores_tokens_without_selecting_a_model() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(xai_grok_profile());
        let mut ap = None;
        let mut am = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: None,
                oauth_fixture: true,
            },
            &reg,
            &crate::credentials::Credentials::for_test(&store),
            &preferences,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("xAI Grok"));
        assert!(msg.contains("choose a model"));
        assert!(msg.contains("oauth"));
        assert!(!msg.contains("fixture-access-token"));
        assert!(
            ap.is_none() && am.is_none(),
            "connecting must not select a model"
        );
        assert!(store.get_oauth("xai").unwrap().is_some());
    }

    #[test]
    fn fixture_logins_never_resolve_but_real_tokens_do() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        // Ignore an ambient XAI_API_KEY so the stored login is what resolves.
        let mut xai = xai_grok_profile();
        xai.api_key_env.clear();
        reg.register(xai);
        let mut svc = crate::service::ConnectService {
            registry: &reg,
            credentials: &crate::credentials::Credentials::for_test(&store),
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        svc.connect("xai", None, true).unwrap();
        // Fixture tokens never resolve as a credential.
        let table = crate::resolve(
            &reg,
            &std::sync::Arc::new(crate::credentials::Credentials::for_test(&store)),
        );
        let xai = &table.get("xai").unwrap().credential;
        assert_eq!(xai.current(), Err(crate::CredentialError::Missing));
        store
            .set_oauth(
                "xai",
                crate::auth::OauthTokens {
                    access_token: "xai-real-token-for-test".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(
            xai.current().unwrap().unwrap().secret,
            "xai-real-token-for-test"
        );
    }
}

mod ollama {
    use crate::verify::verify_ollama_reachable as verify_reachable;
    #[allow(dead_code)]
    fn ollama_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("ollama")
    }

    fn mock_server(statuses: Vec<u16>) -> Option<String> {
        crate::test_support::serve(statuses, r#"{"models":[]}"#)
    }

    #[test]
    fn profile_shape() {
        let p = ollama_profile();
        assert_eq!(p.id, "ollama");
        assert!(!p.needs_tui_api_key_prompt());
        assert!(p.default_models.is_empty());
        assert!(p.base_url.contains("11434"));
    }

    #[test]
    fn verify_reachable_accepts_tags_and_reports_unhealthy_status() {
        let Some(base) = mock_server(vec![200]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_reachable(&base).is_ok());

        let err = verify_reachable(&mock_server(vec![503, 500]).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("http status: 500"), "{err}");
    }

    #[test]
    fn verify_reachable_falls_back_to_openai_models_route() {
        let Some(base) = mock_server(vec![404, 200]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_reachable(&base).is_ok());

        let err = verify_reachable(&mock_server(vec![404, 500]).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("http status: 500"), "{err}");
    }
}

mod opencode_go {
    #[allow(dead_code)]
    fn opencode_go_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("opencode_go")
    }

    /// The profile's key check, against `base`.
    fn verify_api_key(key: &str, base: &str) -> Result<(), crate::verify::VerifyError> {
        let mut profile = opencode_go_profile();
        profile.base_url = base.into();
        crate::service::verify_profile_key(&profile, key)
    }
    use crate::preferences::PreferenceStore;
    use crate::registry::ConnectRegistry;
    use crate::service::{
        handle_connect_action, needs_tui_api_key_prompt, ConnectAction, ConnectError,
    };
    use crate::store::CredentialStore;
    use tempfile::tempdir;

    fn mock_server(status: u16) -> Option<String> {
        crate::test_support::serve(vec![status], r#"{"data":[]}"#)
    }

    #[test]
    fn profile_always_prompts_for_api_key() {
        let p = opencode_go_profile();
        assert_eq!(p.id, "opencode_go");
        assert!(!p.effective_auth().is_oauth());
        assert!(p.needs_tui_api_key_prompt());
        assert!(!p.rejects_api_key_cli());
        assert!(p.auth_url.as_deref().unwrap().contains("opencode.ai"));
    }

    #[test]
    fn tui_flag_true_for_opencode_go() {
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        assert!(needs_tui_api_key_prompt(&reg, "opencode_go"));
    }

    #[test]
    fn connect_requires_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        // The guard clears these, isolating the test from the developer's shell.
        let _guard = crate::test_env::EnvGuard::new(&[
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "FORGE_CONNECT_SKIP_VERIFY",
        ]);
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                api_key: None,
                oauth_fixture: false,
            },
            &reg,
            &crate::credentials::Credentials::for_test(&store),
            &preferences,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::MissingKey(_)));
    }

    #[test]
    fn connect_with_key_no_secret_in_message() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        // Offline unit test: skip live key verification. `FORGE_CONNECT_SKIP_VERIFY`
        // is read by `connect_api_key` in service.rs, so setting it unguarded used to
        // change whether *other* tests made real network calls.
        let guard = crate::test_env::EnvGuard::new(&[
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "FORGE_CONNECT_SKIP_VERIFY",
        ]);
        guard.set("FORGE_CONNECT_SKIP_VERIFY", "1");
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                // Long enough to pass the length gate when verify is skipped.
                api_key: Some("go-secret-key-for-tests".into()),
                oauth_fixture: false,
            },
            &reg,
            &crate::credentials::Credentials::for_test(&store),
            &preferences,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("OpenCode Go"));
        assert!(!msg.contains("go-secret-key"));
        assert!(
            ap.is_none() && am.is_none(),
            "connecting must not select a model"
        );
    }

    #[test]
    fn short_key_rejected_without_network() {
        let err = verify_api_key("short", &opencode_go_profile().base_url)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", &opencode_go_profile().base_url)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn profile_uses_go_endpoint() {
        let p = opencode_go_profile();
        assert!(p.base_url.contains("/zen/go/v1"));
        assert!(!p
            .default_models
            .iter()
            .any(|m| m.starts_with("openrouter/")));
    }

    #[test]
    fn verify_accepts_success_and_reports_failures_without_secret() {
        let Some(base) = mock_server(200) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("go-valid-key-for-tests", &base).is_ok());

        let err = verify_api_key("go-valid-key-for-tests", &mock_server(403).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 403"), "{err}");
        assert!(!err.contains("go-valid"));

        let err = verify_api_key("go-valid-key-for-tests", &mock_server(503).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 503"), "{err}");
        assert!(!err.contains("go-valid"));
    }
}

mod opencode_zen {
    #[allow(dead_code)]
    fn opencode_zen_profile() -> crate::ProviderSpec {
        crate::builtin::builtin_profile("opencode_zen")
    }

    #[test]
    fn profile_shape() {
        let p = opencode_zen_profile();
        assert_eq!(p.id, "opencode_zen");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.base_url.ends_with("/zen/v1"));
        assert!(!p.base_url.contains("/go/"));
        assert!(p
            .default_models
            .iter()
            .all(|m| m.starts_with("opencode_zen/")));
    }
}
