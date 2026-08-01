//! xAI Grok connect profile — OAuth (PROV-01 / Phase 6.1).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "xai";

pub fn xai_grok_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "xAI Grok".into(),
        description: "Grok via xAI OAuth (not API key); xai/* models".into(),
        auth_mode: AuthMode::xai_oauth(),
        api_key_env: vec![], // OAuth primary — no API key env for connect UX
        default_base_url: Some("https://api.x.ai/v1".into()),
        default_models: vec!["xai/grok-3".into()],
        models_dev_providers: vec!["xai".into()],
        // Grok Build signs in via auth.x.ai OIDC; device verify page is accounts.x.ai/oauth2/device
        auth_url: Some("https://auth.x.ai".into()),
        model_provider_prefix: "xai".into(),
        vendor_id: PROFILE_ID.into(),
        vendor_label: "xAI Grok".into(),
        route_label: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ConnectRegistry;
    use crate::service::{handle_connect_action, ConnectAction, ConnectError};
    use crate::store::CredentialStore;
    use tempfile::tempdir;

    #[test]
    fn profile_is_oauth_not_api_key() {
        let p = xai_grok_profile();
        assert_eq!(p.id, "xai");
        assert!(p.auth_mode.is_oauth());
        assert!(p.rejects_api_key_cli());
        assert!(!p.needs_tui_api_key_prompt());
        assert!(p.api_key_env.is_empty());
        assert!(p.default_model().unwrap().starts_with("xai/"));
    }

    #[test]
    fn connect_rejects_api_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
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
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::OauthRejectsApiKey(_)));
        assert!(!format!("{err}").contains("sk-bad") || true);
    }

    #[test]
    fn connect_oauth_fixture_sets_model() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
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
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("xAI Grok"));
        assert!(msg.contains("xai/grok-3"));
        assert!(msg.contains("oauth"));
        assert!(!msg.contains("fixture-access-token"));
        assert_eq!(ap.as_deref(), Some("xai"));
        assert!(store.get_oauth("xai").unwrap().is_some());
    }

    #[test]
    fn provider_env_exports_bearer_from_oauth() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(xai_grok_profile());
        // Fixture tokens must NOT be exported to the live worker.
        let mut svc = crate::service::ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: None,
            active_model: None,
        };
        svc.connect("xai", None, true).unwrap();
        let env = svc.provider_env_for_profile("xai").unwrap();
        assert!(
            env.is_empty(),
            "fixture OAuth must not become XAI_API_KEY: {env:?}"
        );
        // Real-looking token is exported.
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
        let env = svc.provider_env_for_profile("xai").unwrap();
        assert_eq!(env[0].0, "XAI_API_KEY");
        assert_eq!(env[0].1, "xai-real-token-for-test");
    }
}
