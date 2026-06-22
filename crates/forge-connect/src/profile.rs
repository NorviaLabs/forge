//! Connect profile schema (connect-command.md §3.3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectProfile {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Env vars checked for an existing API key (first present wins).
    pub api_key_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_base_url: Option<String>,
    pub default_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    pub litellm_provider_prefix: String,
}

impl ConnectProfile {
    pub fn default_model(&self) -> Option<&str> {
        self.default_models.first().map(|s| s.as_str())
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
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Provided => "provided",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectStatus {
    pub profile_id: Option<String>,
    pub model: Option<String>,
    pub key_source: Option<KeySource>,
    /// Profiles that currently have a key available (env or file) — ids only.
    pub connected_profile_ids: Vec<String>,
}
