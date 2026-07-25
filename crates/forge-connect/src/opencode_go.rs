//! OpenCode Go connect profile — API key + TUI always prompt (PROV-02 / Phase 6.1).
//!
//! OpenCode Go is OpenAI-compatible at `https://opencode.ai/zen/go/v1`.
//! Native routing maps `opencode-go/<id>` to this OpenAI-compatible endpoint.

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "opencode_go";

/// OpenAI-compatible chat/completions base for the Go subscription.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// Env var the worker reads for the Go API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_API_BASE";

pub fn opencode_go_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenCode Go".into(),
        description: "OpenCode Go coding models — API key required (TUI prompts)".into(),
        auth_mode: AuthMode::opencode_go_api_key(),
        api_key_env: vec!["OPENCODE_API_KEY".into(), "OPENCODE_GO_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        // Distinctive prefix avoids hijacking real OpenAI (`openai/gpt-*`) routes.
        default_models: vec!["opencode-go/gpt-4.1-mini".into()],
        auth_url: Some("https://opencode.ai/auth".into()),
        model_provider_prefix: "opencode-go".into(),
    }
}

/// Live-check an OpenCode Go API key against `GET {base}/models`.
/// Returns Ok(()) on 200; never includes the key in error messages.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key is empty".into());
    }
    // Real Zen/Go keys are long; reject obvious placeholders without a network call.
    if key.len() < 16 {
        return Err(format!(
            "API key looks too short ({n} chars). Get a key from https://opencode.ai/auth \
(OpenCode Zen → subscribe to Go → copy API key).",
            n = key.len()
        ));
    }
    let base = base_url.trim().trim_end_matches('/');
    let url = format!("{base}/models");
    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!(
                "OpenCode Go rejected the API key (HTTP {code}). \
Sign in at https://opencode.ai/auth, copy a fresh key, and reconnect."
            ),
            other => format!(
                "Could not reach OpenCode Go to verify key ({other}). \
Check network access to {base}."
            ),
        })?;
    let status = resp.status();
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 401 || status == 403 {
        Err("OpenCode Go rejected the API key (unauthorized). \
Get a key from https://opencode.ai/auth and run `forge connect opencode_go --key …`."
            .into())
    } else {
        Err(format!(
            "OpenCode Go key verification failed (HTTP {status}). Try again or check account status."
        ))
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
        // Isolate from real env vars.
        std::env::remove_var("OPENCODE_API_KEY");
        std::env::remove_var("OPENCODE_GO_API_KEY");
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
        // Offline unit test: skip live key verification.
        std::env::set_var("FORGE_CONNECT_SKIP_VERIFY", "1");
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                // Long enough to pass the length gate when verify is skipped.
                api_key: Some("go-secret-key-for-tests".into()),
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap();
        std::env::remove_var("FORGE_CONNECT_SKIP_VERIFY");
        assert!(msg.contains("OpenCode Go"));
        assert!(!msg.contains("go-secret-key"));
        assert_eq!(ap.as_deref(), Some("opencode_go"));
        assert!(am.as_deref().unwrap().starts_with("opencode-go/"));
    }

    #[test]
    fn short_key_rejected_without_network() {
        let err = verify_api_key("short", DEFAULT_BASE_URL).unwrap_err();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn profile_uses_go_endpoint() {
        let p = opencode_go_profile();
        assert!(p
            .default_base_url
            .as_deref()
            .unwrap()
            .contains("/zen/go/v1"));
        assert!(!p
            .default_models
            .iter()
            .any(|m| m.starts_with("openrouter/")));
    }
}
