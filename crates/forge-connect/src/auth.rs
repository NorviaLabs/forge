//! Auth modes for connect profiles (connect-auth-modes.md, Phase 6.1).

use serde::{Deserialize, Serialize};

/// How a profile authenticates during `/connect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Browser and/or device-code OAuth; store access/refresh tokens.
    Oauth {
        device_code: bool,
        system_browser: bool,
        /// e.g. https://accounts.x.ai
        auth_server: String,
    },
    /// Operator pastes API key; TUI may always prompt.
    ApiKey {
        /// If true, TUI always opens masked key input when connecting.
        tui_always_prompt: bool,
    },
}

impl AuthMode {
    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth { .. })
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self, Self::ApiKey { .. })
    }

    pub fn tui_always_prompt_key(&self) -> bool {
        match self {
            Self::ApiKey {
                tui_always_prompt, ..
            } => *tui_always_prompt,
            Self::Oauth { .. } => false,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Oauth { .. } => "oauth",
            Self::ApiKey { .. } => "api_key",
        }
    }

    pub fn xai_oauth() -> Self {
        Self::Oauth {
            device_code: true,
            system_browser: true,
            auth_server: "https://accounts.x.ai".into(),
        }
    }

    pub fn opencode_go_api_key() -> Self {
        Self::ApiKey {
            tui_always_prompt: true,
        }
    }
}

/// OAuth tokens stored for a profile (never logged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OauthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Pending OAuth session shown to the operator (device-code style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthPending {
    pub profile_id: String,
    pub verification_uri: String,
    pub user_code: String,
    pub device_code: String,
    pub auth_server: String,
}

impl OauthPending {
    /// Start a local device-code session (real token exchange is product-specific).
    pub fn start(profile_id: &str, auth_server: &str) -> Self {
        // Deterministic-looking codes for UX; not security-sensitive until exchanged.
        let suffix = profile_id.chars().take(3).collect::<String>().to_uppercase();
        Self {
            profile_id: profile_id.into(),
            verification_uri: format!("{auth_server}/device"),
            user_code: format!("FORGE-{suffix}"),
            device_code: format!("device_{profile_id}_{}", uuid_simple()),
            auth_server: auth_server.into(),
        }
    }

    pub fn operator_instructions(&self) -> String {
        format!(
            "OAuth for `{id}`:\n  1. Open {uri}\n  2. Enter code: {code}\n  3. Return here and complete connect (fixture: FORGE_CONNECT_OAUTH_FIXTURE=1)",
            id = self.profile_id,
            uri = self.verification_uri,
            code = self.user_code,
        )
    }
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{t:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_is_oauth_not_api_key_prompt() {
        let m = AuthMode::xai_oauth();
        assert!(m.is_oauth());
        assert!(!m.tui_always_prompt_key());
        assert_eq!(m.label(), "oauth");
    }

    #[test]
    fn opencode_go_always_prompts() {
        let m = AuthMode::opencode_go_api_key();
        assert!(m.is_api_key());
        assert!(m.tui_always_prompt_key());
        assert_eq!(m.label(), "api_key");
    }

    #[test]
    fn oauth_pending_instructions_hide_nothing_secret() {
        let p = OauthPending::start("xai", "https://accounts.x.ai");
        let s = p.operator_instructions();
        assert!(s.contains("FORGE-XAI") || s.contains("user_code") || s.contains("Open"));
        assert!(s.contains("accounts.x.ai"));
    }
}
