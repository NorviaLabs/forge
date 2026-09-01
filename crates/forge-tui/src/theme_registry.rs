//! Discover and load TUI themes from bundled files and optional directories.

use forge_config::{
    is_system_theme, parse_theme_toml, ThemeDefinition, ThemePalette,
    ACCENT_STATUS_MIN_HUE_DISTANCE, DEFAULT_THEME_ID, THEME_SYSTEM,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/builtin_themes.rs"));

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
        let mut by_id = builtin_definitions();
        let mut diagnostics = Vec::new();

        for dir in discovery_directories(workspace) {
            merge_directory(&mut by_id, &dir, &mut diagnostics);
        }

        (Self::from_definitions(by_id), diagnostics)
    }

    /// Built-in themes only, skipping user and workspace discovery.
    ///
    /// Palette invariants are asserted against this set rather than
    /// [`ThemeRegistry::load`]: discovery reads `~/.config/forge/themes`, so
    /// testing against the loaded registry would let a drop-in theme on a
    /// contributor's machine fail forge's own suite.
    #[allow(dead_code)] // public registry surface for theme authors and tooling
    pub fn builtin() -> Self {
        Self::from_definitions(builtin_definitions())
    }

    fn from_definitions(by_id: HashMap<String, ThemeDefinition>) -> Self {
        let mut themes: Vec<ThemeDefinition> = by_id.into_values().collect();
        themes.sort_by(|a, b| a.name.cmp(&b.name));
        Self { themes }
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
        if is_system_theme(&id) || self.contains(&id) {
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
    items.push((THEME_SYSTEM.to_string(), "System".to_string()));
    items.sort_by(|a, b| match (a.0 == THEME_SYSTEM, b.0 == THEME_SYSTEM) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.1.cmp(&b.1),
    });
    items
}

fn builtin_definitions() -> HashMap<String, ThemeDefinition> {
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
    by_id
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
                // Warn, don't reject: the palette is the author's call, and
                // a theme that loads with a note is far easier to fix than
                // one that silently vanishes from the picker.
                if let Some((role, distance)) = theme.palette.accent_status_collision() {
                    diagnostics.push(format!(
                        "theme: {} accent {} is only {distance:.0}° from {role} \
                         (want {ACCENT_STATUS_MIN_HUE_DISTANCE:.0}°) — the focused \
                         border, caret and selected row will read as {role}",
                        theme.id, theme.palette.accent,
                    ));
                }
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
    use forge_config::{Rgb, THEME_FORGE_DARK, THEME_FORGE_LIGHT};

    #[test]
    fn builtins_include_all_shipped_themes() {
        let registry = ThemeRegistry::load(None);
        let expected = [
            (THEME_FORGE_DARK, "Forge Dark", Rgb(11, 15, 13)),
            (THEME_FORGE_LIGHT, "Forge Light", Rgb(242, 245, 239)),
        ];
        for (id, name, background) in expected {
            let theme = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(theme.name, name);
            assert_eq!(theme.palette.background, background);
        }
        assert_eq!(registry.themes().len(), expected.len());
    }

    #[test]
    fn workspace_drop_in_overrides_builtin_with_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("forge-dark.toml"),
            include_str!("../themes/forge-dark.toml")
                .replace("name = \"Forge Dark\"", "name = \"Forge Dark (Custom)\""),
        )
        .unwrap();
        let registry = ThemeRegistry::load(Some(dir.path()));
        assert_eq!(
            registry.get(THEME_FORGE_DARK).unwrap().name,
            "Forge Dark (Custom)"
        );
    }

    #[test]
    fn invalid_drop_in_is_skipped_but_reported() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("broken.toml"),
            include_str!("../themes/forge-dark.toml")
                .replace("id = \"forge-dark\"", "id = \"broken\"")
                // Truncated hex value: 5 digits instead of 6.
                .replace("cursor = \"#439EFD\"", "cursor = \"#439E\""),
        )
        .unwrap();
        let (registry, diagnostics) = ThemeRegistry::load_with_diagnostics(Some(dir.path()));
        assert!(!registry.contains("broken"));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("broken.toml"));
    }

    /// The Δ60 rule is asserted over built-ins at test time, but a drop-in
    /// theme has to reach its author somehow. It warns rather than rejects:
    /// the palette is the author's call, and a theme that vanishes from the
    /// picker is far harder to debug than one that loads with a note.
    #[test]
    fn drop_in_theme_with_a_colliding_accent_loads_but_warns() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("collides.toml"),
            include_str!("../themes/forge-dark.toml")
                .replace("id = \"forge-dark\"", "id = \"collides\"")
                // Point the accent straight at `success`.
                .replace("accent = \"#439EFD\"", "accent = \"#5BDB87\""),
        )
        .unwrap();

        let (registry, diagnostics) = ThemeRegistry::load_with_diagnostics(Some(dir.path()));

        assert!(
            registry.contains("collides"),
            "a colliding theme must still load"
        );
        let notice = diagnostics
            .iter()
            .find(|d| d.contains("collides"))
            .expect("expected a diagnostic for the colliding accent");
        assert!(notice.contains("success"), "{notice}");
        assert!(notice.contains("0°"), "{notice}");
    }

    #[test]
    fn compliant_drop_in_theme_loads_without_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("fine.toml"),
            include_str!("../themes/forge-dark.toml")
                .replace("id = \"forge-dark\"", "id = \"fine\""),
        )
        .unwrap();

        let (registry, diagnostics) = ThemeRegistry::load_with_diagnostics(Some(dir.path()));

        assert!(registry.contains("fine"));
        assert!(
            diagnostics.is_empty(),
            "a compliant theme should load silently: {diagnostics:?}"
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
            registry.resolve_startup_id(THEME_FORGE_LIGHT),
            THEME_FORGE_LIGHT
        );
    }

    #[test]
    fn picker_includes_system_and_startup_accepts_it() {
        let registry = ThemeRegistry::load(None);
        assert!(picker_entries(&registry)
            .iter()
            .any(|(id, name)| id == "system" && name == "System"));
        assert_eq!(registry.resolve_startup_id("system"), "system");
    }
}
