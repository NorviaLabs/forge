//! Remote model catalog fetch + on-disk cache (connect-command.md §3.5).
//!
//! After `/connect`, Forge can list live models from each provider and feed `/model`.
//! A models.dev registry supplies durable public metadata; provider catalogs remain
//! account-specific availability signals.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::profile::{CatalogMode, ConnectProfile, ProviderTransport};
use crate::store::{resolve_key, CredentialStore};

/// Default TTL for cached catalog entries (1 hour).
/// Live picker refreshes do not wait for this window; they re-fetch the
/// account catalog and keep the cache for instant open / offline fallback.
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
    /// Declared on a `static` provider spec. The author entitled these ids.
    Configured,
}

impl CatalogSource {
    /// Whether this row represents a model returned by the selected account
    /// (directly or from that account's persisted cache). Public registry and
    /// built-in fallback rows are metadata/safety nets, not entitlement proof.
    pub fn is_runnable(self) -> bool {
        matches!(self, Self::Live | Self::Cached | Self::Configured)
    }
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

/// A model's advertised token limits, from models.dev `limit`.
///
/// Context compaction reads these rather than assuming a window size: the
/// pressure threshold and the retained-tail target are both fractions of
/// `context`, and `output` is the reserve held back for the model's reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogLimits {
    pub context: usize,
    pub output: usize,
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
    /// provider/model id → advertised context/output token limits.
    #[serde(default)]
    registry_limits: BTreeMap<String, CatalogLimits>,
    /// provider/model ids whose models.dev `modalities.input` includes `image`.
    /// Missing from this set is fail-closed: the model cannot take image input.
    #[serde(default)]
    registry_image_input: BTreeSet<String>,
    /// Set when this process has ingested `modalities.input`. Pre-feature
    /// catalog files stay `false` even if `registry_fetched_at` is fresh.
    #[serde(default)]
    registry_image_input_ready: bool,
    /// provider/model id → discrete models.dev effort values (`low`, `xhigh`, …).
    /// An empty vec means the model is known and has no adjustable effort.
    #[serde(default)]
    registry_effort: BTreeMap<String, Vec<String>>,
    /// Set when this process has ingested `reasoning_options`.
    #[serde(default)]
    registry_effort_ready: bool,
    /// Unix seconds when the public registry was refreshed.
    #[serde(default)]
    registry_fetched_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ModelCatalogCache {
    path: PathBuf,
    ttl_secs: u64,
    /// Parsed once per cache instance. Picker construction asks several catalog
    /// questions in succession; re-reading and re-parsing TOML for each one
    /// made opening it scale with catalog size and filesystem latency.
    file: RefCell<CatalogFile>,
}

impl ModelCatalogCache {
    pub fn new(path: PathBuf) -> Self {
        let file = Self::read_file(&path);
        Self {
            path,
            ttl_secs: DEFAULT_TTL_SECS,
            file: RefCell::new(file),
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

    fn read_file(path: &Path) -> CatalogFile {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn load(&self) -> CatalogFile {
        self.file.borrow().clone()
    }

    fn save(&self, file: &CatalogFile) -> Result<(), CatalogError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(file)?;
        fs::write(&self.path, text)?;
        *self.file.borrow_mut() = file.clone();
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

    /// Advertised context/output limits for `model_id`.
    ///
    /// `None` means this cache has no row — offline, a pre-feature catalog
    /// file, or an id models.dev does not publish — and the caller should
    /// keep its own default rather than assume a window.
    pub fn model_limits(&self, model_id: &str) -> Option<CatalogLimits> {
        if model_id.is_empty() {
            return None;
        }
        let file = self.load();
        metadata_id_candidates(model_id)
            .into_iter()
            .find_map(|id| file.registry_limits.get(&id).copied())
    }

    /// Fail-closed: unknown or missing `modalities.input` ⇒ no image input.
    pub fn model_accepts_image_input(&self, model_id: &str) -> bool {
        if model_id.is_empty() {
            return false;
        }
        let file = self.load();
        image_input_candidates(model_id)
            .into_iter()
            .any(|id| file.registry_image_input.contains(&id))
    }

    /// Whether `modalities.input` has been ingested at least once.
    pub fn image_input_ready(&self) -> bool {
        self.load().registry_image_input_ready
    }

    /// Discrete effort values advertised for `model_id` by models.dev.
    ///
    /// `None` means this cache has no row (offline / pre-feature / unknown
    /// id) and callers should use the built-in fallback. `Some([])` means
    /// the registry knows the model and it has no adjustable effort.
    pub fn model_effort_options(&self, model_id: &str) -> Option<Vec<String>> {
        if model_id.is_empty() {
            return None;
        }
        let file = self.load();
        if !file.registry_effort_ready {
            return None;
        }
        metadata_id_candidates(model_id)
            .into_iter()
            .find_map(|id| file.registry_effort.get(&id).cloned())
    }

    /// Whether `reasoning_options` has been ingested at least once.
    pub fn effort_ready(&self) -> bool {
        self.load().registry_effort_ready
    }

    fn put_registry(
        &self,
        models: BTreeMap<String, Vec<String>>,
        costs: BTreeMap<String, CatalogCost>,
        limits: BTreeMap<String, CatalogLimits>,
        image_input: BTreeSet<String>,
        effort: BTreeMap<String, Vec<String>>,
    ) -> Result<(), CatalogError> {
        let mut file = self.load();
        file.registry_models = models;
        file.registry_costs = costs;
        file.registry_limits = limits;
        file.registry_image_input = image_input;
        file.registry_image_input_ready = true;
        file.registry_effort = effort;
        file.registry_effort_ready = true;
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
        .header("User-Agent", &ua)
        .config()
        .timeout_per_call(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .map_err(|e| CatalogError::Transport {
            label: "models.dev registry".into(),
            detail: e.to_string(),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| CatalogError::Decode {
            label: "models.dev registry JSON".into(),
            detail: e.to_string(),
        })?;

    let mut by_profile = BTreeMap::new();
    let mut costs = BTreeMap::new();
    let mut limits: BTreeMap<String, CatalogLimits> = BTreeMap::new();
    let mut image_input = BTreeSet::new();
    let mut effort_by_model = BTreeMap::new();
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
                    if let Some(model_limits) = models_dev_limits(model) {
                        limits.insert(id.clone(), model_limits);
                        // ChatGPT Codex and the OpenAI API share models.dev's
                        // `openai` row; stamp both route prefixes.
                        for alias in image_input_route_aliases(&id) {
                            limits.insert(alias, model_limits);
                        }
                    }
                    if models_dev_image_input(model) {
                        image_input.insert(id.clone());
                        // ChatGPT Codex and the OpenAI API share models.dev's
                        // `openai` row; stamp both route prefixes.
                        for alias in image_input_route_aliases(&id) {
                            image_input.insert(alias);
                        }
                    }
                    let effort = models_dev_effort_options(model);
                    insert_effort_metadata(&mut effort_by_model, &id, effort);
                    ids.insert(id);
                }
            }
        }
        total += ids.len();
        by_profile.insert(profile.id.clone(), ids.into_iter().collect());
    }
    cache.put_registry(by_profile, costs, limits, image_input, effort_by_model)?;
    Ok(total)
}

fn models_dev_cost(model: &serde_json::Value) -> Option<CatalogCost> {
    let cost = model.get("cost")?;
    Some(CatalogCost {
        input: cost.get("input").and_then(serde_json::Value::as_f64)?,
        output: cost.get("output").and_then(serde_json::Value::as_f64)?,
    })
}

fn models_dev_limits(model: &serde_json::Value) -> Option<CatalogLimits> {
    let limit = model.get("limit")?;
    let context = limit.get("context").and_then(serde_json::Value::as_u64)? as usize;
    if context == 0 {
        return None;
    }
    Some(CatalogLimits {
        context,
        output: limit
            .get("output")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
    })
}

fn image_input_candidates(model_id: &str) -> Vec<String> {
    let mut ids = vec![model_id.to_string()];
    ids.extend(image_input_route_aliases(model_id));
    ids
}

fn metadata_id_candidates(model_id: &str) -> Vec<String> {
    image_input_candidates(model_id)
}

fn insert_effort_metadata(
    effort: &mut BTreeMap<String, Vec<String>>,
    model_id: &str,
    values: Vec<String>,
) {
    effort.insert(model_id.to_string(), values.clone());
    for alias in image_input_route_aliases(model_id) {
        effort.insert(alias, values.clone());
    }
}

fn models_dev_effort_options(model: &serde_json::Value) -> Vec<String> {
    let Some(options) = model
        .get("reasoning_options")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for option in options {
        if option.get("type").and_then(serde_json::Value::as_str) != Some("effort") {
            continue;
        }
        let Some(raw) = option.get("values").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for value in raw {
            let Some(value) = value.as_str() else {
                continue;
            };
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "none" | "auto" | "default" | "") {
                continue;
            }
            if !values.iter().any(|existing| existing == &value) {
                values.push(value);
            }
        }
    }
    values
}

fn image_input_route_aliases(model_id: &str) -> Vec<String> {
    let Some((prefix, rest)) = model_id.split_once('/') else {
        return Vec::new();
    };
    match prefix {
        "openai" => vec![format!("openai-codex/{rest}")],
        "openai-codex" => vec![format!("openai/{rest}")],
        _ => Vec::new(),
    }
}

fn models_dev_image_input(model: &serde_json::Value) -> bool {
    model
        .pointer("/modalities/input")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|input| input.iter().any(|m| m.as_str() == Some("image")))
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
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .max_redirects(0)
        // Redirects and auth failures are both inspected below for their
        // `Location` header and body, which ureq 3 does not carry on an
        // `Err`. Take every status as a response and branch on it here.
        .http_status_as_error(false)
        .build()
        .into();

    let mut cur = url.to_string();
    for _ in 0..4 {
        let mut req = agent.get(&cur);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = match req.call() {
            Ok(r) if matches!(r.status().as_u16(), 301 | 302 | 303 | 307 | 308) => {
                if let Some(loc) = r
                    .headers()
                    .get("Location")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                {
                    cur = loc;
                    continue;
                }
                return Err(CatalogError::Http {
                    label: format!("catalog GET {cur}"),
                    status: r.status().as_u16(),
                    body: Some("(missing Location)".into()),
                });
            }
            Ok(mut r) if !(200..300).contains(&r.status().as_u16()) => {
                // Best-effort include small body to help operator debug auth failures.
                let body = r
                    .body_mut()
                    .read_to_string()
                    .unwrap_or_default()
                    .chars()
                    .take(240)
                    .collect::<String>();
                let body = body.trim().to_string();
                return Err(CatalogError::Http {
                    label: format!("catalog GET {cur}"),
                    status: r.status().as_u16(),
                    body: (!body.is_empty()).then_some(body),
                });
            }
            Ok(r) => r,
            Err(e) => {
                return Err(CatalogError::Transport {
                    label: format!("catalog GET {cur}"),
                    detail: e.to_string(),
                })
            }
        };

        let mut resp = resp;
        let body: serde_json::Value =
            resp.body_mut()
                .read_json()
                .map_err(|e| CatalogError::Decode {
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
    let mut resp = ureq::get(url)
        .header("User-Agent", ua)
        .config()
        // The status check below reports the tag endpoint's own failure, so
        // it has to see the response rather than a transport error.
        .http_status_as_error(false)
        .timeout_per_call(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .map_err(|e| CatalogError::Transport {
            label: "ollama tags".into(),
            detail: e.to_string(),
        })?;
    if !(200..300).contains(&resp.status().as_u16()) {
        return Err(CatalogError::Http {
            label: "ollama tags".into(),
            status: resp.status().as_u16(),
            body: None,
        });
    }
    let body: serde_json::Value =
        resp.body_mut()
            .read_json()
            .map_err(|e| CatalogError::Decode {
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
///
/// `refresh_stale` always re-fetches each profile's live account catalog.
/// The on-disk cache is used for instant cache-only reads (`refresh_stale =
/// false`) and as a fallback when that live fetch fails. models.dev still
/// uses its own longer TTL because it is public metadata, not entitlement.
pub fn models_for_picker(
    profiles: &[ConnectProfile],
    store: &CredentialStore,
    cache: &ModelCatalogCache,
    refresh_stale: bool,
) -> Vec<CatalogEntry> {
    let mut out = Vec::new();

    if refresh_stale && !cache.registry_is_fresh() {
        // Registry availability must never prevent opening the picker. The existing
        // cache and built-ins below remain usable offline.
        let _ = refresh_models_dev_registry(profiles, cache);
    }

    for p in profiles {
        let mut entries = Vec::new();
        let live = matches!(
            p.catalog_mode,
            CatalogMode::Live | CatalogMode::LiveRegistry
        );
        if live && refresh_stale {
            if let Ok(m) = refresh_profile_catalog(p, store, cache) {
                entries.extend(m.into_iter().map(|id| (id, CatalogSource::Live)));
            }
        }
        if live && entries.is_empty() && !(p.catalog_mode == CatalogMode::Live && refresh_stale) {
            entries.extend(
                cache
                    .get_cached(&p.id)
                    .into_iter()
                    .map(|id| (id, CatalogSource::Cached)),
            );
        }
        if p.transport == ProviderTransport::Codex {
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
        let include_registry_rows = match p.catalog_mode {
            CatalogMode::Registry => true,
            CatalogMode::LiveRegistry => {
                !p.models_dev_providers.is_empty() && p.transport != ProviderTransport::Codex
            }
            CatalogMode::Live | CatalogMode::Static => false,
        };
        if include_registry_rows {
            entries.extend(
                cache
                    .get_registry_cached(&p.id)
                    .into_iter()
                    .map(|id| (id, CatalogSource::Registry)),
            );
        }
        if p.catalog_mode == CatalogMode::Static {
            entries.extend(
                p.default_models
                    .iter()
                    .cloned()
                    .map(|id| (id, CatalogSource::Configured)),
            );
        } else if entries.is_empty() && p.catalog_mode != CatalogMode::Live {
            entries.extend(
                p.default_models
                    .iter()
                    .cloned()
                    .map(|id| (id, CatalogSource::Default)),
            );
        }
        // Dedup within this profile's own sourcing tiers (live/cached/registry/
        // default can overlap on the same id), but never across profiles — two
        // profiles offering the same model id are distinct, independently
        // reachable routes, not duplicates. See `group_routes` for the
        // route-aware view the picker renders.
        let mut seen = std::collections::BTreeSet::new();
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

/// Return only account-backed rows suitable for normal model selection.
///
/// `models_for_picker` intentionally retains registry/default rows for the
/// first-run fallback and metadata views. Callers that let a connected user
/// switch models must use this projection so a public registry entry cannot
/// become an apparently selectable (but unauthorized) model.
pub fn runnable_models_for_picker(
    profiles: &[ConnectProfile],
    store: &CredentialStore,
    cache: &ModelCatalogCache,
    refresh_stale: bool,
) -> Vec<CatalogEntry> {
    models_for_picker(profiles, store, cache, refresh_stale)
        .into_iter()
        .filter(|entry| entry.source.is_runnable())
        .collect()
}

/// One user-facing model, grouped from every [`CatalogEntry`] route that offers it.
///
/// `models_for_picker` preserves every profile's route as a separate flat entry;
/// this groups those routes by their bare model name so the picker can show one
/// row per model with a route count, instead of one row per route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPickerEntry {
    /// Bare model name shared by every route, e.g. `gpt-5.6` (no provider prefix).
    pub model_id: String,
    pub routes: Vec<ModelRoute>,
}

/// One way to reach a [`ModelPickerEntry`]'s model: a specific connect profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRoute {
    /// Bare model name (matches the owning `ModelPickerEntry::model_id`).
    pub model_id: String,
    pub profile_id: String,
    /// The `provider/model` string sent downstream to select this route.
    pub display_id: String,
    pub source: CatalogSource,
}

/// The bare model name grouping key: the id's suffix after its last `/`, or the
/// whole id when it has none.
pub fn route_model_id(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Group flat catalog routes by model, preserving every profile's route.
///
/// Grouping (not dropping) is the fix for the bug where two profiles offering
/// the same model id used to leave only the first profile's route reachable.
pub fn group_routes(entries: &[CatalogEntry]) -> Vec<ModelPickerEntry> {
    let mut out: Vec<ModelPickerEntry> = Vec::new();
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in entries {
        let model_id = route_model_id(&entry.id);
        let route = ModelRoute {
            model_id: model_id.to_string(),
            profile_id: entry.profile_id.clone(),
            display_id: entry.id.clone(),
            source: entry.source,
        };
        match index.get(model_id) {
            Some(&i) => out[i].routes.push(route),
            None => {
                index.insert(model_id, out.len());
                out.push(ModelPickerEntry {
                    model_id: model_id.to_string(),
                    routes: vec![route],
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
    fn cache_keeps_an_in_memory_snapshot_until_reopened() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("catalog.toml");
        let cache = ModelCatalogCache::new(path.clone());
        cache.put("openai", vec!["openai/first".into()]).unwrap();

        fs::write(
            &path,
            "[models]\nopenai = [\"openai/external\"]\n\n[fetched_at]\nopenai = 1\n",
        )
        .unwrap();

        assert_eq!(cache.get_cached("openai"), vec!["openai/first"]);
        assert_eq!(
            ModelCatalogCache::new(path).get_cached("openai"),
            vec!["openai/external"]
        );
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
                BTreeMap::from([(
                    "openai/gpt-test".into(),
                    CatalogLimits {
                        context: 200_000,
                        output: 32_000,
                    },
                )]),
                BTreeSet::new(),
                BTreeMap::new(),
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
        assert_eq!(
            cache.model_limits("openai/gpt-test"),
            Some(CatalogLimits {
                context: 200_000,
                output: 32_000,
            }),
            "limits must survive the TOML round-trip alongside costs"
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
        assert!(models_dev_image_input(&serde_json::json!({
            "modalities": { "input": ["text", "image"] }
        })));
        assert!(!models_dev_image_input(&serde_json::json!({
            "modalities": { "input": ["text"] }
        })));
        assert!(!models_dev_image_input(&serde_json::json!({})));
        assert_eq!(
            models_dev_effort_options(&serde_json::json!({
                "reasoning_options": [
                    {"type": "effort", "values": ["low", "medium", "high", "xhigh"]},
                    {"type": "budget_tokens", "min": 1024}
                ]
            })),
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            models_dev_effort_options(&serde_json::json!({
                "reasoning_options": [{"type": "effort", "values": ["none", "low"]}]
            })),
            vec!["low"]
        );
        assert!(models_dev_effort_options(&serde_json::json!({})).is_empty());
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
    fn only_account_backed_catalog_sources_are_runnable() {
        assert!(CatalogSource::Live.is_runnable());
        assert!(CatalogSource::Cached.is_runnable());
        assert!(!CatalogSource::Registry.is_runnable());
        assert!(!CatalogSource::Default.is_runnable());
    }

    #[test]
    fn registry_models_are_used_when_account_catalog_is_unavailable() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = openai_profile();
        let mut registry = BTreeMap::new();
        registry.insert(profile.id.clone(), vec!["openai/gpt-5.6-terra".into()]);
        cache
            .put_registry(
                registry,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap();

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
        cache
            .put_registry(
                registry,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap();

        let entries = models_for_picker(&[profile], &store, &cache, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, CatalogSource::Cached);
        assert_eq!(entries[1].id, "openai/gpt-5.6-terra");
        assert_eq!(entries[1].source, CatalogSource::Registry);
    }

    #[test]
    fn picker_refresh_replaces_a_fresh_account_catalog() {
        // `refresh_stale: true` below reaches `refresh_models_dev_registry`,
        // which reads `FORGE_MODELS_DEV_URL`. Two consequences this test used
        // to have, both invisible from reading it:
        //
        //   * unguarded, it raced `refresh_models_dev_registry_uses_override_url`
        //     — that test sets the variable to its own single-response mock,
        //     and whichever request arrived first consumed it, so the other
        //     one starved and its assertion failed. That was the flake.
        //   * with the variable unset it fetched the *real* models.dev over
        //     the network, making an offline or slow run fail for a third
        //     reason entirely.
        //
        // Taking the guard serialises it against the other test, and pointing
        // the variable at a dead port keeps the registry refresh local and
        // deterministic: it is allowed to fail, the assertions below are about
        // the account catalog, not the registry.
        use crate::test_env::EnvGuard;
        let guard = EnvGuard::new(&["FORGE_MODELS_DEV_URL"]);
        guard.set("FORGE_MODELS_DEV_URL", "http://127.0.0.1:1/registry.json");

        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml")).with_ttl(3600);
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let mut profile = crate::xai::xai_grok_profile();
        store
            .set_oauth(
                &profile.id,
                OauthTokens {
                    access_token: "xai-real-token".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        cache.put(&profile.id, vec!["xai/grok-3".into()]).unwrap();
        assert!(cache.is_fresh(&profile.id));

        let Some(mock_url) = mock_http(vec![(
            200,
            r#"{"data":[{"id":"grok-3"},{"id":"grok-4.6"}]}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        profile.default_base_url = Some(mock_url);

        let entries = models_for_picker(&[profile], &store, &cache, true);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source == CatalogSource::Live)
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["xai/grok-3", "xai/grok-4.6"]
        );
        assert_eq!(
            cache.get_cached("xai"),
            vec!["xai/grok-3".to_string(), "xai/grok-4.6".to_string()]
        );
    }

    #[test]
    fn picker_preserves_routes_across_profiles_with_shared_model_ids() {
        // Two profiles offering the identical model id used to leave only the
        // first profile's route reachable (the second was silently dropped by
        // a dedup keyed on the bare id across all profiles). Both routes must
        // now survive as separate flat entries, disambiguated by profile_id.
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let mut first = openai_profile();
        first.id = "first".into();
        first.default_models = vec!["shared/model".into(), "first/alpha".into()];
        let mut second = openai_profile();
        second.id = "second".into();
        second.default_models = vec!["shared/model".into(), "second/beta".into()];

        let entries = models_for_picker(&[first, second], &store, &cache, false);
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.id.as_str(), entry.profile_id.as_str()))
                .collect::<Vec<_>>(),
            [
                ("shared/model", "first"),
                ("first/alpha", "first"),
                ("shared/model", "second"),
                ("second/beta", "second"),
            ]
        );
    }

    #[test]
    fn group_routes_groups_shared_model_ids_across_profiles() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let mut first = openai_profile();
        first.id = "first".into();
        first.default_models = vec!["shared/model".into(), "first/alpha".into()];
        let mut second = openai_profile();
        second.id = "second".into();
        second.default_models = vec!["shared/model".into(), "second/beta".into()];

        let entries = models_for_picker(&[first, second], &store, &cache, false);
        let grouped = group_routes(&entries);

        let shared = grouped
            .iter()
            .find(|e| e.model_id == "model")
            .expect("shared/model grouped by its bare model id");
        assert_eq!(shared.routes.len(), 2, "both routes must be preserved");
        assert_eq!(
            shared
                .routes
                .iter()
                .map(|r| r.profile_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(shared.routes.iter().all(|r| r.display_id == "shared/model"));

        let first_exclusive = grouped
            .iter()
            .find(|e| e.model_id == "alpha" && e.routes[0].profile_id == "first")
            .expect("first/alpha present with a single route");
        assert_eq!(first_exclusive.routes.len(), 1);
    }

    #[test]
    fn group_routes_single_route_per_model_is_unaffected() {
        let entries = vec![CatalogEntry {
            id: "openai/gpt-4.1-mini".into(),
            profile_id: "openai".into(),
            source: CatalogSource::Live,
        }];
        let grouped = group_routes(&entries);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].model_id, "gpt-4.1-mini");
        assert_eq!(grouped[0].routes.len(), 1);
        assert_eq!(grouped[0].routes[0].display_id, "openai/gpt-4.1-mini");
    }

    #[test]
    fn group_routes_bare_id_without_slash_groups_on_whole_string() {
        let entries = vec![
            CatalogEntry {
                id: "local-model".into(),
                profile_id: "ollama".into(),
                source: CatalogSource::Live,
            },
            CatalogEntry {
                id: "local-model".into(),
                profile_id: "lmstudio".into(),
                source: CatalogSource::Live,
            },
        ];
        let grouped = group_routes(&entries);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].model_id, "local-model");
        assert_eq!(grouped[0].routes.len(), 2);
    }

    #[test]
    fn codex_does_not_use_public_registry_rows() {
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let store = CredentialStore::new(dir.path().join("k.toml"));
        let profile = crate::openai_codex::openai_codex_profile();
        let mut registry = BTreeMap::new();
        registry.insert(profile.id.clone(), vec!["openai-codex/not-entitled".into()]);
        cache
            .put_registry(
                registry,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap();

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
        let Some(mock_url) = mock_http(vec![(
            200,
            r#"{"data":[{"id":"gpt-4.1-mini"},{"id":"text-embedding-3-small"}]}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        openai.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&openai, Some("sk-test")).unwrap(),
            vec!["openai/gpt-4.1-mini"]
        );

        let mut go = crate::opencode_go::opencode_go_profile();
        let Some(mock_url) = mock_http(vec![(200, r#"{"data":[{"id":"gpt-4.1-mini"}]}"#, vec![])])
        else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        go.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&go, Some("go-test")).unwrap(),
            vec!["opencode-go/gpt-4.1-mini"]
        );

        let mut zen = crate::opencode_zen::opencode_zen_profile();
        let Some(mock_url) = mock_http(vec![(200, r#"{"models":["claude-sonnet-4"]}"#, vec![])])
        else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        zen.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&zen, Some("zen-test")).unwrap(),
            vec!["opencode-zen/claude-sonnet-4"]
        );

        let mut xai = crate::xai::xai_grok_profile();
        let Some(mock_url) = mock_http(vec![(
            200,
            r#"{"data":[{"slug":"grok-code-fast"}]}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        xai.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&xai, Some("xai-token")).unwrap(),
            vec!["xai/grok-code-fast"]
        );

        let mut codex = crate::openai_codex::openai_codex_profile();
        let Some(mock_url) = mock_http(vec![(
            200,
            r#"{"models":[{"slug":"gpt-5.6-sol"},{"slug":"codex-auto-review"}]}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        codex.default_base_url = Some(mock_url);
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
        let Some(mock_url) = mock_http(vec![
            (400, "query rejected", vec![]),
            (
                200,
                r#"{"data":[{"id":"claude-sonnet-4-20250514"}]}"#,
                vec![],
            ),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        anthropic.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&anthropic, Some("sk-ant-test")).unwrap(),
            vec!["anthropic/claude-sonnet-4-20250514"]
        );

        let mut ollama = crate::ollama::ollama_profile();
        let Some(mock_url) = mock_http(vec![(
            200,
            r#"{"models":[{"name":"qwen2.5-coder:latest"}]}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        ollama.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&ollama, None).unwrap(),
            vec!["ollama/qwen2.5-coder:latest"]
        );

        let mut ollama_fallback = crate::ollama::ollama_profile();
        let Some(mock_url) = mock_http(vec![
            (200, r#"{"models":[]}"#, vec![]),
            (200, r#"{"data":[{"id":"llama3.2"}]}"#, vec![]),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        ollama_fallback.default_base_url = Some(mock_url);
        assert_eq!(
            fetch_remote_models(&ollama_fallback, None).unwrap(),
            vec!["ollama/llama3.2"]
        );
    }

    #[test]
    fn http_get_json_ids_reports_redirect_and_json_errors() {
        let Some(base) = mock_http(vec![(
            302,
            "",
            vec![("Location", "http://127.0.0.1:9/models")],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let err = http_get_json_ids(&format!("{base}/models"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("catalog GET"), "{err}");

        let Some(base) = mock_http(vec![(200, "{not-json", vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let err = http_get_json_ids(&format!("{base}/models"), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("catalog JSON"), "{err}");

        let Some(base) = mock_http(vec![(400, "bad request body", vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
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
        let Some(base) = mock_http(vec![(
            200,
            r#"{"openai":{"models":{"gpt-4.1-mini":{"cost":{"input":1.0,"output":2.0},"limit":{"context":128000,"output":16384},"tool_call":true,"modalities":{"output":["text"]}},"gpt-4o":{"cost":{"input":1.0,"output":2.0},"tool_call":true,"modalities":{"input":["text","image"],"output":["text"]},"reasoning_options":[{"type":"effort","values":["low","medium","high"]}]},"gpt-5.2":{"cost":{"input":1.0,"output":2.0},"tool_call":true,"modalities":{"output":["text"]},"reasoning_options":[{"type":"effort","values":["none","low","medium","high","xhigh"]}]}}},"xai":{"models":{"grok-4.6":{"cost":{"input":2.0,"output":6.0},"tool_call":true,"modalities":{"output":["text"]},"reasoning_options":[{"type":"effort","values":["low","medium","high","xhigh"]}]}}}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set("FORGE_MODELS_DEV_URL", &base);

        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        let profiles = vec![openai_profile(), crate::xai::xai_grok_profile()];
        let count = refresh_models_dev_registry(&profiles, &cache).unwrap();
        assert_eq!(count, 4);
        let entries = models_for_picker(
            &profiles,
            &CredentialStore::new(dir.path().join("k.toml")),
            &cache,
            false,
        );
        assert!(entries
            .iter()
            .any(|entry| entry.id == "openai/gpt-4.1-mini"));
        assert!(!cache.model_accepts_image_input("openai/gpt-4.1-mini"));
        assert!(cache.model_accepts_image_input("openai/gpt-4o"));
        assert!(
            cache.model_accepts_image_input("openai-codex/gpt-4o"),
            "ChatGPT Codex route must inherit openai models.dev image input"
        );
        assert!(!cache.model_accepts_image_input("unknown/model"));
        assert!(!cache.model_accepts_image_input(""));
        assert!(cache.image_input_ready());
        assert!(cache.effort_ready());
        assert_eq!(
            cache.model_effort_options("openai/gpt-4.1-mini"),
            Some(Vec::<String>::new())
        );
        assert_eq!(
            cache.model_effort_options("openai/gpt-5.2"),
            Some(vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into()
            ])
        );
        assert_eq!(
            cache.model_effort_options("xai/grok-4.6"),
            Some(vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into()
            ])
        );
        assert_eq!(
            cache.model_effort_options("openai-codex/gpt-5.2"),
            Some(vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into()
            ])
        );
        assert_eq!(cache.model_effort_options("unknown/model"), None);

        // Context compaction sizes itself from these, so an id the registry
        // does not publish must report `None` rather than a guessed window.
        assert_eq!(
            cache.model_limits("openai/gpt-4.1-mini"),
            Some(CatalogLimits {
                context: 128_000,
                output: 16_384
            })
        );
        assert_eq!(
            cache.model_limits("openai-codex/gpt-4.1-mini"),
            Some(CatalogLimits {
                context: 128_000,
                output: 16_384
            }),
            "the Codex route shares the openai models.dev row"
        );
        assert_eq!(cache.model_limits("openai/gpt-4o"), None);
        assert_eq!(cache.model_limits("unknown/model"), None);
        assert_eq!(cache.model_limits(""), None);
    }

    #[test]
    fn image_input_aliases_openai_api_and_codex_routes() {
        assert_eq!(
            image_input_candidates("openai-codex/gpt-5.6-sol"),
            vec![
                "openai-codex/gpt-5.6-sol".to_string(),
                "openai/gpt-5.6-sol".to_string()
            ]
        );
        let dir = tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("c.toml"));
        cache
            .put_registry(
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeSet::from(["openai/gpt-5.6-sol".into()]),
                BTreeMap::new(),
            )
            .unwrap();
        assert!(cache.model_accepts_image_input("openai-codex/gpt-5.6-sol"));
        assert!(cache.model_accepts_image_input("openai/gpt-5.6-sol"));
        assert!(!cache.model_accepts_image_input("openai-codex/mystery"));
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
