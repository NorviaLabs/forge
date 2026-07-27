//! OpenCode Zen connect profile — API key + TUI always prompt.
//!
//! OpenCode Zen is OpenAI-compatible at `https://opencode.ai/zen/v1` (pay-per-use catalog).
//! The same account API key works for Zen and Go; bases differ.
//! Native routing maps `opencode-zen/<id>` to the Zen OpenAI-compatible endpoint.

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "opencode_zen";

/// OpenAI-compatible chat/completions base for Zen (not the Go subscription path).
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/v1";

/// Env var the worker reads for the Zen API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_ZEN_API_BASE";

pub fn opencode_zen_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenCode Zen".into(),
        description: "OpenCode Zen model catalog — API key required (TUI prompts)".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        // Same key family as Go; Zen-specific env allowed first if set.
        api_key_env: vec![
            "OPENCODE_ZEN_API_KEY".into(),
            "OPENCODE_API_KEY".into(),
            "OPENCODE_GO_API_KEY".into(),
        ],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        // Fallbacks until live catalog refresh; /model pulls full Zen list after connect.
        default_models: vec!["opencode-zen/gpt-4.1-mini".into()],
        models_dev_providers: vec![],
        auth_url: Some("https://opencode.ai/auth".into()),
        model_provider_prefix: "opencode-zen".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_shape() {
        let p = opencode_zen_profile();
        assert_eq!(p.id, "opencode_zen");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_base_url.as_deref().unwrap().ends_with("/zen/v1"));
        assert!(!p.default_base_url.as_deref().unwrap().contains("/go/"));
        assert!(p
            .default_models
            .iter()
            .all(|m| m.starts_with("opencode-zen/")));
    }
}
