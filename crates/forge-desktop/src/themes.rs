//! Installed themes, shared with the terminal frontend.
//!
//! The desktop renders HTML, not terminal cells, so it cannot use the TUI's
//! palette directly. It maps the same semantic tokens onto CSS custom
//! properties, which keeps a drop-in `~/.config/forge/themes/*.toml` working
//! in both frontends.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_config::{parse_theme_toml, Rgb, ThemeDefinition, ThemePalette};
use serde::Serialize;

const BUILTIN: &[(&str, &str)] = &[
    ("forge-dark", include_str!("../themes/forge-dark.toml")),
    ("forge-light", include_str!("../themes/forge-light.toml")),
];

/// One theme the picker can install. `system` is resolved in the page from
/// the OS preference, because only the webview can observe it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ThemeView {
    pub id: String,
    pub name: String,
    /// Background is dark; the page uses this for `color-scheme` and the
    /// terminal's own defaults.
    pub dark: bool,
    /// CSS custom property names (without the leading `--`) to `#RRGGBB`.
    pub colors: BTreeMap<&'static str, String>,
}

pub fn list(workspace: Option<&Path>) -> Vec<ThemeView> {
    let mut by_id: BTreeMap<String, ThemeDefinition> = BTreeMap::new();
    for (label, content) in BUILTIN {
        match parse_theme_toml(content) {
            Ok(theme) => {
                by_id.insert(theme.id.clone(), theme);
            }
            Err(error) => debug_assert!(false, "built-in theme {label} failed to parse: {error}"),
        }
    }
    for dir in discovery_directories(workspace) {
        merge_directory(&mut by_id, &dir);
    }
    let mut themes: Vec<ThemeView> = by_id.into_values().map(view).collect();
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

fn view(theme: ThemeDefinition) -> ThemeView {
    ThemeView {
        dark: theme.palette.background.to_hsl().2 < 50.0,
        id: theme.id,
        name: theme.name,
        colors: colors(&theme.palette),
    }
}

fn colors(palette: &ThemePalette) -> BTreeMap<&'static str, String> {
    // Desktop tokens are deliberately coarser than the TUI's; map each one
    // to the token it already stood for in app.css.
    [
        ("bg", palette.background),
        ("bg-deep", palette.background_deep),
        ("surface", palette.surface),
        ("raised", palette.surface_raised),
        ("hover", palette.surface_hover),
        ("border", palette.border_muted),
        ("border-strong", palette.border),
        ("text", palette.text_primary),
        ("secondary", palette.text_secondary),
        ("muted", palette.text_muted),
        ("accent", palette.accent),
        ("accent-text", accent_text(palette.accent)),
        ("accent-soft", palette.accent_soft),
        ("success", palette.success),
        ("warning", palette.warning),
        ("error", palette.error),
        ("add", palette.diff_add),
        ("remove", palette.diff_remove),
        ("selection", palette.selection),
        ("strong", palette.md_strong),
    ]
    .into_iter()
    .map(|(name, color): (&'static str, Rgb)| (name, color.to_string()))
    .collect()
}

fn accent_text(Rgb(r, g, b): Rgb) -> Rgb {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    if luminance > 0.179 {
        Rgb(0, 0, 0)
    } else {
        Rgb(255, 255, 255)
    }
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

fn merge_directory(by_id: &mut BTreeMap<String, ThemeDefinition>, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(theme) = parse_theme_toml(&content) {
            by_id.insert(theme.id.clone(), theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-desktop-themes-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builtins_load_with_desktop_colors() {
        let themes = list(None);
        let dark = themes
            .iter()
            .find(|theme| theme.id == "forge-dark")
            .expect("forge-dark");
        assert!(dark.dark);
        assert_eq!(dark.colors["bg"], "#14171B");
        assert_eq!(dark.colors["border"], "#2A333E");
        assert_eq!(dark.colors["border-strong"], "#65778C");

        let light = themes
            .iter()
            .find(|theme| theme.id == "forge-light")
            .expect("forge-light");
        assert!(!light.dark);
    }

    fn contrast(a: &str, b: &str) -> f64 {
        fn luminance(hex: &str) -> f64 {
            let hex = hex.trim_start_matches('#');
            let channel = |index: usize| {
                let value = u8::from_str_radix(&hex[index..index + 2], 16).unwrap();
                let value = f64::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
        }
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The desktop's mapped tokens must keep the same AA floor as the TUI:
    /// 4.5:1 for text and 3:1 for the control border.
    #[test]
    fn builtin_desktop_tokens_meet_wcag_aa() {
        let themes = list(None);
        let grounds = ["bg", "bg-deep", "surface", "raised"];
        let text = [
            "text",
            "secondary",
            "muted",
            "accent",
            "success",
            "warning",
            "error",
            "strong",
        ];
        for id in ["forge-dark", "forge-light"] {
            let colors = &themes
                .iter()
                .find(|theme| theme.id == id)
                .unwrap_or_else(|| panic!("{id} missing"))
                .colors;
            for fg in text {
                for bg in grounds {
                    let ratio = contrast(&colors[fg], &colors[bg]);
                    assert!(ratio >= 4.5, "{id}: {fg} on {bg} = {ratio:.2} < 4.5");
                }
            }
            for bg in grounds {
                let ratio = contrast(&colors["border-strong"], &colors[bg]);
                assert!(
                    ratio >= 3.0,
                    "{id}: border-strong on {bg} = {ratio:.2} < 3.0"
                );
            }
        }
    }

    #[test]
    fn primary_labels_contrast_with_custom_accents() {
        for accent in [
            Rgb(0, 0, 0),
            Rgb(255, 255, 255),
            Rgb(128, 128, 128),
            Rgb(255, 0, 0),
            Rgb(0, 0, 255),
        ] {
            let foreground = accent_text(accent);
            assert!(contrast(&foreground.to_string(), &accent.to_string()) >= 4.5);
        }
        for theme in list(None) {
            assert!(contrast(&theme.colors["accent-text"], &theme.colors["accent"]) >= 4.5);
        }
    }

    #[test]
    fn a_workspace_drop_in_overrides_the_builtin() {
        let dir = tempdir("override");
        let themes = dir.join(".forge").join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("forge-dark.toml"),
            include_str!("../themes/forge-dark.toml")
                .replace("name = \"Forge Dark\"", "name = \"Forge Dark (Custom)\""),
        )
        .unwrap();

        let loaded = list(Some(&dir));
        let dark = loaded
            .iter()
            .find(|theme| theme.id == "forge-dark")
            .unwrap();
        assert_eq!(dark.name, "Forge Dark (Custom)");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_invalid_drop_in_is_skipped() {
        let dir = tempdir("invalid");
        let themes = dir.join(".forge").join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("broken.toml"), "id = \"broken\"").unwrap();

        let loaded = list(Some(&dir));
        assert!(!loaded.iter().any(|theme| theme.id == "broken"));
        assert!(loaded.iter().any(|theme| theme.id == "forge-dark"));
        std::fs::remove_dir_all(dir).ok();
    }
}
