//! TUI colour palettes loaded from `.toml` theme files.

use serde::Deserialize;
use std::fmt;

/// Legacy theme id retained for configuration compatibility.
pub const THEME_SYSTEM: &str = "system";
/// Built-in Solarized Dark theme id.
pub const THEME_SOLARIZED_DARK: &str = "solarized-dark";
/// Built-in Forge Dark theme id.
pub const THEME_FORGE_DARK: &str = "forge-dark";
/// Built-in Solarized Light theme id.
pub const THEME_SOLARIZED_LIGHT: &str = "solarized-light";
/// Default when no preference is stored.
pub const DEFAULT_THEME_ID: &str = THEME_FORGE_DARK;

/// RGB triplet used in theme files (no terminal dependency).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }

    /// `(hue degrees 0-360, saturation %, lightness %)`.
    ///
    /// Achromatic colours report hue 0; check saturation before trusting a
    /// hue, since every grey is nominally "red".
    pub fn to_hsl(self) -> (f64, f64, f64) {
        let (r, g, b) = (
            self.0 as f64 / 255.0,
            self.1 as f64 / 255.0,
            self.2 as f64 / 255.0,
        );
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let lightness = (max + min) / 2.0;
        let delta = max - min;
        if delta == 0.0 {
            return (0.0, 0.0, lightness * 100.0);
        }
        let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
        let hue = if max == r {
            60.0 * (((g - b) / delta) % 6.0)
        } else if max == g {
            60.0 * (((b - r) / delta) + 2.0)
        } else {
            60.0 * (((r - g) / delta) + 4.0)
        };
        ((hue + 360.0) % 360.0, saturation * 100.0, lightness * 100.0)
    }

    /// Shortest distance between two hues, in degrees (0-180).
    pub fn hue_distance(self, other: Self) -> f64 {
        let raw = (self.to_hsl().0 - other.to_hsl().0).abs() % 360.0;
        raw.min(360.0 - raw)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_hex_color(&s).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

/// Parse `#RRGGBB` or `RRGGBB` hex colours.
pub fn parse_hex_color(s: &str) -> Result<Rgb, String> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("expected #RRGGBB hex colour, got `{s}`"));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
    Ok(Rgb(r, g, b))
}

/// Normalize user/config theme ids and accept legacy `dark` / `light` aliases.
pub fn normalize_theme_id(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => DEFAULT_THEME_ID.to_string(),
        "dark" => THEME_FORGE_DARK.to_string(),
        "light" => THEME_SOLARIZED_LIGHT.to_string(),
        "system" => THEME_SYSTEM.to_string(),
        other => other.to_string(),
    }
}

pub fn is_system_theme(id: &str) -> bool {
    id.eq_ignore_ascii_case(THEME_SYSTEM)
}

/// Parse an optional `[tui] theme` preference from `forge.toml`.
///
/// Accepts legacy `dark` / `light` / `system` aliases and built-in theme ids.
/// Unknown values are ignored so a typo does not change the default theme.
pub fn parse_theme_preference(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "dark" | "forge-dark" => Some(THEME_FORGE_DARK.to_string()),
        "solarized-dark" => Some(THEME_SOLARIZED_DARK.to_string()),
        "light" | "solarized-light" => Some(THEME_SOLARIZED_LIGHT.to_string()),
        "system" => Some(THEME_SYSTEM.to_string()),
        _ => None,
    }
}

/// Tree-sitter syntax colours for one theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxPalette {
    pub comment: Rgb,
    pub keyword: Rgb,
    pub string: Rgb,
    pub number: Rgb,
    pub function: Rgb,
    pub type_: Rgb,
    pub variable: Rgb,
    pub operator: Rgb,
    pub punctuation: Rgb,
    pub property: Rgb,
    pub tag: Rgb,
    pub attribute: Rgb,
    pub default: Rgb,
}

/// Semantic tokens for one TUI theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    pub background: Rgb,
    pub background_deep: Rgb,
    pub surface: Rgb,
    pub surface_raised: Rgb,
    pub surface_hover: Rgb,
    pub border: Rgb,
    pub border_muted: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub text_muted: Rgb,
    pub accent: Rgb,
    pub accent_soft: Rgb,
    pub agent: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub info: Rgb,
    pub diff_add: Rgb,
    pub diff_remove: Rgb,
    pub selection: Rgb,
    pub cursor: Rgb,
    pub tag: Rgb,
    pub search_match: Rgb,
    /// Border for the composer while an approval is pending ("paused" look).
    pub waiting_border: Rgb,
    pub syntax: SyntaxPalette,
}

/// Minimum hue separation between a theme's `accent` and any hue that
/// reports an outcome.
///
/// The accent answers "where am I and what will my next keystroke touch";
/// `success`/`warning`/`error` answer "what happened to that thing". A theme
/// that renders both in the same hue cannot say both at once — the focused
/// border, the caret and the selected row start reading as status.
///
/// 60 is calibrated rather than chosen: it is the threshold that separates
/// the built-in themes whose accent is legible against their status colours
/// from the ones where it is not. `info` and `agent` are deliberately not
/// included — neither reports an outcome, so both belong to the accent's own
/// family and are expected to sit near it.
pub const ACCENT_STATUS_MIN_HUE_DISTANCE: f64 = 60.0;

impl ThemePalette {
    /// The outcome hue closest to `accent`, as `(role, distance in degrees)`.
    pub fn nearest_status_hue(&self) -> (&'static str, f64) {
        [
            ("success", self.success),
            ("warning", self.warning),
            ("error", self.error),
        ]
        .into_iter()
        .map(|(role, colour)| (role, self.accent.hue_distance(colour)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("three status roles")
    }

    /// `Some((role, distance))` when `accent` sits too close to an outcome
    /// hue to be told apart from it.
    pub fn accent_status_collision(&self) -> Option<(&'static str, f64)> {
        let (role, distance) = self.nearest_status_hue();
        (distance < ACCENT_STATUS_MIN_HUE_DISTANCE).then_some((role, distance))
    }
}

/// One installable theme (metadata + palette).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeDefinition {
    pub id: String,
    pub name: String,
    pub palette: ThemePalette,
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    id: String,
    name: String,
    background: Rgb,
    background_deep: Rgb,
    surface: Rgb,
    surface_raised: Rgb,
    surface_hover: Rgb,
    border: Rgb,
    border_muted: Rgb,
    text_primary: Rgb,
    text_secondary: Rgb,
    text_muted: Rgb,
    accent: Rgb,
    accent_soft: Rgb,
    agent: Rgb,
    success: Rgb,
    warning: Rgb,
    error: Rgb,
    info: Rgb,
    diff_add: Rgb,
    diff_remove: Rgb,
    selection: Rgb,
    cursor: Rgb,
    tag: Rgb,
    search_match: Rgb,
    /// Optional: falls back to the theme's `warning` when absent, so existing
    /// user theme drops without this key keep parsing.
    #[serde(default)]
    waiting_border: Option<Rgb>,
    syntax: ThemeFileSyntax,
}

#[derive(Debug, Deserialize)]
struct ThemeFileSyntax {
    comment: Rgb,
    keyword: Rgb,
    string: Rgb,
    number: Rgb,
    function: Rgb,
    #[serde(rename = "type")]
    type_: Rgb,
    variable: Rgb,
    operator: Rgb,
    punctuation: Rgb,
    property: Rgb,
    tag: Rgb,
    attribute: Rgb,
    default: Rgb,
}

impl From<ThemeFile> for ThemeDefinition {
    fn from(file: ThemeFile) -> Self {
        Self {
            id: file.id,
            name: file.name,
            palette: ThemePalette {
                background: file.background,
                background_deep: file.background_deep,
                surface: file.surface,
                surface_raised: file.surface_raised,
                surface_hover: file.surface_hover,
                border: file.border,
                border_muted: file.border_muted,
                text_primary: file.text_primary,
                text_secondary: file.text_secondary,
                text_muted: file.text_muted,
                accent: file.accent,
                accent_soft: file.accent_soft,
                agent: file.agent,
                success: file.success,
                warning: file.warning,
                error: file.error,
                info: file.info,
                diff_add: file.diff_add,
                diff_remove: file.diff_remove,
                selection: file.selection,
                cursor: file.cursor,
                tag: file.tag,
                search_match: file.search_match,
                waiting_border: file.waiting_border.unwrap_or(file.warning),
                syntax: SyntaxPalette {
                    comment: file.syntax.comment,
                    keyword: file.syntax.keyword,
                    string: file.syntax.string,
                    number: file.syntax.number,
                    function: file.syntax.function,
                    type_: file.syntax.type_,
                    variable: file.syntax.variable,
                    operator: file.syntax.operator,
                    punctuation: file.syntax.punctuation,
                    property: file.syntax.property,
                    tag: file.syntax.tag,
                    attribute: file.syntax.attribute,
                    default: file.syntax.default,
                },
            },
        }
    }
}

/// Parse a theme definition from TOML (see `crates/forge-tui/themes/` for examples).
pub fn parse_theme_toml(content: &str) -> Result<ThemeDefinition, String> {
    let file: ThemeFile = toml::from_str(content).map_err(|e| e.to_string())?;
    if file.id.trim().is_empty() {
        return Err("theme id must not be empty".into());
    }
    if file.name.trim().is_empty() {
        return Err("theme name must not be empty".into());
    }
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_THEME: &str = r##"
id = "sample"
name = "Sample"

background = "#0D1117"
background_deep = "#090C11"
surface = "#131922"
surface_raised = "#1A2230"
surface_hover = "#202A39"
border = "#2B3545"
border_muted = "#202938"
text_primary = "#E6EDF3"
text_secondary = "#9DAABD"
text_muted = "#8594A8"
accent = "#68A8FF"
accent_soft = "#1C3555"
agent = "#B49CFF"
success = "#56D364"
warning = "#E3B341"
error = "#FF7B72"
info = "#56D4DD"
diff_add = "#244A32"
diff_remove = "#542B31"
selection = "#29486F"
cursor = "#F0F6FC"
# Retired key, deliberately left in this fixture: user themes in the wild
# still carry it, and they must keep parsing. See
# `retired_keys_are_ignored_so_existing_user_themes_keep_parsing`.
user_gutter_active = "#8AC0FF"
tag = "#C0C6D0"
search_match = "#334257"

[syntax]
comment = "#9DAABD"
keyword = "#68A8FF"
string = "#E3B341"
number = "#68A8FF"
function = "#B49CFF"
type = "#68A8FF"
variable = "#E6EDF3"
operator = "#9DAABD"
punctuation = "#9DAABD"
property = "#56D4DD"
tag = "#56D4DD"
attribute = "#E3B341"
default = "#E6EDF3"
"##;

    #[test]
    fn parse_hex_accepts_hash_prefix() {
        assert_eq!(parse_hex_color("#0D1117").unwrap(), Rgb(13, 17, 23));
        assert_eq!(parse_hex_color("0D1117").unwrap(), Rgb(13, 17, 23));
    }

    #[test]
    fn parse_hex_rejects_invalid() {
        assert!(parse_hex_color("blue").is_err());
        assert!(parse_hex_color("#ABC").is_err());
    }

    #[test]
    fn normalize_theme_id_maps_legacy_aliases() {
        assert_eq!(normalize_theme_id("dark"), THEME_FORGE_DARK);
        assert_eq!(normalize_theme_id("light"), THEME_SOLARIZED_LIGHT);
        assert_eq!(normalize_theme_id("system"), THEME_SYSTEM);
        assert_eq!(normalize_theme_id("custom"), "custom");
    }

    #[test]
    fn forge_dark_is_the_default_and_configurable() {
        assert_eq!(DEFAULT_THEME_ID, THEME_FORGE_DARK);
        assert_eq!(
            parse_theme_preference("forge-dark"),
            Some(THEME_FORGE_DARK.to_string())
        );
        assert_eq!(
            parse_theme_preference("solarized-dark"),
            Some(THEME_SOLARIZED_DARK.to_string())
        );
    }

    #[test]
    fn parse_theme_toml_round_trip() {
        let theme = parse_theme_toml(SAMPLE_THEME).unwrap();
        assert_eq!(theme.id, "sample");
        assert_eq!(theme.name, "Sample");
        assert_eq!(theme.palette.accent, Rgb(104, 168, 255));
    }

    /// `user_gutter_active` and `approval_accent` were retired: the first
    /// was parsed but never mapped into the runtime palette, and the second
    /// only existed to work around an accent that matched `success`.
    ///
    /// `ThemeFile` does not set `deny_unknown_fields`, so a drop-in theme
    /// that still lists them parses with the keys ignored. Dropping a theme
    /// out of the picker because it names a token forge no longer has would
    /// be a silent failure — `merge_directory` skips files it cannot parse.
    #[test]
    fn retired_keys_are_ignored_so_existing_user_themes_keep_parsing() {
        // SAMPLE_THEME still carries `user_gutter_active`.
        assert!(SAMPLE_THEME.contains("user_gutter_active"));
        let legacy = SAMPLE_THEME.replace(
            "search_match = \"#334257\"",
            "search_match = \"#334257\"\napproval_accent = \"#68A8FF\"",
        );
        let theme = parse_theme_toml(&legacy).expect("legacy theme should still parse");
        assert_eq!(theme.palette.accent, Rgb(104, 168, 255));
    }

    /// The reverse: a theme omitting the retired keys is now valid, where
    /// `user_gutter_active` used to be a required field.
    #[test]
    fn themes_without_the_retired_keys_parse() {
        let modern = SAMPLE_THEME
            .lines()
            .filter(|line| !line.starts_with("user_gutter_active"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!modern.contains("user_gutter_active = "));
        let theme = parse_theme_toml(&modern).expect("theme without retired keys should parse");
        assert_eq!(theme.palette.cursor, Rgb(240, 246, 252));
    }
}
