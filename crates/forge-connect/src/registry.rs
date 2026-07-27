//! Connect profile registry (CONN-01).

use crate::profile::ConnectProfile;

#[derive(Debug, Default, Clone)]
pub struct ConnectRegistry {
    profiles: Vec<ConnectProfile>,
}

impl ConnectRegistry {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
        }
    }

    pub fn register(&mut self, profile: ConnectProfile) {
        if let Some(pos) = self.profiles.iter().position(|p| p.id == profile.id) {
            self.profiles[pos] = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn profiles(&self) -> &[ConnectProfile] {
        &self.profiles
    }

    pub fn get(&self, id: &str) -> Option<&ConnectProfile> {
        let id = id.trim();
        self.profiles.iter().find(|p| p.id.eq_ignore_ascii_case(id))
    }

    pub fn ids(&self) -> Vec<&str> {
        self.profiles.iter().map(|p| p.id.as_str()).collect()
    }
}

/// Built-in Phase 6 registry. Profiles added in provider commits.
pub fn builtin_registry() -> ConnectRegistry {
    let mut r = ConnectRegistry::new();
    // Profiles registered by crate root once modules exist.
    crate::register_builtin_profiles(&mut r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry() {
        let r = ConnectRegistry::new();
        assert!(r.profiles().is_empty());
        assert!(r.get("xai").is_none());
    }

    #[test]
    fn register_and_get() {
        let mut r = ConnectRegistry::new();
        r.register(ConnectProfile {
            id: "demo".into(),
            title: "Demo".into(),
            description: "d".into(),
            auth_mode: crate::auth::AuthMode::ApiKey {
                tui_always_prompt: false,
            },
            api_key_env: vec!["DEMO_KEY".into()],
            default_base_url: None,
            default_models: vec!["demo/m".into()],
            models_dev_providers: vec![],
            auth_url: None,
            model_provider_prefix: "demo".into(),
        });
        assert_eq!(r.get("DEMO").unwrap().title, "Demo");
        assert_eq!(r.ids(), vec!["demo"]);
    }
}
