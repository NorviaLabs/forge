//! OpenAI Codex device authorization used directly by Forge.

use serde::Deserialize;
use thiserror::Error;

use crate::{OauthPending, OauthTokens};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE: &str = "https://auth.openai.com";
const DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

#[derive(Debug, Error)]
pub enum OpenAiCodexOauthError {
    #[error("HTTP: {0}")]
    Http(String),
    #[error("device authorization failed: {0}")]
    Device(String),
    #[error("authorization pending")]
    Pending,
    #[error("slow down")]
    SlowDown,
    #[error("token exchange failed: {0}")]
    Token(String),
}

pub struct OpenAiCodexOauthClient;

impl OpenAiCodexOauthClient {
    fn post_json(
        url: &str,
        body: serde_json::Value,
    ) -> Result<(u16, String), OpenAiCodexOauthError> {
        let request = ureq::post(url).set("Content-Type", "application/json").set(
            "User-Agent",
            &format!("forge/{}", env!("CARGO_PKG_VERSION")),
        );
        match request.send_json(body) {
            Ok(response) => Ok((
                response.status(),
                response.into_string().unwrap_or_default(),
            )),
            Err(ureq::Error::Status(status, response)) => {
                Ok((status, response.into_string().unwrap_or_default()))
            }
            Err(error) => Err(OpenAiCodexOauthError::Http(error.to_string())),
        }
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), OpenAiCodexOauthError> {
        let request = ureq::post(url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set(
                "User-Agent",
                &format!("forge/{}", env!("CARGO_PKG_VERSION")),
            );
        match request.send_form(form) {
            Ok(response) => Ok((
                response.status(),
                response.into_string().unwrap_or_default(),
            )),
            Err(ureq::Error::Status(status, response)) => {
                Ok((status, response.into_string().unwrap_or_default()))
            }
            Err(error) => Err(OpenAiCodexOauthError::Http(error.to_string())),
        }
    }

    pub fn start_device_code() -> Result<OauthPending, OpenAiCodexOauthError> {
        let (status, body) =
            Self::post_json(DEVICE_CODE_URL, serde_json::json!({"client_id": CLIENT_ID}))?;
        if !(200..300).contains(&status) {
            return Err(OpenAiCodexOauthError::Device(format!(
                "HTTP {status}: {body}"
            )));
        }
        let response: DeviceCodeResponse = serde_json::from_str(&body)
            .map_err(|error| OpenAiCodexOauthError::Device(error.to_string()))?;
        Ok(OauthPending {
            profile_id: crate::openai_codex::PROFILE_ID.into(),
            verification_uri: "https://auth.openai.com/codex/device".into(),
            verification_uri_complete: None,
            user_code: response.user_code,
            device_code: response.device_auth_id,
            auth_server: AUTH_BASE.into(),
            interval_secs: response.interval.max(1),
            expires_in_secs: Some(15 * 60),
            client_id: CLIENT_ID.into(),
        })
    }

    pub fn poll_token_once(pending: &OauthPending) -> Result<OauthTokens, OpenAiCodexOauthError> {
        let (status, body) = Self::post_json(
            DEVICE_TOKEN_URL,
            serde_json::json!({
                "device_auth_id": pending.device_code,
                "user_code": pending.user_code,
            }),
        )?;
        if status == 403 || status == 404 {
            return Err(OpenAiCodexOauthError::Pending);
        }
        if !(200..300).contains(&status) {
            if body.contains("slow_down") {
                return Err(OpenAiCodexOauthError::SlowDown);
            }
            if body.contains("authorization_pending") {
                return Err(OpenAiCodexOauthError::Pending);
            }
            return Err(OpenAiCodexOauthError::Device(format!(
                "HTTP {status}: {body}"
            )));
        }
        let code: DeviceTokenResponse = serde_json::from_str(&body)
            .map_err(|error| OpenAiCodexOauthError::Device(error.to_string()))?;
        Self::exchange_code(&code.authorization_code, &code.code_verifier)
    }

    fn exchange_code(
        authorization_code: &str,
        verifier: &str,
    ) -> Result<OauthTokens, OpenAiCodexOauthError> {
        let (status, body) = Self::post_form(
            TOKEN_URL,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", authorization_code),
                ("code_verifier", verifier),
                ("redirect_uri", DEVICE_REDIRECT_URI),
            ],
        )?;
        Self::parse_tokens(status, &body, None)
    }

    pub fn refresh(refresh_token: &str) -> Result<OauthTokens, OpenAiCodexOauthError> {
        let (status, body) = Self::post_form(
            TOKEN_URL,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CLIENT_ID),
            ],
        )?;
        Self::parse_tokens(status, &body, Some(refresh_token))
    }

    fn parse_tokens(
        status: u16,
        body: &str,
        previous_refresh: Option<&str>,
    ) -> Result<OauthTokens, OpenAiCodexOauthError> {
        if !(200..300).contains(&status) {
            return Err(OpenAiCodexOauthError::Token(format!(
                "HTTP {status}: {body}"
            )));
        }
        let response: TokenResponse = serde_json::from_str(body)
            .map_err(|error| OpenAiCodexOauthError::Token(error.to_string()))?;
        if response.access_token.trim().is_empty() {
            return Err(OpenAiCodexOauthError::Token("empty access token".into()));
        }
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|now| (now.as_secs() + response.expires_in).to_string());
        Ok(OauthTokens {
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .or_else(|| previous_refresh.map(str::to_string)),
            expires_at,
        })
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(deserialize_with = "number_from_string_or_number")]
    interval: u64,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

fn number_from_string_or_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("invalid interval")),
        serde_json::Value::String(text) => text.parse().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom("invalid interval")),
    }
}
