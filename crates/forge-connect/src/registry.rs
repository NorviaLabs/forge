//! Provider spec registry: builtins plus user-global TOML.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::profile::{
    AuthKind, AuthPrompt, AuthSpec, EditableField, ProviderSpec, SpecOrigin, WireKind,
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

/// The fields a custom provider's edit form changes in its own
/// `[[providers]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderDetails {
    pub title: String,
    pub label: String,
    pub api_key_env: Vec<String>,
}

/// Edit a provider's base URL (custom providers and built-ins whose base
/// URL is `editable`), add an optional manual model, and, for a custom
/// provider, replace its title, label, and key env names. Everything is
/// validated before the first write.
pub fn update_custom_provider(
    id: &str,
    base_url: &str,
    model: &str,
    details: Option<&ProviderDetails>,
) -> Result<ProviderSpec, String> {
    update_custom_provider_to(&user_providers_path(), id, base_url, model, details)
}

fn update_custom_provider_to(
    path: &Path,
    id: &str,
    base_url: &str,
    model: &str,
    details: Option<&ProviderDetails>,
) -> Result<ProviderSpec, String> {
    let registry = loaded_registry_from(path);
    // Custom providers, and built-ins whose definition lists `base_url` as
    // editable (Ollama), take a `[provider_overrides]` base URL.
    let profile = registry
        .get(id)
        .filter(|profile| profile.is_user() || profile.editable.contains(&EditableField::BaseUrl))
        .ok_or("this provider's base URL cannot be edited")?;
    check_base_url(base_url)?;
    if let Some(details) = details {
        if !profile.is_user() {
            return Err("only a custom provider's details can be edited".into());
        }
        check_details(details)?;
        replace_provider_entry(path, &profile.id, details)?;
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

fn check_details(details: &ProviderDetails) -> Result<(), String> {
    if details.title.trim().is_empty() {
        return Err("a title is required".into());
    }
    if [&details.title, &details.label]
        .iter()
        .any(|text| text.chars().any(char::is_control))
    {
        return Err("title and label cannot contain control characters".into());
    }
    // The message never echoes the value: a key pasted here is a secret.
    if details
        .api_key_env
        .iter()
        .any(|name| !is_valid_key_env_name(name))
    {
        return Err(
            "key env names must look like MY_API_KEY (A-Z, 0-9, _; ending in _KEY or _TOKEN)"
                .into(),
        );
    }
    // Same rule the loader applies, checked before writing: a reserved name
    // would be saved, then rejected on load. A reserved name is a known
    // variable name, never a secret, so it may be echoed.
    let builtin_names = credential_env_names(&builtin_registry());
    if let Some(name) = details
        .api_key_env
        .iter()
        .find(|name| is_reserved_key_env_name(&builtin_names, name))
    {
        return Err(format!(
            "key env name `{name}` is reserved by a built-in provider"
        ));
    }
    Ok(())
}

/// A built-in provider's key variable, or any `FORGE_*_API_KEY`.
fn is_reserved_key_env_name(builtin_names: &[String], name: &str) -> bool {
    builtin_names.iter().any(|builtin| builtin == name)
        || (name.starts_with("FORGE_") && name.ends_with("_API_KEY"))
}

const LAYOUT: &str = "cannot safely edit this TOML layout; edit providers.toml directly";

/// The byte range of the `index`th `[[providers]]` entry: from its header to
/// the next header that is not one of its own sub-tables (`[providers.auth]`,
/// ...). Comments and blank lines just above that next header belong to what
/// follows, so they are outside the range.
fn provider_entry_range(original: &str, index: usize) -> Option<std::ops::Range<usize>> {
    let mut seen = 0;
    let mut start = None;
    let mut end = original.len();
    let mut offset = 0;
    for line in original.split_inclusive('\n') {
        let header = line.trim();
        if header == "[[providers]]" {
            if start.is_some() {
                end = offset;
                break;
            }
            if seen == index {
                start = Some(offset);
            }
            seen += 1;
        } else if start.is_some()
            && header.starts_with('[')
            && !header.starts_with("[providers.")
            && !header.starts_with("[[providers.")
        {
            end = offset;
            break;
        }
        offset += line.len();
    }
    let start = start?;
    Some(start..without_trailing_comments(original, start, end))
}

/// `end`, moved back over the comments and blank lines that close
/// `original[start..end]`.
fn without_trailing_comments(original: &str, start: usize, end: usize) -> usize {
    let mut kept = end;
    for line in original[start..end].split_inclusive('\n').rev() {
        let text = line.trim();
        if !(text.is_empty() || text.starts_with('#')) {
            break;
        }
        kept -= line.len();
    }
    kept.max(start)
}

/// The `[provider_overrides.<id>]` section, written quoted by Forge or bare
/// by hand, up to the next header.
fn override_section_range(original: &str, id: &str) -> Option<std::ops::Range<usize>> {
    let mut start = None;
    let mut offset = 0;
    for line in original.split_inclusive('\n') {
        let header = line.trim();
        if start.is_some() && header.starts_with('[') {
            let start = start?;
            return Some(start..without_trailing_comments(original, start, offset));
        }
        if start.is_none() {
            let key = header
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|inner| !inner.starts_with('['))
                .and_then(|inner| inner.trim().strip_prefix("provider_overrides."))
                .map(|key| key.trim().trim_matches('"'));
            if key.is_some_and(|key| key.eq_ignore_ascii_case(id)) {
                start = Some(offset);
            }
        }
        offset += line.len();
    }
    let start = start?;
    Some(start..without_trailing_comments(original, start, original.len()))
}

/// The lines of the `<id> = [...]` key inside `[manual_models]`, to the line
/// where its brackets balance.
fn manual_models_key_range(original: &str, id: &str) -> Option<std::ops::Range<usize>> {
    let mut in_section = false;
    let mut start = None;
    let mut depth = 0i32;
    let mut offset = 0;
    for line in original.split_inclusive('\n') {
        let text = line.trim();
        if let Some(start) = start {
            depth += bracket_depth(text);
            offset += line.len();
            if depth <= 0 {
                return Some(start..offset);
            }
            continue;
        }
        if text.starts_with('[') {
            in_section = text == "[manual_models]";
        } else if in_section {
            let key = text
                .split_once('=')
                .map(|(key, _)| key.trim().trim_matches('"'));
            if key.is_some_and(|key| key.eq_ignore_ascii_case(id)) {
                start = Some(offset);
                depth = bracket_depth(text.split_once('=').map_or("", |(_, value)| value));
                if depth <= 0 {
                    return Some(offset..offset + line.len());
                }
            }
        }
        offset += line.len();
    }
    start.map(|start| start..original.len())
}

/// Open minus close brackets on a line, ignoring a trailing `#` comment.
fn bracket_depth(text: &str) -> i32 {
    let code = text.split('#').next().unwrap_or("");
    code.matches('[').count() as i32 - code.matches(']').count() as i32
}

/// `table` without `id` (any case); an empty table counts as absent.
fn without_id(table: Option<&toml::Value>, id: &str) -> Option<toml::Table> {
    let mut table = table.and_then(toml::Value::as_table).cloned()?;
    table.retain(|key, _| !key.eq_ignore_ascii_case(id));
    (!table.is_empty()).then_some(table)
}

/// Number of `[[providers]]` entries whose id is `id` (any case).
pub(crate) fn provider_entry_count(document: &toml::Value, id: &str) -> usize {
    document
        .get("providers")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .get("id")
                .and_then(toml::Value::as_str)
                .is_some_and(|entry_id| entry_id.eq_ignore_ascii_case(id))
        })
        .count()
}

/// `original` without everything `providers.toml` holds for `id`: its
/// `[[providers]]` entry when `remove_entry` is set, its override section, and
/// its `[manual_models]` key. The result is re-parsed: it must hold exactly
/// the original data minus those parts, or the edit is refused with
/// [`LAYOUT`]. Nothing to cut returns `original` unchanged.
fn strip_provider_sections(original: &str, id: &str, remove_entry: bool) -> Result<String, String> {
    let document: toml::Value = toml::from_str(original).map_err(|_| "invalid providers TOML")?;
    let entries = document
        .get("providers")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ranges = Vec::new();
    let mut expected = entries.clone();
    if remove_entry {
        let index = entries
            .iter()
            .position(|entry| {
                entry
                    .get("id")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|entry_id| entry_id.eq_ignore_ascii_case(id))
            })
            .ok_or("this provider is not defined in providers.toml")?;
        ranges.push(provider_entry_range(original, index).ok_or(LAYOUT)?);
        expected.remove(index);
    }
    let overrides = document.get("provider_overrides");
    let manual = document.get("manual_models");
    let has_override = overrides
        .and_then(toml::Value::as_table)
        .is_some_and(|table| table.keys().any(|key| key.eq_ignore_ascii_case(id)));
    let has_manual = manual
        .and_then(toml::Value::as_table)
        .is_some_and(|table| table.keys().any(|key| key.eq_ignore_ascii_case(id)));
    if has_override {
        ranges.push(override_section_range(original, id).ok_or(LAYOUT)?);
    }
    if has_manual {
        ranges.push(manual_models_key_range(original, id).ok_or(LAYOUT)?);
    }
    if ranges.is_empty() {
        return Ok(original.to_string());
    }
    ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
    let mut updated = original.to_string();
    let mut floor = usize::MAX;
    for range in ranges {
        if range.end > floor {
            return Err(LAYOUT.into());
        }
        updated.replace_range(range.clone(), "");
        floor = range.start;
    }
    let left: toml::Value = toml::from_str(&updated).map_err(|_| LAYOUT)?;
    let left_entries = left
        .get("providers")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut left_rest = left.as_table().cloned().unwrap_or_default();
    let mut doc_rest = document.as_table().cloned().unwrap_or_default();
    for key in ["providers", "provider_overrides", "manual_models"] {
        left_rest.remove(key);
        doc_rest.remove(key);
    }
    if left_entries != expected
        || left_rest != doc_rest
        || without_id(left.get("provider_overrides"), id) != without_id(overrides, id)
        || without_id(left.get("manual_models"), id) != without_id(manual, id)
    {
        return Err(LAYOUT.into());
    }
    Ok(updated)
}

/// Remove the `id` entry from `providers.toml`, with its override section and
/// its `[manual_models]` key, in one checked write. Every other byte of the
/// file is kept.
pub(crate) fn remove_provider_entry(path: &Path, id: &str) -> Result<(), String> {
    let original = read_provider_text(path)?;
    let updated = strip_provider_sections(&original, id, true)?;
    write_checked(path, &original, &updated)
}

/// Entry fields the add and edit forms set from what the user types.
const FORM_INPUT_FIELDS: &[&str] = &[
    "id",
    "title",
    "label",
    "api_key_env",
    "base_url",
    "wire",
    "manual_models",
    "default_models",
];

/// Fields of a raw `[[providers]]` entry that the picker cannot recreate:
/// every field, other than the ones the forms take as input, whose value
/// differs from what the add form writes for the same id, base URL and wire.
pub(crate) fn unrecoverable_fields(entry: &toml::Table) -> Vec<String> {
    let mut entry = entry.clone();
    let id = entry
        .get("id")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if upgrade_legacy_keys(&mut entry, &id).is_err() {
        return Vec::new();
    }
    let base_url = entry
        .get("base_url")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let wire = entry
        .get("wire")
        .and_then(|wire| wire.clone().try_into().ok())
        .unwrap_or(WireKind::ChatCompletions);
    let form = toml::Value::try_from(form_spec(&id, &base_url, "", wire))
        .ok()
        .and_then(|form| form.as_table().cloned())
        .unwrap_or_default();
    entry
        .iter()
        .filter(|(key, _)| !FORM_INPUT_FIELDS.contains(&key.as_str()))
        .filter(|(key, value)| form.get(key.as_str()) != Some(value))
        .map(|(key, _)| key.clone())
        .collect()
}

/// Replace the `id` entry of the `[[providers]]` array: its lines are
/// removed and a rewritten copy is appended at the end of the file, so
/// comments inside that entry are lost and the provider moves last. Every
/// other byte of the file is kept.
fn replace_provider_entry(path: &Path, id: &str, details: &ProviderDetails) -> Result<(), String> {
    let original = read_provider_text(path)?;
    let document: toml::Value = toml::from_str(&original).map_err(|_| "invalid providers TOML")?;
    let entries = document
        .get("providers")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let index = entries
        .iter()
        .position(|entry| entry.get("id").and_then(toml::Value::as_str) == Some(id))
        .ok_or("this provider is not defined in providers.toml")?;
    let range = provider_entry_range(&original, index).ok_or(LAYOUT)?;
    let remainder = format!("{}{}", &original[..range.start], &original[range.end..]);
    // The cut must have removed exactly that entry and nothing else.
    let mut expected = entries.clone();
    let mut entry = expected.remove(index);
    let left: toml::Value = toml::from_str(&remainder).map_err(|_| LAYOUT)?;
    let left_entries = left
        .get("providers")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut left_rest = left.as_table().cloned().unwrap_or_default();
    left_rest.remove("providers");
    let mut doc_rest = document.as_table().cloned().unwrap_or_default();
    doc_rest.remove("providers");
    if left_entries != expected || left_rest != doc_rest {
        return Err(LAYOUT.into());
    }
    let table = entry.as_table_mut().ok_or(LAYOUT)?;
    upgrade_legacy_keys(table, id)?;
    table.insert("title".into(), details.title.trim().into());
    if details.label.trim().is_empty() {
        table.remove("label");
    } else {
        table.insert("label".into(), details.label.trim().into());
    }
    table.insert(
        "api_key_env".into(),
        toml::Value::try_from(&details.api_key_env).map_err(|_| "cannot serialize provider")?,
    );
    let mut file = toml::Table::new();
    file.insert("providers".into(), toml::Value::Array(vec![entry]));
    let addition =
        toml::to_string_pretty(&file).map_err(|err| format!("cannot serialize provider: {err}"))?;
    let updated = format!("{}\n\n{addition}", remainder.trim_end_matches('\n'));
    let check: toml::Value = toml::from_str(&updated).map_err(|_| LAYOUT)?;
    let rewritten = check
        .get("providers")
        .and_then(toml::Value::as_array)
        .and_then(|entries| entries.last())
        .ok_or(LAYOUT)?;
    check_custom_keys(rewritten)?;
    write_checked(path, &original, &updated)
}

/// Rewrite a raw custom entry in the current schema: renamed fields take
/// their current names and legacy keys are converted or dropped, so a
/// rewritten entry never carries `transport`, `auth_mode`, `route_id`, or
/// `catalog_mode`. An older custom `model_provider_prefix` stays, since saved
/// model ids still normalize from it.
fn upgrade_legacy_keys(table: &mut toml::Table, id: &str) -> Result<(), String> {
    for (old, new) in [
        ("default_base_url", "base_url"),
        ("vendor_id", "provider"),
        ("vendor_label", "provider_label"),
        ("route_label", "label"),
    ] {
        if let Some(value) = table.remove(old) {
            table.entry(new).or_insert(value);
        }
    }
    if let Some(transport) = table.remove("transport") {
        if !table.contains_key("wire") {
            let wire = transport
                .as_str()
                .and_then(crate::normalize::wire_from_legacy_transport)
                .ok_or("this provider's transport cannot be converted")?;
            table.insert(
                "wire".into(),
                toml::Value::try_from(wire).map_err(|_| "cannot serialize provider")?,
            );
        }
    }
    // Custom providers only use key auth (`check_custom_keys`).
    if table.remove("auth_mode").is_some() && !table.contains_key("auth") {
        let mut auth = toml::Table::new();
        auth.insert("kind".into(), "api-key".into());
        table.insert("auth".into(), toml::Value::Table(auth));
    }
    table.remove("route_id");
    table.remove("catalog_mode");
    if table
        .get("model_provider_prefix")
        .and_then(toml::Value::as_str)
        == Some(id)
    {
        table.remove("model_provider_prefix");
    }
    Ok(())
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
    write_checked(path, original, &updated)
}

/// Swap `updated` in for `original`, refusing if the file changed meanwhile.
fn write_checked(path: &Path, original: &str, updated: &str) -> Result<(), String> {
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
            p.id.eq_ignore_ascii_case(prefix)
                || p.model_provider_prefix.eq_ignore_ascii_case(prefix)
                || crate::normalize::legacy_prefixes(&p.id)
                    .iter()
                    .any(|old| old.eq_ignore_ascii_case(prefix))
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

/// The entry the add form writes for these inputs.
fn form_spec(id: &str, base_url: &str, model: &str, wire: WireKind) -> ProviderSpec {
    ProviderSpec {
        id: id.into(),
        title: id.into(),
        description: match wire {
            WireKind::AnthropicMessages => "Custom Anthropic-compatible provider",
            _ => "Custom OpenAI-compatible provider",
        }
        .into(),
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
        origin: SpecOrigin::User,
        ..ProviderSpec::blank()
    }
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
    let spec = form_spec(id, base_url, model, wire);
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
        if value.get("id").and_then(toml::Value::as_str) == Some(id) {
            return Err(format!("id `{id}` is already registered"));
        }
    }
    let registry = loaded_registry_from(path);
    validate_user_spec(&registry, &spec)?;
    // Sections a hand deletion left for this id would otherwise apply to the
    // new entry: an old base URL override and old manual models.
    let stripped = strip_provider_sections(original.as_deref().unwrap_or(""), id, false)?;
    let entry =
        toml::Value::try_from(&spec).map_err(|err| format!("cannot serialize provider: {err}"))?;
    let mut file = toml::Table::new();
    file.insert("providers".into(), toml::Value::Array(vec![entry]));
    let addition = format!(
        "\n{}",
        toml::to_string_pretty(&file).map_err(|err| format!("cannot serialize provider: {err}"))?
    );
    // Some valid TOML shapes (e.g. `providers = []`) cannot accept an array-table append.
    toml::from_str::<toml::Value>(&format!("{}{addition}", original.as_deref().unwrap_or("")))
        .map_err(|_| "cannot safely append to this TOML layout")?;
    if let Some(original) = original.as_deref().filter(|original| *original != stripped) {
        write_checked(path, original, &format!("{stripped}{addition}"))?;
        return Ok(spec);
    }
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
    match table
        .get("transport")
        .and_then(toml::Value::as_str)
        .map(crate::normalize::wire_from_legacy_transport)
    {
        None | Some(Some(WireKind::ChatCompletions | WireKind::AnthropicMessages)) => {}
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
            .and_then(crate::normalize::wire_from_legacy_transport);
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
    // The id is the model prefix from now on; an older custom prefix stays
    // only as an alias that saved ids normalize from.
    if !spec.model_provider_prefix.is_empty() && spec.model_provider_prefix != spec.id {
        spec.legacy_prefix = Some(std::mem::take(&mut spec.model_provider_prefix));
    }
    spec.model_provider_prefix = spec.id.clone();
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
        if is_reserved_key_env_name(&builtin_names, name) {
            return Err(format!(
                "api_key_env `{name}` is reserved by a built-in provider"
            ));
        }
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
    fn re_adding_an_id_drops_sections_a_hand_deletion_left() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "# mine\n[provider_overrides.\"labbox\"]\nbase_url = \"https://old.example/v1\"\n\n[manual_models]\nlabbox = [\"labbox/old\"]\n",
        )
        .unwrap();
        save_custom_provider_to(
            &path,
            "labbox",
            "https://new.example/v1",
            "fresh",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        let spec = registry.get("labbox").unwrap();
        assert_eq!(spec.base_url, "https://new.example/v1");
        assert_eq!(spec.manual_models, vec!["labbox/fresh"]);
        assert!(registry.load_warnings().is_empty());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("# mine\n"));
    }

    #[test]
    fn removing_an_edited_provider_drops_the_quoted_override_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(
            &path,
            "custom",
            "https://old.example/v1",
            "first",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        update_custom_provider_to(&path, "custom", "https://new.example/v1", "second", None)
            .unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[provider_overrides.\"custom\"]"));
        remove_provider_entry(&path, "custom").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("custom"), "{text}");
        assert!(loaded_registry_from(&path).load_warnings().is_empty());
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
        update_custom_provider_to(&path, "custom", "https://new.example/v1", "second", None)
            .unwrap();
        let spec = update_custom_provider_to(&path, "custom", "https://final.example/v1", "", None)
            .unwrap();
        assert_eq!(spec.base_url, "https://final.example/v1");
        assert!(spec.default_models.is_empty());
        assert_eq!(
            spec.manual_models,
            vec!["custom/org/manual", "custom/second"]
        );
        assert!(loaded_registry_from(&path).load_warnings().is_empty());
    }

    #[test]
    fn editing_details_replaces_only_that_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "# my providers\n\n[[providers]]\n# inside first\nid = \"first\"\ntitle = \"first\"\nbase_url = \"https://one.example/v1\"\nroute_label = \"old\"\n\n[providers.auth]\nkind = \"api-key\"\n\n# before second\n[[providers]]\nid = \"second\"\ntitle = \"second\"\nbase_url = \"https://two.example/v1\"\n\n[manual_models]\nfirst = [\"first/m\"]\n",
        )
        .unwrap();
        let details = ProviderDetails {
            title: " First Co ".into(),
            label: "Team".into(),
            api_key_env: vec!["FIRST_API_KEY".into()],
        };
        let spec =
            update_custom_provider_to(&path, "first", "https://one.example/v1", "", Some(&details))
                .unwrap();
        assert_eq!(spec.title, "First Co");
        assert_eq!(spec.label, "Team");
        assert_eq!(spec.api_key_env, vec!["FIRST_API_KEY"]);
        assert_eq!(spec.manual_models, vec!["first/m"]);
        let text = std::fs::read_to_string(&path).unwrap();
        // Comments outside the entry stay; the entry moves to the end.
        assert!(text.starts_with("# my providers\n"), "{text}");
        assert!(text.contains("# before second"), "{text}");
        assert!(!text.contains("# inside first"), "{text}");
        assert!(!text.contains("route_label"), "{text}");
        let registry = loaded_registry_from(&path);
        assert!(
            registry.load_warnings().is_empty(),
            "{:?}",
            registry.load_warnings()
        );
        assert_eq!(
            registry.get("second").unwrap().base_url,
            "https://two.example/v1"
        );
        assert_eq!(
            registry.get("first").unwrap().effective_auth().kind,
            crate::AuthKind::ApiKey
        );
    }

    #[test]
    fn editing_details_writes_a_legacy_entry_in_the_current_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            "[[providers]]\napi_key_env = []\ncatalog_mode = \"live\"\ndefault_base_url = \"https://api.legacy.example/v1\"\nid = \"legacy\"\nmodel_provider_prefix = \"legacy\"\nroute_id = \"legacy\"\ntitle = \"Legacy\"\ntransport = \"openai-compat\"\n\n[providers.auth_mode.api_key]\ntui_always_prompt = true\n",
        )
        .unwrap();
        let details = ProviderDetails {
            title: "Legacy".into(),
            label: "Team".into(),
            api_key_env: vec!["LEGACY_API_KEY".into()],
        };
        let spec = update_custom_provider_to(
            &path,
            "legacy",
            "https://api.legacy.example/v1",
            "",
            Some(&details),
        )
        .unwrap();
        assert_eq!(spec.effective_wire(), crate::WireKind::ChatCompletions);
        assert_eq!(spec.effective_auth().kind, crate::AuthKind::ApiKey);
        let text = std::fs::read_to_string(&path).unwrap();
        for legacy in [
            "transport",
            "auth_mode",
            "route_id",
            "catalog_mode",
            "default_base_url",
            "model_provider_prefix",
        ] {
            assert!(!text.contains(legacy), "legacy `{legacy}` in:\n{text}");
        }
        assert!(text.contains("wire = \"chat-completions\""), "{text}");
        assert!(
            loaded_registry_from(&path).load_warnings().is_empty(),
            "{text}"
        );
    }

    #[test]
    fn editing_details_rejects_bad_input_before_writing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        save_custom_provider_to(
            &path,
            "custom",
            "https://old.example/v1",
            "",
            crate::WireKind::ChatCompletions,
        )
        .unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        for details in [
            ProviderDetails {
                title: " ".into(),
                ..ProviderDetails::default()
            },
            ProviderDetails {
                title: "t".into(),
                api_key_env: vec!["sk-live-secret-value".into()],
                ..ProviderDetails::default()
            },
            ProviderDetails {
                title: "t".into(),
                api_key_env: vec!["OPENAI_API_KEY".into()],
                ..ProviderDetails::default()
            },
            ProviderDetails {
                title: "t".into(),
                api_key_env: vec!["FORGE_CUSTOM_API_KEY".into()],
                ..ProviderDetails::default()
            },
        ] {
            let err = update_custom_provider_to(
                &path,
                "custom",
                "https://new.example/v1",
                "",
                Some(&details),
            )
            .unwrap_err();
            assert!(!err.contains("sk-live"), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        let details = ProviderDetails {
            title: "Ollama".into(),
            ..ProviderDetails::default()
        };
        assert!(update_custom_provider_to(
            &path,
            "ollama",
            "http://10.0.0.5:11434",
            "",
            Some(&details)
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
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
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &original).unwrap();
        assert!(save_custom_provider_to(
            &path,
            "existing",
            "https://example.com",
            "m",
            crate::WireKind::ChatCompletions
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
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
            update_custom_provider_to(&path, "ollama", "http://10.0.0.5:11434", "", None).unwrap();
        assert_eq!(ollama.base_url, "http://10.0.0.5:11434");
        assert!(
            update_custom_provider_to(&path, "openai", "https://evil.example/v1", "", None)
                .is_err()
        );
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
        assert!(text.contains("wire = \"anthropic-messages\""), "{text}");
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
            crate::builtin::builtin_profile("openai").base_url
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
            "[manual_models]\nopencode_go = [\"opencode/kimi\", \"opencode_go/kimi\", \"glm\"]\nxai = [\"grok/grok-4\"]\n",
        )
        .unwrap();
        let registry = loaded_registry_from(&path);
        assert_eq!(
            registry.get("opencode_go").unwrap().manual_models,
            vec!["opencode_go/kimi", "opencode_go/glm"]
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
        assert_eq!(spec.effective_wire(), crate::WireKind::ChatCompletions);
        assert_eq!(spec.origin, SpecOrigin::User);
        assert!(registry.get("openai").is_some());
    }

    #[test]
    fn a_custom_entry_cannot_take_over_a_builtin_model_prefix() {
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
        // The id is the model prefix now: the old prefix is only an alias for
        // this entry's own saved ids, so `openai/…` still routes to OpenAI.
        let registry = loaded_registry_from(&path);
        let sneaky = registry.get("sneaky").unwrap();
        assert_eq!(sneaky.model_provider_prefix, "sneaky");
        assert_eq!(
            registry.profile_for_model("openai/gpt-5").unwrap().id,
            "openai"
        );
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
