//! OpenCode Go connect profile — API key + TUI always prompt (PROV-02 / Phase 6.1).
//!
//! OpenCode Go is OpenAI-compatible at `https://opencode.ai/zen/go/v1`.
//! Native routing maps `opencode-go/<id>` to this OpenAI-compatible endpoint.

use crate::auth::AuthMode;
use crate::profile::{
    Attach, AuthKind, AuthPrompt, AuthSpec, CatalogKind, CatalogMode, CatalogSpec, ProviderSpec,
    ProviderTransport, SessionHeaderKind, SpecOrigin, WireKind, WireOverride,
};

pub const PROFILE_ID: &str = "opencode_go";

/// OpenAI-compatible chat/completions base for the Go subscription.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// Env var the worker reads for the Go API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_API_BASE";

pub fn opencode_go_profile() -> ProviderSpec {
    ProviderSpec {
        id: PROFILE_ID.into(),
        title: "OpenCode Go".into(),
        description: "OpenCode Go coding models — API key required (TUI prompts)".into(),
        auth_mode: AuthMode::opencode_go_api_key(),
        api_key_env: vec!["OPENCODE_API_KEY".into(), "OPENCODE_GO_API_KEY".into()],
        base_url: DEFAULT_BASE_URL.into(),
        // Distinctive prefix avoids hijacking real OpenAI (`openai/gpt-*`) routes.
        default_models: vec!["opencode-go/gpt-4.1-mini".into()],
        models_dev_providers: vec!["opencode-go".into()],
        auth_url: Some("https://opencode.ai/auth".into()),
        model_provider_prefix: "opencode-go".into(),
        provider: "opencode".into(),
        provider_label: "OpenCode".into(),
        label: "Go".into(),
        route_id: "opencode-go".into(),
        catalog_mode: CatalogMode::LiveRegistry,
        transport: ProviderTransport::OpenaiCompat,
        auth: Some(AuthSpec {
            kind: AuthKind::ApiKey,
            prompt: AuthPrompt::Tui,
            ..AuthSpec::default()
        }),
        base_url_env: vec![API_BASE_ENV.into(), "OPENCODE_GO_API_BASE".into()],
        wire: Some(WireKind::ChatCompletions),
        wire_overrides: vec![
            WireOverride {
                models: vec![
                    "muse-spark-1.2-contributor*".into(),
                    "muse-spark-1.3-contributor*".into(),
                    "grok-4.6".into(),
                    "gpt-5.6-luna".into(),
                ],
                wire: WireKind::Responses,
            },
            WireOverride {
                models: [
                    "union-alpha",
                    "minimax-m3",
                    "minimax-m2.7",
                    "minimax-m2.5",
                    "qwen3.8-max",
                    "qwen3.8-flash",
                    "qwen3.7-max",
                    "qwen3.7-plus",
                    "qwen3.6-plus",
                ]
                .map(Into::into)
                .to_vec(),
                wire: WireKind::AnthropicMessages,
            },
        ],
        attach: Some(Attach::Bearer),
        session_header: Some(SessionHeaderKind::Opencode),
        catalog: CatalogSpec {
            kind: Some(CatalogKind::OpenaiModels),
            filter: None,
        },
        import_aliases: vec!["opencode-go".into(), "opencode_go".into()],
        billing_url: "https://opencode.ai/".into(),
        guidance: "Get a key from https://opencode.ai/auth (OpenCode Zen → subscribe to Go → copy API key).".into(),
        min_key_len: 16,
        billing_note: "OpenCode does not currently expose wallet balance through its model API.".into(),
        origin: SpecOrigin::Builtin,
        ..ProviderSpec::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(p.auth_mode.is_api_key());
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
        let err = verify_api_key("short", DEFAULT_BASE_URL)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", DEFAULT_BASE_URL)
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
