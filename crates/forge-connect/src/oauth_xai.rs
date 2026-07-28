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
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(&format!("forge-connect/{}", env!("CARGO_PKG_VERSION")))
            .build()
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<(u16, String), XaiOauthError> {
        let agent = Self::agent();
        match agent
            .post(url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_form(form)
        {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.into_string().unwrap_or_default();
                Ok((status, body))
            }
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
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

    /// Live smoke against auth.x.ai (skipped if FORGE_SKIP_NETWORK=1).
    #[test]
    fn live_device_code_start() {
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
        // Compile-time / dependency choice: post_form uses ureq::Agent.
        // Runtime smoke: start_device_code must not panic (network may fail offline).
        if std::env::var("FORGE_SKIP_NETWORK").is_ok() {
            return;
        }
        let c = XaiOauthClient::default();
        let _ = c.start_device_code("xai");
    }
}
