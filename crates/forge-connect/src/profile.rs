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
}

impl ConnectProfile {
    pub fn default_model(&self) -> Option<&str> {
        self.default_models.first().map(|s| s.as_str())
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
