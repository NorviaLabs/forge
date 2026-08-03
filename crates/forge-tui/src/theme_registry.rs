//! Discover and load TUI themes from bundled files and optional directories.

use forge_config::{parse_theme_toml, ThemeDefinition, ThemePalette, DEFAULT_THEME_ID};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const BUILTIN_THEMES: &[(&str, &str)] = &[
    (
        "solarized-dark.toml",
        include_str!("../themes/solarized-dark.toml"),
    ),
    (
        "solarized-light.toml",
        include_str!("../themes/solarized-light.toml"),
    ),
];

/// All themes available to the TUI (built-ins, user, and workspace drops).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeRegistry {
    themes: Vec<ThemeDefinition>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::load(None)
    }
}

impl ThemeRegistry {
    /// Load built-in themes plus any `.toml` files from discovery directories.
    ///
    /// Drop-in files that fail to parse are skipped without feedback; use
    /// [`ThemeRegistry::load_with_diagnostics`] when the caller can surface
    /// the resulting error messages to the user.
    pub fn load(workspace: Option<&Path>) -> Self {
        Self::load_with_diagnostics(workspace).0
    }

    /// Same as [`ThemeRegistry::load`], but also returns a human-readable
    /// message for every drop-in file that failed to parse, so a bad
    /// `theme.toml` doesn't vanish from the picker with no explanation.
    pub fn load_with_diagnostics(workspace: Option<&Path>) -> (Self, Vec<String>) {
        let mut by_id: HashMap<String, ThemeDefinition> = HashMap::new();
        let mut diagnostics = Vec::new();

        for (label, content) in BUILTIN_THEMES {
            match parse_theme_toml(content) {
                Ok(theme) => {
                    by_id.insert(theme.id.clone(), theme);
                }
                Err(error) => {
                    debug_assert!(false, "built-in theme {label} failed to parse: {error}");
                }
            }
        }

        for dir in discovery_directories(workspace) {
            merge_directory(&mut by_id, &dir, &mut diagnostics);
        }

        let mut themes: Vec<ThemeDefinition> = by_id.into_values().collect();
        themes.sort_by(|a, b| a.name.cmp(&b.name));
        (Self { themes }, diagnostics)
    }

    #[allow(dead_code)] // public registry surface for theme authors and tooling
    pub fn themes(&self) -> &[ThemeDefinition] {
        &self.themes
    }

    pub fn contains(&self, id: &str) -> bool {
        self.themes.iter().any(|t| t.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&ThemeDefinition> {
        self.themes.iter().find(|theme| theme.id == id)
    }

    #[allow(dead_code)] // public registry surface for theme authors and tooling
    pub fn palette(&self, id: &str) -> Option<&ThemePalette> {
        self.get(id).map(|theme| &theme.palette)
    }

    pub fn display_name(&self, id: &str) -> String {
        self.get(id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn resolve_startup_id(&self, preference: &str) -> String {
        let id = forge_config::normalize_theme_id(preference);
        if self.contains(&id) {
            id
        } else {
            DEFAULT_THEME_ID.to_string()
        }
    }
}

/// Picker list of installed themes.
pub fn picker_entries(registry: &ThemeRegistry) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = registry
        .themes
        .iter()
        .map(|theme| (theme.id.clone(), theme.name.clone()))
        .collect();
    items.sort_by(|a, b| a.1.cmp(&b.1));
    items
}

fn discovery_directories(workspace: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(workspace) = workspace {
        let local = workspace.join(".forge").join("themes");
        if local.is_dir() {
            dirs.push(local);
        }
    }
    if let Some(config) = dirs::config_dir() {
        let user = config.join("forge").join("themes");
        if user.is_dir() {
            dirs.push(user);
        }
    }
    dirs
}

fn merge_directory(
    by_id: &mut HashMap<String, ThemeDefinition>,
    dir: &Path,
    diagnostics: &mut Vec<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        match parse_theme_toml(&content) {
            Ok(theme) => {
                by_id.insert(theme.id.clone(), theme);
            }
            Err(error) => {
                diagnostics.push(format!("theme: skipped {} ({error})", path.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_config::{Rgb, THEME_SOLARIZED_DARK, THEME_SOLARIZED_LIGHT};

    #[test]
    fn builtins_include_solarized_dark_and_light() {
        let registry = ThemeRegistry::load(None);
        let dark = registry.get(THEME_SOLARIZED_DARK).expect("solarized-dark");
        assert_eq!(dark.name, "Solarized Dark");
        assert_eq!(dark.palette.background, Rgb(0, 43, 54));
        let light = registry
            .get(THEME_SOLARIZED_LIGHT)
            .expect("solarized-light");
        assert_eq!(light.name, "Solarized Light");
    }

    #[test]
    fn workspace_drop_in_overrides_builtin_with_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("solarized-dark.toml"),
            include_str!("../themes/solarized-dark.toml").replace(
                "name = \"Solarized Dark\"",
                "name = \"Solarized Dark (Custom)\"",
            ),
        )
        .unwrap();
        let registry = ThemeRegistry::load(Some(dir.path()));
        assert_eq!(
            registry.get(THEME_SOLARIZED_DARK).unwrap().name,
            "Solarized Dark (Custom)"
        );
    }

    #[test]
    fn invalid_drop_in_is_skipped_but_reported() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("broken.toml"),
            include_str!("../themes/solarized-dark.toml")
                .replace("id = \"solarized-dark\"", "id = \"broken\"")
                // Truncated hex value: 5 digits instead of 6.
                .replace(
                    "user_gutter_active = \"#58A2D3\"",
                    "user_gutter_active = \"#58A2D\"",
                ),
        )
        .unwrap();
        let (registry, diagnostics) = ThemeRegistry::load_with_diagnostics(Some(dir.path()));
        assert!(!registry.contains("broken"));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("broken.toml"));
    }

    #[test]
    fn resolve_startup_falls_back_to_default_for_unknown() {
        let registry = ThemeRegistry::load(None);
        assert_eq!(
            registry.resolve_startup_id("no-such-theme"),
            DEFAULT_THEME_ID
        );
        assert_eq!(
            registry.resolve_startup_id(THEME_SOLARIZED_LIGHT),
            THEME_SOLARIZED_LIGHT
        );
    }
}
