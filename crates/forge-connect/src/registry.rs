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
            vendor_id: "demo".into(),
            vendor_label: "Demo".into(),
            route_label: String::new(),
        });
        assert_eq!(r.get("DEMO").unwrap().title, "Demo");
        assert_eq!(r.ids(), vec!["demo"]);
    }

    fn profile(id: &str, title: &str) -> ConnectProfile {
        ConnectProfile {
            id: id.into(),
            title: title.into(),
            description: "d".into(),
            auth_mode: crate::auth::AuthMode::ApiKey {
                tui_always_prompt: false,
            },
            api_key_env: vec![],
            default_base_url: None,
            default_models: vec![],
            models_dev_providers: vec![],
            auth_url: None,
            model_provider_prefix: id.into(),
            vendor_id: id.into(),
            vendor_label: title.into(),
            route_label: String::new(),
        }
    }

    #[test]
    fn registering_the_same_id_replaces_rather_than_duplicates() {
        let mut r = ConnectRegistry::new();
        r.register(profile("demo", "First"));
        r.register(profile("other", "Other"));
        r.register(profile("demo", "Second"));

        // The re-registration overwrites in place, so the id is not duplicated
        // and the original registration order is preserved.
        assert_eq!(r.ids(), vec!["demo", "other"]);
        assert_eq!(r.get("demo").unwrap().title, "Second");
        assert_eq!(r.profiles().len(), 2);
    }
}
