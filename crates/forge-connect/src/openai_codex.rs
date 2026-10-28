//! OpenAI Codex subscription profile.

use crate::profile::ProviderSpec;

pub const PROFILE_ID: &str = crate::builtin::profile_ids::OPENAI_CODEX;

/// The embedded built-in definition (`providers/*.toml`).
pub fn openai_codex_profile() -> ProviderSpec {
    crate::builtin::builtin_profile(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn profile_uses_forge_oauth() {
        let profile = openai_codex_profile();
        assert!(profile.effective_auth().is_oauth());
        assert!(profile.api_key_env.is_empty());
        assert!(profile
            .default_models
            .iter()
            .all(|model| model.starts_with("openai_codex/")));
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
            crate::oauth_chatgpt::account_id_from_token(&format!("header.{encoded}.signature"))
                .unwrap(),
            "account-123"
        );
    }
}
