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
}

/// File-backed store for the operator's last interactive selections.
pub struct PreferenceStore {
    path: PathBuf,
}

/// Which saved selection groups (profile, model, effort) point at a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForgetOutcome {
    /// The default selection (`last_*`).
    pub cleared_last: bool,
}

/// True when the saved `(profile id, model)` pair belongs to a provider.
pub type SelectionMatch<'a> = &'a dyn Fn(&str, &str) -> bool;

fn group_matches(
    matches: SelectionMatch<'_>,
    profile: &Option<String>,
    model: &Option<String>,
) -> bool {
    let profile = profile.as_deref().unwrap_or("").trim();
    let model = model.as_deref().unwrap_or("").trim();
    !(profile.is_empty() && model.is_empty()) && matches(profile, model)
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
        crate::atomic::write_atomic(&self.path, toml::to_string_pretty(file)?.as_bytes(), None)?;
        Ok(())
    }

    pub fn last_selection(&self) -> Result<Option<(String, String)>, StoreError> {
        let file = self.load();
        Ok(
            match (file.last_profile_id.as_deref(), file.last_model.as_deref()) {
                (Some(profile_id), Some(model))
                    if !profile_id.trim().is_empty() && !model.trim().is_empty() =>
                {
                    // Saved before the id became the model prefix? Read it in the
                    // current form; the next save writes that form.
                    Some((
                        profile_id.to_string(),
                        crate::normalize::normalize_saved_model_id(model),
                    ))
                }
                _ => None,
            },
        )
    }

    pub fn set_last_selection(&self, profile_id: &str, model: &str) -> Result<(), StoreError> {
        let mut file = self.load();
        file.last_profile_id = Some(profile_id.trim().to_string());
        file.last_model = Some(crate::normalize::normalize_saved_model_id(model));
        self.save(&file)
    }

    /// Compose the last provider/model/effort selection into one
    /// `ModelSelection`, when a complete selection was recorded.
    pub fn last_selection_struct(
        &self,
        registry: &crate::ConnectRegistry,
    ) -> Result<Option<ModelSelection>, StoreError> {
        let Some((profile_id, model)) = self.last_selection()? else {
            return Ok(None);
        };
        // The model id carries its provider prefix; a saved profile that
        // disagrees with it is stale and would otherwise pair a foreign model
        // with the wrong route. The model wins, with the saved profile as the
        // fallback for un-namespaced ids.
        let Some(spec) = registry
            .profile_for_model(&model)
            .or_else(|| registry.get(&profile_id))
        else {
            return Ok(None);
        };
        let effort = self.last_effort()?.unwrap_or_default();
        Ok(Some(ModelSelection {
            provider: "native".into(),
            model,
            profile_id: Some(spec.id.clone()),
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

    /// Which selection groups `forget_provider` would clear. Writes nothing.
    pub fn would_forget(&self, matches: SelectionMatch<'_>) -> ForgetOutcome {
        let file = self.load();
        ForgetOutcome {
            cleared_last: group_matches(matches, &file.last_profile_id, &file.last_model),
        }
    }

    /// Clear the saved selection when it points at a removed provider. The
    /// file is saved only when something matched.
    pub fn forget_provider(
        &self,
        matches: SelectionMatch<'_>,
    ) -> Result<ForgetOutcome, StoreError> {
        let outcome = self.would_forget(matches);
        if outcome == ForgetOutcome::default() {
            return Ok(outcome);
        }
        let mut file = self.load();
        if outcome.cleared_last {
            file.last_profile_id = None;
            file.last_model = None;
            file.last_effort = None;
        }
        self.save(&file)?;
        Ok(outcome)
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
    fn forgetting_a_provider_clears_the_selection_that_points_at_it() {
        let (_dir, prefs) = prefs();
        let text =
            "last_profile_id = \"labbox\"\nlast_model = \"labbox/m\"\nlast_effort = \"high\"\n";
        std::fs::write(prefs.path(), text).unwrap();
        let matches =
            |profile: &str, model: &str| profile == "labbox" || model.starts_with("labbox/");
        let expected = ForgetOutcome { cleared_last: true };
        assert_eq!(prefs.would_forget(&matches), expected);
        assert_eq!(std::fs::read_to_string(prefs.path()).unwrap(), text);
        assert_eq!(prefs.forget_provider(&matches).unwrap(), expected);
        assert!(prefs.last_selection().unwrap().is_none());
        assert!(prefs.last_effort().unwrap().is_none());
    }

    #[test]
    fn forgetting_an_unused_provider_leaves_the_file_alone() {
        let (_dir, prefs) = prefs();
        let text = "last_profile_id = \"anthropic\"\nlast_model = \"anthropic/m\"\n";
        std::fs::write(prefs.path(), text).unwrap();
        let matches = |profile: &str, _: &str| profile == "labbox";
        assert_eq!(
            prefs.forget_provider(&matches).unwrap(),
            ForgetOutcome::default()
        );
        assert_eq!(std::fs::read_to_string(prefs.path()).unwrap(), text);
    }

    #[test]
    fn a_missing_file_reads_as_nothing_chosen() {
        let (_dir, prefs) = prefs();
        assert!(prefs.last_selection().unwrap().is_none());
        assert!(prefs.last_effort().unwrap().is_none());
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
    fn a_stale_saved_profile_yields_to_the_model_provider_prefix() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        // A corrupt/legacy pair: the xAI profile with an OpenCode model id.
        prefs
            .set_last_selection("xai", "opencode_go/deepseek-v4.1-flash")
            .unwrap();

        let selection = prefs
            .last_selection_struct(&crate::builtin_registry())
            .unwrap()
            .unwrap();
        assert_eq!(selection.profile_id.as_deref(), Some("opencode_go"));
        assert_eq!(selection.model, "opencode_go/deepseek-v4.1-flash");
    }

    #[test]
    fn a_consistent_saved_selection_is_preserved() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        prefs
            .set_last_selection("opencode_go", "opencode_go/deepseek-v4.1-flash")
            .unwrap();

        let selection = prefs
            .last_selection_struct(&crate::builtin_registry())
            .unwrap()
            .unwrap();
        assert_eq!(selection.profile_id.as_deref(), Some("opencode_go"));
    }

    #[test]
    fn profile_for_model_resolves_the_namespaced_provider() {
        let registry = crate::loaded_registry();
        assert_eq!(
            registry
                .profile_for_model("opencode_go/deepseek-v4.1-flash")
                .map(|spec| spec.id.as_str()),
            Some("opencode_go")
        );
        assert!(registry.profile_for_model("not-a-provider/model").is_none());
    }

    #[test]
    fn last_selection_struct_composes_profile_model_and_effort() {
        let dir = tempdir().unwrap();
        let prefs = PreferenceStore::new(dir.path().join("p.toml"));
        assert_eq!(
            prefs
                .last_selection_struct(&crate::builtin_registry())
                .unwrap(),
            None
        );

        prefs
            .set_last_selection("openai_codex", "openai_codex/gpt-5.6-luna")
            .unwrap();
        prefs.set_last_effort("high").unwrap();

        assert_eq!(
            prefs
                .last_selection_struct(&crate::builtin_registry())
                .unwrap(),
            Some(ModelSelection {
                provider: "native".into(),
                model: "openai_codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
                effort: "high".into(),
            })
        );
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
