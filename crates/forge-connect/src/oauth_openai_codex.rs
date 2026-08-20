//! OpenAI Codex device authorization used directly by Forge.

use serde::Deserialize;
use thiserror::Error;

use crate::{OauthPending, OauthTokens};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_AUTH_BASE: &str = "https://auth.openai.com";
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

#[derive(Debug, Clone)]
pub struct OpenAiCodexOauthClient {
    pub auth_base: String,
}

impl Default for OpenAiCodexOauthClient {
    fn default() -> Self {
        Self::from_env()
    }
}

impl OpenAiCodexOauthClient {
    pub fn from_env() -> Self {
        Self {
            auth_base: std::env::var("FORGE_OPENAI_CODEX_OAUTH_ISSUER")
                .or_else(|_| std::env::var("OPENAI_CODEX_OAUTH_ISSUER"))
                .unwrap_or_else(|_| DEFAULT_AUTH_BASE.into())
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

    fn post_json(
        url: &str,
        body: serde_json::Value,
    ) -> Result<(u16, String), OpenAiCodexOauthError> {
        let request = ureq::post(url)
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                &format!("forge/{}", env!("CARGO_PKG_VERSION")),
            )
            .config()
            // The OAuth error body carries `error`/`error_description`, which
            // callers parse. ureq 3 drops the body when it raises a status as
            // an error, so take every status as a response instead.
            .http_status_as_error(false)
            .timeout_per_call(Some(std::time::Duration::from_secs(30)))
            .build();
        match request.send_json(body) {
            Ok(response) => Ok((response.status().as_u16(), Self::response_body(response))),
            Err(error) => Err(OpenAiCodexOauthError::Http(error.to_string())),
        }
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), OpenAiCodexOauthError> {
        let request = ureq::post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header(
                "User-Agent",
                &format!("forge/{}", env!("CARGO_PKG_VERSION")),
            )
            .config()
            .http_status_as_error(false)
            .timeout_per_call(Some(std::time::Duration::from_secs(30)))
            .build();
        match request.send_form(form.iter().copied()) {
            Ok(response) => Ok((response.status().as_u16(), Self::response_body(response))),
            Err(error) => Err(OpenAiCodexOauthError::Http(error.to_string())),
        }
    }

    pub fn start_device_code(&self) -> Result<OauthPending, OpenAiCodexOauthError> {
        let (status, body) = Self::post_json(
            &self.device_code_url(),
            serde_json::json!({"client_id": CLIENT_ID}),
        )?;
        if !(200..300).contains(&status) {
            return Err(OpenAiCodexOauthError::Device(format!(
                "HTTP {status}: {body}"
            )));
        }
        let response: DeviceCodeResponse = serde_json::from_str(&body)
            .map_err(|error| OpenAiCodexOauthError::Device(error.to_string()))?;
        Ok(OauthPending {
            profile_id: crate::openai_codex::PROFILE_ID.into(),
            verification_uri: format!("{}/codex/device", self.auth_base),
            verification_uri_complete: None,
            user_code: response.user_code,
            device_code: response.device_auth_id,
            auth_server: self.auth_base.clone(),
            interval_secs: response.interval.max(1),
            expires_in_secs: Some(15 * 60),
            client_id: CLIENT_ID.into(),
        })
    }

    pub fn poll_token_once(
        &self,
        pending: &OauthPending,
    ) -> Result<OauthTokens, OpenAiCodexOauthError> {
        let (status, body) = Self::post_json(
            &self.device_token_url(),
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
        self.exchange_code(&code.authorization_code, &code.code_verifier)
    }

    fn exchange_code(
        &self,
        authorization_code: &str,
        verifier: &str,
    ) -> Result<OauthTokens, OpenAiCodexOauthError> {
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

    pub fn refresh(&self, refresh_token: &str) -> Result<OauthTokens, OpenAiCodexOauthError> {
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

    fn mock_client(base: String) -> OpenAiCodexOauthClient {
        OpenAiCodexOauthClient { auth_base: base }
    }

    fn pending_for(client: &OpenAiCodexOauthClient) -> OauthPending {
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
        }
    }

    #[test]
    fn post_json_and_form_return_status_and_body_for_success_and_error_status() {
        let base = mock_http(vec![(200, r#"{"ok":true}"#, vec![])]);
        let (status, body) =
            OpenAiCodexOauthClient::post_json(&base, serde_json::json!({"client_id": CLIENT_ID}))
                .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));

        let base = mock_http(vec![(400, "bad request", vec![])]);
        let (status, body) = OpenAiCodexOauthClient::post_form(&base, &[("a", "b")]).unwrap();
        assert_eq!(status, 400);
        assert_eq!(body, "bad request");
    }

    #[test]
    fn post_helpers_report_transport_errors() {
        let err =
            OpenAiCodexOauthClient::post_json("http://127.0.0.1:9/token", serde_json::json!({}))
                .unwrap_err();
        assert!(matches!(err, OpenAiCodexOauthError::Http(_)));

        let err = OpenAiCodexOauthClient::post_form("http://127.0.0.1:9/token", &[("a", "b")])
            .unwrap_err();
        assert!(matches!(err, OpenAiCodexOauthError::Http(_)));
    }

    #[test]
    fn parse_tokens_accepts_success_and_preserves_previous_refresh() {
        let tokens = OpenAiCodexOauthClient::parse_tokens(
            200,
            r#"{"access_token":"access","expires_in":3600}"#,
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        assert!(tokens.expires_at.is_some());

        let tokens = OpenAiCodexOauthClient::parse_tokens(
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
            OpenAiCodexOauthClient::parse_tokens(400, "bad", None),
            Err(OpenAiCodexOauthError::Token(message)) if message.contains("HTTP 400")
        ));
        assert!(matches!(
            OpenAiCodexOauthClient::parse_tokens(200, "not-json", None),
            Err(OpenAiCodexOauthError::Token(message)) if message.contains("expected")
        ));
        assert!(matches!(
            OpenAiCodexOauthClient::parse_tokens(
                200,
                r#"{"access_token":"   ","expires_in":1}"#,
                None,
            ),
            Err(OpenAiCodexOauthError::Token(message)) if message == "empty access token"
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
            OpenAiCodexOauthError::Pending.to_string(),
            "authorization pending"
        );
        assert_eq!(OpenAiCodexOauthError::SlowDown.to_string(), "slow down");
        assert!(OpenAiCodexOauthError::Device("x".into())
            .to_string()
            .contains("device authorization failed"));
    }

    #[test]
    fn start_device_code_success() {
        let base = mock_http(vec![(
            200,
            r#"{"device_auth_id":"device-1","user_code":"AB12-CD34","interval":3}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let pending = client.start_device_code().unwrap();
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
        let base = mock_http(vec![
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
        ]);
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
            let base = mock_http(vec![(status, body, vec![])]);
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
        let base = mock_http(vec![(
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":120}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let tokens = client.refresh("old-refresh").unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));

        let base = mock_http(vec![(503, "maintenance", vec![])]);
        let client = mock_client(base);
        let err = client.refresh("old-refresh").unwrap_err();
        assert!(matches!(err, OpenAiCodexOauthError::Token(msg) if msg.contains("HTTP 503")));
    }
}
