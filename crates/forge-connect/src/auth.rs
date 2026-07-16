//! Auth modes for connect profiles (connect-auth-modes.md, Phase 6.1).
//!
//! xAI follows Grok Build: OIDC at `https://auth.x.ai` (device code and/or browser PKCE).

use serde::{Deserialize, Serialize};

/// How a profile authenticates during `/connect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Browser and/or device-code OAuth; store access/refresh tokens.
    Oauth {
        device_code: bool,
        system_browser: bool,
        /// OIDC issuer, e.g. https://auth.x.ai (Grok Build default)
        auth_server: String,
    },
    /// Operator pastes API key; TUI may always prompt.
    ApiKey {
        /// If true, TUI always opens masked key input when connecting.
        tui_always_prompt: bool,
    },
}

impl AuthMode {
    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth { .. })
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self, Self::ApiKey { .. })
    }

    pub fn tui_always_prompt_key(&self) -> bool {
        match self {
            Self::ApiKey {
                tui_always_prompt, ..
            } => *tui_always_prompt,
            Self::Oauth { .. } => false,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Oauth { .. } => "oauth",
            Self::ApiKey { .. } => "api_key",
        }
    }

    /// Match Grok Build: SpaceXAI OAuth issuer `auth.x.ai`.
    pub fn xai_oauth() -> Self {
        Self::Oauth {
            device_code: true,
            system_browser: true,
            auth_server: crate::oauth_xai::DEFAULT_ISSUER.into(),
        }
    }

    pub fn opencode_go_api_key() -> Self {
        Self::ApiKey {
            tui_always_prompt: true,
        }
    }
}

/// OAuth tokens stored for a profile (never logged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OauthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl OauthTokens {
    /// True when `expires_at` is within `skew` of now or already past.
    /// If `expires_at` is missing, returns false (use token until upstream 401).
    pub fn needs_refresh(&self, skew: std::time::Duration) -> bool {
        let Some(ref exp) = self.expires_at else {
            return false;
        };
        let Some(exp_t) = parse_rfc3339_approx(exp) else {
            return false;
        };
        let now = std::time::SystemTime::now();
        match exp_t.duration_since(now) {
            Ok(remaining) => remaining <= skew,
            Err(_) => true, // already expired
        }
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (the shape we write from OAuth).
fn parse_rfc3339_approx(s: &str) -> Option<std::time::SystemTime> {
    if let Ok(epoch) = s.trim().parse::<u64>() {
        return Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch));
    }
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: u32 = t.next()?.parse().ok()?;
    let mi: u32 = t.next()?.parse().ok()?;
    let se: u32 = t.next()?.parse().ok()?;
    // days since 1970-01-01 via inverse of civil_from_days (approx via chrono-free)
    let days = days_from_civil(y as i32, mo, day)?;
    let secs = days * 86400 + (h as i64) * 3600 + (mi as i64) * 60 + se as i64;
    if secs < 0 {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    // Howard Hinnant days_from_civil
    let m = m as i32;
    let d = d as i32;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5) + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era as i64 * 146097 + doe as i64 - 719468)
}

/// Pending OAuth session shown to the operator (RFC 8628 device-code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthPending {
    pub profile_id: String,
    pub verification_uri: String,
    /// Prefer opening this when present (includes user_code query).
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub device_code: String,
    pub auth_server: String,
    pub interval_secs: u64,
    pub expires_in_secs: Option<u64>,
    pub client_id: String,
}

impl OauthPending {
    /// Local stub for tests / offline fixture (not a real xAI session).
    pub fn start_stub(profile_id: &str, auth_server: &str) -> Self {
        let suffix = profile_id
            .chars()
            .take(3)
            .collect::<String>()
            .to_uppercase();
        Self {
            profile_id: profile_id.into(),
            verification_uri: format!("{auth_server}/oauth2/device"),
            verification_uri_complete: None,
            user_code: format!("FORGE-{suffix}"),
            device_code: format!("device_{profile_id}_{}", uuid_simple()),
            auth_server: auth_server.into(),
            interval_secs: 5,
            expires_in_secs: Some(1800),
            client_id: "stub".into(),
        }
    }

    /// Back-compat alias used by older call sites / tests.
    pub fn start(profile_id: &str, auth_server: &str) -> Self {
        Self::start_stub(profile_id, auth_server)
    }

    pub fn open_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(self.verification_uri.as_str())
    }

    pub fn operator_instructions(&self) -> String {
        if self.profile_id == crate::openai_codex::PROFILE_ID {
            return format!(
                "Sign in with ChatGPT for Codex subscription access:\n\
  1. Open {uri}\n\
  2. Enter code: {code}\n\
  3. Return here — Forge will continue automatically",
                uri = self.open_url(),
                code = self.user_code,
            );
        }
        format!(
            "Sign in with your xAI account (same OAuth as Grok Build):\n\
  1. Open {uri}\n\
  2. Enter code: {code}\n\
  3. Return here — Forge polls until login completes\n\
  (issuer: {issuer})",
            uri = self.open_url(),
            code = self.user_code,
            issuer = self.auth_server,
        )
    }
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{t:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_is_oauth_not_api_key_prompt() {
        let m = AuthMode::xai_oauth();
        assert!(m.is_oauth());
        assert!(!m.tui_always_prompt_key());
        assert_eq!(m.label(), "oauth");
        match &m {
            AuthMode::Oauth { auth_server, .. } => {
                assert!(auth_server.contains("auth.x.ai"));
            }
            _ => panic!("expected oauth"),
        }
    }

    #[test]
    fn opencode_go_always_prompts() {
        let m = AuthMode::opencode_go_api_key();
        assert!(m.is_api_key());
        assert!(m.tui_always_prompt_key());
        assert_eq!(m.label(), "api_key");
    }

    #[test]
    fn oauth_pending_instructions_hide_nothing_secret() {
        let p = OauthPending::start("xai", "https://auth.x.ai");
        let s = p.operator_instructions();
        assert!(s.contains("FORGE-XAI") || s.contains("Open"));
        assert!(s.contains("auth.x.ai"));
    }

    #[test]
    fn needs_refresh_respects_expires_at() {
        let fresh = OauthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some("2099-01-01T00:00:00Z".into()),
        };
        assert!(!fresh.needs_refresh(std::time::Duration::from_secs(300)));
        let expired = OauthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some("2000-01-01T00:00:00Z".into()),
        };
        assert!(expired.needs_refresh(std::time::Duration::from_secs(300)));
        let no_exp = OauthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: None,
        };
        assert!(!no_exp.needs_refresh(std::time::Duration::from_secs(300)));
    }
}
