//! UI state persistence for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Loads and saves per-repository Files visibility
//! and theme choice — routed through the centralized runtime-storage resolver
//! (`.forge/local/ui-state`, or an application-data fallback outside a Git
//! repository) rather than a hardcoded `.forge/` join.

use super::*;
use forge_storage::{LocalRuntimeStorage, RuntimeDataKind, RuntimeStorage};

impl TuiApp {
    /// Pure path computation — no directory created, no Git exclude
    /// touched. Used for reads (`load_*`, called on every `TuiApp::new`)
    /// so simply opening Forge never triggers lazy initialization.
    fn ui_state_storage_dir_for_read(&self) -> PathBuf {
        let workspace = self.session_view.workspace_root();
        LocalRuntimeStorage::new(workspace)
            .path_for_read(RuntimeDataKind::UiState)
            .unwrap_or_else(|| workspace.join(".forge"))
    }

    /// Establishes Git exclusion / the application-data fallback as a side
    /// effect — used only by `save_*`, right before an actual write.
    fn ui_state_storage_dir_for_write(&self) -> PathBuf {
        let workspace = self.session_view.workspace_root();
        LocalRuntimeStorage::new(workspace)
            .path_for(RuntimeDataKind::UiState)
            .unwrap_or_else(|_| workspace.join(".forge"))
    }

    pub(super) fn ui_state_path(&self) -> PathBuf {
        self.ui_state_storage_dir_for_read().join("ui-state.json")
    }

    pub(super) fn repository_or_workspace_id(&self) -> String {
        self.session
            .workspace_root()
            .canonicalize()
            .unwrap_or_else(|_| self.session_view.workspace_root().to_path_buf())
            .display()
            .to_string()
    }

    pub(super) fn load_ui_state(&mut self) {
        let Ok(text) = fs::read_to_string(self.ui_state_path()) else {
            return;
        };
        let Ok(state) = serde_json::from_str::<RepositoryUiState>(&text) else {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "workspace UI state is malformed; using default Files visibility",
            );
            return;
        };
        if state.version >= 1
            && state.version <= UI_STATE_VERSION
            && state.repository_or_workspace_id == self.repository_or_workspace_id()
        {
            self.workspace_files.visible = state.files_visibility.is_open();
            if let Some(ref name) = state.theme {
                let theme_id = forge_config::normalize_theme_id(name);
                if crate::theme::registry().contains(&theme_id) {
                    self.apply_theme(theme_id, false);
                }
            }
            if let Some(mode) = state.permission_mode {
                self.session.apply_permission_mode(mode);
            }
        }
    }

    pub(super) fn save_ui_state(&mut self) {
        let path = self.ui_state_storage_dir_for_write().join("ui-state.json");
        let state = RepositoryUiState {
            version: UI_STATE_VERSION,
            repository_or_workspace_id: self.repository_or_workspace_id(),
            files_visibility: FilesVisibility::from_open(self.workspace_files.visible),
            theme: Some(self.runtime.theme_id.clone()),
            permission_mode: Some(self.session.permission_mode()),
        };
        let result = fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))
            .and_then(|_| fs::write(&path, serde_json::to_vec_pretty(&state).unwrap_or_default()));
        if let Err(error) = result {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("could not persist Files visibility: {error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::helpers::{init_repo, session_for_workspace};
    use tempfile::TempDir;

    async fn test_app(dir: &std::path::Path) -> TuiApp {
        let session = session_for_workspace(dir).await;
        TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                file_icons: forge_config::FileIconMode::Unicode,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        )
    }

    #[tokio::test]
    async fn ui_state_resolves_repository_locally_and_round_trips() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let mut app = test_app(dir.path()).await;

        // Routed through the runtime-storage resolver: under `.forge/local/`,
        // never the naive `.forge/ui-state.json`. Canonicalize the tempdir
        // side: `git rev-parse --show-toplevel` resolves symlinks (e.g.
        // macOS's `/var` -> `/private/var`), the raw tempdir path doesn't.
        let path = app.ui_state_path();
        let repo_local_root = dir
            .path()
            .canonicalize()
            .unwrap()
            .join(".forge")
            .join("local");
        assert!(path.starts_with(&repo_local_root));
        assert!(path.ends_with(std::path::Path::new("ui-state").join("ui-state.json")));

        app.workspace_files.visible = false;
        app.save_ui_state();
        assert!(path.is_file());

        let mut reloaded = test_app(dir.path()).await;
        reloaded.workspace_files.visible = true;
        reloaded.load_ui_state();
        assert!(!reloaded.workspace_files.visible);
    }
}
