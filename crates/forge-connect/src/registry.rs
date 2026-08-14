//! Provider spec registry: builtins plus user-global TOML.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::auth::AuthMode;
use crate::profile::{CatalogMode, ProviderSpec, ProviderTransport, SpecOrigin};

#[derive(Debug, Default, Clone)]
pub struct ConnectRegistry {
    profiles: Vec<ProviderSpec>,
}

impl ConnectRegistry {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
        }
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

/// Builtins plus user-global `providers.toml`. Invalid user entries are skipped.
pub fn loaded_registry() -> ConnectRegistry {
    loaded_registry_from(user_providers_path())
}

pub fn user_providers_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("forge")
        .join("providers.toml")
}

pub fn loaded_registry_from(path: impl AsRef<Path>) -> ConnectRegistry {
    let mut registry = builtin_registry();
    let Ok(text) = std::fs::read_to_string(path) else {
        return registry;
    };
    let Ok(specs) = load_user_specs(&text) else {
        return registry;
    };
    for spec in specs {
        if validate_user_spec(&registry, &spec).is_ok() {
            registry.register(spec);
        }
    }
    registry
}

#[derive(Debug, Deserialize)]
struct UserProvidersFile {
    #[serde(default)]
    providers: Vec<ProviderSpec>,
}

fn load_user_specs(text: &str) -> Result<Vec<ProviderSpec>, String> {
    let file: UserProvidersFile =
        toml::from_str(text).map_err(|err| format!("invalid toml ({err})"))?;
    Ok(file
        .providers
        .into_iter()
        .map(|mut spec| {
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
        })
        .collect())
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
    fn empty_registry() {
        let r = ConnectRegistry::new();
        assert!(r.profiles().is_empty());
        assert!(r.get("xai").is_none());
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
