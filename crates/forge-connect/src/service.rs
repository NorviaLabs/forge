//! Connect service: list / status / connect / disconnect (CONN-01).

use thiserror::Error;

use crate::profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
use crate::registry::ConnectRegistry;
use crate::store::{resolve_key, CredentialStore, StoreError};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConnectError {
    #[error("unknown profile `{0}` (known: {1})")]
    UnknownProfile(String, String),
    #[error("api key required for profile `{0}`")]
    MissingKey(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    /// Interactive list / open flow
    Open,
    List,
    Status,
    Connect {
        profile_id: String,
        /// Optional key from CLI/TUI; if None, use env/store or error.
        api_key: Option<String>,
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
            // `/connect xai` or `/connect xai <key>`
            let key = parts.next().map(|s| s.to_string());
            Ok(ConnectAction::Connect {
                profile_id: other.to_string(),
                api_key: key,
            })
        }
    }
}

pub struct ConnectService<'a> {
    pub registry: &'a ConnectRegistry,
    pub store: &'a CredentialStore,
    /// Currently active profile id (session).
    pub active_profile_id: Option<String>,
    /// Currently active LiteLLM model string.
    pub active_model: Option<String>,
}

impl<'a> ConnectService<'a> {
    pub fn list_lines(&self) -> Result<Vec<String>, ConnectError> {
        let mut lines = Vec::new();
        for p in self.registry.profiles() {
            let connected = resolve_key(&p.api_key_env, &p.id, self.store)?.is_some();
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
                "{badge} {id} — {title}",
                id = p.id,
                title = p.title
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
            if resolve_key(&p.api_key_env, &p.id, self.store)?.is_some() {
                connected.push(p.id.clone());
            }
        }
        let key_source = if let Some(ref id) = self.active_profile_id {
            if let Some(p) = self.registry.get(id) {
                resolve_key(&p.api_key_env, &p.id, self.store)?.map(|(_, s)| s)
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
            s.key_source
                .map(|k| k.as_str())
                .unwrap_or("-"),
            s.connected_profile_ids.join(", ")
        ))
    }

    /// Connect profile: persist optional key, return outcome (never echoes key).
    pub fn connect(
        &mut self,
        profile_id: &str,
        api_key: Option<&str>,
    ) -> Result<ConnectOutcome, ConnectError> {
        let profile = self
            .registry
            .get(profile_id)
            .ok_or_else(|| {
                ConnectError::UnknownProfile(
                    profile_id.into(),
                    self.registry.ids().join(", "),
                )
            })?
            .clone();

        let (key_source, need_store) = if let Some(k) = api_key.map(str::trim).filter(|s| !s.is_empty())
        {
            self.store.set_api_key(&profile.id, k)?;
            (KeySource::Provided, true)
        } else if let Some((_, src)) = resolve_key(&profile.api_key_env, &profile.id, self.store)? {
            (src, false)
        } else {
            return Err(ConnectError::MissingKey(profile.id.clone()));
        };

        let model = profile
            .default_model()
            .ok_or_else(|| ConnectError::Message("profile has no default models".into()))?
            .to_string();

        self.active_profile_id = Some(profile.id.clone());
        self.active_model = Some(model.clone());

        let _ = need_store;
        Ok(ConnectOutcome {
            profile_id: profile.id,
            model,
            key_source,
        })
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
                "cleared stored key for `{id}` (env keys unchanged if set)"
            ))
        } else {
            Ok(format!(
                "no stored key for `{id}` (if using env, clear the shell env var)"
            ))
        }
    }

    pub fn profile(&self, id: &str) -> Option<&ConnectProfile> {
        self.registry.get(id)
    }

    /// Env vars to inject into LiteLLM worker for active/connected profiles.
    pub fn worker_env_for_profile(
        &self,
        profile_id: &str,
    ) -> Result<Vec<(String, String)>, ConnectError> {
        let profile = self.registry.get(profile_id).ok_or_else(|| {
            ConnectError::UnknownProfile(profile_id.into(), self.registry.ids().join(", "))
        })?;
        let mut out = Vec::new();
        if let Some((key, _)) = resolve_key(&profile.api_key_env, &profile.id, self.store)? {
            if let Some(primary) = profile.api_key_env.first() {
                out.push((primary.clone(), key));
            }
        }
        Ok(out)
    }
}

/// Format a connect confirmation without secrets.
pub fn format_connected(outcome: &ConnectOutcome, title: &str) -> String {
    format!(
        "Connected {title} · model {} · key_source={}",
        outcome.model,
        outcome.key_source.as_str()
    )
}

/// Apply a `/connect` action; returns operator-facing message (never secrets).
/// Updates `active_profile` / `active_model` on successful connect.
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
                "Connect profiles (use /connect <id> [api_key]):".into(),
            ];
            lines.extend(svc.list_lines()?);
            for p in registry.profiles() {
                if let Some(url) = &p.auth_url {
                    lines.push(format!("  {} signup: {url}", p.id));
                }
            }
            Ok(lines.join("\n"))
        }
        ConnectAction::Status => svc.status_message(),
        ConnectAction::Connect {
            profile_id,
            api_key,
        } => {
            let profile = registry.get(&profile_id).ok_or_else(|| {
                ConnectError::UnknownProfile(profile_id.clone(), registry.ids().join(", "))
            })?;
            let title = profile.title.clone();
            let auth = profile.auth_url.clone();
            let out = svc.connect(&profile_id, api_key.as_deref())?;
            *active_profile = Some(out.profile_id.clone());
            *active_model = Some(out.model.clone());
            let mut msg = format_connected(&out, &title);
            if let Some(url) = auth {
                if out.key_source != KeySource::Provided && out.key_source != KeySource::File {
                    msg.push_str(&format!("\nsignup: {url}"));
                }
            }
            // If missing key path already errored; if connected via env, note it.
            Ok(msg)
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
    use crate::profile::ConnectProfile;
    use crate::registry::ConnectRegistry;
    use tempfile::tempdir;

    fn demo_registry() -> ConnectRegistry {
        let mut r = ConnectRegistry::new();
        r.register(ConnectProfile {
            id: "demo".into(),
            title: "Demo".into(),
            description: "test".into(),
            api_key_env: vec!["DEMO_API_KEY".into()],
            default_base_url: None,
            default_models: vec!["demo/model-1".into()],
            auth_url: Some("https://example.com".into()),
            litellm_provider_prefix: "demo".into(),
        });
        r
    }

    #[test]
    fn parse_connect_args_variants() {
        assert_eq!(parse_connect_args("").unwrap(), ConnectAction::Open);
        assert_eq!(parse_connect_args("list").unwrap(), ConnectAction::List);
        assert_eq!(parse_connect_args("status").unwrap(), ConnectAction::Status);
        assert_eq!(
            parse_connect_args("disconnect").unwrap(),
            ConnectAction::Disconnect { profile_id: None }
        );
        assert_eq!(
            parse_connect_args("disconnect xai").unwrap(),
            ConnectAction::Disconnect {
                profile_id: Some("xai".into())
            }
        );
        assert_eq!(
            parse_connect_args("xai").unwrap(),
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: None
            }
        );
        assert_eq!(
            parse_connect_args("xai sk-test").unwrap(),
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: Some("sk-test".into())
            }
        );
    }

    #[test]
    fn connect_with_provided_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = demo_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: None,
            active_model: None,
        };
        let out = svc.connect("demo", Some("secret")).unwrap();
        assert_eq!(out.profile_id, "demo");
        assert_eq!(out.model, "demo/model-1");
        assert_eq!(out.key_source, KeySource::Provided);
        let msg = format_connected(&out, "Demo");
        assert!(msg.contains("demo/model-1"));
        assert!(!msg.contains("secret"));
        assert_eq!(store.get_api_key("demo").unwrap().as_deref(), Some("secret"));
    }

    #[test]
    fn connect_missing_key_errors() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = demo_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: None,
            active_model: None,
        };
        assert!(matches!(
            svc.connect("demo", None),
            Err(ConnectError::MissingKey(_))
        ));
    }

    #[test]
    fn list_and_status_no_secrets() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("demo", "top-secret-value").unwrap();
        let reg = demo_registry();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: Some("demo".into()),
            active_model: Some("demo/model-1".into()),
        };
        let list = svc.list_lines().unwrap().join("\n");
        assert!(list.contains("demo"));
        assert!(!list.contains("top-secret"));
        let st = svc.status_message().unwrap();
        assert!(st.contains("active_profile=demo"));
        assert!(!st.contains("top-secret"));
    }

    #[test]
    fn unknown_profile() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let reg = demo_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            active_profile_id: None,
            active_model: None,
        };
        assert!(matches!(
            svc.connect("nope", Some("k")),
            Err(ConnectError::UnknownProfile(_, _))
        ));
    }
}
