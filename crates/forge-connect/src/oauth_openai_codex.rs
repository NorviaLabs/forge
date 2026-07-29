//! OpenAI Codex device authorization used directly by Forge.

use std::io::Read;

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
    fn response_body(response: ureq::Response) -> String {
        let mut body = String::new();
        let _ = response.into_reader().read_to_string(&mut body);
        body
    }

    fn post_json(
        url: &str,
        body: serde_json::Value,
    ) -> Result<(u16, String), OpenAiCodexOauthError> {
        let request = ureq::post(url).set("Content-Type", "application/json").set(
            "User-Agent",
            &format!("forge/{}", env!("CARGO_PKG_VERSION")),
        );
        match request.send_json(body) {
            Ok(response) => Ok((response.status(), Self::response_body(response))),
            Err(ureq::Error::Status(status, response)) => {
                Ok((status, Self::response_body(response)))
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
            Ok(response) => Ok((response.status(), Self::response_body(response))),
            Err(ureq::Error::Status(status, response)) => {
                Ok((status, Self::response_body(response)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[derive(Deserialize)]
    struct IntervalFixture {
        #[serde(deserialize_with = "number_from_string_or_number")]
        interval: u64,
    }

    fn mock_server(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                let Ok(n) = stream.read(&mut buf) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_len {
                    break;
                }
            }
            let reason = if status < 400 { "OK" } else { "Bad Request" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n{body}",
                body.as_bytes().len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        format!("http://{addr}/token")
    }

    #[test]
    fn post_json_and_form_return_status_and_body_for_success_and_error_status() {
        let (status, body) = OpenAiCodexOauthClient::post_json(
            &mock_server(200, r#"{"ok":true}"#),
            serde_json::json!({"client_id": CLIENT_ID}),
        )
        .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));

        let (status, body) =
            OpenAiCodexOauthClient::post_form(&mock_server(400, "bad request"), &[("a", "b")])
                .unwrap();
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
}
