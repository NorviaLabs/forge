//! Credential store — keys + OAuth tokens (connect-command.md §3.4, Phase 6.1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::OauthTokens;
use crate::profile::KeySource;

/// A credential-store operation failed.
///
/// `Io` and the TOML variants carry their source rather than a flattened string,
/// so a caller can distinguish a missing file from a permissions problem. The
/// `PartialEq` derive is gone because `std::io::Error` does not implement it;
/// nothing compared these values.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("credentials file permissions too open (expected 0600)")]
    InsecurePermissions,
    /// The file declares a schema newer than this build understands. Refusing is
    /// deliberate: silently mis-reading a newer token layout would look like a
    /// missing credential and prompt the user to re-authenticate needlessly.
    #[error(
        "credentials file declares schema version {found}, but this build understands up to {supported}; upgrade forge to read it"
    )]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
}

/// Highest `credentials.toml` schema version this build can read.
///
/// A file with no `version` key predates versioning and is read as version 1 —
/// the shape those files already have — so existing credentials keep loading.
pub const CREDENTIALS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CredentialsFile {
    /// On-disk schema version. Declared first because TOML requires scalar keys
    /// to precede tables, and `keys`/`oauth` below serialise as tables.
    /// Absent in files written before versioning; `save` always writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    /// profile_id → api key (ApiKey profiles)
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// profile_id → oauth tokens
    #[serde(default)]
    oauth: BTreeMap<String, OauthTokens>,
    /// Last non-secret provider/model/effort selection used by the interactive client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_effort: Option<String>,
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

    pub fn clear_all(&self) -> Result<bool, StoreError> {
        let mut file = self.load()?;
        let removed = !file.keys.is_empty()
            || !file.oauth.is_empty()
            || file.last_profile_id.is_some()
            || file.last_model.is_some()
            || file.last_effort.is_some();
        if removed {
            file.keys.clear();
            file.oauth.clear();
            file.last_profile_id = None;
            file.last_model = None;
            file.last_effort = None;
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

    /// Return the last provider/model selection, if one was recorded.
    pub fn last_selection(&self) -> Result<Option<(String, String)>, StoreError> {
        let file = self.load()?;
        Ok(match (file.last_profile_id, file.last_model) {
            (Some(profile_id), Some(model))
                if !profile_id.trim().is_empty() && !model.trim().is_empty() =>
            {
                Some((profile_id, model))
            }
            _ => None,
        })
    }

    /// Persist the last provider/model selection. This contains no credentials.
    pub fn set_last_selection(&self, profile_id: &str, model: &str) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.last_profile_id = Some(profile_id.trim().to_string());
        file.last_model = Some(model.trim().to_string());
        self.save(&file)
    }

    /// Return the last reasoning effort selected by the interactive client.
    pub fn last_effort(&self) -> Result<Option<String>, StoreError> {
        let file = self.load()?;
        Ok(file.last_effort.filter(|effort| !effort.trim().is_empty()))
    }

    /// Persist the last reasoning effort. This contains no credentials.
    pub fn set_last_effort(&self, effort: &str) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.last_effort = Some(effort.trim().to_string());
        self.save(&file)
    }

    pub fn clear_last_selection(&self, profile_id: Option<&str>) -> Result<(), StoreError> {
        let mut file = self.load()?;
        if profile_id.is_none() || file.last_profile_id.as_deref() == profile_id {
            file.last_profile_id = None;
            file.last_model = None;
            file.last_effort = None;
            self.save(&file)?;
        }
        Ok(())
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
        let text = fs::read_to_string(&self.path)?;
        let file: CredentialsFile = toml::from_str(&text)?;
        // A file with no `version` predates versioning and is read as v1.
        if let Some(found) = file.version {
            if found > CREDENTIALS_SCHEMA_VERSION {
                return Err(StoreError::UnsupportedSchemaVersion {
                    found,
                    supported: CREDENTIALS_SCHEMA_VERSION,
                });
            }
        }
        Ok(file)
    }

    fn save(&self, file: &CredentialsFile) -> Result<(), StoreError> {
        // Stamp the current version on every write so a future build can tell
        // what layout it is reading instead of guessing.
        let mut file = file.clone();
        file.version = Some(CREDENTIALS_SCHEMA_VERSION);
        let file = &file;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(file)?;
        fs::write(&self.path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.path, perms)?;
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
    fn last_selection_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert_eq!(store.last_selection().unwrap(), None);
        store
            .set_last_selection("anthropic", "anthropic/claude-sonnet-4-5")
            .unwrap();
        store.set_last_effort("high").unwrap();
        assert_eq!(
            store.last_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet-4-5".into()))
        );
        assert_eq!(store.last_effort().unwrap().as_deref(), Some("high"));
        store.clear_last_selection(Some("openai")).unwrap();
        assert!(store.last_selection().unwrap().is_some());
        store.clear_last_selection(Some("anthropic")).unwrap();
        assert_eq!(store.last_selection().unwrap(), None);
        assert_eq!(store.last_effort().unwrap(), None);
    }

    #[test]
    fn clear_all_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();
        store
            .set_oauth(
                "openai_codex",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        store
            .set_last_selection("openai", "openai/gpt-4.1-mini")
            .unwrap();
        store.set_last_effort("medium").unwrap();
        assert!(store.clear_all().unwrap());
        assert!(store.get_api_key("xai").unwrap().is_none());
        assert!(store.get_oauth("openai_codex").unwrap().is_none());
        assert_eq!(store.last_selection().unwrap(), None);
        assert_eq!(store.last_effort().unwrap(), None);
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

    /// The regression that matters most here: an existing `credentials.toml` has
    /// no `version` key, and it must keep loading. A failure locks users out of
    /// stored tokens and looks like a missing credential.
    #[test]
    fn credentials_without_a_version_key_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        std::fs::write(&path, "[keys]\nopenai = \"sk-existing\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = CredentialStore::new(path);
        assert_eq!(
            store.get_api_key("openai").unwrap(),
            Some("sk-existing".to_string()),
            "an unversioned credentials file must keep loading"
        );
    }

    /// A newer file is refused rather than mis-read. Silently failing to find a
    /// token would prompt a needless re-authentication.
    #[test]
    fn credentials_from_a_newer_build_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let future = CREDENTIALS_SCHEMA_VERSION + 1;
        std::fs::write(
            &path,
            format!("version = {future}\n[keys]\nopenai = \"x\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = CredentialStore::new(path);
        let err = store.get_api_key("openai").unwrap_err();
        // Matched on the variant rather than compared for equality, so this test
        // does not depend on StoreError deriving PartialEq.
        assert!(
            matches!(
                err,
                StoreError::UnsupportedSchemaVersion { found: f, supported }
                    if f == future && supported == CREDENTIALS_SCHEMA_VERSION
            ),
            "expected an unsupported-version error, got {err:?}"
        );
    }

    /// Writing stamps the version, and it round-trips alongside a populated
    /// `[keys]` table — which also pins that the scalar is emitted before the
    /// table, as TOML requires.
    #[test]
    fn saving_stamps_the_version_and_round_trips_with_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        store.set_api_key("openai", "sk-written").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("version = {CREDENTIALS_SCHEMA_VERSION}")),
            "save must stamp the schema version, got:\n{text}"
        );
        assert!(
            text.find("version").unwrap() < text.find("[keys]").unwrap(),
            "the version scalar must precede the [keys] table, got:\n{text}"
        );
        assert_eq!(
            store.get_api_key("openai").unwrap(),
            Some("sk-written".to_string())
        );
    }
}
