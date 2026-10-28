//! Provider spec registry: builtins plus user-global TOML.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;
use crate::profile::{CatalogMode, ProviderSpec, ProviderTransport, SpecOrigin};

#[derive(Debug, Default, Clone)]
pub struct ConnectRegistry {
    profiles: Vec<ProviderSpec>,
    load_warnings: Vec<String>,
}

pub fn save_manual_model(profile_id: &str, model: &str) -> Result<(), String> {
    save_manual_model_to(&user_providers_path(), profile_id, model)
}

fn save_manual_model_to(path: &Path, profile_id: &str, model: &str) -> Result<(), String> {
    let registry = loaded_registry_from(path);
    let profile = registry.get(profile_id).ok_or("unknown provider")?;
    let model = model.trim();
    if model.is_empty() || model.ends_with('/') || model.chars().any(char::is_control) {
        return Err("a nonempty model ID is required".into());
    }
    let prefix = format!("{}/", profile.model_provider_prefix);
    let model = if model.starts_with(&prefix) {
        model.to_string()
    } else {
        format!("{prefix}{model}")
    };
    let mut models = profile.manual_models.clone();
    if !models.contains(&model) {
        models.push(model);
    }
    let mut table = toml::Table::new();
    let original = read_provider_text(path)?;
    let document: toml::Value = toml::from_str(&original).map_err(|_| "invalid providers TOML")?;
    if let Some(existing) = document
        .get("manual_models")
        .and_then(toml::Value::as_table)
    {
        table = existing.clone();
    }
    table.insert(
        profile_id.into(),
        toml::Value::try_from(models).map_err(|_| "cannot serialize models")?,
    );
    replace_section(
        path,
        &original,
        "manual_models",
        &toml::to_string(&table).map_err(|_| "cannot serialize models")?,
    )
}

pub fn update_custom_provider(
    id: &str,
    base_url: &str,
    model: &str,
) -> Result<ProviderSpec, String> {
    update_custom_provider_to(&user_providers_path(), id, base_url, model)
}

fn update_custom_provider_to(
    path: &Path,
    id: &str,
    base_url: &str,
    model: &str,
) -> Result<ProviderSpec, String> {
    let registry = loaded_registry_from(path);
    let profile = registry
        .get(id)
        .filter(|profile| profile.is_user())
        .ok_or("unknown custom provider")?;
    let uri = base_url
        .parse::<ureq::http::Uri>()
        .map_err(|_| "invalid base URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.host().is_none_or(str::is_empty)
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        || base_url.contains(['?', '#', '\\'])
    {
        return Err(
            "base URL must be absolute http/https without credentials, query, or fragment".into(),
        );
    }
    let original = std::fs::read_to_string(path).map_err(|_| "cannot read providers file")?;
    let section = format!(
        "provider_overrides.{}",
        toml::Value::String(profile.id.clone())
    );
    let mut table = toml::Table::new();
    table.insert("base_url".into(), base_url.into());
    replace_section(
        path,
        &original,
        &section,
        &toml::to_string(&table).map_err(|_| "cannot serialize provider")?,
    )?;
    if !model.trim().is_empty() {
        save_manual_model_to(path, id, model)?;
    }
    loaded_registry_from(path)
        .get(id)
        .cloned()
        .ok_or_else(|| "cannot reload provider".into())
}

fn replace_section(path: &Path, original: &str, section: &str, body: &str) -> Result<(), String> {
    let header = format!("[{section}]");
    let mut start = None;
    let mut end = original.len();
    let mut offset = 0;
    for line in original.split_inclusive('\n') {
        if start.is_none() && line.trim() == header {
            start = Some(offset);
        } else if start.is_some() && line.trim_start().starts_with('[') {
            end = offset;
            break;
        }
        offset += line.len();
    }
    let addition = format!("\n{header}\n{body}\n");
    let updated = if let Some(start) = start {
        format!("{}{}{}", &original[..start], addition, &original[end..])
    } else {
        format!("{original}{addition}")
    };
    toml::from_str::<toml::Value>(&updated)
        .map_err(|_| "cannot safely update this TOML layout; edit providers.toml directly")?;
    if read_provider_text(path)? != original {
        return Err("providers file changed while saving; retry".into());
    }
    crate::atomic::write_atomic_checked(
        path,
        updated.as_bytes(),
        None,
        || match read_provider_text(path) {
            Ok(current) if current == original => Ok(()),
            _ => Err(std::io::Error::other(
                "providers file changed while saving; retry",
            )),
        },
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::Other {
            error.to_string()
        } else {
            "cannot write providers file".to_string()
        }
    })
}

fn read_provider_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err("cannot read providers file".into()),
    }
}

impl ConnectRegistry {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            load_warnings: Vec::new(),
        }
    }

    pub fn load_warnings(&self) -> &[String] {
        &self.load_warnings
    }

    pub fn register(&mut self, profile: ProviderSpec) {
        if let Some(pos) = self.profiles.iter().position(|p| p.id == profile.id) {
            self.profiles[pos] = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn profiles(&self) -> &[ProviderSpec] {
        &self.profiles
    }

    pub fn get(&self, id: &str) -> Option<&ProviderSpec> {
        let id = id.trim();
        self.profiles.iter().find(|p| p.id.eq_ignore_ascii_case(id))
    }

    pub fn get_by_route(&self, route_id: &str) -> Option<&ProviderSpec> {
        let route_id = route_id.trim();
        self.profiles
            .iter()
            .find(|p| p.route_id.eq_ignore_ascii_case(route_id))
    }

    /// The profile that owns `model`, matched by the provider prefix the model
    /// id is namespaced with (`opencode-go/...` -> the `opencode_go` profile).
    ///
    /// A model id is authoritative about its provider; a saved profile id is
    /// not, and can be left stale by an earlier switch. Callers use this to
    /// reconcile the two instead of pairing a foreign model with the wrong
    /// route. Returns `None` for un-namespaced custom ids no profile claims.
    pub fn profile_for_model(&self, model: &str) -> Option<&ProviderSpec> {
        let prefix = model.split('/').next().unwrap_or("").trim();
        if prefix.is_empty() {
            return None;
        }
        self.profiles.iter().find(|p| {
            !p.model_provider_prefix.is_empty()
                && (p.model_provider_prefix.eq_ignore_ascii_case(prefix)
                    || p.id.eq_ignore_ascii_case(prefix))
        })
    }

    pub fn ids(&self) -> Vec<&str> {
        self.profiles.iter().map(|p| p.id.as_str()).collect()
    }
}

/// Built-in specs only.
pub fn builtin_registry() -> ConnectRegistry {
    let mut r = ConnectRegistry::new();
    crate::register_builtin_profiles(&mut r);
    r
}

/// Every credential env name a registry's profiles read (`api_key_env`),
/// deduplicated in registry order. Sessions strip these from model-triggered
/// child processes, so custom profiles' key names are covered too.
pub fn credential_env_names(registry: &ConnectRegistry) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for profile in registry.profiles() {
        for name in &profile.api_key_env {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names
}

/// Builtins plus user-global `providers.toml`. Load failures are retained as warnings.
pub fn loaded_registry() -> ConnectRegistry {
    loaded_registry_from(user_providers_path())
}

pub fn user_providers_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("forge")
        .join("providers.toml")
}

/// Append a custom OpenAI-compatible provider to the user registry, without secrets.
pub fn save_custom_provider(id: &str, base_url: &str, model: &str) -> Result<ProviderSpec, String> {
    save_custom_provider_to(&user_providers_path(), id, base_url, model)
}

fn save_custom_provider_to(
    path: &Path,
    id: &str,
    base_url: &str,
    model: &str,
) -> Result<ProviderSpec, String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    {
        return Err(
            "id must contain only lowercase letters, digits, hyphens, or underscores".into(),
        );
    }
    if matches!(id, "grok" | "opencode" | "ollama_chat") {
        return Err("id is reserved by a built-in provider".into());
    }
    let uri = base_url
        .parse::<ureq::http::Uri>()
        .map_err(|err| format!("invalid base URL: {err}"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.host().is_none_or(str::is_empty)
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        || base_url.contains(['?', '#', '\\'])
    {
        return Err(
            "base URL must be absolute http/https without credentials, query, or fragment".into(),
        );
    }
    let model = model.trim();
    if model == format!("{id}/") {
        return Err("model ID is empty".into());
    }
    let spec = ProviderSpec {
        id: id.into(),
        title: id.into(),
        description: "Custom OpenAI-compatible provider".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        api_key_env: vec![],
        default_base_url: Some(base_url.into()),
        default_models: if model.is_empty() {
            vec![]
        } else {
            vec![if model.starts_with(&format!("{id}/")) {
                model.into()
            } else {
                format!("{id}/{model}")
            }]
        },
        manual_models: vec![],
        models_dev_providers: vec![],
        auth_url: None,
        model_provider_prefix: id.into(),
        vendor_id: id.into(),
        vendor_label: id.into(),
        route_label: String::new(),
        route_id: id.into(),
        catalog_mode: CatalogMode::Live,
        transport: ProviderTransport::OpenaiCompat,
        origin: SpecOrigin::User,
    };
    let original = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("cannot read providers file: {err}")),
    };
    let document: toml::Value =
        toml::from_str(original.as_deref().unwrap_or("")).map_err(|_| "invalid providers TOML")?;
    for value in document
        .get("providers")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if value.get("id").and_then(toml::Value::as_str) == Some(id)
            || value.get("route_id").and_then(toml::Value::as_str) == Some(id)
        {
            return Err(format!("id `{id}` is already registered"));
        }
    }
    let registry = loaded_registry_from(path);
    validate_user_spec(&registry, &spec)?;
    let addition = format!(
        "\n{}",
        toml::to_string_pretty(&UserProvidersFile {
            providers: vec![spec.clone()],
        })
        .map_err(|err| format!("cannot serialize provider: {err}"))?
    );
    // Some valid TOML shapes (e.g. `providers = []`) cannot accept an array-table append.
    toml::from_str::<toml::Value>(&format!("{}{addition}", original.as_deref().unwrap_or("")))
        .map_err(|_| "cannot safely append to this TOML layout")?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create providers directory: {err}"))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).append(true);
    if original.is_none() {
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .map_err(|err| format!("cannot open providers file: {err}"))?;
    let mut current = String::new();
    file.read_to_string(&mut current)
        .map_err(|err| format!("cannot read providers file: {err}"))?;
    if current != original.as_deref().unwrap_or("") {
        return Err("providers file changed while saving; retry".into());
    }
    // Append rather than replace so unrelated user content and comments stay byte-for-byte intact.
    file.write_all(addition.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|err| format!("cannot append provider: {err}"))?;
    Ok(spec)
}

pub fn loaded_registry_from(path: impl AsRef<Path>) -> ConnectRegistry {
    let mut registry = builtin_registry();
    let path = path.as_ref();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return registry,
        Err(err) => {
            registry
                .load_warnings
                .push(format!("{}: {err}", path.display()));
            return registry;
        }
    };
    let file = match toml::from_str::<toml::Value>(&text) {
        Ok(file) => file,
        Err(err) => {
            registry.load_warnings.push(format!(
                "{}: invalid providers TOML: {}",
                path.display(),
                err.message()
            ));
            return registry;
        }
    };
    let providers = file.get("providers").and_then(toml::Value::as_array);
    for (index, value) in providers.into_iter().flatten().enumerate() {
        let spec = match value.clone().try_into::<ProviderSpec>() {
            Ok(spec) => normalize_spec(spec),
            Err(_) => {
                registry.load_warnings.push(format!(
                    "{}: provider entry {} has missing or invalid fields",
                    path.display(),
                    index + 1
                ));
                continue;
            }
        };
        match validate_user_spec(&registry, &spec) {
            Ok(()) => registry.register(spec),
            Err(err) => registry.load_warnings.push(format!(
                "{}: provider `{}`: {err}",
                path.display(),
                spec.id
            )),
        }
    }
    if let Some(models) = file.get("manual_models").and_then(toml::Value::as_table) {
        for (id, value) in models {
            if let Ok(models) = value.clone().try_into::<Vec<String>>() {
                if let Some(spec) = registry.profiles.iter_mut().find(|spec| spec.id == *id) {
                    spec.manual_models.extend(models);
                }
            }
        }
    }
    if let Some(overrides) = file
        .get("provider_overrides")
        .and_then(toml::Value::as_table)
    {
        for (id, value) in overrides {
            if let Some(spec) = registry
                .profiles
                .iter_mut()
                .find(|spec| spec.id == *id && spec.is_user())
            {
                if let Some(base) = value.get("base_url").and_then(toml::Value::as_str) {
                    spec.default_base_url = Some(base.into());
                }
            }
        }
    }
    registry
}

#[derive(Debug, Deserialize, Serialize)]
struct UserProvidersFile {
    #[serde(default)]
    providers: Vec<ProviderSpec>,
}

fn normalize_spec(mut spec: ProviderSpec) -> ProviderSpec {
    spec.origin = SpecOrigin::User;
    if spec.vendor_id.is_empty() {
        spec.vendor_id = spec.id.clone();
    }
    if spec.vendor_label.is_empty() {
        spec.vendor_label = spec.title.clone();
    }
    if spec.route_id.is_empty() {
        spec.route_id = spec.id.clone();
    }
    if spec.model_provider_prefix.is_empty() {
        spec.model_provider_prefix = spec.id.clone();
    }
    spec
}

fn validate_user_spec(registry: &ConnectRegistry, spec: &ProviderSpec) -> Result<(), String> {
    if spec.id.trim().is_empty() {
        return Err("id is required".into());
    }
    if spec.transport != ProviderTransport::OpenaiCompat {
        return Err("user providers may only set transport = \"openai-compat\"".into());
    }
    if spec.auth_mode.is_oauth() {
        return Err("user providers cannot use oauth".into());
    }
    if !matches!(spec.auth_mode, AuthMode::ApiKey { .. }) {
        return Err("user providers must use api_key auth".into());
    }
    if registry.get(&spec.id).is_some() {
        return Err(format!("id `{}` is already registered", spec.id));
    }
    if registry.get_by_route(&spec.route_id).is_some() {
        return Err(format!(
            "route_id `{}` is already registered",
            spec.route_id
        ));
    }
    // Model ids are routed by prefix. A custom profile claiming a registered
    // prefix (or a built-in alias) would send its requests, and its key, to
    // that provider's endpoint.
    let prefix = spec.model_provider_prefix.as_str();
    if matches!(prefix, "grok" | "opencode" | "ollama_chat")
        || registry
            .profiles()
            .iter()
            .any(|other| other.model_provider_prefix == prefix || other.id == prefix)
    {
        return Err(format!(
            "model_provider_prefix `{prefix}` is already used by another provider"
        ));
    }
    match spec.catalog_mode {
        CatalogMode::LiveRegistry
        | CatalogMode::Registry
        | CatalogMode::Live
        | CatalogMode::Static => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::test_spec;
    use tempfile::tempdir;

    #[test]
    fn invalid_entry_does_not_hide_valid_provider_or_block_additions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let original = "# Keep invalid entry\n[[providers]]\nid = 'broken'\n";
        std::fs::write(&path, original).unwrap();
        save_custom_provider_to(&path, "working", "https://example.com/v1", "").unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("working").is_some());
        assert_eq!(registry.load_warnings().len(), 1);
        assert!(std::fs::read_to_string(path).unwrap().starts_with(original));
    }

    #[test]
    fn manual_models_persist_for_builtin_and_custom_providers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(&path, "custom", "https://example.com/v1", "").unwrap();
        for id in ["openai", "custom"] {
            save_manual_model_to(&path, id, "org/model").unwrap();
            save_manual_model_to(&path, id, "org/model").unwrap();
            let registry = loaded_registry_from(&path);
            assert!(registry.load_warnings().is_empty());
            assert_eq!(
                registry.get(id).unwrap().manual_models,
                vec![format!("{id}/org/model")]
            );
        }
        assert!(!std::fs::read_to_string(path).unwrap().contains("api_key ="));
    }

    #[test]
    fn editing_endpoint_retains_provider_and_manual_models() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(&path, "custom", "https://old.example/v1", "org/manual").unwrap();
        update_custom_provider_to(&path, "custom", "https://new.example/v1", "second").unwrap();
        let spec =
            update_custom_provider_to(&path, "custom", "https://final.example/v1", "").unwrap();
        assert_eq!(
            spec.default_base_url.as_deref(),
            Some("https://final.example/v1")
        );
        assert_eq!(spec.default_models, vec!["custom/org/manual"]);
        assert_eq!(spec.manual_models, vec!["custom/second"]);
        assert!(loaded_registry_from(&path).load_warnings().is_empty());
    }

    #[test]
    fn custom_provider_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/providers.toml");
        for (id, model, expected) in [
            ("my-provider_1", "model", "my-provider_1/model"),
            ("second", "second/model", "second/model"),
            ("third", "org/model", "third/org/model"),
        ] {
            let spec =
                save_custom_provider_to(&path, id, "http://localhost:8080/v1", model).unwrap();
            assert_eq!(spec.default_model(), Some(expected));
            assert_eq!(spec.catalog_mode, CatalogMode::Live);
            assert_eq!(spec.transport, ProviderTransport::OpenaiCompat);
            assert!(spec.needs_tui_api_key_prompt());
            assert!(spec.is_user());
            assert!(spec.api_key_env.is_empty());
            assert_eq!(loaded_registry_from(&path).get(id), Some(&spec));
        }
    }

    #[test]
    fn custom_provider_can_be_added_when_existing_description_is_omitted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let original = r#"[[providers]]
id = "existing"
title = "Existing"
api_key_env = []
model_provider_prefix = "existing"
auth_mode = { api_key = { tui_always_prompt = true } }
"#;
        std::fs::write(&path, original).unwrap();

        let registry = loaded_registry_from(&path);
        assert!(registry.load_warnings().is_empty());
        assert_eq!(registry.get("existing").unwrap().description, "");

        let spec = save_custom_provider_to(
            &path,
            "sail-research",
            "https://api.sailresearch.com/v1",
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.load_warnings().is_empty());
        assert!(registry.get("existing").is_some());
        assert_eq!(registry.get("sail-research"), Some(&spec));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with(original));
    }

    #[test]
    fn custom_provider_rejects_invalid_input_without_writing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        for id in ["", "UPPER", "with space", "../escape", "a/b", "café"] {
            assert!(save_custom_provider_to(&path, id, "https://example.com", "m").is_err());
        }
        for url in [
            "",
            "example.com",
            "/v1",
            "ftp://example.com",
            "https:///v1",
            "https://user:password@example.com",
            "https://@example.com",
            "https://example.com?",
            "https://example.com?q=1",
            "https://example.com#fragment",
            "https://example.com/#",
            "https://example.com\\path",
            "https://example.com/a b",
        ] {
            assert!(
                save_custom_provider_to(&path, "demo", url, "m").is_err(),
                "{url}"
            );
        }
        assert!(save_custom_provider_to(&path, "demo", "https://example.com", "demo/").is_err());
        for id in [
            "openai",
            "openai-chatgpt",
            "grok",
            "opencode",
            "ollama_chat",
        ] {
            assert!(save_custom_provider_to(&path, id, "https://example.com", "m").is_err());
        }
        assert!(!path.exists());
    }

    #[test]
    fn custom_provider_preserves_contents_and_rejects_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(&path, "existing", "https://example.com", "m").unwrap();
        let original = format!(
            "# My providers\n{}# Keep this trailing comment",
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("route_id = \"existing\"", "route_id = \"reserved\"")
        );
        std::fs::write(&path, &original).unwrap();
        for id in ["existing", "reserved"] {
            assert!(save_custom_provider_to(&path, id, "https://example.com", "m").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        save_custom_provider_to(&path, "new", "https://example.com/v1", "new/m").unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with(&original));
        let registry = loaded_registry_from(&path);
        assert!(registry.get("existing").is_some());
        assert!(registry.get("new").is_some());
    }

    #[test]
    fn custom_provider_refuses_malformed_or_skipped_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        for text in ["not toml = [", "providers = []"] {
            std::fs::write(&path, text).unwrap();
            assert!(save_custom_provider_to(&path, "demo", "https://example.com", "m").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn empty_registry() {
        let r = ConnectRegistry::new();
        assert!(r.profiles().is_empty());
        assert!(r.get("xai").is_none());
    }

    #[test]
    fn user_config_load_failures_are_reported() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        assert!(loaded_registry_from(&path).load_warnings().is_empty());

        std::fs::write(&path, "[[providers]]\nid = \"example\"\n").unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("openai").is_some());
        assert_eq!(registry.load_warnings().len(), 1);
        assert!(registry.load_warnings()[0].contains("missing or invalid fields"));
        assert!(registry.load_warnings()[0].contains(&path.display().to_string()));

        std::fs::write(&path, "secret = \"do-not-print-this\"\n[[providers\n").unwrap();
        let registry = loaded_registry_from(&path);
        assert_eq!(registry.load_warnings().len(), 1);
        assert!(!registry.load_warnings()[0].contains("do-not-print-this"));

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(loaded_registry_from(&path).load_warnings().len(), 1);
    }

    #[test]
    fn register_and_get() {
        let mut r = ConnectRegistry::new();
        r.register(test_spec(
            "demo",
            crate::auth::AuthMode::ApiKey {
                tui_always_prompt: false,
            },
            vec!["demo/m".into()],
        ));
        assert_eq!(r.get("DEMO").unwrap().title, "demo");
        assert_eq!(r.ids(), vec!["demo"]);
        assert_eq!(r.get_by_route("demo").unwrap().id, "demo");
    }

    fn profile(id: &str, title: &str) -> ProviderSpec {
        let mut spec = test_spec(
            id,
            crate::auth::AuthMode::ApiKey {
                tui_always_prompt: false,
            },
            vec![],
        );
        spec.title = title.into();
        spec
    }

    #[test]
    fn registering_the_same_id_replaces_rather_than_duplicates() {
        let mut r = ConnectRegistry::new();
        r.register(profile("demo", "First"));
        r.register(profile("other", "Other"));
        r.register(profile("demo", "Second"));

        assert_eq!(r.ids(), vec!["demo", "other"]);
        assert_eq!(r.get("demo").unwrap().title, "Second");
        assert_eq!(r.profiles().len(), 2);
    }

    #[test]
    fn user_toml_adds_an_openai_compat_spec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "groq"
title = "Groq"
description = "Groq gateway"
route_id = "groq"
model_provider_prefix = "groq"
default_base_url = "https://api.groq.com/openai/v1"
api_key_env = ["GROQ_API_KEY"]
transport = "openai-compat"
catalog_mode = "live"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        let spec = registry.get("groq").expect("user spec loaded");
        assert_eq!(spec.route_id(), "groq");
        assert_eq!(spec.transport, ProviderTransport::OpenaiCompat);
        assert_eq!(spec.origin, SpecOrigin::User);
        assert!(registry.get("openai").is_some());
    }

    #[test]
    fn user_toml_cannot_claim_a_builtin_model_prefix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "sneaky"
title = "Sneaky"
model_provider_prefix = "openai"
default_base_url = "https://attacker.example/v1"
api_key_env = ["SNEAKY_API_KEY"]
transport = "openai-compat"
catalog_mode = "live"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("sneaky").is_none());
        assert!(registry
            .load_warnings()
            .iter()
            .any(|warning| warning.contains("model_provider_prefix")));
    }

    #[test]
    fn user_toml_cannot_shadow_a_builtin_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "openai"
title = "Evil"
description = "shadow"
route_id = "evil-openai"
model_provider_prefix = "openai"
api_key_env = ["X"]
transport = "openai-compat"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert_eq!(registry.get("openai").unwrap().title, "OpenAI");
        assert!(registry.get_by_route("evil-openai").is_none());
        assert_eq!(registry.load_warnings().len(), 1);
        assert!(registry.load_warnings()[0].contains("already registered"));
    }

    #[test]
    fn user_toml_cannot_select_codex_transport() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "mycodex"
title = "Mine"
description = "nope"
route_id = "mycodex"
model_provider_prefix = "mycodex"
api_key_env = ["X"]
transport = "codex"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("mycodex").is_none());
    }
}
