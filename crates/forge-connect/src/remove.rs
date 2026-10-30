//! Removing a custom provider and everything Forge keeps for it.
//!
//! The order is fixed: the `providers.toml` edit comes first and is the only
//! step that can fail the call. The API key, the cached model list and the
//! saved selections are cleared after it, each best-effort, and a failure is
//! reported rather than undone. Adding a provider clears the same stores for
//! its id (`clear_provider_state`), so nothing left behind comes back.

use std::path::{Path, PathBuf};

use crate::catalog::ModelCatalogCache;
use crate::normalize::normalize_saved_model_id;
use crate::preferences::ForgetOutcome;
use crate::profile::ProviderSpec;
use crate::registry::{provider_entry_count, remove_provider_entry, unrecoverable_fields};
use crate::resolve::{env_names_for, first_env};
use crate::service::ConnectService;

/// A store `remove_custom_provider` clears after `providers.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Credentials,
    Catalog,
    Preferences,
}

impl std::fmt::Display for StoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Credentials => "credentials",
            Self::Catalog => "model cache",
            Self::Preferences => "preferences",
        })
    }
}

/// What removing a provider deletes (a preview), or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalReport {
    pub profile_id: String,
    pub providers_path: PathBuf,
    pub manual_models: usize,
    /// In a preview, the selections that would be cleared; after removal,
    /// the ones that were.
    pub forget: ForgetOutcome,
    /// Custom API-key variables still set in this process's environment.
    /// Names only, never values.
    pub env_still_set: Vec<String>,
    /// Entry fields the picker cannot recreate.
    pub unrecoverable_fields: Vec<String>,
    /// Stores that could not be cleared, with the reason. Empty in a preview.
    pub failed: Vec<(StoreKind, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemovalError {
    #[error("no custom provider `{0}`")]
    NotFound(String),
    #[error("`{0}` is built in; only custom providers can be removed")]
    BuiltIn(String),
    #[error(
        "`{0}` appears more than once in providers.toml; remove the extra entry by hand first"
    )]
    DuplicateId(String),
    #[error("could not update providers.toml: {0}")]
    Providers(String),
}

impl<'a> ConnectService<'a> {
    /// Check that `id` can be removed and describe what removal would do.
    /// Writes nothing.
    pub fn preview_removal(
        &self,
        providers_path: &Path,
        id: &str,
    ) -> Result<RemovalReport, RemovalError> {
        self.checked_removal(providers_path, id)
            .map(|(_, report)| report)
    }

    /// Remove custom provider `id`: its `providers.toml` entry, override
    /// section and manual models in one checked write, then its API key,
    /// cached models and saved selections. Only the pre-checks and the
    /// `providers.toml` write return `Err`; a later store that fails is
    /// listed in `failed`.
    pub fn remove_custom_provider(
        &self,
        providers_path: &Path,
        catalog: &ModelCatalogCache,
        id: &str,
    ) -> Result<RemovalReport, RemovalError> {
        let (spec, mut report) = self.checked_removal(providers_path, id)?;
        remove_provider_entry(providers_path, &spec.id).map_err(RemovalError::Providers)?;
        (report.forget, report.failed) = self.clear_provider_state(catalog, spec);
        Ok(report)
    }

    /// Clear the API key, cached models and saved selections of `spec`,
    /// each step running even when an earlier one failed. `spec` must still
    /// be in `self.registry`.
    pub fn clear_provider_state(
        &self,
        catalog: &ModelCatalogCache,
        spec: &ProviderSpec,
    ) -> (ForgetOutcome, Vec<(StoreKind, String)>) {
        let mut failed = Vec::new();
        if let Err(error) = self.credentials.clear(spec) {
            failed.push((StoreKind::Credentials, error.to_string()));
        }
        if let Err(error) = catalog.clear_profile(&spec.id) {
            failed.push((StoreKind::Catalog, error.to_string()));
        }
        let forget = match self
            .preferences
            .forget_provider(&|profile, model| selection_belongs_to(spec, profile, model))
        {
            Ok(forget) => forget,
            Err(error) => {
                failed.push((StoreKind::Preferences, error.to_string()));
                ForgetOutcome::default()
            }
        };
        (forget, failed)
    }

    fn checked_removal(
        &self,
        providers_path: &Path,
        id: &str,
    ) -> Result<(&'a ProviderSpec, RemovalReport), RemovalError> {
        let spec = self
            .registry
            .get(id)
            .ok_or_else(|| RemovalError::NotFound(id.to_string()))?;
        if !spec.is_user() {
            return Err(RemovalError::BuiltIn(spec.id.clone()));
        }
        let text = std::fs::read_to_string(providers_path)
            .map_err(|error| RemovalError::Providers(error.to_string()))?;
        let document: toml::Value = toml::from_str(&text)
            .map_err(|error| RemovalError::Providers(error.message().to_string()))?;
        match provider_entry_count(&document, &spec.id) {
            0 => return Err(RemovalError::NotFound(spec.id.clone())),
            1 => {}
            _ => return Err(RemovalError::DuplicateId(spec.id.clone())),
        }
        let entry = document
            .get("providers")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| {
                entry
                    .get("id")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|entry_id| entry_id.eq_ignore_ascii_case(&spec.id))
            })
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        let env_still_set = env_names_for(spec)
            .0
            .into_iter()
            .filter(|name| !(name.starts_with("FORGE_") && name.ends_with("_API_KEY")))
            .filter(|name| first_env(std::slice::from_ref(name)).is_some())
            .collect();
        let report = RemovalReport {
            profile_id: spec.id.clone(),
            providers_path: providers_path.to_path_buf(),
            manual_models: spec.manual_models.len(),
            forget: self
                .preferences
                .would_forget(&|profile, model| selection_belongs_to(spec, profile, model)),
            env_still_set,
            unrecoverable_fields: unrecoverable_fields(&entry),
            failed: Vec::new(),
        };
        Ok((spec, report))
    }
}

/// True when a saved `(profile id, model)` selection points at `spec`: by
/// profile id, or by the model's provider prefix (its id, or an older custom
/// prefix it was saved under).
fn selection_belongs_to(spec: &ProviderSpec, profile: &str, model: &str) -> bool {
    if profile.eq_ignore_ascii_case(&spec.id) {
        return true;
    }
    let model = normalize_saved_model_id(model);
    let Some((prefix, _)) = model.split_once('/') else {
        return false;
    };
    prefix.eq_ignore_ascii_case(&spec.id)
        || spec
            .legacy_prefix
            .as_deref()
            .is_some_and(|old| prefix.eq_ignore_ascii_case(old))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Credentials;
    use crate::preferences::PreferenceStore;
    use crate::registry::{loaded_registry_from, ConnectRegistry};
    use crate::store::CredentialStore;
    use crate::test_env::EnvGuard;
    use tempfile::{tempdir, TempDir};

    const LABBOX: &str = "# my providers\n\n[[providers]]\nid = \"labbox\"\ntitle = \"labbox\"\nbase_url = \"https://lab.example/v1\"\nwire = \"chat-completions\"\napi_key_env = [\"FORGE_TEST_LABBOX_KEY\"]\n\n[providers.auth]\nkind = \"api-key\"\n\n# keep me\n[[providers]]\nid = \"other\"\ntitle = \"other\"\nbase_url = \"https://other.example/v1\"\nwire = \"chat-completions\"\n\n[provider_overrides.labbox]\nbase_url = \"https://lab2.example/v1\"\n\n# before manual models\n[manual_models]\nlabbox = [\n  \"labbox/a\",\n  \"labbox/b\",\n]\nother = [\"other/x\"] # theirs\n";

    struct Fixture {
        dir: TempDir,
        registry: ConnectRegistry,
        credentials: Credentials,
        preferences: PreferenceStore,
        catalog: ModelCatalogCache,
    }

    impl Fixture {
        fn new(providers: &str) -> Self {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("providers.toml"), providers).unwrap();
            Self {
                registry: loaded_registry_from(dir.path().join("providers.toml")),
                credentials: Credentials::new(CredentialStore::new(
                    dir.path().join("credentials.toml"),
                )),
                preferences: PreferenceStore::new(dir.path().join("preferences.toml")),
                catalog: ModelCatalogCache::new(dir.path().join("model-catalog.toml")),
                dir,
            }
        }

        fn providers_path(&self) -> PathBuf {
            self.dir.path().join("providers.toml")
        }

        fn service(&self) -> ConnectService<'_> {
            ConnectService {
                registry: &self.registry,
                credentials: &self.credentials,
                preferences: &self.preferences,
                active_profile_id: None,
                active_model: None,
            }
        }

        fn remove(&self, id: &str) -> Result<RemovalReport, RemovalError> {
            self.service()
                .remove_custom_provider(&self.providers_path(), &self.catalog, id)
        }

        fn providers_text(&self) -> String {
            std::fs::read_to_string(self.providers_path()).unwrap()
        }
    }

    #[test]
    fn removes_entry_override_and_manual_models_and_keeps_every_other_byte() {
        let fixture = Fixture::new(LABBOX);
        let report = fixture.remove("labbox").unwrap();
        assert_eq!(report.manual_models, 2);
        assert!(report.failed.is_empty());
        assert_eq!(
            fixture.providers_text(),
            "# my providers\n\n\n# keep me\n[[providers]]\nid = \"other\"\ntitle = \"other\"\nbase_url = \"https://other.example/v1\"\nwire = \"chat-completions\"\n\n\n# before manual models\n[manual_models]\nother = [\"other/x\"] # theirs\n"
        );
        let reloaded = loaded_registry_from(fixture.providers_path());
        assert!(reloaded.get("labbox").is_none());
        assert!(reloaded.load_warnings().is_empty());
    }

    #[test]
    fn refuses_built_in_unknown_and_duplicate_ids() {
        let fixture = Fixture::new(LABBOX);
        assert_eq!(
            fixture.remove("anthropic"),
            Err(RemovalError::BuiltIn("anthropic".into()))
        );
        assert_eq!(
            fixture.remove("nope"),
            Err(RemovalError::NotFound("nope".into()))
        );
        let duplicated = format!("{LABBOX}\n[[providers]]\nid = \"labbox\"\ntitle = \"again\"\nbase_url = \"https://again.example/v1\"\n");
        let fixture = Fixture::new(&duplicated);
        assert_eq!(
            fixture.remove("labbox"),
            Err(RemovalError::DuplicateId("labbox".into()))
        );
        assert_eq!(fixture.providers_text(), duplicated);
    }

    #[test]
    fn a_layout_the_edit_cannot_cut_changes_nothing() {
        // An inline array passes the pre-check but has no `[[providers]]`
        // header to cut, so the providers.toml write is refused.
        let fixture = Fixture::new(
            "providers = [{ id = \"labbox\", title = \"labbox\", base_url = \"https://lab.example/v1\", wire = \"chat-completions\" }]\n",
        );
        let spec = fixture.registry.get("labbox").unwrap().clone();
        fixture.credentials.set_api_key(&spec, "sk-test").unwrap();
        fixture
            .catalog
            .put("labbox", vec!["labbox/a".into()])
            .unwrap();
        let before = fixture.providers_text();
        assert!(matches!(
            fixture.remove("labbox"),
            Err(RemovalError::Providers(_))
        ));
        assert_eq!(fixture.providers_text(), before);
        assert_eq!(fixture.catalog.get_cached("labbox"), vec!["labbox/a"]);
        assert!(
            std::fs::read_to_string(fixture.dir.path().join("credentials.toml"))
                .unwrap()
                .contains("sk-test")
        );
    }

    #[test]
    fn clears_key_cache_and_both_selection_groups() {
        let fixture = Fixture::new(LABBOX);
        let spec = fixture.registry.get("labbox").unwrap().clone();
        fixture.credentials.set_api_key(&spec, "sk-test").unwrap();
        fixture
            .catalog
            .put("labbox", vec!["labbox/a".into()])
            .unwrap();
        fixture
            .catalog
            .put("other", vec!["other/x".into()])
            .unwrap();
        std::fs::write(
            fixture.preferences.path(),
            "last_profile_id = \"other\"\nlast_model = \"labbox/a\"\nprevious_profile_id = \"labbox\"\nprevious_model = \"labbox/b\"\n",
        )
        .unwrap();
        let preview = fixture
            .service()
            .preview_removal(&fixture.providers_path(), "labbox")
            .unwrap();
        assert_eq!(
            preview.forget,
            ForgetOutcome {
                cleared_last: true,
                cleared_previous: true
            }
        );
        let report = fixture.remove("labbox").unwrap();
        assert_eq!(report.forget, preview.forget);
        assert!(
            !std::fs::read_to_string(fixture.dir.path().join("credentials.toml"))
                .unwrap()
                .contains("sk-test")
        );
        assert!(fixture.catalog.get_cached("labbox").is_empty());
        assert_eq!(fixture.catalog.get_cached("other"), vec!["other/x"]);
        assert!(fixture.preferences.last_selection().unwrap().is_none());
        assert!(fixture.preferences.previous_selection().unwrap().is_none());
    }

    #[test]
    fn a_store_that_fails_is_listed_and_the_others_are_still_cleared() {
        let fixture = Fixture::new(LABBOX);
        let spec = fixture.registry.get("labbox").unwrap().clone();
        fixture.credentials.set_api_key(&spec, "sk-test").unwrap();
        fixture
            .catalog
            .put("labbox", vec!["labbox/a".into()])
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let creds = fixture.dir.path().join("credentials.toml");
            std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o644)).unwrap();
            let report = fixture.remove("labbox").unwrap();
            assert_eq!(report.failed.len(), 1);
            assert_eq!(report.failed[0].0, StoreKind::Credentials);
            assert!(fixture.catalog.get_cached("labbox").is_empty());
        }
    }

    #[test]
    fn reports_custom_key_variables_that_are_still_set() {
        let _env = EnvGuard::new(&["FORGE_TEST_LABBOX_KEY", "FORGE_LABBOX_API_KEY"]);
        let fixture = Fixture::new(LABBOX);
        let preview = |fixture: &Fixture| {
            fixture
                .service()
                .preview_removal(&fixture.providers_path(), "labbox")
                .unwrap()
                .env_still_set
        };
        std::env::set_var("FORGE_LABBOX_API_KEY", "sk-generated");
        std::env::set_var("FORGE_TEST_LABBOX_KEY", "  ");
        assert!(preview(&fixture).is_empty());
        std::env::set_var("FORGE_TEST_LABBOX_KEY", "sk-custom");
        assert_eq!(preview(&fixture), vec!["FORGE_TEST_LABBOX_KEY"]);
    }

    #[test]
    fn names_hand_written_settings_only() {
        let fixture = Fixture::new(LABBOX);
        let preview = fixture
            .service()
            .preview_removal(&fixture.providers_path(), "labbox")
            .unwrap();
        assert!(preview.unrecoverable_fields.is_empty(), "{preview:?}");
        let fixture = Fixture::new(&LABBOX.replace(
            "[providers.auth]\nkind = \"api-key\"",
            "description = \"my lab\"\n\n[providers.auth]\nkind = \"api-key-optional\"",
        ));
        let preview = fixture
            .service()
            .preview_removal(&fixture.providers_path(), "labbox")
            .unwrap();
        assert_eq!(preview.unrecoverable_fields, vec!["auth", "description"]);
    }

    #[test]
    fn an_older_custom_prefix_still_matches_saved_selections() {
        let fixture = Fixture::new(&LABBOX.replace(
            "wire = \"chat-completions\"\napi_key_env",
            "wire = \"chat-completions\"\nmodel_provider_prefix = \"oldlab\"\napi_key_env",
        ));
        std::fs::write(
            fixture.preferences.path(),
            "last_profile_id = \"other\"\nlast_model = \"oldlab/a\"\n",
        )
        .unwrap();
        let report = fixture.remove("labbox").unwrap();
        assert!(report.forget.cleared_last);
        assert!(fixture.preferences.last_selection().unwrap().is_none());
    }
}
