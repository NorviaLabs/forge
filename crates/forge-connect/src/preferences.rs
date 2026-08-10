//! Which provider, model, and reasoning effort the operator last chose.
//!
//! These are not secrets, and they used to live in the credentials file
//! anyway. That had two costs. Every model or effort switch rewrote the file
//! holding every API key and OAuth token, giving a secrets file far more
//! writes than its contents warrant. And reads of that file are permission
//! gated — correctly, 0600 enforced on every read — so a `chmod` problem took
//! preference loading down along with credential loading.
//!
//! They live in their own file now. Nothing here is sensitive, so there is no
//! permission gate and no parse cache: unlike credentials, these are read a
//! handful of times per session rather than on the render path.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::selection::ModelSelection;
use crate::store::StoreError;

/// On-disk shape. Every field is optional: a missing or empty file simply
/// means nothing has been chosen yet, which is the first-run state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PreferencesFile {
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

/// File-backed store for the operator's last interactive selections.
pub struct PreferenceStore {
    path: PathBuf,
}

impl PreferenceStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn user_default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("forge")
            .join("preferences.toml");
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A missing or unreadable file reads as "nothing chosen yet" rather than
    /// an error: a preference is a convenience, and failing to load one should
    /// never be able to block starting a session.
    fn load(&self) -> PreferencesFile {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return PreferencesFile::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    fn save(&self, file: &PreferencesFile) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, toml::to_string_pretty(file)?)?;
        Ok(())
    }

    pub fn last_selection(&self) -> Result<Option<(String, String)>, StoreError> {
        let file = self.load();
        Ok(
            match (file.last_profile_id.as_deref(), file.last_model.as_deref()) {
                (Some(profile_id), Some(model))
                    if !profile_id.trim().is_empty() && !model.trim().is_empty() =>
                {
                    Some((profile_id.to_string(), model.to_string()))
                }
                _ => None,
            },
        )
    }

    pub fn set_last_selection(&self, profile_id: &str, model: &str) -> Result<(), StoreError> {
        let mut file = self.load();
        file.last_profile_id = Some(profile_id.trim().to_string());
        file.last_model = Some(model.trim().to_string());
        self.save(&file)
    }

    /// Compose the last provider/model/effort selection into one
    /// `ModelSelection`, when a complete selection was recorded.
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

    pub fn last_effort(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .load()
            .last_effort
            .filter(|effort| !effort.trim().is_empty()))
    }

    pub fn set_last_effort(&self, effort: &str) -> Result<(), StoreError> {
        let mut file = self.load();
        file.last_effort = Some(effort.trim().to_string());
        self.save(&file)
    }

    pub fn clear_last_selection(&self, profile_id: Option<&str>) -> Result<(), StoreError> {
        let mut file = self.load();
        if profile_id.is_none() || file.last_profile_id.as_deref() == profile_id {
            file.last_profile_id = None;
            file.last_model = None;
            file.last_effort = None;
            self.save(&file)?;
        }
        Ok(())
    }

    /// The selection active immediately before the current `last_*` (Quick
    /// Switch's toggle target), if one was recorded.
    pub fn previous_selection(&self) -> Result<Option<(String, String)>, StoreError> {
        let file = self.load();
        Ok(
            match (
                file.previous_profile_id.as_deref(),
                file.previous_model.as_deref(),
            ) {
                (Some(profile_id), Some(model))
                    if !profile_id.trim().is_empty() && !model.trim().is_empty() =>
                {
                    Some((profile_id.to_string(), model.to_string()))
                }
                _ => None,
            },
        )
    }

    /// The effort paired with [`Self::previous_selection`], if any.
    pub fn previous_effort(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .load()
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
        let mut file = self.load();
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
        let mut file = self.load();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn prefs() -> (TempDir, PreferenceStore) {
        let dir = TempDir::new().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("prefs.toml"));
        (dir, prefs)
    }

    #[test]
    fn a_missing_file_reads_as_nothing_chosen() {
        let (_dir, prefs) = prefs();
        assert!(prefs.last_selection().unwrap().is_none());
        assert!(prefs.last_effort().unwrap().is_none());
        assert!(prefs.quick_switch().unwrap().is_none());
    }

    /// A preference is a convenience. Failing to parse one must never be able
    /// to block starting a session, so a corrupt file reads as empty.
    #[test]
    fn a_corrupt_file_reads_as_nothing_chosen() {
        let (_dir, prefs) = prefs();
        std::fs::write(prefs.path(), "this is not toml {{{").unwrap();
        assert!(prefs.last_selection().unwrap().is_none());
    }

    /// The whole point of the split: writing a preference must not touch the
    /// credentials file.
    #[test]
    fn writing_a_preference_does_not_create_a_credentials_file() {
        let dir = TempDir::new().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("prefs.toml"));
        prefs.set_last_effort("high").unwrap();
        assert!(!dir.path().join("credentials.toml").exists());
    }

    #[test]
    fn last_selection_roundtrip() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        assert_eq!(prefs.last_selection().unwrap(), None);
        prefs
            .set_last_selection("anthropic", "anthropic/claude-sonnet-4-5")
            .unwrap();
        prefs.set_last_effort("high").unwrap();
        assert_eq!(
            prefs.last_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet-4-5".into()))
        );
        assert_eq!(prefs.last_effort().unwrap().as_deref(), Some("high"));
        prefs.clear_last_selection(Some("openai")).unwrap();
        assert!(prefs.last_selection().unwrap().is_some());
        prefs.clear_last_selection(Some("anthropic")).unwrap();
        assert_eq!(prefs.last_selection().unwrap(), None);
        assert_eq!(prefs.last_effort().unwrap(), None);
    }

    #[test]
    fn last_selection_struct_composes_profile_model_and_effort() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        assert_eq!(prefs.last_selection_struct().unwrap(), None);

        prefs
            .set_last_selection("openai_codex", "openai-codex/gpt-5.6-luna")
            .unwrap();
        prefs.set_last_effort("high").unwrap();

        assert_eq!(
            prefs.last_selection_struct().unwrap(),
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
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        assert_eq!(prefs.previous_selection().unwrap(), None);

        prefs
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        assert_eq!(
            prefs.last_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        // First switch ever: nothing to rotate into previous yet.
        assert_eq!(prefs.previous_selection().unwrap(), None);

        prefs
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        assert_eq!(
            prefs.last_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet".into()))
        );
        assert_eq!(prefs.last_effort().unwrap().as_deref(), Some("high"));
        assert_eq!(
            prefs.previous_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        assert_eq!(prefs.previous_effort().unwrap().as_deref(), Some("medium"));
    }

    #[test]
    fn record_switch_reselecting_the_active_combo_does_not_rotate() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        prefs
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        prefs
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        // Reselecting the same model+effort that's already active must not
        // clobber the real previous combo with a duplicate of itself.
        prefs
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        assert_eq!(
            prefs.previous_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
    }

    #[test]
    fn quick_switch_swaps_last_and_previous_and_toggles_back() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));

        // Nothing to switch to yet.
        assert_eq!(prefs.quick_switch().unwrap(), None);

        prefs
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        prefs
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();

        let switched = prefs.quick_switch().unwrap();
        assert_eq!(
            switched,
            Some(("openai".into(), "openai/gpt-5.6".into(), "medium".into()))
        );
        assert_eq!(
            prefs.last_selection().unwrap(),
            Some(("openai".into(), "openai/gpt-5.6".into()))
        );
        assert_eq!(
            prefs.previous_selection().unwrap(),
            Some(("anthropic".into(), "anthropic/claude-sonnet".into()))
        );

        // A second Quick Switch toggles back to where we started.
        let switched_back = prefs.quick_switch().unwrap();
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
    fn clearing_the_selection_leaves_the_quick_switch_target() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        prefs
            .record_switch(("openai", "openai/gpt-5.6", "medium"))
            .unwrap();
        prefs
            .record_switch(("anthropic", "anthropic/claude-sonnet", "high"))
            .unwrap();
        prefs.clear_last_selection(None).unwrap();
        // Clearing the current selection leaves the Quick Switch target
        // intact — it is a separate slot, and the operator did not ask for it
        // to go.
        assert!(prefs.previous_selection().unwrap().is_some());
    }

    #[test]
    /// A recorded effort with nothing else is still a real preference.
    fn an_effort_alone_is_recorded() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        prefs.set_last_effort("low").unwrap();
        assert_eq!(prefs.last_effort().unwrap().as_deref(), Some("low"));
    }
}
