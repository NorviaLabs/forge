//! Anthropic connect profile — API key (LiteLLM `anthropic/*`).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "anthropic";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub fn anthropic_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "Anthropic".into(),
        description: "Anthropic API key — LiteLLM anthropic/* (Claude) models".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        api_key_env: vec!["ANTHROPIC_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        default_models: vec!["anthropic/claude-sonnet-4-20250514".into()],
        auth_url: Some("https://console.anthropic.com/settings/keys".into()),
        litellm_provider_prefix: "anthropic".into(),
    }
}

/// Verify key with a lightweight messages probe (or models if available).
/// Uses Anthropic's `x-api-key` header. Never includes the key in errors.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key is empty".into());
    }
    if key.len() < 16 {
        return Err(format!(
            "API key looks too short ({n} chars). Create a key at \
https://console.anthropic.com/settings/keys.",
            n = key.len()
        ));
    }
    let base = base_url.trim().trim_end_matches('/');
    // Minimal auth check: list models when supported; otherwise POST with invalid body
    // still returns 401 for bad keys vs 400 for good keys.
    let url = format!("{base}/v1/models");
    let resp = ureq::get(&url)
        .set("x-api-key", key)
        .set("anthropic-version", "2023-06-01")
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .call();
    match resp {
        Ok(r) if (200..300).contains(&r.status()) => Ok(()),
        Ok(r) if r.status() == 401 || r.status() == 403 => {
            Err("Anthropic rejected the API key (unauthorized). \
Create a key at https://console.anthropic.com/settings/keys."
                .into())
        }
        Ok(r) if r.status() == 404 => {
            // Older APIs may not expose /v1/models — fall back to a tiny messages call.
            verify_via_messages(key, base)
        }
        Ok(r) => {
            // 400/other with auth accepted is fine for key validity.
            if r.status() < 500 {
                Ok(())
            } else {
                Err(format!(
                    "Anthropic key verification failed (HTTP {}).",
                    r.status()
                ))
            }
        }
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            Err("Anthropic rejected the API key (unauthorized). \
Create a key at https://console.anthropic.com/settings/keys."
                .into())
        }
        Err(ureq::Error::Status(code, _)) if code == 404 => verify_via_messages(key, base),
        Err(ureq::Error::Status(code, _)) if code < 500 => Ok(()),
        Err(other) => Err(format!(
            "Could not reach Anthropic to verify key ({other}). Check network."
        )),
    }
}

fn verify_via_messages(key: &str, base: &str) -> Result<(), String> {
    // Intentionally tiny/invalid payload — we only care about auth status codes.
    let url = format!("{base}/v1/messages");
    let body = r#"{"model":"","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#;
    match ureq::post(&url)
        .set("x-api-key", key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .send_string(body)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
            Err("Anthropic rejected the API key (unauthorized). \
Create a key at https://console.anthropic.com/settings/keys."
                .into())
        }
        Err(ureq::Error::Status(_, _)) => Ok(()), // 400/404/etc. means auth passed
        Err(other) => Err(format!(
            "Could not reach Anthropic to verify key ({other}). Check network."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_shape() {
        let p = anthropic_profile();
        assert_eq!(p.id, "anthropic");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("anthropic/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-ant-short", DEFAULT_BASE_URL).unwrap_err();
        assert!(err.contains("too short"), "{err}");
    }
}
