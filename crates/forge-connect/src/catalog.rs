//! Remote model catalog fetch + on-disk cache (connect-command.md §3.5).
//!
//! After `/connect`, Forge can list live models from each provider and feed `/model`.
//! A models.dev registry supplies durable public metadata; provider catalogs remain
//! account-specific availability signals.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::profile::ConnectProfile;
use crate::store::{resolve_key, CredentialStore};

/// Default TTL for cached catalog entries (1 hour).
pub const DEFAULT_TTL_SECS: u64 = 3600;
/// Public registry data changes less frequently than a provider's account catalog.
pub const MODELS_DEV_TTL_SECS: u64 = 24 * 60 * 60;
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

use thiserror::Error;

/// A model-catalogue operation failed.
///
/// These operations previously returned `Result<_, String>`, so a caller could
/// not tell an expired credential from a rate limit or an unreachable host
/// without matching on message text. `status` is now structural.
///
/// `label` carries the operator-facing prefix each message already had, so
/// `Display` output is unchanged.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CatalogError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// A credential the caller did not supply is needed to list models.
    #[error("{0}")]
    CredentialRequired(String),
    /// No remote catalogue is implemented for this profile.
    #[error("no remote catalog implemented for profile `{0}`")]
    UnsupportedProfile(String),
    /// Server returned a non-success status.
    #[error("{label}: HTTP {status}{}", .body.as_deref().map(|b| format!(" {b}")).unwrap_or_default())]
    Http {
        label: String,
        status: u16,
        body: Option<String>,
    },
    /// Could not reach the server.
    #[error("{label}: {detail}")]
    Transport { label: String, detail: String },
    /// Reached the server but could not decode the response.
    #[error("{label}: {detail}")]
    Decode { label: String, detail: String },
    #[error("{label}: too many redirects")]
    TooManyRedirects { label: String },
    /// No structured category applies. Kept deliberately narrow — nothing
    /// branches on these, they are operator-facing detail only.
    #[error("{0}")]
    Message(String),
}

impl CatalogError {
    /// HTTP status the server returned, when there was one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The credential was refused — re-authenticating is the fix.
    pub fn is_auth_failure(&self) -> bool {
        match self {
            Self::CredentialRequired(_) => true,
            Self::Http { status, .. } => *status == 401 || *status == 403,
            _ => false,
        }
    }

    /// Transient — a retry may succeed without user action.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Http { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogSource {
    /// Directly returned by the connected provider for this account.
    Live,
    /// Previously returned by the connected provider for this account.
    Cached,
    /// Public models.dev metadata; an account may still lack access.
    Registry,
    /// Built-in emergency fallback when no catalog data is available.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Native provider/model string, e.g. `openai/gpt-4.1-mini`.
    pub id: String,
    pub profile_id: String,
    pub source: CatalogSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CatalogCost {
    pub input: f64,
    pub output: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogFile {
    /// profile_id → fetched provider/model ids
    #[serde(default)]
    models: BTreeMap<String, Vec<String>>,
    /// profile_id → unix secs when last refreshed
    #[serde(default)]
    fetched_at: BTreeMap<String, u64>,
    /// profile_id → public models.dev model ids, namespaced for Forge routing.
    #[serde(default)]
    registry_models: BTreeMap<String, Vec<String>>,
    /// provider/model id → dollars per million tokens.
    #[serde(default)]
    registry_costs: BTreeMap<String, CatalogCost>,
    /// Unix seconds when the public registry was refreshed.
    #[serde(default)]
    registry_fetched_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ModelCatalogCache {
    path: PathBuf,
    ttl_secs: u64,
}

impl ModelCatalogCache {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }

    pub fn user_default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("forge")
            .join("model-catalog.toml");
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn with_ttl(mut self, ttl_secs: u64) -> Self {
        self.ttl_secs = ttl_secs.max(1);
        self
    }

    fn load(&self) -> CatalogFile {
        if !self.path.exists() {
            return CatalogFile::default();
        }
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, file: &CatalogFile) -> Result<(), CatalogError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(file)?;
        fs::write(&self.path, text)?;
        Ok(())
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub fn is_fresh(&self, profile_id: &str) -> bool {
        let file = self.load();
        match file.fetched_at.get(profile_id) {
            Some(&t) => Self::now_secs().saturating_sub(t) < self.ttl_secs,
            None => false,
        }
    }

    pub fn get_cached(&self, profile_id: &str) -> Vec<String> {
        self.load()
            .models
            .get(profile_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn put(&self, profile_id: &str, models: Vec<String>) -> Result<(), CatalogError> {
        let mut file = self.load();
        file.models.insert(profile_id.to_string(), models);
        file.fetched_at
            .insert(profile_id.to_string(), Self::now_secs());
        self.save(&file)
    }

    pub fn clear_profile(&self, profile_id: &str) -> Result<(), CatalogError> {
        let mut file = self.load();
        file.models.remove(profile_id);
        file.fetched_at.remove(profile_id);
        self.save(&file)
    }

    pub fn registry_is_fresh(&self) -> bool {
        self.load()
            .registry_fetched_at
            .is_some_and(|t| Self::now_secs().saturating_sub(t) < MODELS_DEV_TTL_SECS)
    }

    pub fn get_registry_cached(&self, profile_id: &str) -> Vec<String> {
        self.load()
            .registry_models
            .get(profile_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_registry_cost(&self, model_id: &str) -> Option<CatalogCost> {
        self.load().registry_costs.get(model_id).copied()
    }

    fn put_registry(
        &self,
        models: BTreeMap<String, Vec<String>>,
        costs: BTreeMap<String, CatalogCost>,
    ) -> Result<(), CatalogError> {
        let mut file = self.load();
        file.registry_models = models;
        file.registry_costs = costs;
        file.registry_fetched_at = Some(Self::now_secs());
        self.save(&file)
    }
}

/// Refresh the public models.dev fallback registry for the configured Forge profiles.
/// The registry is intentionally metadata-only: it never claims a model is entitled
/// for the current account, which remains the job of a provider's live catalog.
pub fn refresh_models_dev_registry(
    profiles: &[ConnectProfile],
    cache: &ModelCatalogCache,
) -> Result<usize, CatalogError> {
    let ua = format!("forge-connect/{}", env!("CARGO_PKG_VERSION"));
    let models_dev_url =
        std::env::var("FORGE_MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
    let body: serde_json::Value = ureq::get(&models_dev_url)
        .set("User-Agent", &ua)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| CatalogError::Transport {
            label: "models.dev registry".into(),
            detail: e.to_string(),
        })?
        .into_json()
        .map_err(|e| CatalogError::Decode {
            label: "models.dev registry JSON".into(),
            detail: e.to_string(),
        })?;

    let mut by_profile = BTreeMap::new();
    let mut costs = BTreeMap::new();
    let mut total = 0usize;
    for profile in profiles {
        let mut ids = std::collections::BTreeSet::new();
        for provider_id in &profile.models_dev_providers {
            let Some(models) = body
                .get(provider_id)
                .and_then(|provider| provider.get("models"))
                .and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            for (model_id, model) in models {
                if models_dev_model_is_agent_compatible(model) {
                    let id = format!("{}/{}", profile.model_provider_prefix, model_id);
                    if let Some(cost) = models_dev_cost(model) {
                        costs.insert(id.clone(), cost);
                    }
                    ids.insert(id);
                }
            }
        }
        total += ids.len();
        by_profile.insert(profile.id.clone(), ids.into_iter().collect());
    }
    cache.put_registry(by_profile, costs)?;
    Ok(total)
}

fn models_dev_cost(model: &serde_json::Value) -> Option<CatalogCost> {
    let cost = model.get("cost")?;
    Some(CatalogCost {
        input: cost.get("input").and_then(serde_json::Value::as_f64)?,
        output: cost.get("output").and_then(serde_json::Value::as_f64)?,
    })
}

fn models_dev_model_is_agent_compatible(model: &serde_json::Value) -> bool {
    if model.get("status").and_then(|v| v.as_str()) == Some("deprecated") {
        return false;
    }
    if model.get("tool_call").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    model
        .pointer("/modalities/output")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|output| output.iter().any(|m| m.as_str() == Some("text")))
}

/// Resolve API key for catalog HTTP (env or store). OAuth access token for xAI.
pub fn credential_for_catalog(profile: &ConnectProfile, store: &CredentialStore) -> Option<String> {
    if profile.auth_mode.is_oauth() {
        return store
            .get_oauth(&profile.id)
            .ok()
            .flatten()
            .map(|t| t.access_token)
            .filter(|s| {
                let s = s.trim();
                !s.is_empty() && !s.starts_with("fixture-") && s != "fixture-access-token"
            });
    }
    resolve_key(&profile.api_key_env, &profile.id, store)
        .ok()
        .flatten()
        .map(|(k, _)| k)
}

/// Fetch remote model ids and rewrite to provider/model strings. Network call.
pub fn fetch_remote_models(
    profile: &ConnectProfile,
    api_key: Option<&str>,
) -> Result<Vec<String>, CatalogError> {
    let base = profile
        .default_base_url
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches('/');
    let prefix = profile.model_provider_prefix.as_str();
    let ua = format!("forge-connect/{}", env!("CARGO_PKG_VERSION"));

    match profile.id.as_str() {
        "openai_codex" => {
            let token = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("OpenAI Codex login required for catalog".into())
            })?;
            let account_id = crate::openai_codex::account_id_from_token(token)
                .map_err(|e| CatalogError::CredentialRequired(e.to_string()))?;
            let url = format!(
                "{}/codex/models?client_version={}",
                if base.is_empty() {
                    crate::openai_codex::DEFAULT_BASE_URL
                } else {
                    base
                },
                env!("CARGO_PKG_VERSION")
            );
            let raw = http_get_json_ids(
                &url,
                &[
                    ("Authorization", &format!("Bearer {token}")),
                    ("chatgpt-account-id", &account_id),
                    ("User-Agent", &ua),
                ],
            );
            let raw = match raw {
                Ok(models) if !models.is_empty() => models,
                // Codex's endpoint can temporarily return an empty response for a
                // non-Codex client version. Its own account-scoped cache remains a
                // useful, conservative fallback when the CLI is installed.
                Ok(_) | Err(_) => codex_cli_cached_model_ids(),
            };
            Ok(map_prefix(prefix, raw, |id| {
                !id.eq_ignore_ascii_case("codex-auto-review")
            }))
        }
        "openai" => {
            let key = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("OpenAI API key required for catalog".into())
            })?;
            let url = format!(
                "{}/models",
                if base.is_empty() {
                    "https://api.openai.com/v1"
                } else {
                    base
                }
            );
            let raw = http_get_json_ids(
                &url,
                &[
                    ("Authorization", &format!("Bearer {key}")),
                    ("User-Agent", &ua),
                ],
            )?;
            Ok(map_prefix(prefix, raw, filter_openai_chat_ish))
        }
        "anthropic" => {
            let key = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("Anthropic API key required for catalog".into())
            })?;
            let b = if base.is_empty() {
                "https://api.anthropic.com"
            } else {
                base
            };
            // Anthropic list endpoints may paginate; ask for a large page size when supported.
            // If the API rejects query params, fall back to the plain endpoint.
            let url_big = format!("{b}/v1/models?limit=1000");
            let url_plain = format!("{b}/v1/models");
            let raw = match http_get_json_ids(
                &url_big,
                &[
                    ("x-api-key", key),
                    ("anthropic-version", "2023-06-01"),
                    ("User-Agent", &ua),
                ],
            ) {
                Ok(v) => v,
                Err(e) => {
                    // Retry without query params on strict servers. This used to
                    // match on "HTTP 400" inside the rendered message; the status
                    // is now structural.
                    if e.status() == Some(400) {
                        http_get_json_ids(
                            &url_plain,
                            &[
                                ("x-api-key", key),
                                ("anthropic-version", "2023-06-01"),
                                ("User-Agent", &ua),
                            ],
                        )?
                    } else {
                        return Err(e);
                    }
                }
            };
            Ok(map_prefix(prefix, raw, |_| true))
        }
        "ollama" => {
            let b = if base.is_empty() {
                "http://localhost:11434"
            } else {
                base
            };
            // Native tags API
            let tags_url = format!("{b}/api/tags");
            match http_get_ollama_names(&tags_url, &ua) {
                Ok(names) if !names.is_empty() => Ok(map_prefix(prefix, names, |_| true)),
                _ => {
                    let url = format!("{b}/v1/models");
                    let raw = http_get_json_ids(&url, &[("User-Agent", &ua)])?;
                    Ok(map_prefix(prefix, raw, |_| true))
                }
            }
        }
        "opencode_go" => {
            let key = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("OpenCode Go API key required for catalog".into())
            })?;
            let b = if base.is_empty() {
                crate::opencode_go::DEFAULT_BASE_URL
            } else {
                base
            };
            let url = format!("{b}/models");
            let raw = http_get_json_ids(
                &url,
                &[
                    ("Authorization", &format!("Bearer {key}")),
                    ("User-Agent", &ua),
                ],
            )?;
            // Worker rewrites opencode-go/* → openai/* + base
            Ok(map_prefix("opencode-go", raw, |_| true))
        }
        "opencode_zen" => {
            let key = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("OpenCode Zen API key required for catalog".into())
            })?;
            let b = if base.is_empty() {
                crate::opencode_zen::DEFAULT_BASE_URL
            } else {
                base
            };
            let url = format!("{b}/models");
            let raw = http_get_json_ids(
                &url,
                &[
                    ("Authorization", &format!("Bearer {key}")),
                    ("User-Agent", &ua),
                ],
            )?;
            Ok(map_prefix("opencode-zen", raw, |_| true))
        }
        "xai" => {
            let key = api_key.ok_or_else(|| {
                CatalogError::CredentialRequired("xAI OAuth token required for catalog".into())
            })?;
            let b = if base.is_empty() {
                "https://api.x.ai/v1"
            } else {
                base
            };
            let url = format!("{b}/models");
            let raw = http_get_json_ids(
                &url,
                &[
                    ("Authorization", &format!("Bearer {key}")),
                    ("User-Agent", &ua),
                ],
            )?;
            Ok(map_prefix(prefix, raw, |_| true))
        }
        other => Err(CatalogError::UnsupportedProfile(other.to_string())),
    }
}

fn filter_openai_chat_ish(id: &str) -> bool {
    // Drop obvious non-chat endpoints to keep the picker usable.
    let l = id.to_ascii_lowercase();
    if l.contains("embedding")
        || l.contains("whisper")
        || l.contains("tts")
        || l.contains("dall-e")
        || l.contains("davinci")
        || l.contains("babbage")
        || l.contains("moderation")
        || l.starts_with("ft:")
    {
        return false;
    }
    true
}

fn map_prefix(prefix: &str, raw: Vec<String>, keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = raw
        .into_iter()
        .filter(|id| keep(id))
        .map(|id| {
            let id = id.trim().to_string();
            if id.contains('/') {
                id
            } else {
                format!("{prefix}/{id}")
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn http_get_json_ids(url: &str, headers: &[(&str, &str)]) -> Result<Vec<String>, CatalogError> {
    // ureq's built-in redirect handling may drop auth headers on redirect in some cases.
    // We follow redirects manually to ensure credentials are preserved.
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .redirects(0)
        .build();

    let mut cur = url.to_string();
    for _ in 0..4 {
        let mut req = agent.get(&cur);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) if matches!(code, 301 | 302 | 303 | 307 | 308) => {
                if let Some(loc) = r.header("Location").map(str::to_string) {
                    cur = loc;
                    continue;
                }
                return Err(CatalogError::Http {
                    label: format!("catalog GET {cur}"),
                    status: code,
                    body: Some("(missing Location)".into()),
                });
            }
            Err(ureq::Error::Status(code, r)) => {
                // Best-effort include small body to help operator debug auth failures.
                let body = r
                    .into_string()
                    .unwrap_or_default()
                    .chars()
                    .take(240)
                    .collect::<String>();
                let body = body.trim().to_string();
                return Err(CatalogError::Http {
                    label: format!("catalog GET {cur}"),
                    status: code,
                    body: (!body.is_empty()).then_some(body),
                });
            }
            Err(e) => {
                return Err(CatalogError::Transport {
                    label: format!("catalog GET {cur}"),
                    detail: e.to_string(),
                })
            }
        };

        if !(200..300).contains(&resp.status()) {
            return Err(CatalogError::Http {
                label: format!("catalog GET {cur}"),
                status: resp.status(),
                body: None,
            });
        }
        let body: serde_json::Value = resp.into_json().map_err(|e| CatalogError::Decode {
            label: "catalog JSON".into(),
            detail: e.to_string(),
        })?;
        return parse_openai_style_model_ids(&body);
    }
    Err(CatalogError::TooManyRedirects {
        label: format!("catalog GET {url}"),
    })
}

fn parse_openai_style_model_ids(body: &serde_json::Value) -> Result<Vec<String>, CatalogError> {
    // OpenAI / Anthropic / OpenCode: { "data": [ { "id": "..." }, ... ] }
    if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        let mut ids = Vec::new();
        for item in arr {
            if let Some(id) = item
                .get("id")
                .or_else(|| item.get("slug"))
                .and_then(|v| v.as_str())
            {
                ids.push(id.to_string());
            } else if let Some(id) = item.as_str() {
                ids.push(id.to_string());
            }
        }
        return Ok(ids);
    }
    // Some gateways: { "models": [ ... ] }
    if let Some(arr) = body.get("models").and_then(|d| d.as_array()) {
        let mut ids = Vec::new();
        for item in arr {
            if let Some(id) = item
                .get("id")
                .or_else(|| item.get("slug"))
                .and_then(|v| v.as_str())
            {
                ids.push(id.to_string());
            } else if let Some(id) = item.as_str() {
                ids.push(id.to_string());
            }
        }
        return Ok(ids);
    }
    Err(CatalogError::Message(
        "catalog response missing data[] models".into(),
    ))
}

/// Read the account-scoped model list cached by the official Codex CLI.
///
/// This is deliberately only a fallback for the OpenAI Codex subscription
/// profile. Unlike models.dev, it reflects the models offered to the signed-in
/// ChatGPT account and preserves the server's ordering through `priority`.
fn codex_cli_cached_model_ids() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".codex").join("models_cache.json");
    codex_cli_cached_model_ids_at(&path)
}

fn codex_cli_cached_model_ids_at(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    parse_codex_cli_cached_model_ids(&value)
}

fn parse_codex_cli_cached_model_ids(body: &serde_json::Value) -> Vec<String> {
    let Some(models) = body.get("models").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut rows: Vec<(u64, String)> = models
        .iter()
        .filter(|model| model.get("visibility").and_then(|v| v.as_str()) == Some("list"))
        .filter_map(|model| {
            let slug = model.get("slug")?.as_str()?.trim();
            (!slug.is_empty()).then(|| {
                (
                    model
                        .get("priority")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(u64::MAX),
                    slug.to_string(),
                )
            })
        })
        .collect();
    rows.sort();
    rows.into_iter().map(|(_, slug)| slug).collect()
}

fn http_get_ollama_names(url: &str, ua: &str) -> Result<Vec<String>, CatalogError> {
    let resp = ureq::get(url)
        .set("User-Agent", ua)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| CatalogError::Transport {
            label: "ollama tags".into(),
            detail: e.to_string(),
        })?;
    if !(200..300).contains(&resp.status()) {
        return Err(CatalogError::Http {
            label: "ollama tags".into(),
            status: resp.status(),
            body: None,
        });
    }
    let body: serde_json::Value = resp.into_json().map_err(|e| CatalogError::Decode {
        label: "ollama tags JSON".into(),
        detail: e.to_string(),
    })?;
    Ok(parse_ollama_model_names(&body))
}

fn parse_ollama_model_names(body: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(arr) = body.get("models").and_then(|m| m.as_array()) {
        for m in arr {
            if let Some(n) = m.get("name").and_then(|v| v.as_str()) {
                names.push(n.to_string());
            }
        }
    }
    names
}

/// Refresh one profile into the cache. On failure, keep old cache and return Err.
pub fn refresh_profile_catalog(
    profile: &ConnectProfile,
    store: &CredentialStore,
    cache: &ModelCatalogCache,
) -> Result<Vec<String>, CatalogError> {
    let key = credential_for_catalog(profile, store);
    let models = fetch_remote_models(profile, key.as_deref())?;
    if models.is_empty() {
        return Err(CatalogError::Message(format!(
            "catalog for `{}` returned no models",
            profile.id
        )));
    }
    cache.put(&profile.id, models.clone())?;
    Ok(models)
}

/// Models for the picker: account-returned rows first, supplemented by models.dev
/// public metadata, then the profile's minimal built-in fallback.
pub fn models_for_picker(
    profiles: &[ConnectProfile],
    store: &CredentialStore,
    cache: &ModelCatalogCache,
    refresh_stale: bool,
) -> Vec<CatalogEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    if refresh_stale && !cache.registry_is_fresh() {
        // Registry availability must never prevent opening the picker. The existing
        // cache and built-ins below remain usable offline.
        let _ = refresh_models_dev_registry(profiles, cache);
    }

    for p in profiles {
        let mut entries = Vec::new();
        let is_ollama = p.id == crate::ollama::PROFILE_ID;
        if refresh_stale && (is_ollama || !cache.is_fresh(&p.id)) {
            if let Ok(m) = refresh_profile_catalog(p, store, cache) {
                entries.extend(m.into_iter().map(|id| (id, CatalogSource::Live)));
            }
        }
        if entries.is_empty() && !(is_ollama && refresh_stale) {
            entries.extend(
                cache
                    .get_cached(&p.id)
                    .into_iter()
                    .map(|id| (id, CatalogSource::Cached)),
            );
        }
        if p.id == crate::openai_codex::PROFILE_ID {
            // Keep a previously refreshed Forge cache useful, but supplement it
            // with the official CLI's current account-scoped cache. This avoids
            // waiting for the Forge cache TTL after a temporary empty endpoint
            // response.
            entries.extend(
                map_prefix(
                    &p.model_provider_prefix,
                    codex_cli_cached_model_ids(),
                    |id| !id.eq_ignore_ascii_case("codex-auto-review"),
                )
                .into_iter()
                .map(|id| (id, CatalogSource::Cached)),
            );
        }
        if !is_ollama && !p.models_dev_providers.is_empty() {
            // Provider catalog rows come first; append the opted-in public
            // registry only as explicitly-unverified supplemental metadata.
            entries.extend(
                cache
                    .get_registry_cached(&p.id)
                    .into_iter()
                    .map(|id| (id, CatalogSource::Registry)),
            );
        }
        if entries.is_empty() && !is_ollama {
            entries.extend(
                p.default_models
                    .iter()
                    .cloned()
                    .map(|id| (id, CatalogSource::Default)),
            );
        }
        for (id, source) in entries {
            if seen.insert(id.clone()) {
                out.push(CatalogEntry {
                    id,
                    profile_id: p.id.clone(),
                    source,
                });
            }
        }
    }

    out
}

/// Normalize a `/model` argument into a provider/model string.
///
/// Accepts:
/// - `openai/gpt-4.1-mini`
/// - `openai` + `gpt-4.1-mini` → `openai/gpt-4.1-mini`
/// - bare `gpt-4.1-mini` with optional default prefix
pub fn normalize_model_id(
    first: &str,
    second: Option<&str>,
    default_prefix: Option<&str>,
) -> String {
    let a = first.trim();
    let b = second.map(str::trim).filter(|s| !s.is_empty());
    if let Some(b) = b {
        if a.contains('/') {
            // `/model openai/gpt-4.1 extra` → keep first token only
            a.to_string()
        } else {
            format!("{a}/{b}")
        }
    } else if a.contains('/') {
        a.to_string()
    } else if let Some(p) = default_prefix.filter(|s| !s.is_empty()) {
        format!("{p}/{a}")
    } else {
        a.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OauthTokens;
    use crate::openai::openai_profile;
    use forge_test_support::mock_http;
    use tempfile::tempdir;

    #[test]
    fn normalize_model_id_variants() {
        assert_eq!(
            normalize_model_id("openai/gpt-4.1", None, None),
            "openai/gpt-4.1"
        );
        assert_eq!(
            normalize_model_id("openai", Some("gpt-4.1"), None),
            "openai/gpt-4.1"
        );
        assert_eq!(
            normalize_model_id("llama3.2", None, Some("ollama")),
            "ollama/llama3.2"
        );
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml")).with_ttl(3600);
        cache
            .put("openai", vec!["openai/a".into(), "openai/b".into()])
            .unwrap();
        assert_eq!(cache.get_cached("openai").len(), 2);
        assert!(cache.is_fresh("openai"));
    }

    #[test]
    fn cache_handles_missing_malformed_clear_and_ttl_clamp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("catalog.toml");
        let cache = ModelCatalogCache::new(path.clone()).with_ttl(0);

        assert_eq!(cache.path(), path.as_path());
        assert_eq!(cache.get_cached("openai"), Vec::<String>::new());
        assert!(!cache.is_fresh("openai"));
        assert!(!cache.registry_is_fresh());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not = [valid toml").unwrap();
        assert_eq!(cache.get_cached("openai"), Vec::<String>::new());

        cache.put("openai", vec!["openai/gpt-test".into()]).unwrap();
        assert_eq!(cache.get_cached("openai"), vec!["openai/gpt-test"]);
        assert!(cache.is_fresh("openai"));
        cache.clear_profile("openai").unwrap();
        assert_eq!(cache.get_cached("openai"), Vec::<String>::new());
        assert!(!cache.is_fresh("openai"));
    }

    #[test]
    fn registry_cost_roundtrip() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        cache
            .put_registry(
                BTreeMap::from([("openai".into(), vec!["openai/gpt-test".into()])]),
                BTreeMap::from([(
                    "openai/gpt-test".into(),
                    CatalogCost {
                        input: 2.0,
                        output: 8.0,
                    },
                )]),
            )
            .unwrap();

        assert_eq!(
            cache.get_registry_cost("openai/gpt-test"),
            Some(CatalogCost {
                input: 2.0,
                output: 8.0,
            })
        );
        assert_eq!(
            cache.get_registry_cached("openai"),
            vec!["openai/gpt-test".to_string()]
        );
        assert!(cache.registry_is_fresh());
    }

    #[test]
    fn catalog_cost_and_models_dev_filter_handle_missing_fields() {
        assert_eq!(models_dev_cost(&serde_json::json!({})), None);
        assert_eq!(
            models_dev_cost(&serde_json::json!({
                "cost": { "input": 1.25, "output": 9.5 }
            })),
            Some(CatalogCost {
                input: 1.25,
                output: 9.5,
            })
        );
        assert!(!models_dev_model_is_agent_compatible(&serde_json::json!({
            "tool_call": true,
            "modalities": { "output": ["image"] }
        })));
        assert!(!models_dev_model_is_agent_compatible(&serde_json::json!({
            "tool_call": true
        })));
    }

    #[test]
    fn picker_falls_back_to_defaults() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let p = openai_profile();
        let entries = models_for_picker(std::slice::from_ref(&p), &store, &cache, false);
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| e.id.starts_with("openai/")));
    }

    #[test]
    fn registry_models_are_used_when_account_catalog_is_unavailable() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = openai_profile();
        let mut registry = BTreeMap::new();
        registry.insert(profile.id.clone(), vec!["openai/gpt-5.6-terra".into()]);
        cache.put_registry(registry, BTreeMap::new()).unwrap();

        let entries = models_for_picker(&[profile], &store, &cache, false);
        assert_eq!(entries[0].id, "openai/gpt-5.6-terra");
        assert_eq!(entries[0].source, CatalogSource::Registry);
    }

    #[test]
    fn registry_supplements_a_partial_account_catalog() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = openai_profile();
        cache
            .put(&profile.id, vec!["openai/gpt-5.6-sol".into()])
            .unwrap();
        let mut registry = BTreeMap::new();
        registry.insert(
            profile.id.clone(),
            vec!["openai/gpt-5.6-sol".into(), "openai/gpt-5.6-terra".into()],
        );
        cache.put_registry(registry, BTreeMap::new()).unwrap();

        let entries = models_for_picker(&[profile], &store, &cache, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, CatalogSource::Cached);
        assert_eq!(entries[1].id, "openai/gpt-5.6-terra");
        assert_eq!(entries[1].source, CatalogSource::Registry);
    }

    #[test]
    fn picker_deduplicates_across_profiles_and_preserves_first_owner() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let mut first = openai_profile();
        first.id = "first".into();
        first.default_models = vec!["shared/model".into(), "first/only".into()];
        let mut second = openai_profile();
        second.id = "second".into();
        second.default_models = vec!["shared/model".into(), "second/only".into()];

        let entries = models_for_picker(&[first, second], &store, &cache, false);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["shared/model", "first/only", "second/only"]
        );
        assert_eq!(entries[0].profile_id, "first");
    }

    #[test]
    fn codex_does_not_use_public_registry_rows() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = crate::openai_codex::openai_codex_profile();
        let mut registry = BTreeMap::new();
        registry.insert(profile.id.clone(), vec!["openai-codex/not-entitled".into()]);
        cache.put_registry(registry, BTreeMap::new()).unwrap();

        let entries = models_for_picker(&[profile], &store, &cache, false);
        assert!(entries
            .iter()
            .all(|entry| entry.id != "openai-codex/not-entitled"));
    }

    #[test]
    fn parses_visible_codex_cli_cache_rows_in_priority_order() {
        let body = serde_json::json!({
            "models": [
                {"slug": "hidden", "visibility": "hide", "priority": 1},
                {"slug": "gpt-5.4", "visibility": "list", "priority": 3},
                {"slug": "gpt-5.6-sol", "visibility": "list", "priority": 1},
                {"slug": "gpt-5.5", "visibility": "list", "priority": 2}
            ]
        });
        assert_eq!(
            parse_codex_cli_cached_model_ids(&body),
            vec!["gpt-5.6-sol", "gpt-5.5", "gpt-5.4"]
        );
    }

    #[test]
    fn codex_cli_cache_parser_ignores_bad_or_empty_rows() {
        assert_eq!(
            parse_codex_cli_cached_model_ids(&serde_json::json!({ "models": "bad" })),
            Vec::<String>::new()
        );
        let body = serde_json::json!({
            "models": [
                {"slug": "", "visibility": "list", "priority": 1},
                {"visibility": "list", "priority": 2},
                {"slug": "late", "visibility": "list"},
                {"slug": "early", "visibility": "list", "priority": 0}
            ]
        });
        assert_eq!(
            parse_codex_cli_cached_model_ids(&body),
            vec!["early", "late"]
        );
    }

    #[test]
    fn reads_codex_cli_cache_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        fs::write(
            &path,
            r#"{"models":[{"slug":"gpt-5.6-sol","visibility":"list","priority":1}]}"#,
        )
        .unwrap();
        assert_eq!(codex_cli_cached_model_ids_at(&path), vec!["gpt-5.6-sol"]);
        assert_eq!(
            codex_cli_cached_model_ids_at(&dir.path().join("missing.json")),
            Vec::<String>::new()
        );
        fs::write(&path, "{not json").unwrap();
        assert_eq!(codex_cli_cached_model_ids_at(&path), Vec::<String>::new());
    }

    #[test]
    fn models_dev_filter_excludes_non_agent_models() {
        assert!(models_dev_model_is_agent_compatible(&serde_json::json!({
            "tool_call": true,
            "modalities": { "output": ["text"] }
        })));
        assert!(!models_dev_model_is_agent_compatible(&serde_json::json!({
            "tool_call": false,
            "modalities": { "output": ["text"] }
        })));
        assert!(!models_dev_model_is_agent_compatible(&serde_json::json!({
            "tool_call": true,
            "modalities": { "output": ["text"] },
            "status": "deprecated"
        })));
    }

    #[test]
    fn parse_openai_style_ids() {
        let v = serde_json::json!({
            "data": [
                {"id": "gpt-4.1-mini"},
                {"id": "text-embedding-3-small"}
            ]
        });
        let ids = parse_openai_style_model_ids(&v).unwrap();
        assert!(ids.contains(&"gpt-4.1-mini".into()));
    }

    #[test]
    fn parse_openai_style_ids_accepts_models_strings_slugs_and_rejects_bad_shape() {
        let via_models = serde_json::json!({
            "models": [
                "raw-model",
                { "slug": "slug-model" },
                { "id": "id-model" },
                { "ignored": true }
            ]
        });
        assert_eq!(
            parse_openai_style_model_ids(&via_models).unwrap(),
            vec!["raw-model", "slug-model", "id-model"]
        );
        assert!(
            parse_openai_style_model_ids(&serde_json::json!({ "items": [] }))
                .unwrap_err()
                .to_string()
                .contains("missing data[]")
        );
    }

    #[test]
    fn parse_codex_catalog_slugs() {
        let value = serde_json::json!({
            "models": [
                {"slug": "gpt-5.6-sol"},
                {"slug": "gpt-5.6-terra"}
            ]
        });
        assert_eq!(
            parse_openai_style_model_ids(&value).unwrap(),
            vec!["gpt-5.6-sol", "gpt-5.6-terra"]
        );
    }

    #[test]
    fn parse_ollama_names_preserves_installed_tags() {
        let value = serde_json::json!({
            "models": [
                {"name": "qwen2.5-coder:latest"},
                {"name": "qwen2.5-coder:3b"}
            ]
        });
        assert_eq!(
            parse_ollama_model_names(&value),
            vec!["qwen2.5-coder:latest", "qwen2.5-coder:3b"]
        );
        assert!(parse_ollama_model_names(&serde_json::json!({ "models": "bad" })).is_empty());
    }

    #[test]
    fn picker_does_not_invent_ollama_models() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = crate::ollama::ollama_profile();
        let entries = models_for_picker(&[profile], &store, &cache, false);
        assert!(entries.is_empty());
    }

    #[test]
    fn openai_filter_drops_embeddings() {
        assert!(!filter_openai_chat_ish("text-embedding-3-small"));
        assert!(!filter_openai_chat_ish("whisper-1"));
        assert!(!filter_openai_chat_ish("tts-1"));
        assert!(!filter_openai_chat_ish("dall-e-3"));
        assert!(!filter_openai_chat_ish("ft:gpt-4.1-mini:org:custom"));
        assert!(!filter_openai_chat_ish("omni-moderation-latest"));
        assert!(filter_openai_chat_ish("gpt-4.1-mini"));
    }

    #[test]
    fn fetch_remote_models_uses_profile_specific_http_shapes() {
        let mut openai = openai_profile();
        openai.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"data":[{"id":"gpt-4.1-mini"},{"id":"text-embedding-3-small"}]}"#,
            vec![],
        )]));
        assert_eq!(
            fetch_remote_models(&openai, Some("sk-test")).unwrap(),
            vec!["openai/gpt-4.1-mini"]
        );

        let mut go = crate::opencode_go::opencode_go_profile();
        go.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"data":[{"id":"gpt-4.1-mini"}]}"#,
            vec![],
        )]));
        assert_eq!(
            fetch_remote_models(&go, Some("go-test")).unwrap(),
            vec!["opencode-go/gpt-4.1-mini"]
        );

        let mut zen = crate::opencode_zen::opencode_zen_profile();
        zen.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"models":["claude-sonnet-4"]}"#,
            vec![],
        )]));
        assert_eq!(
            fetch_remote_models(&zen, Some("zen-test")).unwrap(),
            vec!["opencode-zen/claude-sonnet-4"]
        );

        let mut xai = crate::xai::xai_grok_profile();
        xai.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"data":[{"slug":"grok-code-fast"}]}"#,
            vec![],
        )]));
        assert_eq!(
            fetch_remote_models(&xai, Some("xai-token")).unwrap(),
            vec!["xai/grok-code-fast"]
        );

        let mut codex = crate::openai_codex::openai_codex_profile();
        codex.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"models":[{"slug":"gpt-5.6-sol"},{"slug":"codex-auto-review"}]}"#,
            vec![],
        )]));
        let token = codex_access_token_for_tests();
        assert_eq!(
            fetch_remote_models(&codex, Some(&token)).unwrap(),
            vec!["openai-codex/gpt-5.6-sol"]
        );
    }

    fn codex_access_token_for_tests() -> String {
        use base64::Engine;
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("header.{encoded}.signature")
    }

    #[test]
    fn fetch_remote_models_covers_anthropic_and_ollama_fallbacks() {
        let mut anthropic = crate::anthropic::anthropic_profile();
        anthropic.default_base_url = Some(mock_http(vec![
            (400, "query rejected", vec![]),
            (
                200,
                r#"{"data":[{"id":"claude-sonnet-4-20250514"}]}"#,
                vec![],
            ),
        ]));
        assert_eq!(
            fetch_remote_models(&anthropic, Some("sk-ant-test")).unwrap(),
            vec!["anthropic/claude-sonnet-4-20250514"]
        );

        let mut ollama = crate::ollama::ollama_profile();
        ollama.default_base_url = Some(mock_http(vec![(
            200,
            r#"{"models":[{"name":"qwen2.5-coder:latest"}]}"#,
            vec![],
        )]));
        assert_eq!(
            fetch_remote_models(&ollama, None).unwrap(),
            vec!["ollama/qwen2.5-coder:latest"]
        );

        let mut ollama_fallback = crate::ollama::ollama_profile();
        ollama_fallback.default_base_url = Some(mock_http(vec![
            (200, r#"{"models":[]}"#, vec![]),
            (200, r#"{"data":[{"id":"llama3.2"}]}"#, vec![]),
        ]));
        assert_eq!(
            fetch_remote_models(&ollama_fallback, None).unwrap(),
            vec!["ollama/llama3.2"]
        );
    }

    #[test]
    fn http_get_json_ids_reports_redirect_and_json_errors() {
        let base = mock_http(vec![(
            302,
            "",
            vec![("Location", "http://127.0.0.1:9/models")],
        )]);
        let err = http_get_json_ids(&format!("{base}/models"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("catalog GET"), "{err}");

        let base = mock_http(vec![(200, "{not-json", vec![])]);
        let err = http_get_json_ids(&format!("{base}/models"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("catalog JSON"), "{err}");

        let base = mock_http(vec![(400, "bad request body", vec![])]);
        let err = http_get_json_ids(&format!("{base}/models"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 400 bad request body"), "{err}");
    }

    #[test]
    fn map_prefix_trims_sorts_deduplicates_and_preserves_namespaced_ids() {
        assert_eq!(
            map_prefix(
                "openai",
                vec![" z ".into(), "a".into(), "openai/a".into(), "a".into()],
                |_| true
            ),
            vec!["openai/a", "openai/z"]
        );
        assert_eq!(
            map_prefix("openai", vec!["keep".into(), "drop".into()], |id| id
                == "keep"),
            vec!["openai/keep"]
        );
    }

    #[test]
    fn credential_for_catalog_uses_store_and_filters_fixture_oauth_tokens() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("k.toml"));

        let api_profile = openai_profile();
        store.set_api_key(&api_profile.id, "sk-file").unwrap();
        assert_eq!(
            credential_for_catalog(&api_profile, &store),
            Some("sk-file".into())
        );

        let oauth_profile = crate::xai::xai_grok_profile();
        store
            .set_oauth(
                &oauth_profile.id,
                OauthTokens {
                    access_token: "fixture-access-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(credential_for_catalog(&oauth_profile, &store), None);

        store
            .set_oauth(
                &oauth_profile.id,
                OauthTokens {
                    access_token: "real-access-token".into(),
                    refresh_token: Some("refresh".into()),
                    expires_at: None,
                },
            )
            .unwrap();
        assert_eq!(
            credential_for_catalog(&oauth_profile, &store),
            Some("real-access-token".into())
        );
    }

    #[test]
    fn refresh_models_dev_registry_uses_override_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &["FORGE_MODELS_DEV_URL"];
        let guard = EnvGuard::new(ENV);
        let base = mock_http(vec![(
            200,
            r#"{"openai":{"models":{"gpt-4.1-mini":{"cost":{"input":1.0,"output":2.0},"tool_call":true,"modalities":{"output":["text"]}}}}}"#,
            vec![],
        )]);
        guard.set("FORGE_MODELS_DEV_URL", &base);

        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let profiles = vec![openai_profile()];
        let count = refresh_models_dev_registry(&profiles, &cache).unwrap();
        assert_eq!(count, 1);
        let entries = models_for_picker(
            &profiles,
            &CredentialStore::new(dir.path().join("k.toml")),
            &cache,
            false,
        );
        assert!(entries
            .iter()
            .any(|entry| entry.id == "openai/gpt-4.1-mini"));
    }

    #[test]
    fn fetch_remote_models_reports_missing_credentials_and_unknown_profiles_before_network() {
        assert_eq!(
            fetch_remote_models(&openai_profile(), None)
                .unwrap_err()
                .to_string(),
            "OpenAI API key required for catalog"
        );
        assert_eq!(
            fetch_remote_models(&crate::anthropic::anthropic_profile(), None)
                .unwrap_err()
                .to_string(),
            "Anthropic API key required for catalog"
        );
        assert_eq!(
            fetch_remote_models(&crate::opencode_go::opencode_go_profile(), None)
                .unwrap_err()
                .to_string(),
            "OpenCode Go API key required for catalog"
        );
        assert_eq!(
            fetch_remote_models(&crate::opencode_zen::opencode_zen_profile(), None)
                .unwrap_err()
                .to_string(),
            "OpenCode Zen API key required for catalog"
        );
        assert_eq!(
            fetch_remote_models(&crate::xai::xai_grok_profile(), None)
                .unwrap_err()
                .to_string(),
            "xAI OAuth token required for catalog"
        );

        let mut unknown = openai_profile();
        unknown.id = "unknown".into();
        assert!(fetch_remote_models(&unknown, Some("key"))
            .unwrap_err()
            .to_string()
            .contains("no remote catalog implemented"));
    }
}
