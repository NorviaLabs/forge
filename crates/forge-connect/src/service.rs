//! Connect service: list / status / connect / disconnect (CONN-01 + 6.1 auth modes).

use thiserror::Error;

use crate::auth::{AuthMode, OauthPending, OauthTokens};
use crate::catalog::CatalogError;
use crate::catalog::ModelCatalogCache;
use crate::oauth_dispatch::{OauthDispatcher, OauthError, PollResult};
use crate::oauth_xai::try_open_browser;
use crate::preferences::PreferenceStore;
use crate::profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
use crate::registry::ConnectRegistry;
use crate::store::{resolve_connected, resolve_key, CredentialStore, StoreError};
use crate::verify::VerifyError;

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("unknown profile `{0}` (known: {1})")]
    UnknownProfile(String, String),
    #[error("api key required for profile `{0}`")]
    MissingKey(String),
    #[error(
        "profile `{0}` uses OAuth — do not pass an API key; run `/connect {0}` and complete browser/device login"
    )]
    OauthRejectsApiKey(String),
    /// Device-code session started; operator must finish login.
    /// Display shows operator instructions (never tokens).
    /// Boxed: `OauthPending` is ~216 bytes, and inline it made every `Result` in
    /// this module oversized. That was the sole cause of the ten
    /// `clippy::result_large_err` allows this change removes.
    #[error("{}", .0.operator_instructions())]
    OauthDevicePending(Box<OauthPending>),
    #[error("OAuth: {0}")]
    Oauth(#[from] OauthError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error(transparent)]
    Verify(#[from] VerifyError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("{0}")]
    Message(String),
}

impl PartialEq for ConnectError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::OauthDevicePending(a), Self::OauthDevicePending(b)) => {
                a.profile_id == b.profile_id && a.user_code == b.user_code
            }
            _ => self.to_string() == other.to_string(),
        }
    }
}
impl Eq for ConnectError {}

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
    Disconnect {
        profile_id: Option<String>,
    },
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
    /// Non-secret interactive selections. Separate from `store` because they
    /// are not secrets and no longer share its file.
    pub preferences: &'a PreferenceStore,
    pub active_profile_id: Option<String>,
    pub active_model: Option<String>,
}

impl<'a> ConnectService<'a> {
    pub fn list_lines(&self) -> Result<Vec<String>, ConnectError> {
        let mut lines = Vec::new();
        for p in self.registry.profiles() {
            let connected = resolve_connected(&p.api_key_env, &p.id, self.store)?.is_some();
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

        let (key, key_source) = if let Some(k) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
            (k.to_string(), KeySource::Provided)
        } else if let Some((k, src)) = resolve_key(&profile.api_key_env, &profile.id, self.store)? {
            (k, src)
        } else if profile.id == crate::ollama::PROFILE_ID {
            // Local Ollama does not require a cloud API key.
            (
                crate::ollama::LOCAL_PLACEHOLDER_KEY.to_string(),
                KeySource::Provided,
            )
        } else {
            return Err(ConnectError::MissingKey(profile.id.clone()));
        };

        // Live-verify when possible so a bad paste fails at connect, not mid-chat.
        // Skip network when offline tests request it.
        if std::env::var("FORGE_CONNECT_SKIP_VERIFY").is_err() {
            match profile.id.as_str() {
                id if id == crate::opencode_go::PROFILE_ID => {
                    let base = profile
                        .default_base_url
                        .as_deref()
                        .unwrap_or(crate::opencode_go::DEFAULT_BASE_URL);
                    crate::opencode_go::verify_api_key(&key, base)?;
                }
                id if id == crate::opencode_zen::PROFILE_ID => {
                    let base = profile
                        .default_base_url
                        .as_deref()
                        .unwrap_or(crate::opencode_zen::DEFAULT_BASE_URL);
                    // Zen reuses the Go verification path, so its guidance must be
                    // relabelled. Still done on the rendered message: doing it properly
                    // needs Zen-specific guidance threaded through the shared verifier,
                    // which changes user-facing wording and belongs in its own change.
                    crate::opencode_go::verify_api_key(&key, base).map_err(|e| {
                        ConnectError::Message(
                            e.to_string()
                                .replace("OpenCode Go", "OpenCode Zen")
                                .replace("/zen/go", "/zen")
                                .replace("subscribe to Go", "Zen billing / API keys"),
                        )
                    })?;
                }
                id if id == crate::openai::PROFILE_ID => {
                    let base = profile
                        .default_base_url
                        .as_deref()
                        .unwrap_or(crate::openai::DEFAULT_BASE_URL);
                    crate::openai::verify_api_key(&key, base)?;
                }
                id if id == crate::anthropic::PROFILE_ID => {
                    let base = profile
                        .default_base_url
                        .as_deref()
                        .unwrap_or(crate::anthropic::DEFAULT_BASE_URL);
                    crate::anthropic::verify_api_key(&key, base)?;
                }
                id if id == crate::ollama::PROFILE_ID => {
                    let base = profile
                        .default_base_url
                        .as_deref()
                        .unwrap_or(crate::ollama::DEFAULT_BASE_URL);
                    crate::ollama::verify_reachable(base)?;
                }
                _ => {}
            }
        }

        // Always persist for Provided (including Ollama placeholder) so restore works.
        if key_source == KeySource::Provided {
            self.store.set_api_key(&profile.id, &key)?;
        }

        // Best-effort live model catalog for /model picker (never fails connect).
        if std::env::var("FORGE_CONNECT_SKIP_VERIFY").is_err() {
            let _ = crate::catalog::refresh_profile_catalog(
                &profile,
                self.store,
                &crate::catalog::ModelCatalogCache::user_default(),
            );
        } else {
            // Offline tests: seed cache from defaults so /model still has rows.
            let cache = crate::catalog::ModelCatalogCache::user_default();
            let _ = cache.put(&profile.id, profile.default_models.clone());
        }

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
        if std::env::var("FORGE_CONNECT_SKIP_VERIFY").is_err() {
            let _ = crate::catalog::refresh_profile_catalog(
                &profile,
                self.store,
                &crate::catalog::ModelCatalogCache::user_default(),
            );
        } else {
            let cache = crate::catalog::ModelCatalogCache::user_default();
            let _ = cache.put(&profile.id, profile.default_models.clone());
        }
        self.activate(&profile, KeySource::Oauth)
    }

    /// Start an OAuth device-code session for the selected provider.
    pub fn start_oauth(&self, profile_id: &str) -> Result<OauthPending, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        let AuthMode::Oauth {
            auth_server,
            system_browser,
            ..
        } = &profile.auth_mode
        else {
            return Err(ConnectError::Message(format!(
                "profile `{}` is not OAuth",
                profile.id
            )));
        };

        // Offline / unit-test stub
        if std::env::var("FORGE_CONNECT_OAUTH_STUB").is_ok()
            || std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok()
        {
            return Ok(OauthPending::start_stub(&profile.id, auth_server));
        }

        let pending = OauthDispatcher::start(&profile)?;
        if *system_browser {
            try_open_browser(pending.open_url());
        }
        Ok(pending)
    }

    /// Single non-blocking poll of a device-code session (for TUI ticks).
    pub fn poll_oauth_once(
        &mut self,
        pending: &OauthPending,
    ) -> Result<Option<ConnectOutcome>, ConnectError> {
        if pending.profile_id != "xai" && pending.client_id == "stub" {
            return Ok(None);
        }
        match OauthDispatcher::poll(pending)? {
            PollResult::Complete(tokens) => {
                Ok(Some(self.connect_oauth(&pending.profile_id, tokens)?))
            }
            PollResult::Pending | PollResult::SlowDown => Ok(None),
        }
    }

    /// Block until device login completes for CLI callers.
    pub fn complete_oauth_device_flow(
        &mut self,
        pending: &OauthPending,
        max_wait: std::time::Duration,
    ) -> Result<ConnectOutcome, ConnectError> {
        if pending.client_id == "stub" {
            return Err(ConnectError::Oauth(OauthError::Message(
                "stub OAuth cannot complete; unset FORGE_CONNECT_OAUTH_STUB/FIXTURE or use fixture connect".into(),
            )));
        }
        let deadline = std::time::Instant::now() + max_wait;
        let mut interval = std::time::Duration::from_secs(pending.interval_secs.max(1));
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(ConnectError::Oauth(OauthError::Message(
                    "device authorization expired".into(),
                )));
            }
            match OauthDispatcher::poll(pending)? {
                PollResult::Complete(tokens) => {
                    return self.connect_oauth(&pending.profile_id, tokens)
                }
                PollResult::Pending => std::thread::sleep(interval),
                PollResult::SlowDown => {
                    interval += std::time::Duration::from_secs(5);
                    std::thread::sleep(interval);
                }
            }
        }
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
                Err(ConnectError::OauthDevicePending(Box::new(pending)))
            }
            AuthMode::ApiKey { .. } => self.connect_api_key(&profile.id, api_key),
        }
    }

    /// Like `connect` for OAuth, but returns the pending struct for the caller to poll.
    pub fn connect_start_oauth(
        &mut self,
        profile_id: &str,
    ) -> Result<Result<ConnectOutcome, OauthPending>, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        if !profile.auth_mode.is_oauth() {
            return Err(ConnectError::Message(format!(
                "profile `{}` is not OAuth",
                profile.id
            )));
        }
        // Reuse stored tokens across sessions (refresh if near expiry).
        match self.ensure_oauth_fresh(&profile.id) {
            Ok(Some(_)) => return Ok(Ok(self.activate(&profile, KeySource::Oauth)?)),
            Ok(None) => {}
            Err(_) => {
                // Refresh failed — try existing non-fixture token once, else re-login.
                if let Some(tokens) = self.store.get_oauth(&profile.id)? {
                    let at = tokens.access_token.trim();
                    if !at.is_empty()
                        && !at.starts_with("fixture-")
                        && at != "fixture-access-token"
                        && !tokens.needs_refresh(std::time::Duration::from_secs(0))
                    {
                        return Ok(Ok(self.activate(&profile, KeySource::Oauth)?));
                    }
                }
            }
        }
        if std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok() {
            let tokens = OauthTokens {
                access_token: "fixture-access-token".into(),
                refresh_token: Some("fixture-refresh".into()),
                expires_at: None,
            };
            return Ok(Ok(self.connect_oauth(&profile.id, tokens)?));
        }
        if let Ok(at) = std::env::var("FORGE_XAI_OAUTH_ACCESS_TOKEN") {
            if !at.trim().is_empty() && profile.id == "xai" {
                return Ok(Ok(self.connect_oauth(
                    &profile.id,
                    OauthTokens {
                        access_token: at,
                        refresh_token: std::env::var("FORGE_XAI_OAUTH_REFRESH_TOKEN").ok(),
                        expires_at: None,
                    },
                )?));
            }
        }
        Ok(Err(self.start_oauth(&profile.id)?))
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
        self.preferences.clear_last_selection(Some(&id))?;
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

    /// Refresh OAuth tokens if expired / near expiry; persist and return fresh tokens.
    pub fn ensure_oauth_fresh(
        &self,
        profile_id: &str,
    ) -> Result<Option<OauthTokens>, ConnectError> {
        let Some(tok) = self.store.get_oauth(profile_id)? else {
            return Ok(None);
        };
        let at = tok.access_token.trim();
        if at.is_empty() || at.starts_with("fixture-") || at == "fixture-access-token" {
            return Ok(None);
        }
        // Refresh ~5 minutes before expiry (or when already expired).
        let skew = std::time::Duration::from_secs(5 * 60);
        if !tok.needs_refresh(skew) {
            return Ok(Some(tok));
        }
        let Some(refresh) = tok
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|s| s.to_string())
        else {
            // No refresh token — return existing access token and let upstream fail if expired.
            return Ok(Some(tok));
        };
        let profile = self.profile_or_err(profile_id)?;
        let AuthMode::Oauth { .. } = &profile.auth_mode else {
            return Ok(Some(tok));
        };
        match OauthDispatcher::refresh(&profile, &refresh) {
            Ok(fresh) => {
                self.store.set_oauth(profile_id, fresh.clone())?;
                Ok(Some(fresh))
            }
            Err(error) => Err(ConnectError::Oauth(OauthError::Message(format!(
                "token refresh failed ({error}); run `/connect {profile_id}` again"
            )))),
        }
    }

    pub fn provider_env_for_profile(
        &self,
        profile_id: &str,
    ) -> Result<Vec<(String, String)>, ConnectError> {
        let profile = self.profile_or_err(profile_id)?;
        let mut out = Vec::new();
        match &profile.auth_mode {
            AuthMode::Oauth { .. } => {
                // Prefer a fresh token (silent refresh across sessions).
                let tok = match self.ensure_oauth_fresh(&profile.id) {
                    Ok(t) => t,
                    Err(_) => self.store.get_oauth(&profile.id)?,
                };
                if let Some(tok) = tok {
                    let at = tok.access_token.trim();
                    // Never export fixture tokens to the live worker — they only exist for unit tests.
                    if at.is_empty() || at.starts_with("fixture-") || at == "fixture-access-token" {
                        // skip — operator must complete real OAuth
                    } else if profile.id == crate::openai_codex::PROFILE_ID {
                        let account_id = crate::openai_codex::account_id_from_token(at)?;
                        out.push((crate::openai_codex::ACCESS_TOKEN_ENV.into(), at.to_string()));
                        out.push((crate::openai_codex::ACCOUNT_ID_ENV.into(), account_id));
                    } else {
                        // Native xAI transport uses the OAuth token as Bearer auth.
                        out.push(("XAI_API_KEY".into(), at.to_string()));
                    }
                }
            }
            AuthMode::ApiKey { .. } => {
                if let Some((key, _)) = resolve_key(&profile.api_key_env, &profile.id, self.store)?
                {
                    if let Some(primary) = profile.api_key_env.first() {
                        out.push((primary.clone(), key.clone()));
                    }
                    // Also export secondary env names so either documented var works.
                    for name in profile.api_key_env.iter().skip(1) {
                        out.push((name.clone(), key.clone()));
                    }
                    // Provider-specific base URLs for native HTTP routes.
                    if let Some(base) =
                        profile
                            .default_base_url
                            .clone()
                            .or_else(|| match profile.id.as_str() {
                                id if id == crate::opencode_go::PROFILE_ID => {
                                    Some(crate::opencode_go::DEFAULT_BASE_URL.into())
                                }
                                id if id == crate::opencode_zen::PROFILE_ID => {
                                    Some(crate::opencode_zen::DEFAULT_BASE_URL.into())
                                }
                                id if id == crate::ollama::PROFILE_ID => {
                                    Some(crate::ollama::DEFAULT_BASE_URL.into())
                                }
                                _ => None,
                            })
                    {
                        let env_name = match profile.id.as_str() {
                            id if id == crate::opencode_go::PROFILE_ID => {
                                crate::opencode_go::API_BASE_ENV.to_string()
                            }
                            id if id == crate::opencode_zen::PROFILE_ID => {
                                crate::opencode_zen::API_BASE_ENV.to_string()
                            }
                            id if id == crate::ollama::PROFILE_ID => {
                                crate::ollama::API_BASE_ENV.to_string()
                            }
                            id if id == crate::openai::PROFILE_ID => "OPENAI_API_BASE".into(),
                            id if id == crate::anthropic::PROFILE_ID => "ANTHROPIC_API_BASE".into(),
                            other => format!("{}_API_BASE", other.to_ascii_uppercase()),
                        };
                        out.push((env_name, base));
                    }
                }
            }
        }
        Ok(out)
    }
}

/// True when any live (non-fixture) profile is connected in the user store.
pub fn has_connected_profile() -> bool {
    let registry = crate::loaded_registry();
    let store = crate::CredentialStore::user_default();
    let preferences = crate::PreferenceStore::user_default();
    let svc = ConnectService {
        registry: &registry,
        store: &store,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    };
    svc.connected_profiles()
        .map(|profiles| !profiles.is_empty())
        .unwrap_or(false)
}

impl ConnectService<'_> {
    /// Profiles that already have stored credentials (for session restore).
    pub fn connected_profiles(&self) -> Result<Vec<ConnectProfile>, ConnectError> {
        let mut out = Vec::new();
        for p in self.registry.profiles() {
            if resolve_connected(&p.api_key_env, &p.id, self.store)?.is_some() {
                // Only count OAuth when tokens are non-fixture.
                if p.auth_mode.is_oauth() {
                    if let Some(tok) = self.store.get_oauth(&p.id)? {
                        let at = tok.access_token.trim();
                        if at.is_empty()
                            || at.starts_with("fixture-")
                            || at == "fixture-access-token"
                        {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                out.push(p.clone());
            }
        }
        Ok(out)
    }

    fn profile_or_err(&self, profile_id: &str) -> Result<ConnectProfile, ConnectError> {
        self.registry.get(profile_id).cloned().ok_or_else(|| {
            ConnectError::UnknownProfile(profile_id.into(), self.registry.ids().join(", "))
        })
    }

    fn activate(
        &mut self,
        profile: &ConnectProfile,
        key_source: KeySource,
    ) -> Result<ConnectOutcome, ConnectError> {
        // Prefer the live catalog, then a configured default, then a provider-scoped
        // placeholder so connect output is non-empty even before the first refresh.
        // First-run activation stays on the profile default. Live/registry
        // catalogs can sort a different model first and should not hijack
        // the initial selection (for example xAI listing grok-4.20 before grok-3).
        let cache = ModelCatalogCache::user_default();
        let model = profile
            .default_model()
            .map(str::to_string)
            .or_else(|| cache.get_cached(&profile.id).into_iter().next())
            .unwrap_or_else(|| format!("{}/default", profile.model_provider_prefix));
        self.active_profile_id = Some(profile.id.clone());
        self.active_model = Some(model.clone());
        // Selection persistence is convenience metadata; never make a successful
        // credential connection fail because it cannot be written.
        let _ = self.preferences.set_last_selection(&profile.id, &model);
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
    preferences: &PreferenceStore,
    active_profile: &mut Option<String>,
    active_model: &mut Option<String>,
) -> Result<String, ConnectError> {
    let mut svc = ConnectService {
        registry,
        store,
        preferences,
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

    use crate::registry::ConnectRegistry;
    use tempfile::tempdir;

    use crate::test_env::EnvGuard;

    /// Every variable these tests read or write. Named in one place so the guard
    /// clears and restores the whole set, and so the lock covers the same keys
    /// that `connect_api_key` consults.
    const OAUTH_ENV: &[&str] = &[
        "FORGE_CONNECT_OAUTH_FIXTURE",
        "FORGE_CONNECT_OAUTH_STUB",
        "FORGE_CONNECT_SKIP_VERIFY",
        "FORGE_XAI_OAUTH_ACCESS_TOKEN",
        "FORGE_XAI_OAUTH_REFRESH_TOKEN",
    ];

    fn api_key_registry() -> ConnectRegistry {
        let mut r = ConnectRegistry::new();
        let mut spec = crate::profile::test_spec(
            "demo",
            AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            vec!["demo/model-1".into()],
        );
        spec.title = "Demo".into();
        spec.vendor_label = "Demo".into();
        spec.api_key_env = vec!["DEMO_API_KEY".into()];
        spec.auth_url = Some("https://example.com".into());
        r.register(spec);
        r
    }

    fn oauth_registry() -> ConnectRegistry {
        let mut r = ConnectRegistry::new();
        let mut spec =
            crate::profile::test_spec("xai", AuthMode::xai_oauth(), vec!["xai/grok-3".into()]);
        spec.title = "xAI Grok".into();
        spec.api_key_env = vec![];
        spec.auth_url = Some("https://auth.x.ai".into());
        spec.vendor_label = "xAI Grok".into();
        r.register(spec);
        r
    }

    #[test]
    fn parse_connect_args_variants() {
        assert_eq!(parse_connect_args("").unwrap(), ConnectAction::Open);
        assert_eq!(parse_connect_args("list").unwrap(), ConnectAction::List);
        assert_eq!(parse_connect_args("status").unwrap(), ConnectAction::Status);
        assert_eq!(
            parse_connect_args("disconnect demo").unwrap(),
            ConnectAction::Disconnect {
                profile_id: Some("demo".into())
            }
        );
        assert_eq!(
            parse_connect_args("disconnect").unwrap(),
            ConnectAction::Disconnect { profile_id: None }
        );
        assert_eq!(
            parse_connect_args("XAI secret ignored-after-key").unwrap(),
            ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: Some("secret".into()),
                oauth_fixture: false
            }
        );
    }

    #[test]
    fn list_status_and_open_messages_reflect_connection_state() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = api_key_registry();
        store.set_api_key("demo", "stored-key").unwrap();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: Some("demo".into()),
            active_model: Some("demo/model-1".into()),
        };

        let lines = svc.list_lines().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("[connected, active] demo"));
        assert!(lines[0].contains("(api_key)"));

        let status = svc.status().unwrap();
        assert_eq!(status.profile_id.as_deref(), Some("demo"));
        assert_eq!(status.model.as_deref(), Some("demo/model-1"));
        assert_eq!(status.key_source, Some(KeySource::File));
        assert_eq!(status.connected_profile_ids, vec!["demo"]);
        assert!(svc
            .status_message()
            .unwrap()
            .contains("active_profile=demo model=demo/model-1 key_source=file"));
    }

    #[test]
    fn empty_registry_lists_empty_message_and_unknown_profile_reports_known_ids() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let empty = ConnectRegistry::new();
        let svc = ConnectService {
            registry: &empty,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            svc.list_lines().unwrap(),
            vec!["(no connect profiles registered)".to_string()]
        );

        let reg = api_key_registry();
        let mut active_profile = None;
        let mut active_model = None;
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "missing".into(),
                api_key: None,
                oauth_fixture: false,
            },
            &reg,
            &store,
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "unknown profile `missing` (known: demo)");
    }

    #[test]
    fn handle_connect_open_list_status_and_disconnect_update_active_state() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = api_key_registry();
        store.set_api_key("demo", "stored-key").unwrap();
        preferences
            .set_last_selection("demo", "demo/model-1")
            .unwrap();
        let mut active_profile = Some("demo".into());
        let mut active_model = Some("demo/model-1".into());

        let open = handle_connect_action(
            ConnectAction::Open,
            &reg,
            &store,
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(open.contains("Connect profiles"));
        assert!(open.contains("demo auth: https://example.com"));

        let status = handle_connect_action(
            ConnectAction::Status,
            &reg,
            &store,
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(status.contains("active_profile=demo"));

        let msg = handle_connect_action(
            ConnectAction::Disconnect {
                profile_id: Some("demo".into()),
            },
            &reg,
            &store,
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(msg.contains("cleared stored credentials"));
        assert_eq!(active_profile, None);
        assert_eq!(active_model, Some("demo/model-1".into()));
        assert!(preferences.last_selection().unwrap().is_none());

        let msg = handle_connect_action(
            ConnectAction::Disconnect { profile_id: None },
            &reg,
            &store,
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap_err();
        assert_eq!(msg.to_string(), "no profile to disconnect");
    }

    #[test]
    fn connect_api_key_profile() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
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
            &preferences,
            &mut active_profile,
            &mut active_model,
        )
        .unwrap();
        assert!(msg.contains("Demo"));
        assert!(!msg.contains("secret"));
    }

    #[test]
    fn api_key_connect_rejects_missing_key_and_persists_provided_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = api_key_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };

        assert_eq!(
            svc.connect_api_key("demo", Some("   ")).unwrap_err(),
            ConnectError::MissingKey("demo".into())
        );
        let outcome = svc.connect_api_key("demo", Some(" secret ")).unwrap();
        assert_eq!(outcome.key_source, KeySource::Provided);
        assert_eq!(
            store.get_api_key("demo").unwrap().as_deref(),
            Some("secret")
        );
        assert_eq!(svc.active_profile_id.as_deref(), Some("demo"));
        assert_eq!(svc.active_model.as_deref(), Some("demo/model-1"));
    }

    #[test]
    fn oauth_rejects_api_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
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
            &preferences,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::OauthRejectsApiKey(_)));
    }

    #[test]
    fn oauth_connect_rejects_empty_tokens_and_non_oauth_profiles() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let api_reg = api_key_registry();
        let oauth_reg = oauth_registry();
        let mut api_svc = ConnectService {
            registry: &api_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            api_svc
                .connect_oauth(
                    "demo",
                    OauthTokens {
                        access_token: "real".into(),
                        refresh_token: None,
                        expires_at: None,
                    },
                )
                .unwrap_err()
                .to_string(),
            "profile `demo` is not OAuth"
        );

        let mut oauth_svc = ConnectService {
            registry: &oauth_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            oauth_svc
                .connect_oauth(
                    "xai",
                    OauthTokens {
                        access_token: "   ".into(),
                        refresh_token: None,
                        expires_at: None,
                    },
                )
                .unwrap_err()
                .to_string(),
            "OAuth access_token empty"
        );
    }

    #[test]
    fn oauth_fixture_connects() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
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
            &preferences,
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
    fn oauth_stored_tokens_reactivate_without_fixture_or_api_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "real-token".into(),
                    refresh_token: Some("refresh".into()),
                    expires_at: None,
                },
            )
            .unwrap();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let outcome = svc.connect("xai", None, false).unwrap();
        assert_eq!(outcome.key_source, KeySource::Oauth);
        assert_eq!(svc.active_profile_id.as_deref(), Some("xai"));
    }

    #[test]
    fn oauth_pending_without_fixture() {
        // The guard clears every key in OAUTH_ENV, so fixture connect is already
        // off; only the stub flag needs setting.
        let guard = EnvGuard::new(OAUTH_ENV);
        guard.set("FORGE_CONNECT_OAUTH_STUB", "1");
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let err = svc.connect("xai", None, false).unwrap_err();
        assert!(matches!(err, ConnectError::OauthDevicePending(_)));
    }

    #[test]
    fn start_poll_and_complete_oauth_stub_paths_are_non_networked() {
        let guard = EnvGuard::new(OAUTH_ENV);
        guard.set("FORGE_CONNECT_OAUTH_STUB", "1");
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let pending = svc.start_oauth("xai").unwrap();
        assert_eq!(pending.profile_id, "xai");
        assert!(pending.operator_instructions().contains("xAI"));
        assert!(svc.poll_oauth_once(&pending).is_err());
        assert!(svc
            .complete_oauth_device_flow(&pending, std::time::Duration::from_millis(1))
            .unwrap_err()
            .to_string()
            .contains("stub OAuth cannot complete"));

        let other = OauthPending::start_stub("other", "https://auth.example");
        assert_eq!(svc.poll_oauth_once(&other).unwrap(), None);
    }

    #[test]
    fn connect_start_oauth_handles_non_oauth_stored_tokens_and_fixture_env() {
        let guard = EnvGuard::new(OAUTH_ENV);
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let api_reg = api_key_registry();
        let mut api_svc = ConnectService {
            registry: &api_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            api_svc.connect_start_oauth("demo").unwrap_err().to_string(),
            "profile `demo` is not OAuth"
        );

        let oauth_reg = oauth_registry();
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "real-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        let mut oauth_svc = ConnectService {
            registry: &oauth_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert!(oauth_svc.connect_start_oauth("xai").unwrap().is_ok());

        store.clear("xai").unwrap();
        guard.set("FORGE_CONNECT_OAUTH_FIXTURE", "1");
        let fixture = oauth_svc.connect_start_oauth("xai").unwrap();
        assert!(fixture.is_ok());
        assert_eq!(
            store.get_oauth("xai").unwrap().unwrap().access_token,
            "fixture-access-token"
        );
    }

    #[test]
    fn ensure_oauth_fresh_filters_fixture_empty_and_uses_unexpired_real_token() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert!(svc.ensure_oauth_fresh("xai").unwrap().is_none());

        for access_token in ["", "fixture-access-token", "fixture-test"] {
            store
                .set_oauth(
                    "xai",
                    OauthTokens {
                        access_token: access_token.into(),
                        refresh_token: None,
                        expires_at: None,
                    },
                )
                .unwrap();
            assert!(svc.ensure_oauth_fresh("xai").unwrap().is_none());
        }

        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "real-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(
            svc.ensure_oauth_fresh("xai").unwrap().unwrap().access_token,
            "real-token"
        );
    }

    #[test]
    fn provider_env_exports_api_keys_base_urls_and_filters_fixture_oauth() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        let mut custom = crate::profile::test_spec(
            "custom",
            AuthMode::ApiKey {
                tui_always_prompt: false,
            },
            vec!["custom/model".into()],
        );
        custom.api_key_env = vec!["CUSTOM_KEY".into(), "CUSTOM_ALT".into()];
        custom.default_base_url = Some("https://api.example".into());
        custom.title = "Custom".into();
        custom.vendor_label = "Custom".into();
        reg.register(custom);
        store.set_api_key("custom", "secret").unwrap();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            svc.provider_env_for_profile("custom").unwrap(),
            vec![
                ("CUSTOM_KEY".into(), "secret".into()),
                ("CUSTOM_ALT".into(), "secret".into()),
                ("CUSTOM_API_BASE".into(), "https://api.example".into()),
            ]
        );

        let oauth_reg = oauth_registry();
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "fixture-access-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        let oauth_svc = ConnectService {
            registry: &oauth_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert!(oauth_svc
            .provider_env_for_profile("xai")
            .unwrap()
            .is_empty());
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "real-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(
            oauth_svc.provider_env_for_profile("xai").unwrap(),
            vec![("XAI_API_KEY".into(), "real-token".into())]
        );
    }

    #[test]
    fn connected_profiles_include_api_keys_and_skip_fixture_oauth() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(api_key_registry().profiles()[0].clone());
        reg.register(oauth_registry().profiles()[0].clone());
        store.set_api_key("demo", "secret").unwrap();
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "fixture-access-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        assert_eq!(
            svc.connected_profiles()
                .unwrap()
                .into_iter()
                .map(|profile| profile.id)
                .collect::<Vec<_>>(),
            vec!["demo"]
        );

        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "real-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(svc.connected_profiles().unwrap().len(), 2);
    }

    #[test]
    fn needs_tui_flags() {
        let reg = {
            let mut r = ConnectRegistry::new();
            let mut go = crate::profile::test_spec(
                "opencode_go",
                AuthMode::opencode_go_api_key(),
                vec!["m".into()],
            );
            go.title = "Go".into();
            go.api_key_env = vec![];
            go.model_provider_prefix = "o".into();
            go.vendor_id = "opencode".into();
            go.vendor_label = "OpenCode".into();
            go.route_label = "Go".into();
            r.register(go);
            let mut xai =
                crate::profile::test_spec("xai", AuthMode::xai_oauth(), vec!["xai/m".into()]);
            xai.title = "Grok".into();
            xai.api_key_env = vec![];
            xai.vendor_label = "Grok".into();
            r.register(xai);
            r
        };
        assert!(needs_tui_api_key_prompt(&reg, "opencode_go"));
        assert!(!needs_tui_api_key_prompt(&reg, "xai"));
        assert!(needs_tui_oauth(&reg, "xai"));
        assert!(!needs_tui_oauth(&reg, "opencode_go"));
    }

    #[test]
    fn connect_api_key_verifies_against_profile_base_url() {
        use forge_test_support::mock_http;

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        let mut openai = crate::openai::openai_profile();
        openai.default_base_url = Some(mock_http(vec![(200, r#"{"data":[]}"#, vec![])]));
        reg.register(openai);
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let outcome = svc
            .connect_api_key("openai", Some("sk-valid-key-for-tests"))
            .unwrap();
        assert_eq!(outcome.key_source, KeySource::Provided);
        assert_eq!(svc.active_profile_id.as_deref(), Some("openai"));
    }

    #[test]
    fn connect_api_key_surfaces_verify_failures_from_mock_server() {
        use forge_test_support::mock_http;

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        let mut anthropic = crate::anthropic::anthropic_profile();
        anthropic.default_base_url = Some(mock_http(vec![(401, "bad key", vec![])]));
        reg.register(anthropic);
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let err = svc
            .connect_api_key("anthropic", Some("sk-ant-valid-key-for-tests"))
            .unwrap_err();
        assert!(matches!(err, ConnectError::Verify(_)));
        assert!(err.to_string().contains("unauthorized"));
    }

    #[test]
    fn connect_api_key_verifies_opencode_go_and_ollama_against_profile_base_url() {
        use forge_test_support::mock_http;

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();

        let mut go = crate::opencode_go::opencode_go_profile();
        go.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"data":[{"id":"gpt-4.1-mini"}]}"#,
            vec![],
        )]));
        reg.register(go);

        let mut ollama = crate::ollama::ollama_profile();
        ollama.default_base_url = Some(mock_http(vec![(200, r#"{"models":[]}"#, vec![])]));
        reg.register(ollama);

        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };

        let go_outcome = svc
            .connect_api_key("opencode_go", Some("go-valid-key-for-tests"))
            .unwrap();
        assert_eq!(go_outcome.key_source, KeySource::Provided);
        assert_eq!(svc.active_profile_id.as_deref(), Some("opencode_go"));

        svc.active_profile_id = None;
        svc.active_model = None;
        let ollama_outcome = svc.connect_api_key("ollama", None).unwrap();
        assert_eq!(ollama_outcome.key_source, KeySource::Provided);
        assert_eq!(svc.active_profile_id.as_deref(), Some("ollama"));
    }

    #[test]
    fn connect_api_key_verifies_opencode_zen_against_profile_base_url() {
        use forge_test_support::mock_http;

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let mut reg = ConnectRegistry::new();
        let mut zen = crate::opencode_zen::opencode_zen_profile();
        zen.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"data":[{"id":"claude-sonnet-4"}]}"#,
            vec![],
        )]));
        reg.register(zen);
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let outcome = svc
            .connect_api_key("opencode_zen", Some("zen-valid-key-for-tests"))
            .unwrap();
        assert_eq!(outcome.key_source, KeySource::Provided);
        assert_eq!(svc.active_profile_id.as_deref(), Some("opencode_zen"));
    }

    #[test]
    fn connect_rejects_api_key_for_oauth_profile() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let err = svc.connect("xai", Some("sk-secret"), false).unwrap_err();
        assert!(matches!(err, ConnectError::OauthRejectsApiKey(ref id) if id == "xai"));
    }

    #[test]
    fn connect_uses_xai_access_token_env_when_set() {
        let guard = EnvGuard::new(OAUTH_ENV);
        guard.set("FORGE_CONNECT_SKIP_VERIFY", "1");
        guard.set("FORGE_XAI_OAUTH_ACCESS_TOKEN", "env-access");
        guard.set("FORGE_XAI_OAUTH_REFRESH_TOKEN", "env-refresh");
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let outcome = svc.connect("xai", None, false).unwrap();
        assert_eq!(outcome.key_source, KeySource::Oauth);
        let stored = store.get_oauth("xai").unwrap().unwrap();
        assert_eq!(stored.access_token, "env-access");
        assert_eq!(stored.refresh_token.as_deref(), Some("env-refresh"));
    }

    #[test]
    fn connect_oauth_rejects_non_oauth_profile_and_empty_token() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = api_key_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let err = svc
            .connect_oauth(
                "demo",
                OauthTokens {
                    access_token: "tok".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("not OAuth"));

        let oauth_reg = oauth_registry();
        let mut oauth_svc = ConnectService {
            registry: &oauth_reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let err = oauth_svc
            .connect_oauth(
                "xai",
                OauthTokens {
                    access_token: "   ".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn ensure_oauth_fresh_refreshes_expired_tokens_via_mock() {
        use forge_test_support::mock_http;

        const ENV: &[&str] = &["FORGE_XAI_OAUTH_ISSUER", "FORGE_CONNECT_SKIP_VERIFY"];
        let guard = EnvGuard::new(ENV);
        let base = mock_http(vec![(
            200,
            r#"{"access_token":"refreshed","refresh_token":"new-rt","expires_in":3600}"#,
            vec![],
        )]);
        guard.set("FORGE_XAI_OAUTH_ISSUER", &base);
        guard.set("FORGE_CONNECT_SKIP_VERIFY", "1");

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "old-access".into(),
                    refresh_token: Some("old-refresh".into()),
                    expires_at: Some("0".into()),
                },
            )
            .unwrap();
        let svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let fresh = svc.ensure_oauth_fresh("xai").unwrap().unwrap();
        assert_eq!(fresh.access_token, "refreshed");
        assert_eq!(
            store.get_oauth("xai").unwrap().unwrap().access_token,
            "refreshed"
        );
    }

    #[test]
    fn poll_oauth_once_completes_device_flow_via_mock() {
        use forge_test_support::mock_http;

        const ENV: &[&str] = &["FORGE_XAI_OAUTH_ISSUER", "FORGE_CONNECT_SKIP_VERIFY"];
        let guard = EnvGuard::new(ENV);
        let base = mock_http(vec![(
            200,
            r#"{"access_token":"poll-access","refresh_token":"poll-refresh","expires_in":3600}"#,
            vec![],
        )]);
        guard.set("FORGE_XAI_OAUTH_ISSUER", &base);
        guard.set("FORGE_CONNECT_SKIP_VERIFY", "1");

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let preferences = PreferenceStore::new(dir.path().join("p.toml"));
        let reg = oauth_registry();
        let mut svc = ConnectService {
            registry: &reg,
            store: &store,
            preferences: &preferences,
            active_profile_id: None,
            active_model: None,
        };
        let pending = OauthPending {
            profile_id: "xai".into(),
            verification_uri: format!("{base}/oauth2/device"),
            verification_uri_complete: None,
            user_code: "AB12".into(),
            device_code: "dc".into(),
            auth_server: base,
            interval_secs: 1,
            expires_in_secs: Some(900),
            client_id: crate::oauth_xai::DEFAULT_CLIENT_ID.into(),
        };
        let outcome = svc.poll_oauth_once(&pending).unwrap().unwrap();
        assert_eq!(outcome.key_source, KeySource::Oauth);
        assert_eq!(svc.active_profile_id.as_deref(), Some("xai"));
    }

    #[test]
    fn connect_error_partial_eq_compares_oauth_device_pending_by_profile_and_code() {
        let pending_a = OauthPending::start_stub("xai", "https://auth.x.ai");
        let pending_b = OauthPending::start_stub("xai", "https://auth.x.ai");
        let pending_other = OauthPending::start_stub("other", "https://auth.example");
        assert_eq!(
            ConnectError::OauthDevicePending(Box::new(pending_a.clone())),
            ConnectError::OauthDevicePending(Box::new(pending_b))
        );
        assert_ne!(
            ConnectError::OauthDevicePending(Box::new(pending_a)),
            ConnectError::OauthDevicePending(Box::new(pending_other))
        );
    }
}
