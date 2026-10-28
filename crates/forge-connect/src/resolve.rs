//! Resolver: registry + credentials → a `RouteTable` snapshot (design §3).
//!
//! Precedence, first non-blank wins (values are trimmed):
//!
//! | Field | Order |
//! | --- | --- |
//! | API key | session key → `FORGE_<ID>_API_KEY` → `credential_from` target's generated name → `api_key_env` → stored key (target slot, then own old slot) |
//! | OAuth credential | `api_key_env` → stored OAuth token (never from env) |
//! | Base URL | `FORGE_<ID>_BASE_URL` → `base_url_env` → user override (applied by the registry) → definition |

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::credentials::Credentials;
use crate::normalize::env_profile_id;
use crate::profile::{
    Attach, AuthKind, CatalogKind, EffortFamily, KeySource, OauthFlavor, ProviderSpec,
    SessionHeaderKind, StaticHeader, VerifyKind, WireKind, WireOverride,
};
use crate::registry::ConnectRegistry;

/// A credential ready to attach. `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialValue {
    pub secret: String,
    /// The ChatGPT account id, for `attach = bearer-account-id`.
    pub account_id: Option<String>,
}

impl std::fmt::Debug for CredentialValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialValue")
            .field("secret", &"<redacted>")
            .field(
                "account_id",
                &self.account_id.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("no credential; run /connect")]
    Missing,
    #[error("the stored credential was rejected; sign in again with /connect")]
    AuthRequired,
    #[error("cannot read credentials.toml: {0}")]
    Store(String),
}

/// Everything about a profile that is fixed for the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDefinition {
    pub profile_id: String,
    /// Legacy model-id prefix; dispatch matches it through stage 7.
    pub model_provider_prefix: String,
    /// With the catalog kind's request-base convention applied: Ollama ends
    /// in `/v1`, Anthropic is host-only.
    pub base_url: String,
    pub wire: WireKind,
    pub wire_overrides: Vec<WireOverride>,
    pub attach: Attach,
    pub static_headers: Vec<StaticHeader>,
    pub session_header: Option<SessionHeaderKind>,
    pub catalog_kind: CatalogKind,
    pub verify_kind: VerifyKind,
    pub auth_kind: AuthKind,
    pub oauth_flavor: Option<OauthFlavor>,
    pub effort_family: EffortFamily,
}

impl RouteDefinition {
    /// The definition a profile resolves to now (base-URL env is read).
    pub fn from_spec(profile: &ProviderSpec) -> Self {
        definition_for(profile)
    }

    /// The wire for one bare model id: the first matching override, else
    /// the profile's wire.
    pub fn wire_for_model(&self, bare_model: &str) -> WireKind {
        self.wire_overrides
            .iter()
            .find(|entry| entry.matches(bare_model))
            .map(|entry| entry.wire)
            .unwrap_or(self.wire)
    }
}

/// Where a profile's credential comes from. Each variant reads env, the
/// session overlay, and the store on every `current()` call; none does
/// network I/O.
#[derive(Clone)]
pub enum Credential {
    Key(Source),
    /// Like `Key`, but absence is not an error (Ollama).
    OptionalKey(Source),
    Oauth(Source),
    /// A fixed value, for tests and one-off clients.
    Static(Option<CredentialValue>),
}

/// The profile and handle a non-static credential reads through.
#[derive(Clone)]
pub struct Source {
    profile: Arc<ProviderSpec>,
    credentials: Arc<Credentials>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(source) => write!(f, "Key({})", source.profile.id),
            Self::OptionalKey(source) => write!(f, "OptionalKey({})", source.profile.id),
            Self::Oauth(source) => write!(f, "Oauth({})", source.profile.id),
            Self::Static(value) => write!(f, "Static({value:?})"),
        }
    }
}

impl Credential {
    fn for_profile(profile: &ProviderSpec, credentials: &Arc<Credentials>) -> Self {
        let source = Source {
            profile: Arc::new(profile.clone()),
            credentials: Arc::clone(credentials),
        };
        match profile.effective_auth().kind {
            AuthKind::ApiKey => Self::Key(source),
            AuthKind::ApiKeyOptional => Self::OptionalKey(source),
            AuthKind::OauthDevice => Self::Oauth(source),
        }
    }

    /// The credential to attach now, with where it came from.
    pub fn current_with_source(
        &self,
    ) -> Result<Option<(CredentialValue, KeySource)>, CredentialError> {
        match self {
            Self::Static(value) => Ok(value.clone().map(|value| (value, KeySource::Provided))),
            Self::Key(source) | Self::OptionalKey(source) | Self::Oauth(source) => {
                let creds = &source.credentials;
                credential_from_store(
                    &source.profile,
                    creds.store(),
                    creds.session_key(&source.profile),
                )
            }
        }
    }

    /// The credential to attach now. No network I/O.
    pub fn current(&self) -> Result<Option<CredentialValue>, CredentialError> {
        self.current_with_source()
            .map(|found| found.map(|(value, _)| value))
    }

    /// Refresh an OAuth token that expires within five minutes. Blocking and
    /// single-flight; a no-op for keys and fresh tokens. Async callers run
    /// it on a blocking thread.
    pub fn ensure_fresh_blocking(&self) -> Result<(), CredentialError> {
        match self {
            Self::Oauth(source) => source
                .credentials
                .ensure_fresh_blocking(&source.profile)
                .map_err(|error| CredentialError::Store(error.to_string())),
            _ => Ok(()),
        }
    }
}

/// The precedence rules (design §3) over one store and an optional session
/// key. Reading through the caller's store keeps its parse cache.
pub(crate) fn credential_from_store(
    profile: &ProviderSpec,
    store: &crate::store::CredentialStore,
    session_key: Option<String>,
) -> Result<Option<(CredentialValue, KeySource)>, CredentialError> {
    let key = |value: String| CredentialValue {
        secret: value,
        account_id: None,
    };
    let slot = Credentials::slot_for(profile);
    let flagged = || store.needs_authentication(slot).map_err(store_error);
    let auth = profile.effective_auth();
    if auth.is_oauth() {
        if let Some((_, value)) = first_env(&profile.api_key_env) {
            return Ok(Some((key(value), KeySource::Env)));
        }
        let token = store
            .get_oauth(slot)
            .map_err(store_error)?
            .map(|tokens| tokens.access_token.trim().to_string())
            .filter(|token| !token.is_empty() && !token.starts_with("fixture-"));
        let Some(token) = token else {
            return Err(CredentialError::Missing);
        };
        if flagged()? {
            return Err(CredentialError::AuthRequired);
        }
        let account_id = match auth.flavor {
            Some(OauthFlavor::Chatgpt) => Some(
                crate::oauth_chatgpt::account_id_from_token(&token)
                    .map_err(|error| CredentialError::Store(error.to_string()))?,
            ),
            _ => None,
        };
        return Ok(Some((
            CredentialValue {
                secret: token,
                account_id,
            },
            KeySource::Oauth,
        )));
    }
    if let Some(value) = session_key {
        return Ok(Some((key(value.trim().into()), KeySource::Provided)));
    }
    if let Some((_, value)) = first_env(&env_names_for(profile).0) {
        return Ok(Some((key(value), KeySource::Env)));
    }
    // The target slot, then the profile's own old slot (Zen two-slot read).
    let nonblank = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    let mut stored = nonblank(store.get_api_key(slot).map_err(store_error)?);
    if stored.is_none() && slot != profile.id {
        stored = nonblank(store.get_api_key(&profile.id).map_err(store_error)?);
    }
    match stored {
        Some(_) if flagged()? => Err(CredentialError::AuthRequired),
        Some(value) => Ok(Some((key(value.trim().into()), KeySource::File))),
        None if auth.kind == AuthKind::ApiKeyOptional => Ok(None),
        None => Err(CredentialError::Missing),
    }
}

fn store_error(error: crate::store::StoreError) -> CredentialError {
    CredentialError::Store(error.to_string())
}

/// One profile in the snapshot.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub definition: RouteDefinition,
    pub credential: Credential,
}

#[derive(Debug, Default)]
struct Inner {
    profiles: BTreeMap<String, ResolvedProfile>,
    credential_env_names: Vec<String>,
}

/// An immutable snapshot of every profile's route. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct RouteTable(Arc<Inner>);

impl RouteTable {
    /// A one-profile table, for tests and one-off clients.
    pub fn single(definition: RouteDefinition, credential: Credential) -> Self {
        let mut inner = Inner::default();
        inner.profiles.insert(
            definition.profile_id.clone(),
            ResolvedProfile {
                definition,
                credential,
            },
        );
        Self(Arc::new(inner))
    }

    pub fn get(&self, profile_id: &str) -> Option<&ResolvedProfile> {
        self.0.profiles.get(profile_id)
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ResolvedProfile> {
        self.0.profiles.values()
    }

    /// Pick the profile for a request: the model id's prefix is the profile
    /// id (design §4). Returns the profile and the bare model id; `None` for
    /// an unprefixed id or a prefix no profile has.
    pub fn profile_for_request<'a>(&self, model: &'a str) -> Option<(&ResolvedProfile, &'a str)> {
        let (prefix, bare) = model.split_once('/')?;
        self.get(prefix).map(|found| (found, bare))
    }

    /// Every env name a child process must not see: each key profile's
    /// generated name and every `api_key_env` name.
    pub fn credential_env_names(&self) -> &[String] {
        &self.0.credential_env_names
    }
}

/// Build the snapshot. Base-URL env vars are read now.
pub fn resolve(registry: &ConnectRegistry, credentials: &Arc<Credentials>) -> RouteTable {
    let mut inner = Inner {
        credential_env_names: crate::registry::credential_env_names(registry),
        ..Inner::default()
    };
    for profile in registry.profiles() {
        let definition = definition_for(profile);
        inner.profiles.insert(
            profile.id.clone(),
            ResolvedProfile {
                definition,
                credential: Credential::for_profile(profile, credentials),
            },
        );
    }
    RouteTable(Arc::new(inner))
}

fn definition_for(profile: &ProviderSpec) -> RouteDefinition {
    let auth = profile.effective_auth();
    let wire = profile.effective_wire();
    let catalog_kind = profile.effective_catalog_kind();
    let attach = profile.attach.unwrap_or(match wire {
        WireKind::AnthropicMessages => Attach::XApiKey,
        _ => Attach::Bearer,
    });
    let base = first_env(&env_names_for(profile).1)
        .map(|(_, value)| value)
        .unwrap_or_else(|| profile.base_url.trim().to_string());
    RouteDefinition {
        profile_id: profile.id.clone(),
        model_provider_prefix: profile.model_provider_prefix.clone(),
        base_url: request_base(catalog_kind, &base),
        wire,
        wire_overrides: profile.wire_overrides.clone(),
        attach,
        static_headers: profile.static_headers.clone(),
        session_header: profile.session_header,
        catalog_kind,
        verify_kind: profile.verify.kind,
        auth_kind: auth.kind,
        oauth_flavor: auth.flavor,
        effort_family: profile.effort_family,
    }
}

/// The catalog kind's request-base convention: Ollama requests go to
/// `<base>/v1`, Anthropic's base is host-only.
fn request_base(kind: CatalogKind, base: &str) -> String {
    let base = base.trim_end_matches('/');
    match kind {
        CatalogKind::OllamaTags if !base.ends_with("/v1") => format!("{base}/v1"),
        CatalogKind::AnthropicModels => base.strip_suffix("/v1").unwrap_or(base).to_string(),
        _ => base.to_string(),
    }
}

/// `(key env names, base-URL env names)` in precedence order.
pub fn env_names_for(profile: &ProviderSpec) -> (Vec<String>, Vec<String>) {
    let id = env_profile_id(&profile.id);
    let key_names = if profile.effective_auth().is_oauth() {
        profile.api_key_env.clone()
    } else {
        let mut names = vec![format!("FORGE_{id}_API_KEY")];
        if let Some(target) = &profile.credential_from {
            names.push(format!("FORGE_{}_API_KEY", env_profile_id(target)));
        }
        names.extend(profile.api_key_env.iter().cloned());
        names
    };
    let mut base_names = vec![format!("FORGE_{id}_BASE_URL")];
    base_names.extend(profile.base_url_env.iter().cloned());
    (key_names, base_names)
}

/// The env var that currently supplies the profile's credential, for the
/// "set by `$VAR`; unset it to disconnect" text.
pub fn winning_env_var(registry: &ConnectRegistry, profile_id: &str) -> Option<String> {
    let profile = registry.get(profile_id)?;
    first_env(&env_names_for(profile).0).map(|(name, _)| name)
}

/// The first named env var with a non-blank value, trimmed.
fn first_env(names: &[String]) -> Option<(String, String)> {
    names.iter().find_map(|name| {
        let value = std::env::var(name).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| (name.clone(), value.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CredentialStore;
    use crate::test_env::EnvGuard;
    use tempfile::tempdir;

    const ENV: &[&str] = &[
        "FORGE_OPENAI_API_KEY",
        "OPENAI_API_KEY",
        "FORGE_OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "FORGE_OPENCODE_ZEN_API_KEY",
        "FORGE_OPENCODE_GO_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "OPENCODE_API_KEY",
        "OPENCODE_GO_API_KEY",
        "XAI_API_KEY",
        "GROK_CODE_XAI_API_KEY",
        "FORGE_OLLAMA_API_KEY",
        "OLLAMA_API_KEY",
        "FORGE_OLLAMA_BASE_URL",
        "OLLAMA_API_BASE",
        "FORGE_ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_BASE",
    ];

    fn table(dir: &tempfile::TempDir) -> (RouteTable, Arc<Credentials>) {
        let creds = Arc::new(Credentials::new(CredentialStore::new(
            dir.path().join("credentials.toml"),
        )));
        (resolve(&crate::builtin_registry(), &creds), creds)
    }

    fn secret(table: &RouteTable, id: &str) -> Result<Option<String>, CredentialError> {
        table
            .get(id)
            .unwrap()
            .credential
            .current()
            .map(|value| value.map(|value| value.secret))
    }

    #[test]
    fn key_precedence_follows_each_row() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, creds) = table(&dir);
        let openai = crate::openai::openai_profile();
        assert_eq!(secret(&table, "openai"), Err(CredentialError::Missing));
        creds.set_api_key(&openai, " stored ").unwrap();
        assert_eq!(secret(&table, "openai").unwrap().as_deref(), Some("stored"));
        env.set("OPENAI_API_KEY", "vendor");
        assert_eq!(secret(&table, "openai").unwrap().as_deref(), Some("vendor"));
        env.set("FORGE_OPENAI_API_KEY", "  generated  ");
        assert_eq!(
            secret(&table, "openai").unwrap().as_deref(),
            Some("generated")
        );
        creds.set_session_key(&openai, "typed");
        assert_eq!(secret(&table, "openai").unwrap().as_deref(), Some("typed"));
    }

    #[test]
    fn blank_env_values_are_skipped() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, _) = table(&dir);
        env.set("FORGE_OPENAI_API_KEY", "   ");
        env.set("OPENAI_API_KEY", "vendor");
        assert_eq!(secret(&table, "openai").unwrap().as_deref(), Some("vendor"));
        assert_eq!(
            winning_env_var(&crate::builtin_registry(), "openai").as_deref(),
            Some("OPENAI_API_KEY")
        );
    }

    #[test]
    fn a_flagged_stored_credential_is_auth_required_but_env_is_not() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, creds) = table(&dir);
        let openai = crate::openai::openai_profile();
        creds.set_api_key(&openai, "stored").unwrap();
        creds.report_auth_failure(&openai).unwrap();
        assert_eq!(secret(&table, "openai"), Err(CredentialError::AuthRequired));
        env.set("OPENAI_API_KEY", "vendor");
        assert_eq!(secret(&table, "openai").unwrap().as_deref(), Some("vendor"));
    }

    #[test]
    fn zen_reads_go_names_and_slot_then_its_own_old_slot() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, creds) = table(&dir);
        creds
            .store()
            .set_api_key("opencode_zen", "old-zen")
            .unwrap();
        assert_eq!(
            secret(&table, "opencode_zen").unwrap().as_deref(),
            Some("old-zen")
        );
        creds.store().set_api_key("opencode_go", "go").unwrap();
        assert_eq!(
            secret(&table, "opencode_zen").unwrap().as_deref(),
            Some("go")
        );
        env.set("FORGE_OPENCODE_GO_API_KEY", "go-env");
        assert_eq!(
            secret(&table, "opencode_zen").unwrap().as_deref(),
            Some("go-env")
        );
        env.set("FORGE_OPENCODE_ZEN_API_KEY", "zen-env");
        assert_eq!(
            secret(&table, "opencode_zen").unwrap().as_deref(),
            Some("zen-env")
        );
    }

    #[test]
    fn oauth_reads_declared_env_then_the_stored_token_never_a_fixture() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, creds) = table(&dir);
        let xai = crate::xai::xai_grok_profile();
        let token = |access: &str| crate::OauthTokens {
            access_token: access.into(),
            refresh_token: None,
            expires_at: None,
        };
        creds
            .set_oauth(&xai, token("fixture-access-token"))
            .unwrap();
        assert_eq!(secret(&table, "xai"), Err(CredentialError::Missing));
        creds.set_oauth(&xai, token("real")).unwrap();
        assert_eq!(secret(&table, "xai").unwrap().as_deref(), Some("real"));
        env.set("GROK_CODE_XAI_API_KEY", "env-key");
        assert_eq!(secret(&table, "xai").unwrap().as_deref(), Some("env-key"));
    }

    #[test]
    fn optional_keys_may_be_absent() {
        let _env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, _) = table(&dir);
        assert_eq!(secret(&table, "ollama"), Ok(None));
    }

    #[test]
    fn base_urls_follow_env_then_definition_with_kind_conventions() {
        let env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let base =
            |table: &RouteTable, id: &str| table.get(id).unwrap().definition.base_url.clone();
        let (table, _) = table(&dir);
        assert_eq!(base(&table, "ollama"), "http://localhost:11434/v1");
        assert_eq!(base(&table, "anthropic"), "https://api.anthropic.com");
        assert_eq!(base(&table, "openai"), "https://api.openai.com/v1");

        env.set("OLLAMA_API_BASE", "http://10.0.0.5:11434/");
        env.set("ANTHROPIC_API_BASE", "https://proxy.example/v1");
        env.set("OPENAI_API_BASE", "https://vendor.example/v1");
        env.set("FORGE_OPENAI_BASE_URL", "https://generated.example/v1");
        let (table, _) = super::tests::table(&dir);
        assert_eq!(base(&table, "ollama"), "http://10.0.0.5:11434/v1");
        assert_eq!(base(&table, "anthropic"), "https://proxy.example");
        assert_eq!(base(&table, "openai"), "https://generated.example/v1");
    }

    #[test]
    fn requests_pick_the_profile_their_prefix_names() {
        let _env = EnvGuard::new(ENV);
        let dir = tempdir().unwrap();
        let (table, _) = table(&dir);
        let pick = |model: &str| {
            table
                .profile_for_request(model)
                .map(|(found, bare)| (found.definition.profile_id.clone(), bare.to_string()))
        };
        assert_eq!(
            pick("openai_codex/gpt-5"),
            Some(("openai_codex".into(), "gpt-5".into()))
        );
        assert_eq!(
            pick("my-org/org/model"),
            None,
            "an unknown prefix serves nothing"
        );
        // Old prefixes and unprefixed ids no longer route.
        assert_eq!(pick("grok/grok-4"), None);
        assert_eq!(pick("opencode-go/kimi"), None);
        assert_eq!(pick("gpt-4.1"), None);
    }

    #[test]
    fn debug_output_never_shows_a_secret() {
        let value = CredentialValue {
            secret: "sk-secret".into(),
            account_id: Some("acct-secret".into()),
        };
        let table = RouteTable::single(
            definition_for(&crate::openai::openai_profile()),
            Credential::Static(Some(value.clone())),
        );
        let text = format!("{table:?} {value:?}");
        assert!(
            !text.contains("sk-secret") && !text.contains("acct-secret"),
            "{text}"
        );
    }

    #[test]
    fn credential_env_names_cover_generated_names_and_aliases() {
        let dir = tempdir().unwrap();
        let (table, _) = table(&dir);
        let names = table.credential_env_names();
        for name in [
            "FORGE_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "FORGE_OPENCODE_ZEN_API_KEY",
            "XAI_API_KEY",
        ] {
            assert!(names.iter().any(|n| n == name), "{name}");
        }
        assert!(!names.iter().any(|n| n == "FORGE_XAI_API_KEY"));
    }
}
