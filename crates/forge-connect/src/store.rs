//! Credential store — keys + OAuth tokens (connect-command.md §3.4, Phase 6.1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::OauthTokens;
use crate::profile::KeySource;
use crate::selection::ModelSelection;

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
    /// The selection active immediately before `last_*`, for Quick Switch —
    /// toggling between the two most recently, deliberately chosen combos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_effort: Option<String>,
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
            || file.last_effort.is_some()
            || file.previous_profile_id.is_some()
            || file.previous_model.is_some()
            || file.previous_effort.is_some();
        if removed {
            file.keys.clear();
            file.oauth.clear();
            file.last_profile_id = None;
            file.last_model = None;
            file.last_effort = None;
            file.previous_profile_id = None;
            file.previous_model = None;
            file.previous_effort = None;
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

    /// Compose the last provider/model/effort selection into one
    /// `ModelSelection`, when a complete selection was recorded. Doesn't
    /// change the on-disk format — just a single-call convenience over
    /// `last_selection`/`last_effort` for callers that want the structured
    /// value instead of reading the two independently.
    pub fn last_selection_struct(&self) -> Result<Option<ModelSelection>, StoreError> {
        let Some((profile_id, model)) = self.last_selection()? else {
            return Ok(None);
        };
        let effort = self.last_effort()?.unwrap_or_default();
        Ok(Some(ModelSelection {
            provider: "native".into(),
            model,
            profile_id: Some(profile_id),
            effort,
        }))
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

    /// Return the selection active immediately before the current `last_*`
    /// (Quick Switch's toggle target), if one was recorded.
    pub fn previous_selection(&self) -> Result<Option<(String, String)>, StoreError> {
        let file = self.load()?;
        Ok(match (file.previous_profile_id, file.previous_model) {
            (Some(profile_id), Some(model))
                if !profile_id.trim().is_empty() && !model.trim().is_empty() =>
            {
                Some((profile_id, model))
            }
            _ => None,
        })
    }

    /// Return the effort paired with `previous_selection`, if any.
    pub fn previous_effort(&self) -> Result<Option<String>, StoreError> {
        let file = self.load()?;
        Ok(file
            .previous_effort
            .filter(|effort| !effort.trim().is_empty()))
    }

    /// Record a deliberate provider/model/effort switch, for Quick Switch.
    ///
    /// If `new` differs from the current `last_*`, the current `last_*` is
    /// rotated into `previous_*` before `new` becomes the new `last_*`. A
    /// no-op when `new` already matches `last_*` (e.g. reselecting the
    /// active model), so an accidental reselect can't clobber real history.
    /// Callers must only invoke this for user-driven selections, never for
    /// automatic fallbacks — see `set_last_selection`/`set_last_effort` for
    /// the non-rotating equivalent used elsewhere (e.g. on shell exit).
    pub fn record_switch(&self, new: (&str, &str, &str)) -> Result<(), StoreError> {
        let mut file = self.load()?;
        let new_profile_id = new.0.trim().to_string();
        let new_model = new.1.trim().to_string();
        let new_effort = new.2.trim().to_string();
        let unchanged = file.last_profile_id.as_deref() == Some(new_profile_id.as_str())
            && file.last_model.as_deref() == Some(new_model.as_str())
            && file.last_effort.as_deref() == Some(new_effort.as_str());
        if unchanged {
            return Ok(());
        }
        let had_complete_last = file
            .last_profile_id
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
            && file
                .last_model
                .as_deref()
                .is_some_and(|m| !m.trim().is_empty());
        if had_complete_last {
            file.previous_profile_id = file.last_profile_id.take();
            file.previous_model = file.last_model.take();
            file.previous_effort = file.last_effort.take();
        }
        file.last_profile_id = Some(new_profile_id);
        file.last_model = Some(new_model);
        file.last_effort = Some(new_effort);
        self.save(&file)
    }

    /// Apply Quick Switch: swap `last_*` and `previous_*` in place, so a
    /// second call toggles back. Returns the combo to apply now (the new
    /// `last_*`), or `None` if there is nothing to switch to.
    pub fn quick_switch(&self) -> Result<Option<(String, String, String)>, StoreError> {
        let mut file = self.load()?;
        let (Some(profile_id), Some(model)) = (
            file.previous_profile_id.clone(),
            file.previous_model.clone(),
        ) else {
            return Ok(None);
        };
        if profile_id.trim().is_empty() || model.trim().is_empty() {
            return Ok(None);
        }
        let effort = file.previous_effort.clone().unwrap_or_default();
        let old_last = (
            file.last_profile_id.take(),
            file.last_model.take(),
            file.last_effort.take(),
        );
        file.last_profile_id = Some(profile_id.clone());
        file.last_model = Some(model.clone());
        file.last_effort = Some(effort.clone());
        file.previous_profile_id = old_last.0;
        file.previous_model = old_last.1;
        file.previous_effort = old_last.2;
        self.save(&file)?;
        Ok(Some((profile_id, model, effort)))
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
    let file = store.load()?;
    if file.oauth.contains_key(profile_id) {
        return Ok(Some(KeySource::Oauth));
    }
    for name in profile_env_names {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Ok(Some(KeySource::Env));
            }
        }
    }
    Ok(file
        .keys
        .get(profile_id)
        .filter(|key| !key.trim().is_empty())
        .map(|_| KeySource::File))
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
    fn last_selection_struct_composes_profile_model_and_effort() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert_eq!(store.last_selection_struct().unwrap(), None);

        store
            .set_last_selection("openai_codex", "openai-codex/gpt-5.6-luna")
            .unwrap();
        store.set_last_effort("high").unwrap();

        assert_eq!(
            store.last_selection_struct().unwrap(),
            Some(ModelSelection {
                provider: "native".into(),
                model: "openai-codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
                effort: "high".into(),
            })
        );
    }

    #[test]
    fn record_switch_rotates_last_into_previous_when_the_combo_changes() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert_eq!(store.previous_selection().unwrap(), None);

        store
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        assert_eq!(
            store.last_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        // First switch ever: nothing to rotate into previous yet.
        assert_eq!(store.previous_selection().unwrap(), None);

        store
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        assert_eq!(
            store.last_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet".into()))
        );
        assert_eq!(store.last_effort().unwrap().as_deref(), Some("high"));
        assert_eq!(
            store.previous_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        assert_eq!(store.previous_effort().unwrap().as_deref(), Some("medium"));
    }

    #[test]
    fn record_switch_reselecting_the_active_combo_does_not_rotate() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        store
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        // Reselecting the same model+effort that's already active must not
        // clobber the real previous combo with a duplicate of itself.
        store
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        assert_eq!(
            store.previous_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
    }

    #[test]
    fn quick_switch_swaps_last_and_previous_and_toggles_back() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));

        // Nothing to switch to yet.
        assert_eq!(store.quick_switch().unwrap(), None);

        store
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        store
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();

        let switched = store.quick_switch().unwrap();
        assert_eq!(
            switched,
            Some(("openai".into(), "openai/gpt-5.6".into(), "medium".into()))
        );
        assert_eq!(
            store.last_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        assert_eq!(
            store.previous_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet".into()))
        );

        // A second Quick Switch toggles back to where we started.
        let switched_back = store.quick_switch().unwrap();
        assert_eq!(
            switched_back,
            Some((
                "anthropic".into(),
                "anthropic/claude-sonnet".into(),
                "high".into()
            ))
        );
    }

    #[test]
    fn clear_all_removes_previous_selection_too() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        store
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        assert!(store.clear_all().unwrap());
        assert_eq!(store.previous_selection().unwrap(), None);
        assert_eq!(store.quick_switch().unwrap(), None);
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
        let guard = crate::test_env::EnvGuard::new(&["FORGE_TEST_XAI_KEY"]);
        guard.set("FORGE_TEST_XAI_KEY", "from-env");
        let r = resolve_key(&["FORGE_TEST_XAI_KEY".into()], "xai", &store)
            .unwrap()
            .unwrap();
        assert_eq!(r.0, "from-env");
        assert_eq!(r.1, KeySource::Env);
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

    #[test]
    fn path_reports_the_backing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        assert_eq!(store.path(), path.as_path());
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        // Nothing has created `a/b` yet; writing a credential must not fail.
        let store = CredentialStore::new(dir.path().join("a").join("b").join("c.toml"));
        store.set_api_key("xai", "k").unwrap();
        assert!(store.path().exists());
        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("k"));
    }

    #[test]
    fn clear_reports_false_when_there_was_nothing_to_remove() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert!(!store.clear("absent").unwrap());
    }

    #[test]
    fn clear_all_removes_every_kind_of_stored_state() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));

        // Nothing stored yet, so there is nothing to clear.
        assert!(!store.clear_all().unwrap());

        store.set_api_key("xai", "k").unwrap();
        store
            .set_oauth(
                "openai",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        store.set_last_selection("xai", "xai/grok").unwrap();
        store.set_last_effort("high").unwrap();

        assert!(store.clear_all().unwrap());

        assert!(store.get_api_key("xai").unwrap().is_none());
        assert!(store.get_oauth("openai").unwrap().is_none());
        assert_eq!(store.last_selection().unwrap(), None);
        assert_eq!(store.last_effort().unwrap(), None);
        assert!(store.list_profile_ids().unwrap().is_empty());
        // Idempotent: a second call has nothing left to do.
        assert!(!store.clear_all().unwrap());
    }

    #[test]
    fn clear_all_reports_true_for_a_selection_with_no_credentials() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        // Each of the five fields independently counts as state worth clearing;
        // a recorded effort alone must still be reported as removed.
        store.set_last_effort("low").unwrap();
        assert!(store.clear_all().unwrap());
    }

    #[test]
    fn list_profile_ids_merges_both_credential_kinds_sorted_and_deduped() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert!(store.list_profile_ids().unwrap().is_empty());

        store.set_api_key("zeta", "k").unwrap();
        store.set_api_key("alpha", "k").unwrap();
        store
            .set_oauth(
                "middle",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        // `alpha` holds both an API key and OAuth tokens; it must appear once.
        store
            .set_oauth(
                "alpha",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();

        assert_eq!(
            store.list_profile_ids().unwrap(),
            vec![
                "alpha".to_string(),
                "middle".to_string(),
                "zeta".to_string()
            ]
        );
    }

    #[test]
    fn resolve_key_prefers_the_environment_then_falls_back_to_the_file() {
        const KEY: &str = "FORGE_TEST_STORE_RESOLVE_KEY";
        let guard = crate::test_env::EnvGuard::new(&[KEY]);
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let names = vec![KEY.to_string()];

        // Neither source has anything.
        assert_eq!(resolve_key(&names, "xai", &store).unwrap(), None);

        // File only.
        store.set_api_key("xai", "from-file").unwrap();
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-file".to_string(), KeySource::File))
        );

        // The environment wins when both are present.
        guard.set(KEY, "from-env");
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-env".to_string(), KeySource::Env))
        );

        // A blank environment value is ignored rather than masking the file.
        guard.set(KEY, "   ");
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-file".to_string(), KeySource::File))
        );
    }

    #[cfg(unix)]
    #[test]
    fn loading_a_world_readable_credentials_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();

        // 0600 is the expected mode and must still load.
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("secret"));

        // Any group or other bit set means the secret is readable by another
        // account, so the store refuses rather than silently continuing.
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = store.get_api_key("xai").unwrap_err();
        assert!(
            matches!(err, StoreError::InsecurePermissions),
            "expected InsecurePermissions, got {err:?}"
        );
        assert!(!err.to_string().contains("secret"));
    }
}
