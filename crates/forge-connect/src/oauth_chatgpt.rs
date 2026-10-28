//! The `chatgpt` OAuth flavor: ChatGPT device login, the account id carried
//! in its tokens, and the plan usage probe.

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{OauthPending, OauthTokens, OPENAI_CODEX_PROFILE_ID};

/// Base-URL env alias for the ChatGPT backend.
pub const API_BASE_ENV: &str = "FORGE_CODEX_API_BASE";
/// The ChatGPT backend the `responses` wire and catalog use.
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_AUTH_BASE: &str = "https://auth.openai.com";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

#[derive(Debug, Error)]
pub enum ChatgptOauthError {
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

#[derive(Debug, Clone)]
pub struct ChatgptOauthClient {
    pub auth_base: String,
}

impl Default for ChatgptOauthClient {
    fn default() -> Self {
        Self {
            auth_base: DEFAULT_AUTH_BASE.into(),
        }
    }
}

impl ChatgptOauthClient {
    /// The flavor owns its endpoints; a profile's `auth.issuer` replaces the
    /// auth host (tests point it at a mock).
    pub fn for_profile(profile: &crate::ProviderSpec) -> Self {
        let auth = profile.effective_auth();
        Self {
            auth_base: auth
                .issuer
                .unwrap_or_else(|| DEFAULT_AUTH_BASE.into())
                .trim_end_matches('/')
                .into(),
        }
    }

    fn device_code_url(&self) -> String {
        format!("{}/api/accounts/deviceauth/usercode", self.auth_base)
    }

    fn device_token_url(&self) -> String {
        format!("{}/api/accounts/deviceauth/token", self.auth_base)
    }

    fn token_url(&self) -> String {
        format!("{}/oauth/token", self.auth_base)
    }

    fn response_body(mut response: ureq::http::Response<ureq::Body>) -> String {
        response.body_mut().read_to_string().unwrap_or_default()
    }

    fn post_json(url: &str, body: serde_json::Value) -> Result<(u16, String), ChatgptOauthError> {
        let request = ureq::post(url)
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                &format!("forge/{}", env!("CARGO_PKG_VERSION")),
            )
            .config()
            // Never follow redirects: a hop could forward the credential.
            .max_redirects(0)
            // The OAuth error body carries `error`/`error_description`, which
            // callers parse. ureq 3 drops the body when it raises a status as
            // an error, so take every status as a response instead.
            .http_status_as_error(false)
            .timeout_per_call(Some(std::time::Duration::from_secs(30)))
            .build();
        match request.send_json(body) {
            Ok(response) => Ok((response.status().as_u16(), Self::response_body(response))),
            Err(error) => Err(ChatgptOauthError::Http(error.to_string())),
        }
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), ChatgptOauthError> {
        let request = ureq::post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header(
                "User-Agent",
                &format!("forge/{}", env!("CARGO_PKG_VERSION")),
            )
            .config()
            // Never follow redirects: a hop could forward the credential.
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_per_call(Some(std::time::Duration::from_secs(30)))
            .build();
        match request.send_form(form.iter().copied()) {
            Ok(response) => Ok((response.status().as_u16(), Self::response_body(response))),
            Err(error) => Err(ChatgptOauthError::Http(error.to_string())),
        }
    }

    pub fn start_device_code(&self, profile_id: &str) -> Result<OauthPending, ChatgptOauthError> {
        let (status, body) = Self::post_json(
            &self.device_code_url(),
            serde_json::json!({"client_id": CLIENT_ID}),
        )?;
        if !(200..300).contains(&status) {
            return Err(ChatgptOauthError::Device(format!("HTTP {status}: {body}")));
        }
        let response: DeviceCodeResponse = serde_json::from_str(&body)
            .map_err(|error| ChatgptOauthError::Device(error.to_string()))?;
        Ok(OauthPending {
            profile_id: profile_id.into(),
            verification_uri: format!("{}/codex/device", self.auth_base),
            verification_uri_complete: None,
            user_code: response.user_code,
            device_code: response.device_auth_id,
            auth_server: self.auth_base.clone(),
            interval_secs: response.interval.max(1),
            expires_in_secs: Some(15 * 60),
            client_id: CLIENT_ID.into(),
            flavor: crate::OauthFlavor::Chatgpt,
        })
    }

    pub fn poll_token_once(
        &self,
        pending: &OauthPending,
    ) -> Result<OauthTokens, ChatgptOauthError> {
        let (status, body) = Self::post_json(
            &self.device_token_url(),
            serde_json::json!({
                "device_auth_id": pending.device_code,
                "user_code": pending.user_code,
            }),
        )?;
        if status == 403 || status == 404 {
            return Err(ChatgptOauthError::Pending);
        }
        if !(200..300).contains(&status) {
            if body.contains("slow_down") {
                return Err(ChatgptOauthError::SlowDown);
            }
            if body.contains("authorization_pending") {
                return Err(ChatgptOauthError::Pending);
            }
            return Err(ChatgptOauthError::Device(format!("HTTP {status}: {body}")));
        }
        let code: DeviceTokenResponse = serde_json::from_str(&body)
            .map_err(|error| ChatgptOauthError::Device(error.to_string()))?;
        self.exchange_code(&code.authorization_code, &code.code_verifier)
    }

    fn exchange_code(
        &self,
        authorization_code: &str,
        verifier: &str,
    ) -> Result<OauthTokens, ChatgptOauthError> {
        let (status, body) = Self::post_form(
            &self.token_url(),
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

    pub fn refresh(&self, refresh_token: &str) -> Result<OauthTokens, ChatgptOauthError> {
        let (status, body) = Self::post_form(
            &self.token_url(),
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
    ) -> Result<OauthTokens, ChatgptOauthError> {
        if !(200..300).contains(&status) {
            return Err(ChatgptOauthError::Token(format!("HTTP {status}: {body}")));
        }
        let response: TokenResponse = serde_json::from_str(body)
            .map_err(|error| ChatgptOauthError::Token(error.to_string()))?;
        if response.access_token.trim().is_empty() {
            return Err(ChatgptOauthError::Token("empty access token".into()));
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

/// The ChatGPT account id in an access token's payload.
pub(crate) fn account_id_from_token(token: &str) -> Result<String, crate::verify::VerifyError> {
    use crate::verify::VerifyError;
    const INVALID: &str = "OpenAI returned an invalid access token";
    let payload = token
        .split('.')
        .nth(1)
        .ok_or(VerifyError::MalformedToken(INVALID))?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| VerifyError::MalformedToken(INVALID))?;
    let value: serde_json::Value =
        serde_json::from_slice(&decoded).map_err(|_| VerifyError::MalformedToken(INVALID))?;
    value
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .ok_or(VerifyError::MalformedToken(
            "OpenAI token does not include a ChatGPT account",
        ))
}

// ChatGPT plan usage (moved from cost.rs; the flavor owns its usage endpoint).

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

fn codex_usage_url() -> String {
    std::env::var(API_BASE_ENV)
        .map(|base| format!("{}/wham/usage", base.trim_end_matches('/')))
        .unwrap_or_else(|_| CODEX_USAGE_URL.to_string())
}

/// The ChatGPT plan usage probe: rate-limit windows and credits for the
/// stored login.
pub(crate) fn usage_report(store: &crate::CredentialStore) -> Result<Vec<String>, String> {
    let tokens = store
        .get_oauth(OPENAI_CODEX_PROFILE_ID)
        .map_err(|error| format!("could not read Codex credentials: {error}"))?;
    let access_token = tokens
        .as_ref()
        .map(|tokens| tokens.access_token.trim())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Codex is not connected; use /connect openai_codex".to_string())?;
    let account_id = account_id_from_token(&access_token)
        .map_err(|_| "Codex credentials do not include a ChatGPT account".to_string())?;

    let mut response = ureq::get(&codex_usage_url())
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("ChatGPT-Account-Id", &account_id)
        .header(
            "User-Agent",
            &format!("forge/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        .timeout_per_call(Some(std::time::Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|error| format!("could not fetch Codex limits: {error}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("could not read Codex limits: {error}"))?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("could not parse Codex limits: {error}"))?;
    Ok(format_codex_usage(&value))
}

fn format_codex_usage(value: &Value) -> Vec<String> {
    let plan = value
        .get("plan_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![format!("Provider: OpenAI Codex ({plan} plan)")];
    let rate_limit = value.get("rate_limit").unwrap_or(&Value::Null);
    if let Some(window) = rate_limit.get("primary_window") {
        lines.push(format_window("Session limit", window));
    }
    if let Some(window) = rate_limit.get("secondary_window") {
        lines.push(format_window("Weekly limit", window));
    }
    if rate_limit
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("Status: limit reached".into());
    }
    if let Some(credits) = value.get("credits") {
        if credits
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push("Credits: unlimited".into());
        } else if let Some(balance) = credits.get("balance").and_then(number_text) {
            lines.push(format!("Credit balance: {balance}"));
        }
    }
    if lines.len() == 1 {
        lines.push("Limit details were not included by the provider.".into());
    }
    lines
}

fn format_window(label: &str, window: &Value) -> String {
    let used = window
        .get("used_percent")
        .and_then(number_text)
        .unwrap_or_else(|| "?".into());
    let remaining = window
        .get("used_percent")
        .and_then(Value::as_f64)
        .map(|used| format_number((100.0 - used).clamp(0.0, 100.0)))
        .unwrap_or_else(|| "?".into());
    let reset = window
        .get("reset_after_seconds")
        .and_then(Value::as_u64)
        .map(format_duration)
        .unwrap_or_else(|| "unknown".into());
    format!("{label}: {remaining}% remaining ({used}% used) · resets in {reset}")
}

fn number_text(value: &Value) -> Option<String> {
    value.as_f64().map(format_number).or_else(|| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn format_number(number: f64) -> String {
    if number.fract().abs() < f64::EPSILON {
        format!("{number:.0}")
    } else {
        format!("{number:.1}")
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_test_support::mock_http;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct IntervalFixture {
        #[serde(deserialize_with = "number_from_string_or_number")]
        interval: u64,
    }

    fn mock_client(base: String) -> ChatgptOauthClient {
        ChatgptOauthClient { auth_base: base }
    }

    fn pending_for(client: &ChatgptOauthClient) -> OauthPending {
        OauthPending {
            profile_id: crate::openai_codex::PROFILE_ID.into(),
            verification_uri: format!("{}/codex/device", client.auth_base),
            verification_uri_complete: None,
            user_code: "FORGE-TEST".into(),
            device_code: "device-under-test".into(),
            auth_server: client.auth_base.clone(),
            interval_secs: 1,
            expires_in_secs: Some(900),
            client_id: CLIENT_ID.into(),
            flavor: crate::OauthFlavor::Chatgpt,
        }
    }

    #[test]
    fn post_json_and_form_return_status_and_body_for_success_and_error_status() {
        let Some(base) = mock_http(vec![(200, r#"{"ok":true}"#, vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let (status, body) =
            ChatgptOauthClient::post_json(&base, serde_json::json!({"client_id": CLIENT_ID}))
                .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));

        let Some(base) = mock_http(vec![(400, "bad request", vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let (status, body) = ChatgptOauthClient::post_form(&base, &[("a", "b")]).unwrap();
        assert_eq!(status, 400);
        assert_eq!(body, "bad request");
    }

    #[test]
    fn post_helpers_report_transport_errors() {
        let err = ChatgptOauthClient::post_json("http://127.0.0.1:9/token", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(err, ChatgptOauthError::Http(_)));

        let err =
            ChatgptOauthClient::post_form("http://127.0.0.1:9/token", &[("a", "b")]).unwrap_err();
        assert!(matches!(err, ChatgptOauthError::Http(_)));
    }

    #[test]
    fn parse_tokens_accepts_success_and_preserves_previous_refresh() {
        let tokens = ChatgptOauthClient::parse_tokens(
            200,
            r#"{"access_token":"access","expires_in":3600}"#,
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        assert!(tokens.expires_at.is_some());

        let tokens = ChatgptOauthClient::parse_tokens(
            201,
            r#"{"access_token":"access","refresh_token":"new-refresh","expires_in":1}"#,
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn parse_tokens_rejects_http_json_and_empty_access_token_errors() {
        assert!(matches!(
            ChatgptOauthClient::parse_tokens(400, "bad", None),
            Err(ChatgptOauthError::Token(message)) if message.contains("HTTP 400")
        ));
        assert!(matches!(
            ChatgptOauthClient::parse_tokens(200, "not-json", None),
            Err(ChatgptOauthError::Token(message)) if message.contains("expected")
        ));
        assert!(matches!(
            ChatgptOauthClient::parse_tokens(
                200,
                r#"{"access_token":"   ","expires_in":1}"#,
                None,
            ),
            Err(ChatgptOauthError::Token(message)) if message == "empty access token"
        ));
    }

    #[test]
    fn device_code_response_accepts_numeric_or_string_interval() {
        let numeric: DeviceCodeResponse =
            serde_json::from_str(r#"{"device_auth_id":"device","user_code":"user","interval":2}"#)
                .unwrap();
        assert_eq!(numeric.interval, 2);

        let string: DeviceCodeResponse = serde_json::from_str(
            r#"{"device_auth_id":"device","user_code":"user","interval":"3"}"#,
        )
        .unwrap();
        assert_eq!(string.interval, 3);
    }

    #[test]
    fn interval_deserializer_rejects_invalid_shapes() {
        let fixture: IntervalFixture = serde_json::from_str(r#"{"interval":4}"#).unwrap();
        assert_eq!(fixture.interval, 4);
        assert!(serde_json::from_str::<IntervalFixture>(r#"{"interval":{}}"#).is_err());
        assert!(serde_json::from_str::<IntervalFixture>(r#"{"interval":"nope"}"#).is_err());
        assert!(serde_json::from_str::<IntervalFixture>(r#"{"interval":-1}"#).is_err());
    }

    #[test]
    fn oauth_error_display_messages_are_stable() {
        assert_eq!(
            ChatgptOauthError::Pending.to_string(),
            "authorization pending"
        );
        assert_eq!(ChatgptOauthError::SlowDown.to_string(), "slow down");
        assert!(ChatgptOauthError::Device("x".into())
            .to_string()
            .contains("device authorization failed"));
    }

    #[test]
    fn start_device_code_success() {
        let Some(base) = mock_http(vec![(
            200,
            r#"{"device_auth_id":"device-1","user_code":"AB12-CD34","interval":3}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let client = mock_client(base);
        let pending = client
            .start_device_code(crate::OPENAI_CODEX_PROFILE_ID)
            .unwrap();
        assert_eq!(pending.profile_id, "openai_codex");
        assert_eq!(pending.device_code, "device-1");
        assert_eq!(pending.user_code, "AB12-CD34");
        assert_eq!(
            pending.verification_uri,
            format!("{}/codex/device", client.auth_base)
        );
        assert_eq!(pending.interval_secs, 3);
    }

    #[test]
    fn poll_token_once_success_exchanges_authorization_code() {
        let Some(base) = mock_http(vec![
            (
                200,
                r#"{"authorization_code":"auth-code","code_verifier":"verifier"}"#,
                vec![],
            ),
            (
                200,
                r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#,
                vec![],
            ),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let client = mock_client(base);
        let pending = pending_for(&client);
        let tokens = client.poll_token_once(&pending).unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
    }

    #[test]
    fn poll_token_once_maps_pending_slowdown_and_http_errors() {
        for (status, body, expected) in [
            (403, "", "authorization pending"),
            (404, "", "authorization pending"),
            (400, r#"{"error":"slow_down"}"#, "slow down"),
            (
                400,
                r#"{"error":"authorization_pending"}"#,
                "authorization pending",
            ),
            (500, "upstream", "device authorization failed"),
        ] {
            let Some(base) = mock_http(vec![(status, body, vec![])]) else {
                eprintln!("skipping: this host denies binding a listener");
                return;
            };
            let client = mock_client(base);
            let err = client.poll_token_once(&pending_for(&client)).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "status={status} body={body} got {err}"
            );
        }
    }

    #[test]
    fn refresh_success_and_error_paths() {
        let Some(base) = mock_http(vec![(
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":120}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let client = mock_client(base);
        let tokens = client.refresh("old-refresh").unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));

        let Some(base) = mock_http(vec![(503, "maintenance", vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let client = mock_client(base);
        let err = client.refresh("old-refresh").unwrap_err();
        assert!(matches!(err, ChatgptOauthError::Token(msg) if msg.contains("HTTP 503")));
    }

    /// A stored ChatGPT login whose token names account `account-123`.
    fn store_codex_login(store: &crate::CredentialStore) {
        store
            .set_oauth(
                crate::OPENAI_CODEX_PROFILE_ID,
                crate::OauthTokens {
                    access_token: "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.signature".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn formats_codex_windows_and_credits() {
        let usage = serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "limit_reached": false,
                "primary_window": { "used_percent": 25, "reset_after_seconds": 7200 },
                "secondary_window": { "used_percent": 40.5, "reset_after_seconds": 432000 }
            },
            "credits": { "balance": 12.5, "unlimited": false }
        });
        let report = format_codex_usage(&usage);
        assert_eq!(report[0], "Provider: OpenAI Codex (plus plan)");
        assert!(report[1].contains("75% remaining"));
        assert!(report[2].contains("59.5% remaining"));
        assert_eq!(report[3], "Credit balance: 12.5");
    }

    #[test]
    fn codex_usage_handles_limit_reached_unlimited_and_missing_details() {
        let usage = serde_json::json!({
            "plan_type": "team",
            "rate_limit": {
                "limit_reached": true,
                "primary_window": { "used_percent": 125.0 },
                "secondary_window": { "used_percent": "41.5", "reset_after_seconds": 59 }
            },
            "credits": { "unlimited": true }
        });
        let report = format_codex_usage(&usage);
        assert!(report.iter().any(|line| line.contains("0% remaining")));
        assert!(report.iter().any(|line| line == "Status: limit reached"));
        assert!(report.iter().any(|line| line == "Credits: unlimited"));

        let sparse = format_codex_usage(&serde_json::json!({}));
        assert_eq!(sparse[0], "Provider: OpenAI Codex (unknown plan)");
        assert!(sparse[1].contains("not included"));
    }

    #[test]
    fn number_and_duration_formatting_handles_edges() {
        assert_eq!(number_text(&serde_json::json!(12.0)).unwrap(), "12");
        assert_eq!(number_text(&serde_json::json!(12.25)).unwrap(), "12.2");
        assert_eq!(number_text(&serde_json::json!("7.5")).unwrap(), "7.5");
        assert!(number_text(&serde_json::json!("")).is_none());

        assert_eq!(format_duration(59), "0m");
        assert_eq!(format_duration(3_660), "1h 1m");
        assert_eq!(format_duration(90_000), "1d 1h");
    }

    #[test]
    fn codex_cost_report_fetches_usage_from_configured_base_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &[API_BASE_ENV];
        let guard = EnvGuard::new(ENV);
        let Some(base) = mock_http(vec![(
            200,
            r#"{"plan_type":"plus","rate_limit":{"limit_reached":false,"primary_window":{"used_percent":10}},"credits":{"balance":5.0}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set(API_BASE_ENV, &base);

        let dir = tempdir().unwrap();
        let store = crate::CredentialStore::new(dir.path().join("c.toml"));
        store_codex_login(&store);
        let report = usage_report(&store).unwrap();
        assert!(report[0].contains("OpenAI Codex (plus plan)"));
        assert!(report.iter().any(|line| line.contains("90% remaining")));
    }

    #[test]
    fn codex_cost_report_requires_connection() {
        use crate::test_env::EnvGuard;

        const ENV: &[&str] = &[API_BASE_ENV];
        let _guard = EnvGuard::new(ENV);
        let dir = tempfile::tempdir().unwrap();
        let store = crate::CredentialStore::new(dir.path().join("c.toml"));
        let err = usage_report(&store).unwrap_err();
        assert!(err.contains("Codex is not connected"));
    }
}
