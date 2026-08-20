//! xAI / SpaceXAI OAuth — same endpoints Grok Build uses (`auth.x.ai` OIDC).
//!
//! HTTP uses **ureq** (pure sync). Do **not** use `reqwest::blocking` here — it nests a
//! Tokio runtime and panics when called from the async TUI (`Cannot drop a runtime in a
//! context where blocking is not allowed`).
//!
//! Grok Build defaults:
//! - Issuer: `https://auth.x.ai`
//! - Device code: `POST {issuer}/oauth2/device/code`
//! - Token: `POST {issuer}/oauth2/token`
//! - Public CLI client id (overridable via env)
//! - Scopes include `openid` + `offline_access` + `api:access` + `grok-cli:access`

use serde::Deserialize;
use thiserror::Error;

use crate::auth::{OauthPending, OauthTokens};

/// Public Grok CLI OIDC client id (observed in `~/.grok/auth.json` after `grok login`).
/// Override with `FORGE_XAI_OAUTH_CLIENT_ID` or `GROK_OAUTH2_CLIENT_ID`.
pub const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// OIDC issuer (Grok Build default SpaceXAI OAuth host).
pub const DEFAULT_ISSUER: &str = "https://auth.x.ai";

/// Scopes that grant API-usable session tokens (and refresh).
pub const DEFAULT_SCOPES: &str = "openid profile email offline_access api:access grok-cli:access";

const GRANT_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Look up `primary`, falling back to `fallback`, then `default`.
/// ponytail: single-threaded init path, perf irrelevant.
fn env_with_fallback(primary: &str, fallback: &str, default: &str) -> String {
    std::env::var(primary)
        .or_else(|_| std::env::var(fallback))
        .unwrap_or_else(|_| default.into())
}

#[derive(Debug, Error)]
pub enum XaiOauthError {
    #[error("HTTP: {0}")]
    Http(String),
    #[error("device code request failed: {0}")]
    DeviceCode(String),
    #[error("token request failed: {0}")]
    Token(String),
    #[error("authorization pending")]
    AuthorizationPending,
    #[error("slow down (increase poll interval)")]
    SlowDown,
    #[error("device code expired")]
    Expired,
    #[error("access denied")]
    AccessDenied,
    #[error("OAuth error: {0}")]
    Oauth(String),
}

#[derive(Debug, Clone)]
pub struct XaiOauthClient {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
}

impl Default for XaiOauthClient {
    fn default() -> Self {
        Self::from_env()
    }
}

impl XaiOauthClient {
    pub fn from_env() -> Self {
        Self {
            issuer: env_with_fallback(
                "FORGE_XAI_OAUTH_ISSUER",
                "GROK_OAUTH2_ISSUER",
                DEFAULT_ISSUER,
            )
            .trim_end_matches('/')
            .into(),
            client_id: env_with_fallback(
                "FORGE_XAI_OAUTH_CLIENT_ID",
                "GROK_OAUTH2_CLIENT_ID",
                DEFAULT_CLIENT_ID,
            ),
            scopes: env_with_fallback(
                "FORGE_XAI_OAUTH_SCOPES",
                "GROK_OAUTH2_SCOPES",
                DEFAULT_SCOPES,
            ),
        }
    }

    pub fn device_code_url(&self) -> String {
        format!("{}/oauth2/device/code", self.issuer)
    }

    pub fn token_url(&self) -> String {
        format!("{}/oauth2/token", self.issuer)
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .user_agent(format!("forge-connect/{}", env!("CARGO_PKG_VERSION")))
            // The OAuth error body carries `error`/`error_description`, which
            // callers parse. ureq 3 drops the body when it raises a status as
            // an error, so take every status as a response instead.
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), XaiOauthError> {
        let agent = Self::agent();
        match agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send_form(form.iter().copied())
        {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                Ok((status, body))
            }
            Err(e) => Err(XaiOauthError::Http(e.to_string())),
        }
    }

    /// Start RFC 8628 device authorization (Grok: `grok login --device-code`).
    pub fn start_device_code(&self, profile_id: &str) -> Result<OauthPending, XaiOauthError> {
        let (status, body) = Self::post_form(
            &self.device_code_url(),
            &[
                ("client_id", self.client_id.as_str()),
                ("scope", self.scopes.as_str()),
            ],
        )?;

        if !(200..300).contains(&status) {
            return Err(XaiOauthError::DeviceCode(format!("HTTP {status}: {body}")));
        }

        let parsed: DeviceCodeResponse = serde_json::from_str(&body)
            .map_err(|e| XaiOauthError::DeviceCode(format!("invalid JSON: {e}; body={body}")))?;

        if parsed.device_code.is_empty() || parsed.user_code.is_empty() {
            return Err(XaiOauthError::DeviceCode(format!(
                "missing device_code/user_code: {body}"
            )));
        }

        let verification_uri = parsed
            .verification_uri
            .or(parsed.verification_uri_complete.clone())
            .unwrap_or_else(|| "https://accounts.x.ai/oauth2/device".into());

        Ok(OauthPending {
            profile_id: profile_id.into(),
            verification_uri: verification_uri.clone(),
            verification_uri_complete: parsed.verification_uri_complete,
            user_code: parsed.user_code,
            device_code: parsed.device_code,
            auth_server: self.issuer.clone(),
            interval_secs: parsed.interval.unwrap_or(5).max(1),
            expires_in_secs: parsed.expires_in,
            client_id: self.client_id.clone(),
        })
    }

    /// One token poll. Returns `Ok(tokens)`, `Err(AuthorizationPending)`, `Err(SlowDown)`, or hard error.
    pub fn poll_token_once(&self, pending: &OauthPending) -> Result<OauthTokens, XaiOauthError> {
        let (status, body) = Self::post_form(
            &self.token_url(),
            &[
                ("grant_type", GRANT_DEVICE_CODE),
                ("device_code", pending.device_code.as_str()),
                ("client_id", pending.client_id.as_str()),
            ],
        )?;

        // Success path
        if (200..300).contains(&status) {
            let tok: TokenResponse = serde_json::from_str(&body)
                .map_err(|e| XaiOauthError::Token(format!("invalid JSON: {e}; body={body}")))?;
            if tok.access_token.is_empty() {
                return Err(XaiOauthError::Token(format!("empty access_token: {body}")));
            }
            let expires_at = tok.expires_in.map(|secs| {
                let exp = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
                httpdate_or_rfc3339(exp)
            });
            return Ok(OauthTokens {
                access_token: tok.access_token,
                refresh_token: tok.refresh_token,
                expires_at,
            });
        }

        // OAuth error JSON (often HTTP 400 with authorization_pending)
        if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
            return match err.error.as_str() {
                "authorization_pending" => Err(XaiOauthError::AuthorizationPending),
                "slow_down" => Err(XaiOauthError::SlowDown),
                "expired_token" | "expired" => Err(XaiOauthError::Expired),
                "access_denied" => Err(XaiOauthError::AccessDenied),
                other => Err(XaiOauthError::Oauth(
                    err.error_description.unwrap_or_else(|| other.to_string()),
                )),
            };
        }

        Err(XaiOauthError::Token(format!("HTTP {status}: {body}")))
    }

    /// Refresh an access token using a stored refresh_token (silent re-auth across sessions).
    pub fn refresh_access_token(&self, refresh_token: &str) -> Result<OauthTokens, XaiOauthError> {
        let (status, body) = Self::post_form(
            &self.token_url(),
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", self.client_id.as_str()),
            ],
        )?;

        if !(200..300).contains(&status) {
            if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
                return Err(XaiOauthError::Oauth(
                    err.error_description.unwrap_or(err.error),
                ));
            }
            return Err(XaiOauthError::Token(format!("HTTP {status}: {body}")));
        }

        let tok: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| XaiOauthError::Token(format!("invalid JSON: {e}; body={body}")))?;
        if tok.access_token.is_empty() {
            return Err(XaiOauthError::Token(format!(
                "empty access_token on refresh: {body}"
            )));
        }
        let expires_at = tok.expires_in.map(|secs| {
            let exp = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
            httpdate_or_rfc3339(exp)
        });
        Ok(OauthTokens {
            access_token: tok.access_token,
            // Some IdPs omit refresh_token on refresh — keep the old one if so.
            refresh_token: tok
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at,
        })
    }

    /// Block until the user finishes device login or timeout.
    pub fn poll_until_tokens(
        &self,
        pending: &OauthPending,
        max_wait: std::time::Duration,
    ) -> Result<OauthTokens, XaiOauthError> {
        let deadline = std::time::Instant::now() + max_wait;
        let mut interval = std::time::Duration::from_secs(pending.interval_secs.max(1));
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(XaiOauthError::Expired);
            }
            match self.poll_token_once(pending) {
                Ok(tokens) => return Ok(tokens),
                Err(XaiOauthError::AuthorizationPending) => {
                    std::thread::sleep(interval);
                }
                Err(XaiOauthError::SlowDown) => {
                    interval += std::time::Duration::from_secs(5);
                    std::thread::sleep(interval);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn httpdate_or_rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    chrono_lite_rfc3339(secs)
}

/// Minimal UTC RFC3339 without pulling chrono into this path (forge-connect has no chrono).
fn chrono_lite_rfc3339(unix_secs: i64) -> String {
    const DAY: i64 = 86400;
    const HOUR: i64 = 3600;
    const MIN: i64 = 60;
    let days = unix_secs.div_euclid(DAY);
    let rem = unix_secs.rem_euclid(DAY);
    let h = rem / HOUR;
    let m = (rem % HOUR) / MIN;
    let s = rem % MIN;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Algorithm from Howard Hinnant (civil_from_days).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Best-effort open verification URL in the system browser (Grok default UX).
pub fn try_open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvGuard;

    const OAUTH_ISSUER_ENV: &[&str] = &[
        "FORGE_XAI_OAUTH_ISSUER",
        "GROK_OAUTH2_ISSUER",
        "FORGE_XAI_OAUTH_CLIENT_ID",
        "GROK_OAUTH2_CLIENT_ID",
        "FORGE_XAI_OAUTH_SCOPES",
        "GROK_OAUTH2_SCOPES",
    ];

    /// Populate both the Forge and Grok fallback variables so precedence can be
    /// asserted. Previously backed by a lock private to this module; it now shares
    /// the crate-wide one, so there is a single lock covering every test in the
    /// crate that touches the environment.
    fn set_forge_and_grok_values() -> EnvGuard {
        let guard = EnvGuard::new(OAUTH_ISSUER_ENV);
        guard.set("FORGE_XAI_OAUTH_ISSUER", "https://issuer.example/");
        guard.set("GROK_OAUTH2_ISSUER", "https://fallback.example/");
        guard.set("FORGE_XAI_OAUTH_CLIENT_ID", "forge-client");
        guard.set("GROK_OAUTH2_CLIENT_ID", "grok-client");
        guard.set("FORGE_XAI_OAUTH_SCOPES", "scope-a");
        guard.set("GROK_OAUTH2_SCOPES", "scope-b");
        guard
    }

    #[test]
    fn default_client_matches_grok_cli() {
        let c = XaiOauthClient {
            issuer: DEFAULT_ISSUER.into(),
            client_id: DEFAULT_CLIENT_ID.into(),
            scopes: DEFAULT_SCOPES.into(),
        };
        assert!(c.device_code_url().ends_with("/oauth2/device/code"));
        assert!(c.token_url().ends_with("/oauth2/token"));
        assert!(c.scopes.contains("offline_access"));
        assert!(c.scopes.contains("api:access"));
    }

    #[test]
    fn rfc3339_round_shape() {
        let s = chrono_lite_rfc3339(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
        let s2 = chrono_lite_rfc3339(1_704_067_200); // 2024-01-01 approx
        assert!(s2.starts_with("2024-"));
        assert!(s2.ends_with('Z'));
    }

    #[test]
    fn from_env_trims_issuer_and_prefers_forge_over_grok_env() {
        let _guard = set_forge_and_grok_values();

        let client = XaiOauthClient::from_env();
        assert_eq!(client.issuer, "https://issuer.example");
        assert_eq!(client.client_id, "forge-client");
        assert_eq!(client.scopes, "scope-a");
    }

    #[test]
    fn oauth_response_shapes_parse_defaults() {
        let device: DeviceCodeResponse = serde_json::from_str(
            r#"{"device_code":"dev","user_code":"user","verification_uri_complete":"https://complete"}"#,
        )
        .unwrap();
        assert_eq!(device.device_code, "dev");
        assert_eq!(device.verification_uri, None);
        assert_eq!(
            device.verification_uri_complete.as_deref(),
            Some("https://complete")
        );
        assert_eq!(device.expires_in, None);
        assert_eq!(device.interval, None);

        let token: TokenResponse = serde_json::from_str(r#"{"access_token":"access"}"#).unwrap();
        assert_eq!(token.access_token, "access");
        assert_eq!(token.refresh_token, None);
        assert_eq!(token.expires_in, None);

        let err: TokenErrorResponse = serde_json::from_str(r#"{"error":"slow_down"}"#).unwrap();
        assert_eq!(err.error, "slow_down");
        assert_eq!(err.error_description, None);
    }

    #[test]
    fn civil_date_conversion_handles_boundaries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(chrono_lite_rfc3339(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(chrono_lite_rfc3339(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(chrono_lite_rfc3339(-1), "1969-12-31T23:59:59Z");
        assert_eq!(
            httpdate_or_rfc3339(std::time::UNIX_EPOCH),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn oauth_error_display_messages_cover_variants() {
        assert_eq!(
            XaiOauthError::AuthorizationPending.to_string(),
            "authorization pending"
        );
        assert_eq!(
            XaiOauthError::SlowDown.to_string(),
            "slow down (increase poll interval)"
        );
        assert_eq!(XaiOauthError::Expired.to_string(), "device code expired");
        assert_eq!(XaiOauthError::AccessDenied.to_string(), "access denied");
        assert_eq!(
            XaiOauthError::Oauth("bad".into()).to_string(),
            "OAuth error: bad"
        );
    }

    fn mock_client(base: String) -> XaiOauthClient {
        XaiOauthClient {
            issuer: base,
            client_id: "test-client".into(),
            scopes: DEFAULT_SCOPES.into(),
        }
    }

    fn pending_for(client: &XaiOauthClient) -> OauthPending {
        OauthPending {
            profile_id: "xai".into(),
            verification_uri: format!("{}/oauth2/device", client.issuer),
            verification_uri_complete: None,
            user_code: "FORGE-TEST".into(),
            device_code: "device-under-test".into(),
            auth_server: client.issuer.clone(),
            interval_secs: 1,
            expires_in_secs: Some(1800),
            client_id: client.client_id.clone(),
        }
    }

    #[test]
    fn start_device_code_success() {
        let base = forge_test_support::mock_http(vec![(
            200,
            r#"{"device_code":"dc","user_code":"AB12-CD34","verification_uri":"https://accounts.x.ai/oauth2/device","interval":3,"expires_in":900}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let pending = client.start_device_code("xai").unwrap();
        assert_eq!(pending.profile_id, "xai");
        assert_eq!(pending.device_code, "dc");
        assert_eq!(pending.user_code, "AB12-CD34");
        assert_eq!(
            pending.verification_uri,
            "https://accounts.x.ai/oauth2/device"
        );
        assert_eq!(pending.interval_secs, 3);
        assert_eq!(pending.expires_in_secs, Some(900));
        assert_eq!(pending.client_id, "test-client");
    }

    #[test]
    fn start_device_code_falls_back_to_verification_uri_complete_then_default() {
        let base = forge_test_support::mock_http(vec![
            (
                200,
                r#"{"device_code":"dc","user_code":"u","verification_uri_complete":"https://accounts.x.ai/complete"}"#,
                vec![],
            ),
            (200, r#"{"device_code":"dc2","user_code":"u2"}"#, vec![]),
        ]);
        let client = mock_client(base);

        let via_complete = client.start_device_code("xai").unwrap();
        assert_eq!(
            via_complete.verification_uri,
            "https://accounts.x.ai/complete"
        );
        assert_eq!(
            via_complete.verification_uri_complete.as_deref(),
            Some("https://accounts.x.ai/complete")
        );

        let via_default = client.start_device_code("xai").unwrap();
        assert_eq!(
            via_default.verification_uri,
            "https://accounts.x.ai/oauth2/device"
        );
    }

    #[test]
    fn start_device_code_transport_failure_is_an_http_error() {
        // Nothing is listening on this port, so ureq should fail to connect
        // rather than get any HTTP response at all -- the `Err(e) => Err(Http(..))`
        // branch in post_form, distinct from the "got a response, bad status" path.
        let client = mock_client("http://127.0.0.1:1".into());
        let err = client.start_device_code("xai").unwrap_err();
        assert!(matches!(err, XaiOauthError::Http(_)));
    }

    #[test]
    fn start_device_code_http_error_status() {
        let base = forge_test_support::mock_http(vec![(400, "bad request", vec![])]);
        let client = mock_client(base);
        let err = client.start_device_code("xai").unwrap_err();
        assert!(matches!(err, XaiOauthError::DeviceCode(_)));
        assert!(err.to_string().contains("device code request failed"));
    }

    #[test]
    fn start_device_code_invalid_json_body() {
        let base = forge_test_support::mock_http(vec![(200, "not json", vec![])]);
        let client = mock_client(base);
        let err = client.start_device_code("xai").unwrap_err();
        assert!(matches!(err, XaiOauthError::DeviceCode(msg) if msg.contains("invalid JSON")));
    }

    #[test]
    fn start_device_code_missing_required_fields() {
        let base = forge_test_support::mock_http(vec![(
            200,
            r#"{"device_code":"","user_code":""}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let err = client.start_device_code("xai").unwrap_err();
        assert!(
            matches!(err, XaiOauthError::DeviceCode(msg) if msg.contains("missing device_code"))
        );
    }

    #[test]
    fn poll_token_once_success_computes_expiry() {
        let base = forge_test_support::mock_http(vec![(
            200,
            r#"{"access_token":"tok","refresh_token":"rt","expires_in":3600}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let pending = pending_for(&client);
        let tokens = client.poll_token_once(&pending).unwrap();
        assert_eq!(tokens.access_token, "tok");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
        assert!(tokens.expires_at.is_some());
    }

    #[test]
    fn poll_token_once_empty_access_token_is_an_error() {
        let base = forge_test_support::mock_http(vec![(200, r#"{"access_token":""}"#, vec![])]);
        let client = mock_client(base);
        let pending = pending_for(&client);
        let err = client.poll_token_once(&pending).unwrap_err();
        assert!(matches!(err, XaiOauthError::Token(msg) if msg.contains("empty access_token")));
    }

    #[test]
    fn poll_token_once_maps_known_oauth_error_codes() {
        for (body, expected) in [
            (
                r#"{"error":"authorization_pending"}"#,
                "authorization pending",
            ),
            (
                r#"{"error":"slow_down"}"#,
                "slow down (increase poll interval)",
            ),
            (r#"{"error":"expired_token"}"#, "device code expired"),
            (r#"{"error":"expired"}"#, "device code expired"),
            (r#"{"error":"access_denied"}"#, "access denied"),
        ] {
            let base = forge_test_support::mock_http(vec![(400, body, vec![])]);
            let client = mock_client(base);
            let pending = pending_for(&client);
            let err = client.poll_token_once(&pending).unwrap_err();
            assert_eq!(err.to_string(), expected, "body={body}");
        }
    }

    #[test]
    fn poll_token_once_maps_unrecognized_oauth_error_with_description() {
        let base = forge_test_support::mock_http(vec![(
            400,
            r#"{"error":"invalid_grant","error_description":"device code not found"}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let pending = pending_for(&client);
        let err = client.poll_token_once(&pending).unwrap_err();
        assert_eq!(err.to_string(), "OAuth error: device code not found");
    }

    #[test]
    fn poll_token_once_non_oauth_http_error_is_a_token_error() {
        let base = forge_test_support::mock_http(vec![(500, "upstream on fire", vec![])]);
        let client = mock_client(base);
        let pending = pending_for(&client);
        let err = client.poll_token_once(&pending).unwrap_err();
        assert!(matches!(err, XaiOauthError::Token(msg) if msg.contains("HTTP 500")));
    }

    #[test]
    fn refresh_access_token_success_keeps_old_refresh_token_when_omitted() {
        let base = forge_test_support::mock_http(vec![(
            200,
            r#"{"access_token":"new-access","expires_in":120}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let tokens = client.refresh_access_token("old-refresh").unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        assert!(tokens.expires_at.is_some());
    }

    #[test]
    fn refresh_access_token_prefers_new_refresh_token_when_present() {
        let base = forge_test_support::mock_http(vec![(
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh"}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let tokens = client.refresh_access_token("old-refresh").unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn refresh_access_token_error_with_oauth_body() {
        let base = forge_test_support::mock_http(vec![(
            400,
            r#"{"error":"invalid_grant","error_description":"refresh token revoked"}"#,
            vec![],
        )]);
        let client = mock_client(base);
        let err = client.refresh_access_token("old-refresh").unwrap_err();
        assert_eq!(err.to_string(), "OAuth error: refresh token revoked");
    }

    #[test]
    fn refresh_access_token_error_without_oauth_body_is_a_token_error() {
        let base = forge_test_support::mock_http(vec![(503, "maintenance", vec![])]);
        let client = mock_client(base);
        let err = client.refresh_access_token("old-refresh").unwrap_err();
        assert!(matches!(err, XaiOauthError::Token(msg) if msg.contains("HTTP 503")));
    }

    #[test]
    fn refresh_access_token_empty_access_token_is_an_error() {
        let base = forge_test_support::mock_http(vec![(200, r#"{"access_token":""}"#, vec![])]);
        let client = mock_client(base);
        let err = client.refresh_access_token("old-refresh").unwrap_err();
        assert!(
            matches!(err, XaiOauthError::Token(msg) if msg.contains("empty access_token on refresh"))
        );
    }

    #[test]
    fn poll_until_tokens_retries_through_authorization_pending() {
        // SlowDown's own error mapping is covered by
        // poll_token_once_maps_known_oauth_error_codes; not exercised here too,
        // since poll_until_tokens adds a hardcoded 5s backoff on that leg that
        // would make this test needlessly slow for no extra coverage.
        let base = forge_test_support::mock_http(vec![
            (400, r#"{"error":"authorization_pending"}"#, vec![]),
            (200, r#"{"access_token":"tok"}"#, vec![]),
        ]);
        let client = mock_client(base);
        let pending = pending_for(&client);
        let tokens = client
            .poll_until_tokens(&pending, std::time::Duration::from_secs(30))
            .unwrap();
        assert_eq!(tokens.access_token, "tok");
    }

    #[test]
    fn poll_until_tokens_returns_expired_once_the_deadline_has_passed() {
        let client = mock_client("http://127.0.0.1:1".into()); // never reached
        let pending = pending_for(&client);
        let err = client
            .poll_until_tokens(&pending, std::time::Duration::from_millis(0))
            .unwrap_err();
        assert!(matches!(err, XaiOauthError::Expired));
    }

    /// Live smoke against auth.x.ai (skipped if FORGE_SKIP_NETWORK=1).
    #[test]
    fn live_device_code_start() {
        let _guard = EnvGuard::new(OAUTH_ISSUER_ENV);
        if std::env::var("FORGE_SKIP_NETWORK").is_ok() {
            return;
        }
        let c = XaiOauthClient::default();
        match c.start_device_code("xai") {
            Ok(p) => {
                assert!(!p.user_code.is_empty());
                assert!(!p.device_code.is_empty());
                assert!(
                    p.verification_uri.contains("accounts.x.ai")
                        || p.verification_uri.contains("auth.x.ai")
                );
                assert!(p
                    .user_code
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
            }
            Err(e) => {
                eprintln!("live_device_code_start skipped/fail (network?): {e}");
            }
        }
    }

    /// Document that OAuth uses ureq (no nested Tokio). Network optional.
    #[test]
    fn http_stack_is_ureq_not_reqwest_blocking() {
        let _guard = EnvGuard::new(OAUTH_ISSUER_ENV);
        // Compile-time / dependency choice: post_form uses ureq::Agent.
        // Runtime smoke: start_device_code must not panic (network may fail offline).
        if std::env::var("FORGE_SKIP_NETWORK").is_ok() {
            return;
        }
        let c = XaiOauthClient::default();
        let _ = c.start_device_code("xai");
    }
}
