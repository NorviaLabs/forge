//! Credential store — keys + OAuth tokens (connect-command.md §3.4, Phase 6.1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::OauthTokens;
use crate::profile::KeySource;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(String),
    #[error("toml: {0}")]
    Toml(String),
    #[error("credentials file permissions too open (expected 0600)")]
    InsecurePermissions,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CredentialsFile {
    /// profile_id → api key (ApiKey profiles)
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// profile_id → oauth tokens
    #[serde(default)]
    oauth: BTreeMap<String, OauthTokens>,
}

/// File-backed credential store. Secrets are never returned in status Display APIs.
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn user_default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("forge")
            .join("credentials.toml");
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_api_key(&self, profile_id: &str) -> Result<Option<String>, StoreError> {
        let file = self.load()?;
        Ok(file.keys.get(profile_id).cloned())
    }

    pub fn set_api_key(&self, profile_id: &str, key: &str) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.keys
            .insert(profile_id.to_string(), key.trim().to_string());
        // Connecting via API key clears oauth for that profile
        file.oauth.remove(profile_id);
        self.save(&file)
    }

    pub fn get_oauth(&self, profile_id: &str) -> Result<Option<OauthTokens>, StoreError> {
        let file = self.load()?;
        Ok(file.oauth.get(profile_id).cloned())
    }

    pub fn set_oauth(&self, profile_id: &str, tokens: OauthTokens) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.oauth.insert(profile_id.to_string(), tokens);
        // OAuth supersedes stored API key for this profile
        file.keys.remove(profile_id);
        self.save(&file)
    }

    pub fn clear(&self, profile_id: &str) -> Result<bool, StoreError> {
        let mut file = self.load()?;
        let removed_key = file.keys.remove(profile_id).is_some();
        let removed_oauth = file.oauth.remove(profile_id).is_some();
        let removed = removed_key || removed_oauth;
        if removed {
            self.save(&file)?;
        }
        Ok(removed)
    }

    pub fn is_connected(&self, profile_id: &str) -> Result<bool, StoreError> {
        let file = self.load()?;
        Ok(file.keys.contains_key(profile_id) || file.oauth.contains_key(profile_id))
    }

    pub fn list_profile_ids(&self) -> Result<Vec<String>, StoreError> {
        let file = self.load()?;
        let mut ids: Vec<String> = file.keys.keys().cloned().collect();
        for k in file.oauth.keys() {
            if !ids.iter().any(|i| i == k) {
                ids.push(k.clone());
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn load(&self) -> Result<CredentialsFile, StoreError> {
        if !self.path.exists() {
            return Ok(CredentialsFile::default());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&self.path) {
                let mode = meta.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(StoreError::InsecurePermissions);
                }
            }
        }
        let text = fs::read_to_string(&self.path).map_err(|e| StoreError::Io(e.to_string()))?;
        toml::from_str(&text).map_err(|e| StoreError::Toml(e.to_string()))
    }

    fn save(&self, file: &CredentialsFile) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        let text = toml::to_string_pretty(file).map_err(|e| StoreError::Toml(e.to_string()))?;
        fs::write(&self.path, text).map_err(|e| StoreError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.path, perms).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

/// Resolve API key: env then file (ApiKey profiles).
pub fn resolve_key(
    profile_env_names: &[String],
    profile_id: &str,
    store: &CredentialStore,
) -> Result<Option<(String, KeySource)>, StoreError> {
    for name in profile_env_names {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Ok(Some((v, KeySource::Env)));
            }
        }
    }
    if let Some(k) = store.get_api_key(profile_id)? {
        if !k.trim().is_empty() {
            return Ok(Some((k, KeySource::File)));
        }
    }
    Ok(None)
}

/// Whether profile has usable credentials (API key path or OAuth tokens).
pub fn resolve_connected(
    profile_env_names: &[String],
    profile_id: &str,
    store: &CredentialStore,
) -> Result<Option<KeySource>, StoreError> {
    if store.get_oauth(profile_id)?.is_some() {
        return Ok(Some(KeySource::Oauth));
    }
    Ok(resolve_key(profile_env_names, profile_id, store)?.map(|(_, s)| s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn set_get_clear_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials.toml"));
        assert!(store.get_api_key("xai").unwrap().is_none());
        store.set_api_key("xai", "secret-key").unwrap();
        assert_eq!(
            store.get_api_key("xai").unwrap().as_deref(),
            Some("secret-key")
        );
        assert!(store.clear("xai").unwrap());
        assert!(store.get_api_key("xai").unwrap().is_none());
    }

    #[test]
    fn oauth_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: Some("rt".into()),
                    expires_at: None,
                },
            )
            .unwrap();
        let t = store.get_oauth("xai").unwrap().unwrap();
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
        assert!(store.is_connected("xai").unwrap());
        assert_eq!(
            resolve_connected(&[], "xai", &store).unwrap(),
            Some(KeySource::Oauth)
        );
    }

    #[test]
    fn file_mode_is_private_on_unix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        store.set_api_key("xai", "k").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn resolve_prefers_env() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "from-file").unwrap();
        std::env::set_var("FORGE_TEST_XAI_KEY", "from-env");
        let r = resolve_key(&["FORGE_TEST_XAI_KEY".into()], "xai", &store)
            .unwrap()
            .unwrap();
        assert_eq!(r.0, "from-env");
        assert_eq!(r.1, KeySource::Env);
        std::env::remove_var("FORGE_TEST_XAI_KEY");
    }
}
