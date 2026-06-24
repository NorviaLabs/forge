//! Connect service: list / status / connect / disconnect (CONN-01 + 6.1 auth modes).

use thiserror::Error;

use crate::auth::{AuthMode, OauthPending, OauthTokens};
use crate::profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
use crate::registry::ConnectRegistry;
use crate::store::{resolve_connected, resolve_key, CredentialStore, StoreError};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConnectError {
    #[error("unknown profile `{0}` (known: {1})")]
    UnknownProfile(String, String),
    #[error("api key required for profile `{0}`")]
    MissingKey(String),
    #[error(
        "profile `{0}` uses OAuth — do not pass an API key; run `/connect {0}` and complete browser/device login"
    )]
    OauthRejectsApiKey(String),
    #[error("OAuth required for `{0}`: {1}")]
    OauthPending(String, String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    Open,
    List,
    Status,
    Connect {
        profile_id: String,
        /// API key (ApiKey profiles only). Forbidden for OAuth profiles.
        api_key: Option<String>,
        /// When true, complete OAuth with fixture tokens (tests / FORGE_CONNECT_OAUTH_FIXTURE).
        oauth_fixture: bool,
    },
    Disconnect { profile_id: Option<String> },
}

/// Parse `/connect …` args (without the leading `/connect`).
pub fn parse_connect_args(args: &str) -> Result<ConnectAction, ConnectError> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(ConnectAction::Open);
    }
    let mut parts = args.split_whitespace();
    let first = parts.next().unwrap_or("").to_ascii_lowercase();
    match first.as_str() {
        "list" => Ok(ConnectAction::List),
        "status" => Ok(ConnectAction::Status),
        "disconnect" => Ok(ConnectAction::Disconnect {
            profile_id: parts.next().map(|s| s.to_string()),
        }),
        other => {
            let key = parts.next().map(|s| s.to_string());
            Ok(ConnectAction::Connect {
                profile_id: other.to_string(),
                api_key: key,
                oauth_fixture: false,
            })
        }
    }
}

pub struct ConnectService<'a> {
    pub registry: &'a ConnectRegistry,
    pub store: &'a CredentialStore,
    pub active_profile_id: Option<String>,
    pub active_model: Option<String>,
}

impl<'a> ConnectService<'a> {
    pub fn list_lines(&self) -> Result<Vec<String>, ConnectError> {
        let mut lines = Vec::new();
        for p in self.registry.profiles() {
            let connected =
                resolve_connected(&p.api_key_env, &p.id, self.store)?.is_some();
            let active = self
                .active_profile_id
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(&p.id));
            let badge = match (connected, active) {
                (true, true) => "[connected, active]",
                (true, false) => "[connected]",
                (false, true) => "[active?]",
                (false, false) => "[ ]",
            };
            lines.push(format!(
                "{badge} {id} — {title} ({mode})",
                id = p.id,
                title = p.title,
                mode = p.auth_mode.label()
            ));
        }
        if lines.is_empty() {
            lines.push("(no connect profiles registered)".into());
        }
        Ok(lines)
    }

    pub fn status(&self) -> Result<ConnectStatus, ConnectError> {
        let mut connected = Vec::new();
        for p in self.registry.profiles() {
            if resolve_connected(&p.api_key_env, &p.id, self.store)?.is_some() {
                connected.push(p.id.clone());
            }
        }
        let key_source = if let Some(ref id) = self.active_profile_id {
            if let Some(p) = self.registry.get(id) {
                resolve_connected(&p.api_key_env, &p.id, self.store)?
            } else {
                None
            }
        } else {
            None
        };
        Ok(ConnectStatus {
            profile_id: self.active_profile_id.clone(),
            model: self.active_model.clone(),
            key_source,
            connected_profile_ids: connected,
        })
    }

    pub fn status_message(&self) -> Result<String, ConnectError> {
        let s = self.status()?;
        Ok(format!(
            "active_profile={} model={} key_source={} connected=[{}]",
            s.profile_id.as_deref().unwrap_or("-"),
            s.model.as_deref().unwrap_or("-"),
            s.key_source.map(|k| k.as_str()).unwrap_or("-"),
            s.connected_profile_ids.join(", ")
        ))
    }

    /// Connect with API key (ApiKey profiles only).
    pub fn connect_api_key(
        &mut self,
        profile_id: &str,
        api_key: Option<&str>,
    ) -> Result<ConnectOutcome, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        if profile.auth_mode.is_oauth() {
            return Err(ConnectError::OauthRejectsApiKey(profile.id));
        }

        let key_source = if let Some(k) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
            self.store.set_api_key(&profile.id, k)?;
            KeySource::Provided
        } else if let Some((_, src)) =
            resolve_key(&profile.api_key_env, &profile.id, self.store)?
        {
            src
        } else {
            return Err(ConnectError::MissingKey(profile.id.clone()));
        };

        self.activate(&profile, key_source)
    }

    /// Complete OAuth for a profile (tokens provided by fixture or real exchange).
    pub fn connect_oauth(
        &mut self,
        profile_id: &str,
        tokens: OauthTokens,
    ) -> Result<ConnectOutcome, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        if !profile.auth_mode.is_oauth() {
            return Err(ConnectError::Message(format!(
                "profile `{}` is not OAuth",
                profile.id
            )));
        }
        if tokens.access_token.trim().is_empty() {
            return Err(ConnectError::Message("OAuth access_token empty".into()));
        }
        self.store.set_oauth(&profile.id, tokens)?;
        self.activate(&profile, KeySource::Oauth)
    }

    /// Start OAuth pending session (device-code UX).
    pub fn start_oauth(&self, profile_id: &str) -> Result<OauthPending, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        let AuthMode::Oauth { auth_server, .. } = &profile.auth_mode else {
            return Err(ConnectError::Message(format!(
                "profile `{}` is not OAuth",
                profile.id
            )));
        };
        Ok(OauthPending::start(&profile.id, auth_server))
    }

    /// Connect dispatch used by CLI/TUI after collecting secrets.
    pub fn connect(
        &mut self,
        profile_id: &str,
        api_key: Option<&str>,
        oauth_fixture: bool,
    ) -> Result<ConnectOutcome, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        match &profile.auth_mode {
            AuthMode::Oauth { .. } => {
                if api_key.map(str::trim).filter(|s| !s.is_empty()).is_some() {
                    return Err(ConnectError::OauthRejectsApiKey(profile.id));
                }
                // Already have tokens?
                if let Some(tokens) = self.store.get_oauth(&profile.id)? {
                    return self.activate(&profile, KeySource::Oauth).map(|mut o| {
                        let _ = tokens;
                        o.key_source = KeySource::Oauth;
                        o
                    });
                }
                if oauth_fixture || std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok() {
                    let tokens = OauthTokens {
                        access_token: "fixture-access-token".into(),
                        refresh_token: Some("fixture-refresh".into()),
                        expires_at: None,
                    };
                    return self.connect_oauth(&profile.id, tokens);
                }
                // Optional: power-user inject after browser login
                if let Ok(at) = std::env::var("FORGE_XAI_OAUTH_ACCESS_TOKEN") {
                    if !at.trim().is_empty() && profile.id == "xai" {
                        return self.connect_oauth(
                            &profile.id,
                            OauthTokens {
                                access_token: at,
                                refresh_token: std::env::var("FORGE_XAI_OAUTH_REFRESH_TOKEN").ok(),
                                expires_at: None,
                            },
                        );
                    }
                }
                let pending = self.start_oauth(&profile.id)?;
                Err(ConnectError::OauthPending(
                    profile.id,
                    pending.operator_instructions(),
                ))
            }
            AuthMode::ApiKey { .. } => self.connect_api_key(&profile.id, api_key),
        }
    }

    pub fn disconnect(&self, profile_id: Option<&str>) -> Result<String, ConnectError> {
        let id = profile_id
            .map(|s| s.to_string())
            .or_else(|| self.active_profile_id.clone())
            .ok_or_else(|| ConnectError::Message("no profile to disconnect".into()))?;
        if self.registry.get(&id).is_none() {
            return Err(ConnectError::UnknownProfile(
                id,
                self.registry.ids().join(", "),
            ));
        }
        let removed = self.store.clear(&id)?;
        if removed {
            Ok(format!(
                "cleared stored credentials for `{id}` (env unchanged if set)"
            ))
        } else {
            Ok(format!("no stored credentials for `{id}`"))
        }
    }

    pub fn profile(&self, id: &str) -> Option<&ConnectProfile> {
        self.registry.get(id)
    }

    pub fn worker_env_for_profile(
        &self,
        profile_id: &str,
    ) -> Result<Vec<(String, String)>, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        let mut out = Vec::new();
        match &profile.auth_mode {
            AuthMode::Oauth { .. } => {
                if let Some(tok) = self.store.get_oauth(&profile.id)? {
                    // LiteLLM / custom adapters may accept bearer via env; document as XAI_API_KEY
                    // for compatibility when OAuth access token is used as bearer.
                    out.push(("XAI_API_KEY".into(), tok.access_token));
                }
            }
            AuthMode::ApiKey { .. } => {
                if let Some((key, _)) =
                    resolve_key(&profile.api_key_env, &profile.id, self.store)?
                {
                    if let Some(primary) = profile.api_key_env.first() {
                        out.push((primary.clone(), key));
                    }
                }
            }
        }
        Ok(out)
    }

    fn profile_or_err(&self, profile_id: &str) -> Result<ConnectProfile, ConnectError> {
        self.registry
            .get(profile_id)
            .cloned()
            .ok_or_else(|| {
                ConnectError::UnknownProfile(profile_id.into(), self.registry.ids().join(", "))
            })
    }

    fn activate(
        &mut self,
        profile: &ConnectProfile,
        key_source: KeySource,
    ) -> Result<ConnectOutcome, ConnectError> {
        let model = profile
            .default_model()
            .ok_or_else(|| ConnectError::Message("profile has no default models".into()))?
            .to_string();
        self.active_profile_id = Some(profile.id.clone());
        self.active_model = Some(model.clone());
        Ok(ConnectOutcome {
            profile_id: profile.id.clone(),
            model,
            key_source,
        })
    }
}

pub fn format_connected(outcome: &ConnectOutcome, title: &str) -> String {
    format!(
        "Connected {title} · model {} · key_source={}",
        outcome.model,
        outcome.key_source.as_str()
    )
}

/// Whether TUI should open API key modal before calling connect.
pub fn needs_tui_api_key_prompt(registry: &ConnectRegistry, profile_id: &str) -> bool {
    registry
        .get(profile_id)
        .map(|p| p.needs_tui_api_key_prompt())
        .unwrap_or(false)
}

/// Whether TUI should open OAuth overlay.
pub fn needs_tui_oauth(registry: &ConnectRegistry, profile_id: &str) -> bool {
    registry
        .get(profile_id)
        .map(|p| p.auth_mode.is_oauth())
        .unwrap_or(false)
}

pub fn handle_connect_action(
    action: ConnectAction,
    registry: &ConnectRegistry,
    store: &CredentialStore,
    active_profile: &mut Option<String>,
    active_model: &mut Option<String>,
) -> Result<String, ConnectError> {
    let mut svc = ConnectService {
        registry,
        store,
        active_profile_id: active_profile.clone(),
        active_model: active_model.clone(),
    };
    match action {
        ConnectAction::Open | ConnectAction::List => {
            let mut lines = vec![
                "Connect profiles (oauth: /connect <id>; api_key: /connect <id> [key]):".into(),
            ];
            lines.extend(svc.list_lines()?);
            for p in registry.profiles() {
                if let Some(url) = &p.auth_url {
                    lines.push(format!("  {} auth: {url} ({})", p.id, p.auth_mode.label()));
                }
            }
            Ok(lines.join("\n"))
        }
        ConnectAction::Status => svc.status_message(),
        ConnectAction::Connect {
            profile_id,
            api_key,
            oauth_fixture,
        } => {
            let profile = registry.get(&profile_id).ok_or_else(|| {
                ConnectError::UnknownProfile(profile_id.clone(), registry.ids().join(", "))
            })?;
            let title = profile.title.clone();
            let out = svc.connect(&profile_id, api_key.as_deref(), oauth_fixture)?;
            *active_profile = Some(out.profile_id.clone());
            *active_model = Some(out.model.clone());
            Ok(format_connected(&out, &title))
        }
        ConnectAction::Disconnect { profile_id } => {
            let msg = svc.disconnect(profile_id.as_deref())?;
            if let Some(ref id) = profile_id {
                if active_profile.as_deref() == Some(id.as_str()) {
                    *active_profile = None;
                }
            } else {
                *active_profile = None;
            }
            Ok(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use crate::profile::ConnectProfile;
    use crate::registry::ConnectRegistry;
    use tempfile::tempdir;

    fn api_key_registry() -> ConnectRegistry {
        let mut r = ConnectRegistry::new();
        r.register(ConnectProfile {
            id: "demo".into(),
            title: "Demo".into(),
            description: "test".into(),
            auth_mode: AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            api_key_env: vec!["DEMO_API_KEY".into()],
            default_base_url: None,
            default_models: vec!["demo/model-1".into()],
            auth_url: Some("https://example.com".into()),
            litellm_provider_prefix: "demo".into(),
        });
        r
    }

    fn oauth_registry() -> ConnectRegistry {
        let mut r = ConnectRegistry::new();
        r.register(ConnectProfile {
            id: "xai".into(),
            title: "xAI Grok".into(),
            description: "oauth".into(),
            auth_mode: AuthMode::xai_oauth(),
            api_key_env: vec![],
            default_base_url: None,
            default_models: vec!["xai/grok-3".into()],
            auth_url: Some("https://accounts.x.ai".into()),
            litellm_provider_prefix: "xai".into(),
        });
        r
    }

    #[test]
    fn parse_connect_args_variants() {
        assert_eq!(parse_connect_args("").unwrap(), ConnectAction::Open);
        assert_eq!(parse_connect_args("list").unwrap(), ConnectAction::List);
        assert_eq!(
            parse_connect_args("xai").unwrap(),
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: None,
                oauth_fixture: false
            }
        );
    }

    #[test]
    fn connect_api_key_profile() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = api_key_registry();
        let mut active_profile = None;
        let mut active_model = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "demo".into(),
                api_key: Some("secret".into()),
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(msg.contains("Demo"));
        assert!(!msg.contains("secret"));
    }

    #[test]
    fn oauth_rejects_api_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = oauth_registry();
        let mut ap = None;
        let mut am = None;
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: Some("nope".into()),
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::OauthRejectsApiKey(_)));
    }

    #[test]
    fn oauth_fixture_connects() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = oauth_registry();
        let mut ap = None;
        let mut am = None;
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: None,
                oauth_fixture: true,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("oauth") || msg.contains("xAI"));
        assert_eq!(am.as_deref(), Some("xai/grok-3"));
        assert!(store.get_oauth("xai").unwrap().is_some());
        assert!(!msg.contains("fixture-access-token"));
    }

    #[test]
    fn oauth_pending_without_fixture() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = oauth_registry();
        // Ensure fixture env off
        std::env::remove_var("FORGE_CONNECT_OAUTH_FIXTURE");
        std::env::remove_var("FORGE_XAI_OAUTH_ACCESS_TOKEN");
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: None,
            active_model: None,
        };
        let err = svc.connect("xai", None, false).unwrap_err();
        assert!(matches!(err, ConnectError::OauthPending(_, _)));
    }

    #[test]
    fn needs_tui_flags() {
        let reg = {
            let mut r = ConnectRegistry::new();
            r.register(ConnectProfile {
                id: "opencode_go".into(),
                title: "Go".into(),
                description: "".into(),
                auth_mode: AuthMode::opencode_go_api_key(),
                api_key_env: vec![],
                default_base_url: None,
                default_models: vec!["m".into()],
                auth_url: None,
                litellm_provider_prefix: "o".into(),
            });
            r.register(ConnectProfile {
                id: "xai".into(),
                title: "Grok".into(),
                description: "".into(),
                auth_mode: AuthMode::xai_oauth(),
                api_key_env: vec![],
                default_base_url: None,
                default_models: vec!["xai/m".into()],
                auth_url: None,
                litellm_provider_prefix: "xai".into(),
            });
            r
        };
        assert!(needs_tui_api_key_prompt(&reg, "opencode_go"));
        assert!(!needs_tui_api_key_prompt(&reg, "xai"));
        assert!(needs_tui_oauth(&reg, "xai"));
        assert!(!needs_tui_oauth(&reg, "opencode_go"));
    }
}
