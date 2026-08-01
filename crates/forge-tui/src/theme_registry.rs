//! Discover and load TUI themes from bundled files and optional directories.

use forge_config::{
    parse_theme_toml, ThemeDefinition, ThemePalette, DEFAULT_THEME_ID, THEME_SYSTEM,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const BUILTIN_THEMES: &[(&str, &str)] = &[
    (
        "forge-midnight.toml",
        include_str!("../themes/forge-midnight.toml"),
    ),
    (
        "forge-daylight.toml",
        include_str!("../themes/forge-daylight.toml"),
    ),
];

/// Virtual entry shown in the theme picker for ANSI terminal colours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemThemeEntry {
    pub id: &'static str,
    pub name: &'static str,
}

impl SystemThemeEntry {
    pub const SYSTEM: Self = Self {
        id: THEME_SYSTEM,
        name: "System (ANSI)",
    };
}

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
    pub fn load(workspace: Option<&Path>) -> Self {
        let mut by_id: HashMap<String, ThemeDefinition> = HashMap::new();

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
            merge_directory(&mut by_id, &dir);
        }

        let mut themes: Vec<ThemeDefinition> = by_id.into_values().collect();
        themes.sort_by(|a, b| a.name.cmp(&b.name));
        Self { themes }
    }

    #[allow(dead_code)] // public registry surface for theme authors and tooling
    pub fn themes(&self) -> &[ThemeDefinition] {
        &self.themes
    }

    pub fn contains(&self, id: &str) -> bool {
        forge_config::is_system_theme(id) || self.themes.iter().any(|t| t.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&ThemeDefinition> {
        self.themes.iter().find(|theme| theme.id == id)
    }

    #[allow(dead_code)] // public registry surface for theme authors and tooling
    pub fn palette(&self, id: &str) -> Option<&ThemePalette> {
        self.get(id).map(|theme| &theme.palette)
    }

    pub fn display_name(&self, id: &str) -> String {
        if forge_config::is_system_theme(id) {
            return SystemThemeEntry::SYSTEM.name.to_string();
        }
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

/// Picker list: installed themes plus the virtual system entry.
pub fn picker_entries(registry: &ThemeRegistry) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = registry
        .themes
        .iter()
        .map(|theme| (theme.id.clone(), theme.name.clone()))
        .collect();
    items.push((
        SystemThemeEntry::SYSTEM.id.to_string(),
        SystemThemeEntry::SYSTEM.name.to_string(),
    ));
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

fn merge_directory(by_id: &mut HashMap<String, ThemeDefinition>, dir: &Path) {
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
            Err(_) => {
                // Skip invalid drop-in files; authors can fix their theme.toml.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_config::{Rgb, THEME_FORGE_DAYLIGHT, THEME_FORGE_MIDNIGHT};

    #[test]
    fn builtins_include_forge_midnight_and_daylight() {
        let registry = ThemeRegistry::load(None);
        let midnight = registry.get(THEME_FORGE_MIDNIGHT).expect("forge-midnight");
        assert_eq!(midnight.name, "Forge Midnight");
        assert_eq!(midnight.palette.background, Rgb(13, 17, 23));
        let daylight = registry.get(THEME_FORGE_DAYLIGHT).expect("forge-daylight");
        assert_eq!(daylight.name, "Forge Daylight");
    }

    #[test]
    fn workspace_drop_in_overrides_builtin_with_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("forge-midnight.toml"),
            include_str!("../themes/forge-midnight.toml").replace(
                "name = \"Forge Midnight\"",
                "name = \"Forge Midnight (Custom)\"",
            ),
        )
        .unwrap();
        let registry = ThemeRegistry::load(Some(dir.path()));
        assert_eq!(
            registry.get(THEME_FORGE_MIDNIGHT).unwrap().name,
            "Forge Midnight (Custom)"
        );
    }

    #[test]
    fn resolve_startup_falls_back_to_default_for_unknown() {
        let registry = ThemeRegistry::load(None);
        assert_eq!(
            registry.resolve_startup_id("no-such-theme"),
            DEFAULT_THEME_ID
        );
        assert_eq!(
            registry.resolve_startup_id(THEME_FORGE_DAYLIGHT),
            THEME_FORGE_DAYLIGHT
        );
    }
}
