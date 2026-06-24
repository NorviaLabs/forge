//! OpenCode Go connect profile — API key + TUI always prompt (PROV-02 / Phase 6.1).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "opencode_go";

pub fn opencode_go_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenCode Go".into(),
        description: "OpenCode Go coding models — API key required (TUI prompts)".into(),
        auth_mode: AuthMode::opencode_go_api_key(),
        api_key_env: vec!["OPENCODE_API_KEY".into(), "OPENCODE_GO_API_KEY".into()],
        default_base_url: Some("https://opencode.ai/zen/v1".into()),
        default_models: vec![
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
    use crate::service::{
        handle_connect_action, needs_tui_api_key_prompt, ConnectAction, ConnectError,
    };
    use crate::store::CredentialStore;
    use tempfile::tempdir;

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
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                api_key: None,
                oauth_fixture: false,
            },
            &reg,
            &store,
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
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                api_key: Some("go-secret-key".into()),
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("OpenCode Go"));
        assert!(!msg.contains("go-secret-key"));
        assert_eq!(ap.as_deref(), Some("opencode_go"));
    }
}
