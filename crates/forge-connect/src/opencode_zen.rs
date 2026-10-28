//! OpenCode Zen connect profile — API key + TUI always prompt.
//!
//! OpenCode Zen is OpenAI-compatible at `https://opencode.ai/zen/v1` (pay-per-use catalog).
//! The same account API key works for Zen and Go; bases differ.
//! Native routing maps `opencode-zen/<id>` to the Zen OpenAI-compatible endpoint.

use crate::auth::AuthMode;
use crate::profile::{
    Attach, AuthKind, AuthPrompt, AuthSpec, CatalogKind, CatalogMode, CatalogSpec, ProviderSpec,
    ProviderTransport, SessionHeaderKind, SpecOrigin, WireKind, WireOverride,
};

pub const PROFILE_ID: &str = "opencode_zen";

/// OpenAI-compatible chat/completions base for Zen (not the Go subscription path).
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/v1";

/// Env var the worker reads for the Zen API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_ZEN_API_BASE";

pub fn opencode_zen_profile() -> ProviderSpec {
    ProviderSpec {
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
        base_url: DEFAULT_BASE_URL.into(),
        // Fallbacks until live catalog refresh; /model pulls full Zen list after connect.
        default_models: vec!["opencode-zen/gpt-4.1-mini".into()],
        models_dev_providers: vec!["opencode".into()],
        auth_url: Some("https://opencode.ai/auth".into()),
        model_provider_prefix: "opencode-zen".into(),
        provider: "opencode".into(),
        provider_label: "OpenCode".into(),
        label: "Zen".into(),
        route_id: "opencode-zen".into(),
        catalog_mode: CatalogMode::LiveRegistry,
        transport: ProviderTransport::OpenaiCompat,
        auth: Some(AuthSpec {
            kind: AuthKind::ApiKey,
            prompt: AuthPrompt::Tui,
            ..AuthSpec::default()
        }),
        base_url_env: vec![API_BASE_ENV.into()],
        credential_from: Some(crate::opencode_go::PROFILE_ID.into()),
        wire: Some(WireKind::ChatCompletions),
        wire_overrides: vec![
            WireOverride {
                models: vec![
                    "muse-spark-1.2-contributor*".into(),
                    "muse-spark-1.3-contributor*".into(),
                ],
                wire: WireKind::Responses,
            },
            WireOverride {
                models: vec!["union-alpha".into()],
                wire: WireKind::AnthropicMessages,
            },
        ],
        attach: Some(Attach::Bearer),
        session_header: Some(SessionHeaderKind::Opencode),
        catalog: CatalogSpec {
            kind: Some(CatalogKind::OpenaiModels),
            filter: None,
        },
        billing_url: "https://opencode.ai/".into(),
        origin: SpecOrigin::Builtin,
        ..ProviderSpec::blank()
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
        assert!(p.base_url.ends_with("/zen/v1"));
        assert!(!p.base_url.contains("/go/"));
        assert!(p
            .default_models
            .iter()
            .all(|m| m.starts_with("opencode-zen/")));
    }
}
