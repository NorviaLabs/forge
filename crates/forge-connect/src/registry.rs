//! Provider spec registry: builtins plus user-global TOML.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::profile::{
    AuthKind, AuthPrompt, AuthSpec, CatalogMode, EditableField, ProviderSpec, SpecOrigin, WireKind,
};

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
    // Custom providers, and built-ins whose definition lists `base_url` as
    // editable (Ollama), take a `[provider_overrides]` base URL.
    let profile = registry
        .get(id)
        .filter(|profile| profile.is_user() || profile.editable.contains(&EditableField::BaseUrl))
        .ok_or("this provider's base URL cannot be edited")?;
    check_base_url(base_url)?;
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

/// Every credential env name a registry's profiles read: each key
/// profile's generated `FORGE_<PROFILE_ID>_API_KEY` and every `api_key_env`
/// name, deduplicated in registry order. Sessions strip these from
/// model-triggered child processes, so custom profiles' key names are
/// covered too.
pub fn credential_env_names(registry: &ConnectRegistry) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for profile in registry.profiles() {
        for name in crate::resolve::env_names_for(profile).0 {
            if !names.contains(&name) {
                names.push(name);
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

/// Append a custom provider (chat-completions or Anthropic Messages) to the
/// user registry, without secrets.
pub fn save_custom_provider(
    id: &str,
    base_url: &str,
    model: &str,
    wire: WireKind,
) -> Result<ProviderSpec, String> {
    save_custom_provider_to(&user_providers_path(), id, base_url, model, wire)
}

fn save_custom_provider_to(
    path: &Path,
    id: &str,
    base_url: &str,
    model: &str,
    wire: WireKind,
) -> Result<ProviderSpec, String> {
    if !matches!(
        wire,
        WireKind::ChatCompletions | WireKind::AnthropicMessages
    ) {
        return Err("custom providers use chat-completions or anthropic-messages".into());
    }
    check_profile_id(id)?;
    if crate::normalize::is_reserved_id(id) {
        return Err("id is reserved by a built-in provider".into());
    }
    check_base_url(base_url)?;
    let model = model.trim();
    if model == format!("{id}/") {
        return Err("model ID is empty".into());
    }
    let spec = ProviderSpec {
        id: id.into(),
        title: id.into(),
        description: match wire {
            WireKind::AnthropicMessages => "Custom Anthropic-compatible provider",
            _ => "Custom OpenAI-compatible provider",
        }
        .into(),
        // The §1.1 fields plus the legacy `auth_mode`, `transport`,
        // `model_provider_prefix`, `route_id` and `catalog_mode`, which stay
        // until stage 8 so an older binary can still read the entry.
        auth: Some(AuthSpec {
            kind: AuthKind::ApiKey,
            prompt: AuthPrompt::Tui,
            ..AuthSpec::default()
        }),
        wire: Some(wire),
        base_url: base_url.into(),
        // The typed model is the user's own pick, so it lists with the
        // manual models and discovery never hides it.
        manual_models: if model.is_empty() {
            vec![]
        } else {
            vec![if model.starts_with(&format!("{id}/")) {
                model.into()
            } else {
                format!("{id}/{model}")
            }]
        },
        models_dev_providers: vec![],
        auth_url: None,
        model_provider_prefix: id.into(),
        provider: id.into(),
        provider_label: id.into(),
        route_id: id.into(),
        catalog_mode: CatalogMode::Live,
        origin: SpecOrigin::User,
        ..ProviderSpec::blank()
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
    // The legacy `auth_mode` and `transport` keys are written until stage 8
    // so an older binary can still read the entry.
    let mut entry =
        toml::Value::try_from(&spec).map_err(|err| format!("cannot serialize provider: {err}"))?;
    if let Some(table) = entry.as_table_mut() {
        table.insert(
            "auth_mode".into(),
            toml::toml! { api_key = { tui_always_prompt = true } }.into(),
        );
        table.insert(
            "transport".into(),
            crate::profile::legacy_transport_for(wire).into(),
        );
    }
    let mut file = toml::Table::new();
    file.insert("providers".into(), toml::Value::Array(vec![entry]));
    let addition = format!(
        "\n{}",
        toml::to_string_pretty(&file).map_err(|err| format!("cannot serialize provider: {err}"))?
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
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            normalize_model_lists(&mut registry);
            return registry;
        }
        Err(err) => {
            registry
                .load_warnings
                .push(format!("{}: {err}", path.display()));
            normalize_model_lists(&mut registry);
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
            normalize_model_lists(&mut registry);
            return registry;
        }
    };
    let providers = file.get("providers").and_then(toml::Value::as_array);
    for (index, value) in providers.into_iter().flatten().enumerate() {
        let name = value
            .get("id")
            .and_then(toml::Value::as_str)
            .map(|id| format!("provider `{id}`"))
            .unwrap_or_else(|| format!("provider entry {}", index + 1));
        if let Err(err) = check_custom_keys(value) {
            registry
                .load_warnings
                .push(format!("{}: {name}: {err}", path.display()));
            continue;
        }
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
            Err(err) => registry
                .load_warnings
                .push(format!("{}: {name}: {err}", path.display())),
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
            if let Err(err) = apply_override(&mut registry, id, value) {
                registry.load_warnings.push(format!(
                    "{}: provider_overrides `{id}`: {err}",
                    path.display()
                ));
            }
        }
    }
    normalize_model_lists(&mut registry);
    registry
}

/// Apply one `[provider_overrides.<id>]` table: a built-in's `editable`
/// fields, or a custom profile's `base_url`.
fn apply_override(
    registry: &mut ConnectRegistry,
    id: &str,
    value: &toml::Value,
) -> Result<(), String> {
    let spec = registry
        .profiles
        .iter_mut()
        .find(|spec| spec.id == id)
        .ok_or("no provider has this id; ignored")?;
    let table = value.as_table().ok_or("must be a table; ignored")?;
    for (key, value) in table {
        if key != "base_url" {
            return Err(format!("`{key}` cannot be overridden; ignored"));
        }
        if !spec.is_user() && !spec.editable.contains(&EditableField::BaseUrl) {
            return Err("this provider's base_url is not editable; ignored".into());
        }
        let base = value.as_str().ok_or("base_url must be a string; ignored")?;
        check_base_url(base)?;
    }
    if let Some(base) = table.get("base_url").and_then(toml::Value::as_str) {
        spec.base_url = base.into();
    }
    Ok(())
}

/// Normalize every profile's model lists (design §2): `default_models`,
/// then `manual_models` without the defaults, each entry re-prefixed with
/// the profile's current prefix.
fn normalize_model_lists(registry: &mut ConnectRegistry) {
    for spec in &mut registry.profiles {
        let defaults = crate::normalize::normalize_models(spec, &spec.default_models);
        let manual = crate::normalize::normalize_models(spec, &spec.manual_models)
            .into_iter()
            .filter(|model| !defaults.contains(model))
            .collect();
        spec.default_models = defaults;
        spec.manual_models = manual;
    }
}

/// Keys a custom entry may set (§1.1 "C"), including the old names of
/// renamed fields and the legacy keys.
const CUSTOM_KEYS: &[&str] = &[
    "id",
    "title",
    "label",
    "route_label",
    "provider",
    "vendor_id",
    "provider_label",
    "vendor_label",
    "description",
    "auth",
    "auth_url",
    "api_key_env",
    "base_url",
    "default_base_url",
    "wire",
    "default_models",
    "manual_models",
    "verify",
    "models_dev_providers",
    "auth_mode",
    "transport",
    "catalog_mode",
    "route_id",
    "model_provider_prefix",
];

/// The C-field rule: a custom entry sets only C fields, and legacy keys
/// meet their constraints.
fn check_custom_keys(value: &toml::Value) -> Result<(), String> {
    let table = value.as_table().ok_or("must be a table")?;
    if let Some(key) = table
        .keys()
        .find(|key| !CUSTOM_KEYS.contains(&key.as_str()))
    {
        return Err(format!(
            "`{key}` is only allowed in built-in providers; disabled"
        ));
    }
    if let Some(auth) = table.get("auth") {
        let auth = auth.as_table().ok_or("`auth` must be a table")?;
        if let Some(key) = auth.keys().find(|key| key.as_str() != "kind") {
            return Err(format!(
                "`auth.{key}` is only allowed in built-in providers; disabled"
            ));
        }
        match auth.get("kind").and_then(toml::Value::as_str) {
            None | Some("api-key" | "api-key-optional") => {}
            Some(other) => {
                return Err(format!(
                    "custom providers may use auth.kind = \"api-key\" or \"api-key-optional\", not \"{other}\""
                ))
            }
        }
    }
    match table.get("wire").and_then(toml::Value::as_str) {
        None | Some("chat-completions" | "anthropic-messages") => {}
        Some(other) => {
            return Err(format!(
                "custom providers may use wire = \"chat-completions\" or \"anthropic-messages\", not \"{other}\""
            ))
        }
    }
    match table.get("transport").and_then(toml::Value::as_str) {
        None | Some("openai-compat" | "anthropic") => {}
        Some(_) => {
            return Err(
                "user providers may only set transport = \"openai-compat\" or \"anthropic\"".into(),
            )
        }
    }
    if let Some(mode) = table.get("auth_mode") {
        if mode.get("api_key").is_none() {
            return Err("user providers must use api_key auth".into());
        }
    }
    Ok(())
}

fn check_profile_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    {
        return Err(
            "id must contain only lowercase letters, digits, hyphens, or underscores".into(),
        );
    }
    Ok(())
}

/// Absolute http/https with no userinfo, query, fragment, or backslash.
fn check_base_url(base_url: &str) -> Result<(), String> {
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
    Ok(())
}

/// `[A-Z][A-Z0-9_]*` ending in `_KEY` or `_TOKEN`.
fn is_valid_key_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|c| c.is_ascii_uppercase())
        && bytes.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        && (name.ends_with("_KEY") || name.ends_with("_TOKEN"))
}

fn normalize_spec(mut spec: ProviderSpec) -> ProviderSpec {
    spec.origin = SpecOrigin::User;
    // Legacy keys map into the §1.1 fields when those are unset.
    if spec.auth.is_none() {
        spec.auth = spec
            .legacy_auth_mode
            .as_ref()
            .and_then(crate::profile::auth_from_legacy_mode);
    }
    if spec.wire.is_none() {
        spec.wire = spec
            .legacy_transport
            .as_deref()
            .and_then(crate::profile::wire_from_legacy_transport);
    }
    // Consumed: the raw legacy values are not part of the loaded profile.
    spec.legacy_auth_mode = None;
    spec.legacy_transport = None;
    if spec.title.trim().is_empty() {
        spec.title = spec.id.clone();
    }
    if spec.provider.is_empty() {
        spec.provider = spec.id.clone();
    }
    if spec.provider_label.is_empty() {
        spec.provider_label = spec.title.clone();
    }
    if spec.route_id.is_empty() {
        spec.route_id = spec.id.clone();
    }
    if spec.model_provider_prefix.is_empty() {
        spec.model_provider_prefix = spec.id.clone();
    }
    // A custom entry's `default_models` are ids its user typed, not a
    // shipped fallback; they list with the manual models, so discovery
    // never hides them.
    let typed = std::mem::take(&mut spec.default_models);
    spec.manual_models.splice(0..0, typed);
    spec
}

/// Rules every profile meets against the others (design §2): unique env
/// id, one `provider_label` per provider, `wire_overrides` patterns, and
/// `credential_from` targets.
fn validate_profile(others: &[ProviderSpec], spec: &ProviderSpec) -> Result<(), String> {
    check_profile_id(&spec.id)?;
    let env_id = crate::normalize::env_profile_id(&spec.id);
    if let Some(other) = others
        .iter()
        .find(|other| other.id != spec.id && crate::normalize::env_profile_id(&other.id) == env_id)
    {
        return Err(format!(
            "id `{}` and `{}` share the env name {env_id}",
            spec.id, other.id
        ));
    }
    if let Some(other) = others.iter().find(|other| {
        other.id != spec.id
            && other.provider == spec.provider
            && other.provider_label != spec.provider_label
    }) {
        return Err(format!(
            "provider `{}` is labelled `{}` by `{}`",
            spec.provider, other.provider_label, other.id
        ));
    }
    for entry in &spec.wire_overrides {
        if entry.models.is_empty() {
            return Err("a wire_overrides entry lists no models".into());
        }
        for pattern in &entry.models {
            let star = pattern.find('*');
            if pattern.is_empty() || star.is_some_and(|at| at + 1 != pattern.len() || at == 0) {
                return Err(format!(
                    "wire_overrides pattern `{pattern}` must be an id or a prefix ending in `*`"
                ));
            }
        }
    }
    if let Some(target) = &spec.credential_from {
        let target = others
            .iter()
            .find(|other| &other.id == target && other.id != spec.id)
            .ok_or_else(|| format!("credential_from `{target}` is not another profile"))?;
        if target.provider != spec.provider || target.credential_from.is_some() {
            return Err(format!(
                "credential_from `{}` must be in the same provider and not use credential_from",
                target.id
            ));
        }
    }
    Ok(())
}

fn validate_user_spec(registry: &ConnectRegistry, spec: &ProviderSpec) -> Result<(), String> {
    if spec.id.trim().is_empty() {
        return Err("id is required".into());
    }
    if registry.get(&spec.id).is_some() {
        return Err(format!("id `{}` is already registered", spec.id));
    }
    if crate::normalize::is_reserved_id(&spec.id) {
        return Err(format!(
            "id `{}` is reserved by a built-in provider",
            spec.id
        ));
    }
    if !matches!(
        spec.effective_wire(),
        WireKind::ChatCompletions | WireKind::AnthropicMessages
    ) {
        return Err(
            "user providers may only set transport = \"openai-compat\" or \"anthropic\"".into(),
        );
    }
    if spec.effective_auth().is_oauth() {
        return Err("user providers cannot use oauth".into());
    }
    if spec.credential_from.is_some() {
        return Err("credential_from is only allowed in built-in providers".into());
    }
    check_base_url(&spec.base_url)?;
    let builtin_names = credential_env_names(&builtin_registry());
    for name in &spec.api_key_env {
        if !is_valid_key_env_name(name) {
            return Err(format!(
                "api_key_env `{name}` must be uppercase and end in _KEY or _TOKEN"
            ));
        }
        if builtin_names.contains(name)
            || (name.starts_with("FORGE_") && name.ends_with("_API_KEY"))
        {
            return Err(format!(
                "api_key_env `{name}` is reserved by a built-in provider"
            ));
        }
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
    if crate::normalize::is_reserved_id(prefix)
        || registry
            .profiles()
            .iter()
            .any(|other| other.model_provider_prefix == prefix || other.id == prefix)
    {
        return Err(format!(
            "model_provider_prefix `{prefix}` is already used by another provider"
        ));
    }
    validate_profile(registry.profiles(), spec)
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
        save_custom_provider_to(
            &path,
            "working",
            "https://example.com/v1",
            "",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("working").is_some());
        assert_eq!(registry.load_warnings().len(), 1);
        assert!(std::fs::read_to_string(path).unwrap().starts_with(original));
    }

    #[test]
    fn manual_models_persist_for_builtin_and_custom_providers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(
            &path,
            "custom",
            "https://example.com/v1",
            "",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
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
        save_custom_provider_to(
            &path,
            "custom",
            "https://old.example/v1",
            "org/manual",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        update_custom_provider_to(&path, "custom", "https://new.example/v1", "second").unwrap();
        let spec =
            update_custom_provider_to(&path, "custom", "https://final.example/v1", "").unwrap();
        assert_eq!(spec.base_url, "https://final.example/v1");
        assert!(spec.default_models.is_empty());
        assert_eq!(
            spec.manual_models,
            vec!["custom/org/manual", "custom/second"]
        );
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
            let spec = save_custom_provider_to(
                &path,
                id,
                "http://localhost:8080/v1",
                model,
                crate::WireKind::ChatCompletions,
            )
            .unwrap();
            assert_eq!(spec.default_model(), Some(expected));
            assert_eq!(spec.catalog_mode, CatalogMode::Live);
            assert_eq!(spec.effective_wire(), crate::WireKind::ChatCompletions);
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
default_base_url = "https://existing.example/v1"
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
            crate::WireKind::ChatCompletions,
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
            assert!(save_custom_provider_to(
                &path,
                id,
                "https://example.com",
                "m",
                crate::WireKind::ChatCompletions
            )
            .is_err());
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
                save_custom_provider_to(&path, "demo", url, "m", crate::WireKind::ChatCompletions)
                    .is_err(),
                "{url}"
            );
        }
        assert!(save_custom_provider_to(
            &path,
            "demo",
            "https://example.com",
            "demo/",
            crate::WireKind::ChatCompletions
        )
        .is_err());
        for id in [
            "openai",
            "openai-chatgpt",
            "grok",
            "opencode",
            "ollama_chat",
        ] {
            assert!(save_custom_provider_to(
                &path,
                id,
                "https://example.com",
                "m",
                crate::WireKind::ChatCompletions
            )
            .is_err());
        }
        assert!(!path.exists());
    }

    #[test]
    fn custom_provider_preserves_contents_and_rejects_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(
            &path,
            "existing",
            "https://example.com",
            "m",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        let original = format!(
            "# My providers\n{}# Keep this trailing comment",
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("route_id = \"existing\"", "route_id = \"reserved\"")
        );
        std::fs::write(&path, &original).unwrap();
        for id in ["existing", "reserved"] {
            assert!(save_custom_provider_to(
                &path,
                id,
                "https://example.com",
                "m",
                crate::WireKind::ChatCompletions
            )
            .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        save_custom_provider_to(
            &path,
            "new",
            "https://example.com/v1",
            "new/m",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
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
            assert!(save_custom_provider_to(
                &path,
                "demo",
                "https://example.com",
                "m",
                crate::WireKind::ChatCompletions
            )
            .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn only_editable_builtin_fields_can_be_edited() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, "").unwrap();
        let ollama =
            update_custom_provider_to(&path, "ollama", "http://10.0.0.5:11434", "").unwrap();
        assert_eq!(ollama.base_url, "http://10.0.0.5:11434");
        assert!(update_custom_provider_to(&path, "openai", "https://evil.example/v1", "").is_err());
        assert!(loaded_registry_from(&path).load_warnings().is_empty());
    }

    #[test]
    fn a_custom_anthropic_messages_provider_saves_and_resolves() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let spec = save_custom_provider_to(
            &path,
            "claude-proxy",
            "https://proxy.example/v1",
            "claude-sonnet-4-6",
            WireKind::AnthropicMessages,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(
            registry.load_warnings().is_empty(),
            "{:?}",
            registry.load_warnings()
        );
        assert_eq!(registry.get("claude-proxy"), Some(&spec));
        let definition = crate::RouteDefinition::from_spec(&spec);
        assert_eq!(definition.wire, WireKind::AnthropicMessages);
        assert_eq!(definition.attach, crate::Attach::XApiKey);
        assert_eq!(definition.base_url, "https://proxy.example");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("transport = \"anthropic\""), "{text}");
        assert!(save_custom_provider_to(
            &path,
            "nope",
            "https://x.example",
            "",
            WireKind::Responses
        )
        .is_err());
    }

    #[test]
    fn builtins_meet_the_profile_rules() {
        let registry = builtin_registry();
        for spec in registry.profiles() {
            validate_profile(registry.profiles(), spec).unwrap_or_else(|err| {
                panic!("{}: {err}", spec.id);
            });
            check_base_url(&spec.base_url).unwrap();
        }
    }

    #[test]
    fn schema_form_custom_entry_loads_with_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "my-llm"
title = "My LLM"
base_url = "https://llm.example.com/v1"
auth = { kind = "api-key-optional" }
api_key_env = ["MY_LLM_TOKEN"]

[manual_models]
my-llm = ["meta-llama/Llama-3.3-70B", "my-llm/meta-llama/Llama-3.3-70B"]
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(
            registry.load_warnings().is_empty(),
            "{:?}",
            registry.load_warnings()
        );
        let spec = registry.get("my-llm").unwrap();
        assert_eq!(spec.provider, "my-llm");
        assert_eq!(spec.provider_label, "My LLM");
        assert_eq!(spec.model_provider_prefix, "my-llm");
        assert_eq!(spec.effective_wire(), WireKind::ChatCompletions);
        assert_eq!(spec.effective_auth().kind, AuthKind::ApiKeyOptional);
        assert!(!spec.needs_tui_api_key_prompt());
        assert_eq!(spec.manual_models, vec!["my-llm/meta-llama/Llama-3.3-70B"]);
    }

    #[test]
    fn custom_entries_breaking_a_rule_are_disabled_with_a_warning() {
        let cases = [
            (
                "wire_overrides = [{ models = [\"m\"], wire = \"responses\" }]",
                "only allowed in built-in",
            ),
            ("auth = { kind = \"oauth-device\" }", "auth.kind"),
            (
                "auth = { kind = \"api-key\", prompt = \"never\" }",
                "auth.prompt",
            ),
            ("wire = \"responses\"", "chat-completions"),
            ("api_key_env = [\"lowercase_key\"]", "must be uppercase"),
            ("api_key_env = [\"OPENAI_API_KEY\"]", "reserved"),
            ("api_key_env = [\"FORGE_DEMO_API_KEY\"]", "reserved"),
            (
                "provider = \"openai\"\nprovider_label = \"Not OpenAI\"",
                "labelled",
            ),
        ];
        for (extra, message) in cases {
            let dir = tempdir().unwrap();
            let path = dir.path().join("providers.toml");
            std::fs::write(
                &path,
                format!(
                    "[[providers]]\nid = \"demo\"\ntitle = \"Demo\"\nbase_url = \"https://demo.example/v1\"\n{extra}\n"
                ),
            )
            .unwrap();
            let registry = loaded_registry_from(&path);
            assert!(registry.get("demo").is_none(), "{extra}");
            assert_eq!(registry.load_warnings().len(), 1, "{extra}");
            assert!(
                registry.load_warnings()[0].contains(message),
                "{extra}: {:?}",
                registry.load_warnings()
            );
        }
    }

    #[test]
    fn reserved_and_env_colliding_ids_are_disabled() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "grok"
title = "Grok"
base_url = "https://grok.example/v1"

[[providers]]
id = "my-llm"
title = "A"
base_url = "https://a.example/v1"

[[providers]]
id = "my_llm"
title = "B"
base_url = "https://b.example/v1"
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert!(registry.get("grok").is_none());
        assert!(registry.get("my-llm").is_some());
        assert!(registry.get("my_llm").is_none());
        let warnings = registry.load_warnings().join("\n");
        assert!(warnings.contains("reserved"), "{warnings}");
        assert!(warnings.contains("MY_LLM"), "{warnings}");
    }

    #[test]
    fn overrides_apply_to_editable_builtin_fields_and_custom_base_urls() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[providers]]
id = "custom"
title = "Custom"
base_url = "https://old.example/v1"

[provider_overrides.ollama]
base_url = "http://10.0.0.5:11434"

[provider_overrides.custom]
base_url = "https://new.example/v1"

[provider_overrides.openai]
base_url = "https://evil.example/v1"

[provider_overrides.gone]
base_url = "https://gone.example/v1"
"#,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert_eq!(
            registry.get("ollama").unwrap().base_url,
            "http://10.0.0.5:11434"
        );
        assert_eq!(
            registry.get("custom").unwrap().base_url,
            "https://new.example/v1"
        );
        assert_eq!(
            registry.get("openai").unwrap().base_url,
            crate::openai_profile().base_url
        );
        let warnings = registry.load_warnings().join("\n");
        assert!(
            warnings.contains("`openai`: this provider's base_url is not editable"),
            "{warnings}"
        );
        assert!(
            warnings.contains("`gone`: no provider has this id"),
            "{warnings}"
        );
    }

    #[test]
    fn builtin_manual_models_are_normalized_to_the_current_prefix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "[manual_models]\nopencode_go = [\"opencode/kimi\", \"opencode-go/kimi\", \"glm\"]\nxai = [\"grok/grok-4\"]\n",
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert_eq!(
            registry.get("opencode_go").unwrap().manual_models,
            vec!["opencode-go/kimi", "opencode-go/glm"]
        );
        assert_eq!(
            registry.get("xai").unwrap().manual_models,
            vec!["xai/grok-4"]
        );
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
            crate::AuthSpec::api_key(crate::AuthPrompt::Never),
            vec!["demo/m".into()],
        ));
        assert_eq!(r.get("DEMO").unwrap().title, "demo");
        assert_eq!(r.ids(), vec!["demo"]);
        assert_eq!(r.get_by_route("demo").unwrap().id, "demo");
    }

    fn profile(id: &str, title: &str) -> ProviderSpec {
        let mut spec = test_spec(
            id,
            crate::AuthSpec::api_key(crate::AuthPrompt::Never),
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
        assert_eq!(spec.effective_wire(), crate::WireKind::ChatCompletions);
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
base_url = "https://evil.example/v1"
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
