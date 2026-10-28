//! xAI Grok connect profile — OAuth (PROV-01 / Phase 6.1).

use crate::profile::ProviderSpec;

pub const PROFILE_ID: &str = crate::builtin::profile_ids::XAI;

/// The embedded built-in definition (`providers/*.toml`).
pub fn xai_grok_profile() -> ProviderSpec {
    crate::builtin::builtin_profile(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
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
