//! OpenAI Codex subscription profile and token helpers.

use base64::Engine;

use crate::{AuthMode, ConnectProfile};

pub const PROFILE_ID: &str = "openai_codex";
pub const ACCESS_TOKEN_ENV: &str = "FORGE_CODEX_ACCESS_TOKEN";
pub const ACCOUNT_ID_ENV: &str = "FORGE_CODEX_ACCOUNT_ID";
pub const AUTH_SERVER: &str = "https://auth.openai.com";

pub fn openai_codex_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenAI Codex subscription".into(),
        description: "Use a ChatGPT plan through Forge's own device login".into(),
        auth_mode: AuthMode::Oauth {
            device_code: true,
            system_browser: true,
            auth_server: AUTH_SERVER.into(),
        },
        api_key_env: vec![],
        default_base_url: Some("https://chatgpt.com/backend-api".into()),
        default_models: vec!["openai-codex/gpt-5.6-sol".into()],
        auth_url: Some("https://auth.openai.com/codex/device".into()),
        litellm_provider_prefix: "openai-codex".into(),
    }
}

pub(crate) fn account_id_from_token(token: &str) -> Result<String, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "OpenAI returned an invalid access token".to_string())?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "OpenAI returned an invalid access token".to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| "OpenAI returned an invalid access token".to_string())?;
    value
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "OpenAI token does not include a ChatGPT account".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_uses_forge_oauth() {
        let profile = openai_codex_profile();
        assert!(profile.auth_mode.is_oauth());
        assert!(profile.api_key_env.is_empty());
        assert!(profile
            .default_models
            .iter()
            .all(|model| model.starts_with("openai-codex/")));
    }

    #[test]
    fn extracts_chatgpt_account_from_access_token() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        assert_eq!(
            account_id_from_token(&format!("header.{encoded}.signature")).unwrap(),
            "account-123"
        );
    }
}
