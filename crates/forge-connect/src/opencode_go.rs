//! OpenCode Go connect profile (PROV-02 / provider-opencode-go.md).

use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "opencode_go";

pub fn opencode_go_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenCode Go".into(),
        description:
            "OpenCode Go low-cost coding models (OpenAI-compatible; LiteLLM-routed)"
                .into(),
        api_key_env: vec![
            "OPENCODE_API_KEY".into(),
            "OPENCODE_GO_API_KEY".into(),
        ],
        // OpenAI-compatible base used by OpenCode Go at implement time; LiteLLM can use api_base.
        default_base_url: Some("https://opencode.ai/zen/v1".into()),
        default_models: vec![
            // Recommended placeholders; live catalog may override in a later release.
            "openrouter/opencode/glm-4.6".into(),
            "openrouter/opencode/minimax-m2.1".into(),
        ],
        auth_url: Some("https://opencode.ai/auth".into()),
        litellm_provider_prefix: "opencode".into(),
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
        let p = opencode_go_profile();
        assert_eq!(p.id, "opencode_go");
        assert!(p.api_key_env.contains(&"OPENCODE_API_KEY".into()));
        assert!(p.auth_url.as_deref().unwrap().contains("opencode.ai"));
        assert!(p.default_base_url.is_some());
        assert!(!p.default_models.is_empty());
    }

    #[test]
    fn connect_opencode_go_with_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut active_profile = None;
        let mut active_model = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                api_key: Some("go-secret".into()),
            },
            &reg,
            &store,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(msg.contains("OpenCode Go"));
        assert!(!msg.contains("go-secret"));
        assert_eq!(active_profile.as_deref(), Some("opencode_go"));
        assert!(active_model.as_ref().unwrap().contains('/'));
    }

    #[test]
    fn list_includes_opencode_go() {
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        assert!(reg.get("opencode_go").is_some());
        assert!(reg.get("OPENCODE_GO").is_some());
    }
}
