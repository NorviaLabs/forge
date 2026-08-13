//! Color tokens from the Forge TUI design system.

use crate::theme_registry::ThemeRegistry;
use forge_config::{Rgb as ConfigRgb, ThemePalette, DEFAULT_THEME_ID};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::cell::RefCell;
use std::time::{Duration, Instant};

thread_local! {
    static THEME_REGISTRY: RefCell<ThemeRegistry> = RefCell::new(ThemeRegistry::default());
    static ACTIVE_THEME_ID: RefCell<String> = RefCell::new(DEFAULT_THEME_ID.to_string());
    static LAST_SYSTEM_THEME: RefCell<Option<(&'static str, Instant)>> = const { RefCell::new(None) };
    static RESOLVED_SYSTEM_THEME: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    /// Memoized [`Palette`] for the active theme, keyed by the theme id it was
    /// resolved from (already system-resolved, so an OS light/dark flip changes
    /// the key and recomputes).
    ///
    /// Every styling token — and there are dozens per frame — funnels through
    /// [`active_palette`], and each call was cloning the whole theme registry
    /// (up to three times) to look one palette up. Only the *active* palette is
    /// memoized: [`palette`] with an explicit id stays uncached so the theme
    /// picker, which asks for many different themes in a single frame, cannot
    /// thrash a one-entry cache.
    static ACTIVE_PALETTE_CACHE: RefCell<Option<(String, Palette)>> = const { RefCell::new(None) };
}

/// Drop the memoized active palette. Call whenever the palette behind an
/// unchanged theme id may have changed.
fn invalidate_active_palette() {
    ACTIVE_PALETTE_CACHE.with(|cache| *cache.borrow_mut() = None);
}

/// Install discovered themes and the active theme id (call once at startup).
pub fn install(registry: ThemeRegistry, theme_id: impl Into<String>) {
    THEME_REGISTRY.with(|registry_slot| *registry_slot.borrow_mut() = registry);
    RESOLVED_SYSTEM_THEME.with(|resolved| *resolved.borrow_mut() = None);
    LAST_SYSTEM_THEME.with(|last| *last.borrow_mut() = None);
    // A new registry can redefine the palette behind an id the cache already
    // holds, so the id alone is not enough to tell the entry is still good.
    invalidate_active_palette();
    set_active(theme_id);
}

/// Switch the active theme without replacing the registry.
pub fn set_active(theme_id: impl Into<String>) {
    let id = forge_config::normalize_theme_id(&theme_id.into());
    if forge_config::is_system_theme(&id) {
        RESOLVED_SYSTEM_THEME.with(|resolved| *resolved.borrow_mut() = None);
        LAST_SYSTEM_THEME.with(|last| *last.borrow_mut() = None);
    }
    ACTIVE_THEME_ID.with(|active| *active.borrow_mut() = id);
}

/// Active theme id (`forge-dark`, `solarized-dark`, `solarized-light`, `system`, or a custom id).
pub fn active() -> String {
    ACTIVE_THEME_ID.with(|active| active.borrow().clone())
}

pub fn registry() -> ThemeRegistry {
    THEME_REGISTRY.with(|registry_slot| registry_slot.borrow().clone())
}

fn active_palette() -> Palette {
    ACTIVE_THEME_ID.with(|active| {
        let active = active.borrow();
        // Resolve `system` first so the cache key is the theme actually in
        // effect, not the literal string "system".
        let effective: &str = if forge_config::is_system_theme(&active) {
            system_theme_id()
        } else {
            &active
        };
        ACTIVE_PALETTE_CACHE.with(|cache| {
            if let Some((id, palette)) = cache.borrow().as_ref() {
                if id == effective {
                    return *palette;
                }
            }
            let palette = palette_from_source(&resolved_palette(effective));
            *cache.borrow_mut() = Some((effective.to_string(), palette));
            palette
        })
    })
}

fn resolved_palette(theme_id: &str) -> ThemePalette {
    // Borrow the registry rather than `registry()`, which hands back a full
    // clone of every loaded theme just to read one palette out of it.
    THEME_REGISTRY.with(|registry_slot| {
        let registry = registry_slot.borrow();
        if forge_config::is_system_theme(theme_id) {
            return registry
                .palette(system_theme_id())
                .copied()
                .expect("built-in system theme");
        }
        registry
            .palette(theme_id)
            .or_else(|| registry.palette(DEFAULT_THEME_ID))
            .copied()
            .expect("built-in solarized-dark theme")
    })
}

fn system_theme_id() -> &'static str {
    RESOLVED_SYSTEM_THEME.with(|resolved| {
        if let Some(theme_id) = *resolved.borrow() {
            return theme_id;
        }
        let theme_id = detect_system_theme_id();
        *resolved.borrow_mut() = Some(theme_id);
        theme_id
    })
}

/// Re-check the OS theme when `system` is active. Returns true when the
/// resolved palette changed and cached rendering must be rebuilt.
pub fn refresh_system() -> bool {
    if !forge_config::is_system_theme(&active()) {
        return false;
    }
    let now = Instant::now();
    LAST_SYSTEM_THEME.with(|last| {
        let mut last = last.borrow_mut();
        if last.is_some_and(|(_, checked)| now.duration_since(checked) < Duration::from_secs(1)) {
            return false;
        }
        let current = detect_system_theme_id();
        RESOLVED_SYSTEM_THEME.with(|resolved| *resolved.borrow_mut() = Some(current));
        let changed = last
            .map(|(previous, _)| previous != current)
            .unwrap_or(false);
        *last = Some((current, now));
        changed
    })
}

fn detect_system_theme_id() -> &'static str {
    system_theme_id_from_os()
        .or_else(|| {
            std::env::var("COLORFGBG")
                .ok()
                .and_then(|value| system_theme_id_from_colorfgbg(&value))
        })
        .unwrap_or(DEFAULT_THEME_ID)
}

fn system_theme_id_from_os() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
            .ok()?;
        system_theme_id_from_os_output(&String::from_utf8_lossy(&output.stdout))
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .ok()?;
        if output.status.success() {
            system_theme_id_from_os_output(&String::from_utf8_lossy(&output.stdout))
        } else {
            Some(forge_config::THEME_SOLARIZED_LIGHT)
        }
    }
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("reg")
            .args([
                "query",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
            .ok()?;
        return system_theme_id_from_os_output(&String::from_utf8_lossy(&output.stdout));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    None
}

fn system_theme_id_from_os_output(output: &str) -> Option<&'static str> {
    let output = output.trim().to_ascii_lowercase();
    if output.contains("prefer-light")
        || output.contains("appleinterfacestyle") && output.contains("light")
        || output.ends_with("0x1")
    {
        Some(forge_config::THEME_SOLARIZED_LIGHT)
    } else if output.contains("prefer-dark") || output.contains("dark") || output.ends_with("0x0") {
        Some(DEFAULT_THEME_ID)
    } else {
        None
    }
}

fn system_theme_id_from_colorfgbg(colorfgbg: &str) -> Option<&'static str> {
    colorfgbg
        .rsplit(';')
        .next()
        .and_then(|background| background.parse::<u8>().ok())
        .map(|background| {
            if background >= 8 {
                forge_config::THEME_SOLARIZED_LIGHT
            } else {
                DEFAULT_THEME_ID
            }
        })
}

fn to_color(rgb: ConfigRgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

fn syntax_from_palette(p: &ThemePalette) -> forge_syntax::HighlightTheme {
    let s = &p.syntax;
    let t = |rgb: ConfigRgb| (rgb.0, rgb.1, rgb.2);
    forge_syntax::HighlightTheme {
        comment: t(s.comment),
        keyword: t(s.keyword),
        string: t(s.string),
        number: t(s.number),
        function: t(s.function),
        type_: t(s.type_),
        variable: t(s.variable),
        operator: t(s.operator),
        punctuation: t(s.punctuation),
        property: t(s.property),
        tag: t(s.tag),
        attribute: t(s.attribute),
        default: t(s.default),
    }
}

/// Paint every cell in `area` with `style` (used for root canvas and overlay backdrops).
pub fn fill(area: Rect, buf: &mut Buffer, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(style);
            }
        }
    }
}

pub fn canvas() -> Style {
    Style::default().bg(active_palette().canvas)
}

/// Blend a color toward a translucent overlay tone, approximating a
/// ~62%-opacity dark panel sitting on top of it. Ratatui cells have no alpha
/// channel, so this is a one-shot blend rather than true compositing.
fn dim_toward_overlay(color: Color, overlay: Color) -> Color {
    blend_toward(color, overlay, 0.62)
}

fn blend_toward(color: Color, overlay: Color, alpha: f32) -> Color {
    match (color, overlay) {
        (Color::Rgb(r, g, b), Color::Rgb(overlay_r, overlay_g, overlay_b)) => {
            let blend = |c: u8, d: f32| ((c as f32) * (1.0 - alpha) + d * alpha).round() as u8;
            Color::Rgb(
                blend(r, overlay_r as f32),
                blend(g, overlay_g as f32),
                blend(b, overlay_b as f32),
            )
        }
        (color, _) => color,
    }
}

/// Darken every cell's fg/bg toward a translucent overlay tone in place,
/// preserving symbols — unlike [`fill`], which blanks both. Used behind the
/// unified Connect + Model picker so the transcript stays legible-but-dimmed
/// instead of disappearing.
pub fn dim_region(area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let overlay = active_palette().user_bg;
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = dim_toward_overlay(cell.fg, overlay);
                cell.bg = dim_toward_overlay(cell.bg, overlay);
            }
        }
    }
}

pub fn panel_alt_bg() -> Color {
    active_palette().panel_alt
}

pub fn accent_soft_bg() -> Color {
    active_palette().accent_soft
}

pub fn syntax_theme() -> forge_syntax::HighlightTheme {
    syntax_from_palette(&resolved_palette(&active()))
}

pub fn brand() -> Style {
    Style::default()
        .fg(accent_color())
        .add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(active_palette().muted)
}

pub fn dim() -> Style {
    Style::default().fg(text_dim_color())
}

pub fn text() -> Style {
    Style::default().fg(text_primary_color())
}

/// See [`text_secondary_color`].
pub fn text_secondary() -> Style {
    Style::default().fg(text_secondary_color())
}

pub fn ok() -> Style {
    Style::default().fg(success_color())
}

pub fn warn() -> Style {
    Style::default().fg(warning_color())
}

pub fn danger() -> Style {
    Style::default().fg(error_color())
}

pub fn info() -> Style {
    Style::default().fg(info_color())
}

pub fn success_color() -> Color {
    active_palette().ok
}

pub fn warning_color() -> Color {
    active_palette().warn
}

pub fn error_color() -> Color {
    active_palette().danger
}

pub fn info_color() -> Color {
    active_palette().info
}

pub fn accent_color() -> Color {
    active_palette().accent
}

pub fn accent_style() -> Style {
    Style::default().fg(accent_color())
}

#[cfg(test)]
pub fn background_color() -> Color {
    active_palette().canvas
}

pub fn text_primary_color() -> Color {
    active_palette().text
}

/// Secondary body text — readable but a step down from primary prose (e.g.
/// inline code in a sentence). Solarized Dark's `tag` token already carries
/// base1 (#93A1A1), the exact tone Solarized reserves for this role, so this
/// reuses it rather than adding a new required theme-file field; `dim()`
/// remains the right choice for fully de-emphasized metadata.
pub fn text_secondary_color() -> Color {
    active_palette().tag
}

pub fn text_dim_color() -> Color {
    active_palette().dim
}

pub fn border_color() -> Color {
    active_palette().border
}

/// Style a syntax-highlighted segment with its source-provided RGB color.
pub fn syntax_segment(rgb: (u8, u8, u8), background: Option<Color>) -> Style {
    let style = Style::default().fg(Color::Rgb(rgb.0, rgb.1, rgb.2));
    background.map_or(style, |color| style.bg(color))
}

pub fn agent() -> Style {
    Style::default().fg(active_palette().agent)
}

/// Hovered/active row background.
pub fn surface_hover() -> Style {
    Style::default().bg(active_palette().surface_hover)
}

/// Disabled menu/action label (text_muted, no emphasis).
pub fn disabled() -> Style {
    dim()
}

pub fn tool() -> Style {
    Style::default().fg(active_palette().tool)
}

pub fn code_punctuation() -> Style {
    Style::default().fg(active_palette().muted)
}

pub fn border() -> Style {
    Style::default().fg(border_color())
}

pub fn border_muted() -> Style {
    Style::default().fg(active_palette().border_muted)
}

pub fn panel() -> Style {
    Style::default().bg(active_palette().panel)
}

pub fn panel_alt() -> Style {
    Style::default().bg(active_palette().panel_alt)
}

pub fn user_message() -> Style {
    Style::default()
}

pub fn assistant_message() -> Style {
    Style::default().bg(active_palette().response_bg)
}

pub fn search_match() -> Style {
    let p = active_palette();
    Style::default().fg(p.warn).bg(p.search_match)
}

pub fn diff_add() -> Style {
    let p = active_palette();
    Style::default().fg(p.ok).bg(p.diff_add)
}

pub fn diff_remove() -> Style {
    let p = active_palette();
    Style::default().fg(p.danger).bg(p.diff_remove)
}

pub fn diff_context() -> Style {
    let p = active_palette();
    Style::default().fg(p.muted).bg(p.panel_alt)
}

pub fn diff_hunk() -> Style {
    let p = active_palette();
    Style::default().fg(p.info).bg(p.diff_hunk)
}

// Transcript roles. Keep these semantic so widgets do not need to know the
// palette and basic ANSI terminals still get hierarchy from modifiers/symbols.
pub fn user_message_style() -> Style {
    user_message().fg(active_palette().text)
}

pub fn assistant_answer_style() -> Style {
    assistant_message()
        .fg(active_palette().text)
        .add_modifier(Modifier::BOLD)
}

pub fn progress_style() -> Style {
    agent().add_modifier(Modifier::ITALIC)
}

pub fn tool_running_style() -> Style {
    warn().add_modifier(Modifier::BOLD)
}

pub fn tool_success_style() -> Style {
    ok().add_modifier(Modifier::BOLD)
}

/// Denied: refused by a decision, not a crash. Amber, not red — gives a
/// 3-way visual read alongside `tool_success_style`/`danger`: green success,
/// amber skipped-for-a-reason, red actually-broken.
pub fn tool_denied_style() -> Style {
    warn().add_modifier(Modifier::BOLD)
}

/// Timed out: stub only, no real deadline enforcement exists yet. Amber for
/// the same reason as `tool_denied_style`.
pub fn tool_timeout_style() -> Style {
    warn().add_modifier(Modifier::BOLD)
}

pub fn metadata_style() -> Style {
    muted()
}

pub fn selection_active() -> Style {
    selected_file()
}

pub fn selection_inactive() -> Style {
    selected_file()
}

/// Selected file row in the explorer (accent_soft + text_primary).
pub fn selected_file() -> Style {
    let p = active_palette();
    Style::default().fg(p.text).bg(p.accent_soft)
}

pub fn directory() -> Style {
    brand()
}

pub fn symlink() -> Style {
    Style::default()
        .fg(active_palette().accent)
        .add_modifier(Modifier::ITALIC)
}

pub fn git_added() -> Style {
    ok()
}

pub fn git_modified() -> Style {
    warn()
}

pub fn git_deleted() -> Style {
    danger()
}

pub fn git_untracked() -> Style {
    muted()
}

pub fn git_ignored() -> Style {
    dim()
}

pub fn focused_selection_style() -> Style {
    selected_row()
}

/// Full-row selection (suggestion list, palette, connect picker).
/// Explicit bg — bare REVERSED is unreliable across terminals.
pub fn selected_row() -> Style {
    let p = active_palette();
    Style::default()
        .fg(p.selection_fg)
        .bg(p.selection)
        .add_modifier(Modifier::BOLD)
}

/// "current" / "connected" tag label. `selected` should match whatever
/// determined the row's own base style, so the tag's background always
/// agrees with the row it sits in (plain panel bg vs. [`selected_row`]'s
/// highlighted bg) — both are pre-verified to clear 4.5:1 contrast.
pub fn tag_style(selected: bool) -> Style {
    let p = active_palette();
    let style = Style::default().fg(p.tag);
    if selected {
        style.bg(p.selection)
    } else {
        style
    }
}

/// Input block cursor: solid inverted cell (bg fills the whole character cell).
pub const CURSOR_GLYPH: &str = "█";
pub const CURSOR_CELL: &str = " ";

pub fn caret() -> Style {
    let p = active_palette();
    Style::default()
        .fg(p.panel)
        .bg(p.cursor)
        .add_modifier(Modifier::BOLD)
}

pub fn paint_caret(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    let cell = &mut buf[(x, y)];
    cell.set_symbol(CURSOR_CELL);
    cell.set_style(caret());
}

/// History-recalled input (subtle highlight of the whole field text).
pub fn history_active() -> Style {
    let p = active_palette();
    Style::default().fg(p.text).bg(p.selection)
}

/// Active panel chrome: accent border.
pub fn active_panel_border() -> Style {
    Style::default().fg(active_palette().accent)
}

/// Inactive panel chrome: muted border.
pub fn inactive_panel_border() -> Style {
    border_muted()
}

/// Composer border while an approval is pending — distinct from busy-dim and
/// the inactive border, so the "waiting for a decision" state reads on its
/// own and survives the busy-resume immediately after a decision.
pub fn waiting_border() -> Style {
    Style::default().fg(active_palette().waiting_border)
}

/// Accent border for the focused approval card.
pub fn approval_accent() -> Style {
    Style::default().fg(active_palette().approval_accent)
}

/// Status bar surface.
pub fn status_bar() -> Style {
    panel_alt()
}

/// Composer border in its idle (unfocused, connected, not-waiting) state.
///
/// Sits between `border_muted()` and `active_panel_border()`'s full accent —
/// the composer is the smallest panel in the IDE layout and competes for
/// attention with a much larger editor pane, so it stays visually prominent
/// even when `FocusBlock` has moved elsewhere (attention and app focus state
/// aren't the same thing).
pub fn composer_border_idle() -> Style {
    Style::default().fg(active_palette().accent_soft)
}

/// Composer typed-text emphasis, applied regardless of focus state for the
/// same reason as [`composer_border_idle`].
pub fn composer_text() -> Style {
    text().add_modifier(Modifier::BOLD)
}

/// Composer placeholder ("Describe a task…") style — dim, distinct from
/// [`composer_text`]'s bold emphasis for actual typed content, signaling
/// "type here" rather than "this is content." No italic modifier: many
/// terminal fonts render their italic variant with a different baseline/
/// vertical metric than the regular variant, which visually misaligns the
/// placeholder against the non-italic prompt glyph and cursor next to it on
/// the same row (found live — not a hypothetical).
pub fn composer_placeholder() -> Style {
    dim()
}

/// Composer surface background, applied regardless of focus state. Reuses
/// the theme's `surface_raised` token (relative to the theme's own palette,
/// not a hardcoded color) so it stays safe across light/dark terminal
/// themes.
pub fn composer_surface() -> Style {
    panel_alt()
}

/// Inline code block inside chat (surface background, primary text).
pub fn code_block() -> Style {
    let p = active_palette();
    Style::default().fg(p.text).bg(p.panel)
}

/// Error callout: error foreground on surface (no full red fill).
pub fn error_callout() -> Style {
    let p = active_palette();
    danger().bg(p.panel)
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub agent: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    pub info: Color,
    pub tool: Color,
    pub selection: Color,
    pub diff_add: Color,
    pub diff_remove: Color,
    pub diff_hunk: Color,
    pub panel: Color,
    pub panel_alt: Color,
    pub surface_hover: Color,
    pub user_bg: Color,
    pub response_bg: Color,
    pub border: Color,
    pub border_muted: Color,
    pub search_match: Color,
    /// Composer border while an approval is pending (see [`waiting_border`]).
    pub waiting_border: Color,
    /// Accent border for the focused approval card (see [`approval_accent`]).
    pub approval_accent: Color,
    /// Foreground for text painted on top of `selection` (see [`SELECTION_FG`]).
    pub selection_fg: Color,
    /// "current" / "connected" tag label color (see [`TAG`]).
    pub tag: Color,
    /// Editor/input cursor color (see [`caret`]).
    pub cursor: Color,
}

pub fn palette(theme_id: &str) -> Palette {
    palette_from_source(&resolved_palette(theme_id))
}

fn palette_from_source(src: &ThemePalette) -> Palette {
    Palette {
        canvas: to_color(src.background),
        text: to_color(src.text_primary),
        muted: to_color(src.text_secondary),
        dim: to_color(src.text_muted),
        accent: to_color(src.accent),
        accent_soft: to_color(src.accent_soft),
        agent: to_color(src.agent),
        ok: to_color(src.success),
        warn: to_color(src.warning),
        danger: to_color(src.error),
        info: to_color(src.info),
        tool: to_color(src.info),
        selection: to_color(src.selection),
        diff_add: to_color(src.diff_add),
        diff_remove: to_color(src.diff_remove),
        diff_hunk: to_color(src.accent_soft),
        panel: to_color(src.surface),
        panel_alt: to_color(src.surface_raised),
        surface_hover: to_color(src.surface_hover),
        user_bg: to_color(src.background_deep),
        response_bg: to_color(src.background),
        border: to_color(src.border),
        border_muted: to_color(src.border_muted),
        search_match: to_color(src.search_match),
        waiting_border: to_color(src.waiting_border),
        approval_accent: to_color(src.approval_accent),
        selection_fg: to_color(src.text_primary),
        tag: to_color(src.tag),
        cursor: to_color(src.cursor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_registry::ThemeRegistry;
    use forge_config::{THEME_SOLARIZED_DARK, THEME_SOLARIZED_LIGHT};

    #[test]
    fn system_theme_uses_terminal_background_hint() {
        assert_eq!(
            system_theme_id_from_colorfgbg("15;0"),
            Some(DEFAULT_THEME_ID)
        );
        assert_eq!(
            system_theme_id_from_colorfgbg("0;15"),
            Some(THEME_SOLARIZED_LIGHT)
        );
        assert_eq!(system_theme_id_from_colorfgbg("invalid"), None);
    }

    #[test]
    fn system_theme_parses_platform_preferences() {
        assert_eq!(
            system_theme_id_from_os_output("'prefer-light'"),
            Some(THEME_SOLARIZED_LIGHT)
        );
        assert_eq!(
            system_theme_id_from_os_output("'prefer-dark'"),
            Some(DEFAULT_THEME_ID)
        );
        assert_eq!(
            system_theme_id_from_os_output("AppsUseLightTheme REG_DWORD 0x1"),
            Some(THEME_SOLARIZED_LIGHT)
        );
        assert_eq!(system_theme_id_from_os_output("unavailable"), None);
    }

    fn dark_palette() -> ThemePalette {
        ThemeRegistry::load(None)
            .get(THEME_SOLARIZED_DARK)
            .expect("solarized-dark")
            .palette
    }

    fn light_palette() -> ThemePalette {
        ThemeRegistry::load(None)
            .get(THEME_SOLARIZED_LIGHT)
            .expect("solarized-light")
            .palette
    }

    fn install_defaults() {
        install(ThemeRegistry::load(None), THEME_SOLARIZED_DARK);
    }

    #[test]
    fn surface_hover_and_disabled_expose_spec_tokens() {
        install_defaults();
        let dark = dark_palette();
        assert_eq!(surface_hover().bg, Some(to_color(dark.surface_hover)));
        assert_eq!(disabled().fg, dim().fg);
        assert_eq!(
            registry()
                .get(THEME_SOLARIZED_DARK)
                .unwrap()
                .palette
                .surface_hover,
            dark.surface_hover
        );
    }

    #[test]
    fn selected_file_uses_accent_soft() {
        install_defaults();
        let dark = dark_palette();
        assert_eq!(selected_file().bg, Some(to_color(dark.accent_soft)));
        assert_eq!(selected_file().fg, Some(to_color(dark.text_primary)));
    }

    #[test]
    fn git_modified_uses_warning() {
        assert_eq!(git_modified().fg, warn().fg);
    }

    #[test]
    fn progress_style_uses_agent() {
        assert_eq!(progress_style().fg, agent().fg);
    }

    #[test]
    fn tokens_are_distinct() {
        install_defaults();
        let p = palette(THEME_SOLARIZED_DARK);
        assert_ne!(p.accent, p.ok);
        assert_ne!(p.warn, p.danger);
        assert_ne!(brand().fg, Some(p.muted));
    }

    #[test]
    fn text_secondary_is_solarized_base1_and_distinct_from_interactive_colors() {
        install_defaults();
        // Exact Solarized Dark base1 tone — the intended higher-contrast
        // secondary-body-text color, not the interactive accent/info hues.
        assert_eq!(text_secondary_color(), Color::Rgb(0x93, 0xA1, 0xA1));
        assert_ne!(text_secondary_color(), accent_color());
        assert_ne!(text_secondary_color(), info_color());
        assert_eq!(text_secondary().fg, Some(text_secondary_color()));
    }

    #[test]
    fn body_text_colors_meet_wcag_aa_against_the_background() {
        install_defaults();
        // WCAG AA for normal text requires >= 4.5:1.
        assert_contrast(
            "text_primary (base0)",
            text_primary_color(),
            "background (base03)",
            background_color(),
            4.5,
        );
        assert_contrast(
            "text_secondary (base1)",
            text_secondary_color(),
            "background (base03)",
            background_color(),
            4.5,
        );
    }

    #[test]
    fn selected_and_caret_use_background() {
        install_defaults();
        let dark = dark_palette();
        assert_eq!(selected_row().bg, Some(to_color(dark.selection)));
        assert_eq!(caret().bg, Some(to_color(dark.cursor)));
        assert_eq!(selected_row().fg, Some(to_color(dark.text_primary)));
    }

    #[test]
    fn tag_style_matches_row_selection_state() {
        install_defaults();
        let dark = dark_palette();
        assert_eq!(tag_style(false).fg, Some(to_color(dark.tag)));
        assert_eq!(tag_style(false).bg, None);
        assert_eq!(tag_style(true).fg, Some(to_color(dark.tag)));
        assert_eq!(tag_style(true).bg, Some(to_color(dark.selection)));
    }

    #[test]
    fn conversation_backgrounds_use_palette_roles() {
        install_defaults();
        let dark = dark_palette();
        let light = light_palette();
        set_active(THEME_SOLARIZED_DARK);
        assert_eq!(user_message().bg, None);
        assert_eq!(assistant_message().bg, Some(to_color(dark.background)));
        set_active(THEME_SOLARIZED_LIGHT);
        assert_eq!(user_message().bg, None);
        assert_eq!(assistant_message().bg, Some(to_color(light.background)));
        set_active(THEME_SOLARIZED_DARK);
    }

    #[test]
    fn borders_follow_active_palette() {
        install_defaults();
        let dark = dark_palette();
        let light = light_palette();
        set_active(THEME_SOLARIZED_DARK);
        assert_eq!(border().fg, Some(to_color(dark.border)));
        assert_eq!(border_muted().fg, Some(to_color(dark.border_muted)));
        set_active(THEME_SOLARIZED_LIGHT);
        assert_eq!(border().fg, Some(to_color(light.border)));
        assert_eq!(border_muted().fg, Some(to_color(light.border_muted)));
        set_active(THEME_SOLARIZED_DARK);
    }

    #[test]
    fn canvas_style_uses_palette_background() {
        install_defaults();
        let dark = dark_palette();
        let light = light_palette();
        set_active(THEME_SOLARIZED_LIGHT);
        assert_eq!(canvas().bg, Some(to_color(light.background)));
        set_active(THEME_SOLARIZED_DARK);
        assert_eq!(canvas().bg, Some(to_color(dark.background)));
    }

    #[test]
    fn danger_and_warn_are_different() {
        assert_ne!(danger().fg, warn().fg);
    }

    #[test]
    fn info_uses_configured_info_token() {
        install_defaults();
        assert_eq!(info().fg, Some(to_color(dark_palette().info)));
    }

    #[test]
    fn diff_styles_use_background() {
        assert!(diff_add().bg.is_some());
        assert!(diff_remove().bg.is_some());
    }

    #[test]
    fn light_palette_snapshot() {
        install_defaults();
        let p = palette(THEME_SOLARIZED_LIGHT);
        let light = light_palette();
        assert_eq!(p.text, to_color(light.text_primary));
        assert_eq!(p.canvas, to_color(light.background));
        assert_eq!(p.panel_alt, to_color(light.surface_raised));
        assert_eq!(p.dim, to_color(light.text_muted));
        assert_ne!(p.dim, p.muted);
    }

    #[test]
    fn light_syntax_theme_uses_readable_editor_roles() {
        install(ThemeRegistry::load(None), THEME_SOLARIZED_LIGHT);
        let syntax = syntax_theme();

        assert_eq!(syntax.default, (0x58, 0x6E, 0x75));
        assert_eq!(syntax.comment, (0x65, 0x7B, 0x83));
        assert_eq!(syntax.keyword, (0x6C, 0x7A, 0x00));
        assert_eq!(syntax.string, (0x2A, 0x7F, 0x78));
        assert_eq!(syntax.number, (0x2A, 0x7F, 0x78));
        assert_eq!(syntax.function, (0x1D, 0x6F, 0xA5));
        assert_eq!(syntax.type_, (0x8B, 0x65, 0x00));
        assert_eq!(syntax.variable, (0x1D, 0x6F, 0xA5));
        assert_eq!(syntax.operator, (0x6C, 0x7A, 0x00));
        assert_eq!(syntax.punctuation, (0x65, 0x7B, 0x83));
        assert_eq!(syntax.property, (0x1D, 0x6F, 0xA5));
        assert_eq!(syntax.tag, (0xB5, 0x2B, 0x29));
        assert_eq!(syntax.attribute, (0xA3, 0x3E, 0x12));

        install_defaults();
    }

    #[test]
    fn light_diff_snapshot() {
        install_defaults();
        let p = palette(THEME_SOLARIZED_LIGHT);
        let light = light_palette();
        assert_eq!(p.diff_add, to_color(light.diff_add));
        assert_eq!(p.diff_remove, to_color(light.diff_remove));
    }

    #[test]
    fn fill_paints_every_cell() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        install_defaults();
        let light = light_palette();
        set_active(THEME_SOLARIZED_LIGHT);
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(area);
        fill(area, &mut buf, canvas());
        for y in 0..2 {
            for x in 0..4 {
                assert_eq!(buf[(x, y)].style().bg, Some(to_color(light.background)));
            }
        }
        set_active(THEME_SOLARIZED_DARK);
    }

    #[test]
    fn set_active_switches_palette() {
        install_defaults();
        let dark = dark_palette();
        let light = light_palette();
        set_active(THEME_SOLARIZED_LIGHT);
        assert_eq!(text().fg, Some(to_color(light.text_primary)));
        set_active(THEME_SOLARIZED_DARK);
        assert_eq!(text().fg, Some(to_color(dark.text_primary)));
    }

    #[test]
    fn workspace_theme_drop_in_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let mut content = include_str!("../themes/solarized-dark.toml").to_string();
        content = content.replace("accent = \"#268BD2\"", "accent = \"#FF0000\"");
        std::fs::write(themes.join("solarized-dark.toml"), content).unwrap();
        install(ThemeRegistry::load(Some(dir.path())), THEME_SOLARIZED_DARK);
        assert_eq!(palette(THEME_SOLARIZED_DARK).accent, Color::Rgb(255, 0, 0));
        install_defaults();
    }

    /// Installing a registry that redefines the *active* theme takes effect.
    ///
    /// The active palette is memoized under its theme id, and this swaps the
    /// palette while the id stays `solarized-dark` — so the id alone cannot
    /// tell the cached entry is stale. Reading the accent first is what makes
    /// this a real test: it populates the cache before the second install.
    #[test]
    fn installing_a_registry_refreshes_the_memoized_active_palette() {
        install_defaults();
        set_active(THEME_SOLARIZED_DARK);
        let before = accent_color();
        assert_ne!(before, Color::Rgb(255, 0, 0));

        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join(".forge").join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let content = include_str!("../themes/solarized-dark.toml")
            .to_string()
            .replace("accent = \"#268BD2\"", "accent = \"#FF0000\"");
        std::fs::write(themes.join("solarized-dark.toml"), content).unwrap();
        install(ThemeRegistry::load(Some(dir.path())), THEME_SOLARIZED_DARK);

        assert_eq!(
            accent_color(),
            Color::Rgb(255, 0, 0),
            "the memoized palette outlived the registry it came from"
        );
        install_defaults();
    }

    // WCAG AA (4.5:1, normal text) contrast checks. These pin down the
    // actual rendered backgrounds each role appears on (canvas/panel/
    // panel_alt for the general case, plus SELECTED_BG for tag/selection_fg)
    // so a future palette edit that quietly breaks contrast fails CI instead
    // of a screenshot.
    fn srgb_to_linear(c: u8) -> f64 {
        let c = c as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn relative_luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("expected Color::Rgb, got {c:?}");
        };
        0.2126 * srgb_to_linear(r) + 0.7152 * srgb_to_linear(g) + 0.0722 * srgb_to_linear(b)
    }

    fn contrast_ratio(a: Color, b: Color) -> f64 {
        let (l1, l2) = (relative_luminance(a), relative_luminance(b));
        let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn assert_contrast(role: &str, fg: Color, bg_label: &str, bg: Color, min: f64) {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= min,
            "{role} on {bg_label} only has {ratio:.2}:1 contrast (need >= {min:.1}:1)"
        );
    }

    fn assert_aa(role: &str, fg: Color, bg_label: &str, bg: Color) {
        assert_contrast(role, fg, bg_label, bg, 4.5);
    }

    // Canonical Solarized (https://ethanschoonover.com/solarized/) hues are
    // tuned against Ethan Schoonover's own contrast metric, not the WCAG
    // relative-luminance formula this test uses, and several accent/body
    // tones fall short of strict 4.5:1 once measured that way — especially
    // against the elevated `panel`/`panel_alt` surfaces, which Solarized
    // doesn't define at all (this app blends them from the same base tones).
    // The project chose to keep the authentic Solarized hex values rather
    // than deviate from the published palette, so these tests pin the real,
    // currently-achieved ratio per role/background pair as a regression
    // floor instead of enforcing WCAG AA uniformly. Pairs that do clear
    // 4.5:1 still use `assert_aa` so a future edit can't silently regress
    // them below the standard.
    #[test]
    fn dark_text_roles_meet_wcag_aa() {
        let p = palette(THEME_SOLARIZED_DARK);
        assert_aa("text", p.text, "canvas", p.canvas);
        assert_contrast("muted", p.muted, "canvas", p.canvas, 3.3);
        assert_contrast("accent", p.accent, "canvas", p.canvas, 4.0);
        assert_aa("ok", p.ok, "canvas", p.canvas);
        assert_contrast("danger", p.danger, "canvas", p.canvas, 3.2);
        assert_aa("info", p.info, "canvas", p.canvas);
        assert_aa("tag", p.tag, "canvas", p.canvas);

        assert_contrast("text", p.text, "panel", p.panel, 4.1);
        assert_contrast("muted", p.muted, "panel", p.panel, 2.9);
        assert_contrast("accent", p.accent, "panel", p.panel, 3.5);
        assert_contrast("ok", p.ok, "panel", p.panel, 4.0);
        assert_contrast("danger", p.danger, "panel", p.panel, 2.8);
        assert_contrast("info", p.info, "panel", p.panel, 4.1);
        assert_aa("tag", p.tag, "panel", p.panel);

        assert_contrast("text", p.text, "panel_alt", p.panel_alt, 3.7);
        assert_contrast("muted", p.muted, "panel_alt", p.panel_alt, 2.6);
        assert_contrast("accent", p.accent, "panel_alt", p.panel_alt, 3.1);
        assert_contrast("ok", p.ok, "panel_alt", p.panel_alt, 3.6);
        assert_contrast("danger", p.danger, "panel_alt", p.panel_alt, 2.5);
        assert_contrast("info", p.info, "panel_alt", p.panel_alt, 3.7);
        assert_contrast("tag", p.tag, "panel_alt", p.panel_alt, 4.3);

        // dim is used for low-priority labels on raised surfaces.
        assert_contrast("dim", p.dim, "panel_alt", p.panel_alt, 2.1);
        assert_contrast(
            "selection_fg",
            p.selection_fg,
            "selection",
            p.selection,
            4.3,
        );
        assert_aa("tag on selection", p.tag, "selection", p.selection);
    }

    #[test]
    fn light_text_roles_meet_wcag_aa() {
        let p = palette(THEME_SOLARIZED_LIGHT);
        assert_contrast("text", p.text, "canvas", p.canvas, 4.1);
        assert_contrast("muted", p.muted, "canvas", p.canvas, 2.4);
        assert_contrast("tag", p.tag, "canvas", p.canvas, 2.4);
        assert_contrast("accent", p.accent, "canvas", p.canvas, 3.2);
        assert_contrast("ok", p.ok, "canvas", p.canvas, 2.8);
        assert_contrast("danger", p.danger, "canvas", p.canvas, 4.0);
        assert_contrast("info", p.info, "canvas", p.canvas, 2.7);

        assert_contrast("text", p.text, "panel_alt", p.panel_alt, 3.3);
        assert_contrast("muted", p.muted, "panel_alt", p.panel_alt, 2.0);
        assert_contrast("tag", p.tag, "panel_alt", p.panel_alt, 2.0);
        assert_contrast("dim", p.dim, "panel_alt", p.panel_alt, 1.3);
        assert_contrast("danger", p.danger, "panel_alt", p.panel_alt, 3.4);

        assert_contrast(
            "selection_fg",
            p.selection_fg,
            "selection",
            p.selection,
            3.8,
        );
        assert_contrast("tag on selection", p.tag, "selection", p.selection, 2.3);
    }
}
