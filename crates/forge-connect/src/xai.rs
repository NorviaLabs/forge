//! xAI Grok connect profile (PROV-01 / provider-xai-grok.md).

use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "xai";

pub fn xai_grok_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "xAI Grok".into(),
        description: "Grok models via xAI API (LiteLLM xai/* model strings)".into(),
        api_key_env: vec!["XAI_API_KEY".into()],
        default_base_url: None,
        default_models: vec![
            "xai/grok-3".into(),
            "xai/grok-3-mini".into(),
            "xai/grok-2".into(),
        ],
        auth_url: Some("https://console.x.ai/".into()),
        litellm_provider_prefix: "xai".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ConnectRegistry;
    use crate::service::{handle_connect_action, ConnectAction};
    use crate::store::CredentialStore;
    use tempfile::tempdir;

    #[test]
    fn profile_fields() {
        let p = xai_grok_profile();
        assert_eq!(p.id, "xai");
        assert_eq!(p.api_key_env, vec!["XAI_API_KEY"]);
        assert!(p.default_model().unwrap().starts_with("xai/"));
        assert!(p.auth_url.is_some());
    }

    #[test]
    fn connect_xai_with_key_sets_model() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(xai_grok_profile());
        let mut active_profile = None;
        let mut active_model = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: Some("test-xai-key".into()),
            },
            &reg,
            &store,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(msg.contains("xAI Grok"));
        assert!(msg.contains("xai/grok-3"));
        assert!(!msg.contains("test-xai-key"));
        assert_eq!(active_profile.as_deref(), Some("xai"));
        assert_eq!(active_model.as_deref(), Some("xai/grok-3"));
        assert_eq!(
            store.get_api_key("xai").unwrap().as_deref(),
            Some("test-xai-key")
        );
    }

    #[test]
    fn worker_env_exports_xai_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "k").unwrap();
        let mut reg = ConnectRegistry::new();
        reg.register(xai_grok_profile());
        let svc = crate::service::ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: Some("xai".into()),
            active_model: Some("xai/grok-3".into()),
        };
        let env = svc.worker_env_for_profile("xai").unwrap();
        assert_eq!(env, vec![("XAI_API_KEY".into(), "k".into())]);
    }
}
