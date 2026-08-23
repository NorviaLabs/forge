//! Reusing a sign-in another CLI already completed.
//!
//! Forge keeps its own credential store, so a user already signed in to
//! `codex`, `opencode` or `grok` on the same machine was still sent through a
//! second device-code flow in a browser. Nothing about that second grant is
//! more secure — it is the same account, the same OAuth client, on the same
//! machine — it is just friction.
//!
//! This module *finds* those sessions and reports them. It never adopts one on
//! its own: discovery is read-only, and the caller only imports the login the
//! user picked. Secrets are read at import time and never logged — every type
//! here has a hand-written `Debug` that redacts them.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::auth::OauthTokens;

/// The secret behind a discovered login.
#[derive(Clone, PartialEq, Eq)]
pub enum DiscoveredSecret {
    ApiKey(String),
    Oauth(Box<OauthTokens>),
}

impl fmt::Debug for DiscoveredSecret {
    /// Redacted on purpose: these end up in error paths and activity logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(<redacted>)"),
            Self::Oauth(_) => f.write_str("Oauth(<redacted>)"),
        }
    }
}

/// A sign-in another tool has already completed, that Forge could reuse.
#[derive(Clone, PartialEq, Eq)]
pub struct DiscoveredLogin {
    /// Forge profile this login satisfies, e.g. `openai_codex`.
    pub profile_id: String,
    /// Human name of the tool it came from, for the picker: "Codex".
    pub source: String,
    /// Which account, when the source says — an email or account id. Shown so
    /// the user can tell *whose* session they are about to reuse.
    pub account: Option<String>,
    pub path: PathBuf,
    pub secret: DiscoveredSecret,
}

impl fmt::Debug for DiscoveredLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveredLogin")
            .field("profile_id", &self.profile_id)
            .field("source", &self.source)
            .field("account", &self.account)
            .field("path", &self.path)
            .field("secret", &self.secret)
            .finish()
    }
}

impl DiscoveredLogin {
    /// The short right-aligned tag in the provider list: "reuse Codex".
    ///
    /// The account goes in [`Self::offer_label`] instead — the tag column is
    /// a few characters wide, and an elided email there would say less than
    /// nothing.
    pub fn tag_label(&self) -> String {
        format!("reuse {}", self.source)
    }

    /// One line for the picker: "reuse Codex sign-in (me@example.com)".
    pub fn offer_label(&self) -> String {
        match &self.account {
            Some(account) => format!("reuse {} sign-in ({account})", self.source),
            None => format!("reuse {} sign-in", self.source),
        }
    }
}

/// Every reusable sign-in found under `home`, newest source first within a
/// profile so the caller can take the first match per profile.
///
/// Read-only and infallible by design: a missing, unreadable or unparseable
/// file is simply not a discovery. A broken `~/.codex/auth.json` must never
/// stop someone connecting the ordinary way.
pub fn discover_logins(home: &Path) -> Vec<DiscoveredLogin> {
    discover_logins_in(home, None)
}

/// [`discover_logins`], with OpenCode's data directory supplied explicitly.
///
/// The env lookup lives in the caller rather than here so this function is a
/// pure function of its arguments: reading `XDG_DATA_HOME` inside meant that
/// with the var set, discovery ignored `home` and read the real user's file —
/// which would have made the tests below pass only by luck.
pub fn discover_logins_in(home: &Path, data_home: Option<&Path>) -> Vec<DiscoveredLogin> {
    let mut found = Vec::new();
    found.extend(from_codex(home));
    found.extend(from_opencode(home, data_home));
    found.extend(from_grok(home));
    found
}

/// `XDG_DATA_HOME` as a path, when it is set and non-empty.
pub fn xdg_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The discovered login Forge would use for `profile_id`, if any.
pub fn login_for_profile(logins: &[DiscoveredLogin], profile_id: &str) -> Option<DiscoveredLogin> {
    logins
        .iter()
        .find(|login| login.profile_id == profile_id)
        .cloned()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn non_empty(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `~/.codex/auth.json` — the Codex CLI. Forge's OpenAI-Codex profile already
/// uses the Codex CLI's own public OAuth client id, so a refresh token from
/// here stays refreshable once imported.
fn from_codex(home: &Path) -> Vec<DiscoveredLogin> {
    let path = home.join(".codex").join("auth.json");
    let Some(root) = read_json(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let account = non_empty(root.pointer("/tokens/account_id"));

    if let Some(access) = non_empty(root.pointer("/tokens/access_token")) {
        out.push(DiscoveredLogin {
            profile_id: crate::openai_codex::PROFILE_ID.into(),
            source: "Codex".into(),
            account: account.clone(),
            path: path.clone(),
            secret: DiscoveredSecret::Oauth(Box::new(OauthTokens {
                access_token: access,
                refresh_token: non_empty(root.pointer("/tokens/refresh_token")),
                // Codex records `last_refresh`, not an expiry. Leaving this
                // `None` means "use until the API rejects it", which is the
                // honest reading — inventing a lifetime would either refresh
                // needlessly or claim validity we cannot know.
                expires_at: None,
            })),
        });
    }
    if let Some(key) = non_empty(root.get("OPENAI_API_KEY")) {
        out.push(DiscoveredLogin {
            profile_id: crate::openai::PROFILE_ID.into(),
            source: "Codex".into(),
            account,
            path,
            secret: DiscoveredSecret::ApiKey(key),
        });
    }
    out
}

/// `~/.local/share/opencode/auth.json` (or `$XDG_DATA_HOME`) — a map of
/// provider id to either an API key or an OAuth session.
fn from_opencode(home: &Path, data_home: Option<&Path>) -> Vec<DiscoveredLogin> {
    let path = match data_home {
        Some(data) => data.join("opencode").join("auth.json"),
        None => home
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    };
    let Some(root) = read_json(&path) else {
        return Vec::new();
    };
    let Some(map) = root.as_object() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (provider, entry) in map {
        // OpenCode's `openai` entry is a ChatGPT sign-in for Codex-subscription
        // access, which is what Forge's `openai_codex` profile wants — not its
        // API-key `openai` profile.
        let profile_id = match provider.as_str() {
            "openai" => crate::openai_codex::PROFILE_ID,
            "xai" => crate::xai::PROFILE_ID,
            "opencode-go" | "opencode_go" => crate::opencode_go::PROFILE_ID,
            "anthropic" => crate::anthropic::PROFILE_ID,
            // Anything else (openrouter, …) has no Forge profile to satisfy.
            _ => continue,
        };
        let account = non_empty(entry.get("accountId"));
        let secret = match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("api") => match non_empty(entry.get("key")) {
                Some(key) => DiscoveredSecret::ApiKey(key),
                None => continue,
            },
            Some("oauth") => match non_empty(entry.get("access")) {
                Some(access) => DiscoveredSecret::Oauth(Box::new(OauthTokens {
                    access_token: access,
                    refresh_token: non_empty(entry.get("refresh")),
                    // OpenCode stores milliseconds; Forge's parser reads bare
                    // digits as epoch *seconds*, so passing it through unchanged
                    // would put the expiry ~50,000 years out and the token would
                    // never be refreshed.
                    expires_at: entry
                        .get("expires")
                        .and_then(serde_json::Value::as_u64)
                        .map(|ms| (ms / 1000).to_string()),
                })),
                None => continue,
            },
            _ => continue,
        };
        out.push(DiscoveredLogin {
            profile_id: profile_id.into(),
            source: "OpenCode".into(),
            account,
            path: path.clone(),
            secret,
        });
    }
    out.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    out
}

/// `~/.grok/auth.json` — keyed by `<issuer>::<client id>`. Forge's xAI profile
/// defaults to that same client id.
fn from_grok(home: &Path) -> Vec<DiscoveredLogin> {
    let path = home.join(".grok").join("auth.json");
    let Some(root) = read_json(&path) else {
        return Vec::new();
    };
    let Some(map) = root.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in map.values() {
        let Some(access) = non_empty(entry.get("key")) else {
            continue;
        };
        out.push(DiscoveredLogin {
            profile_id: crate::xai::PROFILE_ID.into(),
            source: "Grok".into(),
            // Grok records the signed-in email, which is the clearest possible
            // answer to "whose session am I about to reuse".
            account: non_empty(entry.get("email")),
            path: path.clone(),
            secret: DiscoveredSecret::Oauth(Box::new(OauthTokens {
                access_token: access,
                refresh_token: non_empty(entry.get("refresh_token")),
                expires_at: non_empty(entry.get("expires_at")),
            })),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn a_codex_chatgpt_session_satisfies_the_openai_codex_profile() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".codex/auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"at","refresh_token":"rt","account_id":"acct-1"}}"#,
        );

        let found = discover_logins(home.path());
        let login = login_for_profile(&found, crate::openai_codex::PROFILE_ID).unwrap();
        assert_eq!(login.source, "Codex");
        assert_eq!(login.account.as_deref(), Some("acct-1"));
        match login.secret {
            DiscoveredSecret::Oauth(tokens) => {
                assert_eq!(tokens.access_token, "at");
                assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
            }
            other => panic!("expected oauth, got {other:?}"),
        }
    }

    #[test]
    fn opencode_millisecond_expiries_become_seconds() {
        // Passing milliseconds through unchanged would date the token ~50,000
        // years out, so it would never be refreshed.
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".local/share/opencode/auth.json"),
            r#"{"openai":{"type":"oauth","access":"a","refresh":"r","expires":1788215796359}}"#,
        );

        let found = discover_logins(home.path());
        let login = login_for_profile(&found, crate::openai_codex::PROFILE_ID).unwrap();
        match login.secret {
            DiscoveredSecret::Oauth(tokens) => {
                assert_eq!(tokens.expires_at.as_deref(), Some("1788215796"));
            }
            other => panic!("expected oauth, got {other:?}"),
        }
    }

    #[test]
    fn opencode_api_keys_and_unknown_providers_are_handled() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".local/share/opencode/auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-1"},"openrouter":{"type":"api","key":"sk-2"}}"#,
        );

        let found = discover_logins(home.path());
        assert!(
            login_for_profile(&found, crate::opencode_go::PROFILE_ID).is_some(),
            "opencode-go maps to a Forge profile"
        );
        assert_eq!(
            found.len(),
            1,
            "openrouter has no Forge profile to satisfy: {found:?}"
        );
    }

    #[test]
    fn a_grok_session_carries_the_signed_in_email() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".grok/auth.json"),
            r#"{"https://auth.x.ai::client":{"key":"k","refresh_token":"r","expires_at":"2026-08-20T13:28:01Z","email":"me@example.com"}}"#,
        );

        let found = discover_logins(home.path());
        let login = login_for_profile(&found, crate::xai::PROFILE_ID).unwrap();
        assert_eq!(login.account.as_deref(), Some("me@example.com"));
        assert_eq!(
            login.offer_label(),
            "reuse Grok sign-in (me@example.com)",
            "the picker says whose session it is"
        );
    }

    #[test]
    fn a_broken_file_is_not_a_discovery_and_does_not_panic() {
        // A corrupt store from another tool must never block connecting the
        // ordinary way.
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".codex/auth.json"), "{not json");
        write(&home.path().join(".grok/auth.json"), "[]");
        assert!(discover_logins(home.path()).is_empty());
    }

    #[test]
    fn an_explicit_data_home_is_used_instead_of_the_default_location() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(
            &data.path().join("opencode/auth.json"),
            r#"{"xai":{"type":"api","key":"sk-x"}}"#,
        );

        assert!(
            discover_logins(home.path()).is_empty(),
            "the default location is empty"
        );
        let found = discover_logins_in(home.path(), Some(data.path()));
        assert!(login_for_profile(&found, crate::xai::PROFILE_ID).is_some());
    }

    #[test]
    fn nothing_installed_means_nothing_discovered() {
        let home = tempfile::tempdir().unwrap();
        assert!(discover_logins(home.path()).is_empty());
    }

    #[test]
    fn secrets_never_reach_a_debug_line() {
        // These values travel through activity logs and error paths.
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".codex/auth.json"),
            r#"{"tokens":{"access_token":"SUPERSECRET","refresh_token":"ALSOSECRET"}}"#,
        );
        let rendered = format!("{:?}", discover_logins(home.path()));
        assert!(!rendered.contains("SUPERSECRET"), "{rendered}");
        assert!(!rendered.contains("ALSOSECRET"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn an_empty_token_is_not_a_session() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".codex/auth.json"),
            r#"{"tokens":{"access_token":"   "}}"#,
        );
        assert!(discover_logins(home.path()).is_empty());
    }
}
