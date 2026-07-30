//! UI state and run-history persistence for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Loads and saves per-repository Files visibility,
//! theme choice, and recent run records under `.forge/`. Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn run_history_path(&self) -> PathBuf {
        self.session
            .workspace_root()
            .join(".forge/run-history.json")
    }

    pub(super) fn ui_state_path(&self) -> PathBuf {
        self.session.workspace_root().join(".forge/ui-state.json")
    }

    pub(super) fn load_run_history(&mut self) {
        let Ok(text) = fs::read_to_string(self.run_history_path()) else {
            return;
        };
        let Ok(history) = serde_json::from_str::<RunHistoryFile>(&text) else {
            self.run.error = Some("run history is malformed; recent runs were not loaded".into());
            return;
        };
        let workspace_id = self.session.workspace_root().display().to_string();
        if history.version == RUN_HISTORY_VERSION
            && history.repository_or_workspace_id == workspace_id
        {
            self.run.recent = history.recent.into_iter().take(MAX_RECENT_RUNS).collect();
        }
    }

    pub(super) fn save_run_history(&mut self) {
        let path = self.run_history_path();
        let history = RunHistoryFile {
            version: RUN_HISTORY_VERSION,
            repository_or_workspace_id: self.session.workspace_root().display().to_string(),
            recent: self.run.recent.iter().cloned().collect(),
        };
        let result =
            fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new("."))).and_then(|_| {
                fs::write(
                    &path,
                    serde_json::to_vec_pretty(&history).unwrap_or_default(),
                )
            });
        if let Err(error) = result {
            self.run.error = Some(format!("could not persist recent runs: {error}"));
        }
    }

    pub(super) fn repository_or_workspace_id(&self) -> String {
        self.session.workspace_root().display().to_string()
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
            self.files_visible = state.files_visibility.is_open();
            if let Some(ref name) = state.theme {
                if let Ok(theme) = forge_config::Theme::parse_strict(name) {
                    self.apply_theme(theme, false);
                }
            }
        }
    }

    pub(super) fn save_ui_state(&mut self) {
        let path = self.ui_state_path();
        let state = RepositoryUiState {
            version: UI_STATE_VERSION,
            repository_or_workspace_id: self.repository_or_workspace_id(),
            files_visibility: FilesVisibility::from_open(self.files_visible),
            theme: Some(self.runtime.theme.label().to_string()),
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
