//! Connect profile schema (connect-command.md §3.3 + 6.1 auth_mode).

use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectProfile {
    pub id: String,
    pub title: String,
    pub description: String,
    pub auth_mode: AuthMode,
    /// Env vars for ApiKey mode (first present wins). Empty for pure OAuth profiles.
    pub api_key_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_base_url: Option<String>,
    pub default_models: Vec<String>,
    /// models.dev provider ids used for public model metadata and fallbacks.
    ///
    /// A Forge transport may have a distinct id: the ChatGPT Codex subscription
    /// transport, for example, uses the public OpenAI model registry.
    #[serde(default)]
    pub models_dev_providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    pub model_provider_prefix: String,
    /// Groups profiles that are really one provider with multiple offerings
    /// (e.g. `openai` API key vs. ChatGPT sign-in). Equal to `id` for
    /// providers with only one offering.
    #[serde(default)]
    pub vendor_id: String,
    /// Vendor-level display name for the collapsed picker row, e.g. "OpenAI",
    /// "OpenCode" — the same value repeated across every profile sharing a
    /// `vendor_id`.
    #[serde(default)]
    pub vendor_label: String,
    /// This profile's offering label under its vendor, e.g. "API key",
    /// "ChatGPT sign-in", "Go", "Zen". Unused (empty) for vendors with only
    /// one offering, since those never render a nested route row.
    #[serde(default)]
    pub route_label: String,
}

/// Stable public route identity for a credential/profile id, e.g.
/// `openai_codex` → `openai-chatgpt`. Credential/profile IDs remain an
/// implementation detail while the picker migrates to offering identity.
pub fn route_id_for_profile_id(profile_id: &str) -> &str {
    match profile_id {
        "openai" => "openai-api",
        "openai_codex" => "openai-chatgpt",
        "anthropic" => "anthropic-api",
        "xai" => "xai-api",
        "opencode_go" => "opencode-go",
        "opencode_zen" => "opencode-zen",
        "ollama" => "ollama",
        other => other,
    }
}

impl ConnectProfile {
    /// Stable public route identity. Credential/profile IDs remain an
    /// implementation detail while the picker migrates to offering identity.
    pub fn route_id(&self) -> &str {
        route_id_for_profile_id(&self.id)
    }

    pub fn default_model(&self) -> Option<&str> {
        self.default_models.first().map(|s| s.as_str())
    }

    /// True when this vendor has more than one offering registered alongside
    /// it — the only case where the picker nests routes under a chevron.
    pub fn has_multiple_routes(&self, registry: &crate::registry::ConnectRegistry) -> bool {
        registry
            .profiles()
            .iter()
            .filter(|p| p.vendor_id == self.vendor_id)
            .count()
            > 1
    }

    pub fn needs_tui_api_key_prompt(&self) -> bool {
        self.auth_mode.tui_always_prompt_key()
    }

    pub fn rejects_api_key_cli(&self) -> bool {
        self.auth_mode.is_oauth()
    }
}

/// Result of a successful connect (never includes secret material).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOutcome {
    pub profile_id: String,
    pub model: String,
    pub key_source: KeySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    File,
    /// Provided just now (session) — treated as file after persist.
    Provided,
    /// OAuth access token stored.
    Oauth,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Provided => "provided",
            Self::Oauth => "oauth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectStatus {
    pub profile_id: Option<String>,
    pub model: Option<String>,
    pub key_source: Option<KeySource>,
    pub connected_profile_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(auth_mode: AuthMode, default_models: Vec<String>) -> ConnectProfile {
        ConnectProfile {
            id: "demo".into(),
            title: "Demo".into(),
            description: "Demo profile".into(),
            auth_mode,
            api_key_env: vec!["DEMO_KEY".into()],
            default_base_url: Some("https://example.test".into()),
            default_models,
            models_dev_providers: vec![],
            auth_url: None,
            model_provider_prefix: "demo".into(),
            vendor_id: "demo".into(),
            vendor_label: "Demo".into(),
            route_label: String::new(),
        }
    }

    #[test]
    fn profile_helpers_reflect_auth_mode_and_default_model() {
        let api = profile(
            AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            vec!["demo/model-a".into(), "demo/model-b".into()],
        );
        assert_eq!(api.default_model(), Some("demo/model-a"));
        assert!(api.needs_tui_api_key_prompt());
        assert!(!api.rejects_api_key_cli());

        let oauth = profile(
            AuthMode::Oauth {
                device_code: true,
                system_browser: false,
                auth_server: "https://issuer.example".into(),
            },
            vec![],
        );
        assert_eq!(oauth.default_model(), None);
        assert!(!oauth.needs_tui_api_key_prompt());
        assert!(oauth.rejects_api_key_cli());
    }

    #[test]
    fn key_source_labels_are_stable() {
        assert_eq!(KeySource::Env.as_str(), "env");
        assert_eq!(KeySource::File.as_str(), "file");
        assert_eq!(KeySource::Provided.as_str(), "provided");
        assert_eq!(KeySource::Oauth.as_str(), "oauth");
    }

    #[test]
    fn route_id_exposes_offering_identity() {
        let mut profile = profile(
            AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            vec![],
        );
        profile.id = "openai_codex".into();
        assert_eq!(profile.route_id(), "openai-chatgpt");
        profile.id = "openai".into();
        assert_eq!(profile.route_id(), "openai-api");
    }
}
