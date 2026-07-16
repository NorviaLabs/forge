//! OpenAI connect profile — API key (LiteLLM `openai/*`).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "openai";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub fn openai_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenAI".into(),
        description: "OpenAI API key — LiteLLM openai/* models".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        api_key_env: vec!["OPENAI_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        default_models: vec![
            "openai/gpt-4.1-mini".into(),
            "openai/gpt-4.1".into(),
            "openai/gpt-4o".into(),
            "openai/o4-mini".into(),
        ],
        auth_url: Some("https://platform.openai.com/api-keys".into()),
        litellm_provider_prefix: "openai".into(),
    }
}

/// Verify key with `GET /models` (Bearer). Never includes the key in errors.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key is empty".into());
    }
    if key.len() < 16 {
        return Err(format!(
            "API key looks too short ({n} chars). Create a key at https://platform.openai.com/api-keys.",
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
                "OpenAI rejected the API key (HTTP {code}). \
Check the key at https://platform.openai.com/api-keys."
            ),
            other => format!("Could not reach OpenAI to verify key ({other}). Check network."),
        })?;
    let status = resp.status();
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 401 || status == 403 {
        Err("OpenAI rejected the API key (unauthorized). \
Create a key at https://platform.openai.com/api-keys."
            .into())
    } else {
        Err(format!("OpenAI key verification failed (HTTP {status})."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_shape() {
        let p = openai_profile();
        assert_eq!(p.id, "openai");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("openai/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-short", DEFAULT_BASE_URL).unwrap_err();
        assert!(err.contains("too short"), "{err}");
    }
}
