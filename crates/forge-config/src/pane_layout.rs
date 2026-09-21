//! Global TUI pane-layout preferences.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Ratios are relative to the usable workspace width/height. Missing values
/// deliberately preserve the renderer's existing responsive defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneLayoutPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_width_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_width_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_panel_height_ratio: Option<f64>,
}

/// File-backed access to the `[tui.layout]` section of the user config.
#[derive(Debug, Clone)]
pub struct PaneLayoutStore {
    path: PathBuf,
}

impl PaneLayoutStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn user_default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("forge")
            .join("config.toml");
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Preferences are optional convenience state. Missing or malformed data
    /// falls back to the established responsive layout rather than blocking
    /// startup.
    pub fn load(&self) -> PaneLayoutPreferences {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return PaneLayoutPreferences::default();
        };
        let Ok(value) = toml::from_str::<toml::Value>(&text) else {
            return PaneLayoutPreferences::default();
        };
        value
            .get("tui")
            .and_then(|value| value.get("layout"))
            .cloned()
            .and_then(|value| value.try_into().ok())
            .unwrap_or_default()
    }

    pub fn save(&self, preferences: PaneLayoutPreferences) -> Result<(), ConfigError> {
        self.update(Some(preferences))
    }

    pub fn reset(&self) -> Result<(), ConfigError> {
        self.update(None)
    }

    fn update(&self, preferences: Option<PaneLayoutPreferences>) -> Result<(), ConfigError> {
        if preferences.is_none() && !self.path.is_file() {
            return Ok(());
        }
        let mut value = if self.path.is_file() {
            let text = std::fs::read_to_string(&self.path)?;
            toml::from_str(&text)?
        } else {
            toml::Value::Table(toml::map::Map::new())
        };
        let root = value
            .as_table_mut()
            .ok_or_else(|| ConfigError::Message("user config is not a TOML table".into()))?;
        let tui = root
            .entry("tui")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigError::Message("[tui] is not a table".into()))?;
        if let Some(preferences) = preferences {
            tui.insert(
                "layout".into(),
                toml::Value::try_from(preferences)
                    .map_err(|error| ConfigError::Message(error.to_string()))?,
            );
        } else {
            tui.remove("layout");
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &self.path,
            toml::to_string_pretty(&value)
                .map_err(|error| ConfigError::Message(error.to_string()))?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_layout_round_trips_without_overwriting_other_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[model]\nmodel = \"keep-me\"\n").unwrap();
        let store = PaneLayoutStore::new(path.clone());
        let preferences = PaneLayoutPreferences {
            files_width_ratio: Some(0.25),
            conversation_width_ratio: Some(0.4),
            bottom_panel_height_ratio: Some(0.3),
        };

        store.save(preferences).unwrap();
        assert_eq!(store.load(), preferences);
        assert!(std::fs::read_to_string(&path).unwrap().contains("keep-me"));

        store.reset().unwrap();
        assert_eq!(store.load(), PaneLayoutPreferences::default());
        assert!(std::fs::read_to_string(path).unwrap().contains("keep-me"));
    }

    #[test]
    fn reset_does_not_create_a_missing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        PaneLayoutStore::new(path.clone()).reset().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn save_does_not_overwrite_a_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let malformed = "not = valid = toml";
        std::fs::write(&path, malformed).unwrap();

        let result = PaneLayoutStore::new(path.clone()).save(PaneLayoutPreferences {
            files_width_ratio: Some(0.25),
            ..PaneLayoutPreferences::default()
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), malformed);
    }
}
