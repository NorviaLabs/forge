//! Remote model catalog fetch + on-disk cache (connect-command.md §3.5).
//!
//! After `/connect`, Forge can list live models from each provider and feed `/model`.
//! Uses cached or live provider catalogs; providers own model availability.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::profile::ConnectProfile;
use crate::store::{resolve_key, CredentialStore};

/// Default TTL for cached catalog entries (1 hour).
pub const DEFAULT_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// LiteLLM-routable model string, e.g. `openai/gpt-4.1-mini`.
    pub id: String,
    pub profile_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogFile {
    /// profile_id → fetched model ids (LiteLLM form)
    #[serde(default)]
    models: BTreeMap<String, Vec<String>>,
    /// profile_id → unix secs when last refreshed
    #[serde(default)]
    fetched_at: BTreeMap<String, u64>,
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

    fn save(&self, file: &CatalogFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(file).map_err(|e| e.to_string())?;
        fs::write(&self.path, text).map_err(|e| e.to_string())
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

    pub fn put(&self, profile_id: &str, models: Vec<String>) -> Result<(), String> {
        let mut file = self.load();
        file.models.insert(profile_id.to_string(), models);
        file.fetched_at
            .insert(profile_id.to_string(), Self::now_secs());
        self.save(&file)
    }

    pub fn clear_profile(&self, profile_id: &str) -> Result<(), String> {
        let mut file = self.load();
        file.models.remove(profile_id);
        file.fetched_at.remove(profile_id);
        self.save(&file)
    }
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

/// Fetch remote model ids and rewrite to LiteLLM strings. Network call.
pub fn fetch_remote_models(
    profile: &ConnectProfile,
    api_key: Option<&str>,
) -> Result<Vec<String>, String> {
    let base = profile
        .default_base_url
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches('/');
    let prefix = profile.litellm_provider_prefix.as_str();
    let ua = format!("forge-connect/{}", env!("CARGO_PKG_VERSION"));

    match profile.id.as_str() {
        "openai_codex" => {
            let token =
                api_key.ok_or_else(|| "OpenAI Codex login required for catalog".to_string())?;
            let account_id = crate::openai_codex::account_id_from_token(token)?;
            let url = format!(
                "{}/codex/models?client_version={}",
                if base.is_empty() {
                    "https://chatgpt.com/backend-api"
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
            )?;
            Ok(map_prefix(prefix, raw, |id| {
                !id.eq_ignore_ascii_case("codex-auto-review")
            }))
        }
        "openai" => {
            let key = api_key.ok_or_else(|| "OpenAI API key required for catalog".to_string())?;
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
            let key =
                api_key.ok_or_else(|| "Anthropic API key required for catalog".to_string())?;
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
                    // Retry without query params on strict servers.
                    if e.contains("HTTP 400") {
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
            let key =
                api_key.ok_or_else(|| "OpenCode Go API key required for catalog".to_string())?;
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
            let key =
                api_key.ok_or_else(|| "OpenCode Zen API key required for catalog".to_string())?;
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
            let key = api_key.ok_or_else(|| "xAI OAuth token required for catalog".to_string())?;
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
        other => Err(format!(
            "no remote catalog implemented for profile `{other}`"
        )),
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

fn http_get_json_ids(url: &str, headers: &[(&str, &str)]) -> Result<Vec<String>, String> {
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
                return Err(format!("catalog GET {cur}: HTTP {code} (missing Location)"));
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
                if body.is_empty() {
                    return Err(format!("catalog GET {cur}: HTTP {code}"));
                }
                return Err(format!("catalog GET {cur}: HTTP {code} {body}"));
            }
            Err(e) => return Err(format!("catalog GET {cur}: {e}")),
        };

        if !(200..300).contains(&resp.status()) {
            return Err(format!("catalog GET {cur}: HTTP {}", resp.status()));
        }
        let body: serde_json::Value = resp.into_json().map_err(|e| format!("catalog JSON: {e}"))?;
        return parse_openai_style_model_ids(&body);
    }
    return Err(format!("catalog GET {url}: too many redirects"));
}

fn parse_openai_style_model_ids(body: &serde_json::Value) -> Result<Vec<String>, String> {
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
    Err("catalog response missing data[] models".into())
}

fn http_get_ollama_names(url: &str, ua: &str) -> Result<Vec<String>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", ua)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| format!("ollama tags: {e}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("ollama tags: HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("ollama tags JSON: {e}"))?;
    let mut names = Vec::new();
    if let Some(arr) = body.get("models").and_then(|m| m.as_array()) {
        for m in arr {
            if let Some(n) = m.get("name").and_then(|v| v.as_str()) {
                // strip :latest for cleaner ids; LiteLLM accepts either
                let n = n.split(':').next().unwrap_or(n);
                names.push(n.to_string());
            }
        }
    }
    Ok(names)
}

/// Refresh one profile into the cache. On failure, keep old cache and return Err.
pub fn refresh_profile_catalog(
    profile: &ConnectProfile,
    store: &CredentialStore,
    cache: &ModelCatalogCache,
) -> Result<Vec<String>, String> {
    let key = credential_for_catalog(profile, store);
    let models = fetch_remote_models(profile, key.as_deref())?;
    if models.is_empty() {
        return Err(format!("catalog for `{}` returned no models", profile.id));
    }
    cache.put(&profile.id, models.clone())?;
    Ok(models)
}

/// Models for the picker: live/cached catalog for each connected profile, else defaults.
pub fn models_for_picker(
    profiles: &[ConnectProfile],
    store: &CredentialStore,
    cache: &ModelCatalogCache,
    refresh_stale: bool,
) -> Vec<CatalogEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for p in profiles {
        let mut ids = Vec::new();
        if refresh_stale && !cache.is_fresh(&p.id) {
            if let Ok(m) = refresh_profile_catalog(p, store, cache) {
                ids = m;
            }
        }
        if ids.is_empty() {
            ids = cache.get_cached(&p.id);
        }
        if ids.is_empty() {
            ids = p.default_models.clone();
        }
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(CatalogEntry {
                    id,
                    profile_id: p.id.clone(),
                });
            }
        }
    }

    out
}

/// Normalize a `/model` argument into a LiteLLM model string.
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
    use crate::openai::openai_profile;
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
    fn openai_filter_drops_embeddings() {
        assert!(!filter_openai_chat_ish("text-embedding-3-small"));
        assert!(filter_openai_chat_ish("gpt-4.1-mini"));
    }
}
