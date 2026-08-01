//! Color tokens from the Forge TUI design system.

use forge_config::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::cell::Cell;

thread_local! {
    static ACTIVE_THEME: Cell<Theme> = const { Cell::new(Theme::Dark) };
}

/// Install the configured palette for this TUI session (call once at startup).
pub fn set_active(theme: Theme) {
    ACTIVE_THEME.with(|t| t.set(theme));
}

/// Active palette from `[tui] theme` in `forge.toml`.
pub fn active() -> Theme {
    ACTIVE_THEME.with(|t| t.get())
}

// Forge Midnight — dark theme tokens (spec hex → RGB decimal).
pub const CANVAS: Color = Color::Rgb(13, 17, 23); // background
pub const CANVAS_DEEP: Color = Color::Rgb(9, 12, 17); // deepest canvas
pub const PANEL: Color = Color::Rgb(19, 25, 34); // surface
pub const PANEL_ALT: Color = Color::Rgb(26, 34, 48); // surface_raised
pub const SURFACE_HOVER: Color = Color::Rgb(32, 42, 57); // surface_hover
pub const BORDER: Color = Color::Rgb(43, 53, 69); // border
pub const BORDER_MUTED: Color = Color::Rgb(32, 41, 56); // border_muted
pub const TEXT: Color = Color::Rgb(230, 237, 243); // text_primary
pub const MUTED: Color = Color::Rgb(157, 170, 189); // text_secondary
pub const DIM: Color = Color::Rgb(133, 148, 168); // text_muted (clears 4.5 on surface_raised)
pub const ACCENT: Color = Color::Rgb(104, 168, 255); // accent
pub const ACCENT_2: Color = Color::Rgb(86, 212, 221); // info
pub const ACCENT_SOFT: Color = Color::Rgb(28, 53, 85); // accent_soft
pub const AGENT: Color = Color::Rgb(180, 156, 255); // agent
pub const OK: Color = Color::Rgb(86, 211, 100); // success
pub const WARN: Color = Color::Rgb(227, 179, 65); // warning
pub const DANGER: Color = Color::Rgb(255, 123, 114); // error
pub const TOOL: Color = Color::Rgb(86, 212, 221); // informational tool state
pub const CURSOR: Color = Color::Rgb(240, 246, 252); // cursor
/// User message left gutter marker — the accent (blue) interaction colour.
pub const USER_MESSAGE_GUTTER_DARK: Color = ACCENT;
/// Active-input gutter marker — brighter than the submitted marker.
pub const USER_GUTTER_ACTIVE_DARK: Color = Color::Rgb(138, 192, 255);
pub const USER_GUTTER_ACTIVE_LIGHT: Color = Color::Rgb(23, 105, 204);
pub const USER_MESSAGE_GUTTER_LIGHT: Color = LIGHT_ACCENT;
/// Highlighted-row background (selected list rows, focused picker rows).
pub const SELECTED_BG: Color = Color::Rgb(41, 72, 111); // selection
/// Foreground painted on top of [`SELECTED_BG`].
pub const SELECTION_FG: Color = TEXT;
/// "current" / "connected" tag label colour.
pub const TAG: Color = Color::Rgb(192, 198, 208);
pub const USER_BG: Color = CANVAS_DEEP;
pub const RESPONSE_BG: Color = CANVAS;
pub const DIFF_ADD_BG: Color = Color::Rgb(36, 74, 50);
pub const DIFF_REMOVE_BG: Color = Color::Rgb(84, 43, 49);
pub const DIFF_HUNK_BG: Color = ACCENT_SOFT;
pub const DARK_SEARCH_MATCH_BG: Color = Color::Rgb(51, 66, 87);

// Forge Daylight — light theme tokens.
pub const LIGHT_CANVAS: Color = Color::Rgb(244, 246, 248); // background
pub const LIGHT_PANEL: Color = Color::Rgb(255, 255, 255); // surface
pub const LIGHT_PANEL_ALT: Color = Color::Rgb(233, 237, 242); // surface_raised
pub const LIGHT_SURFACE_HOVER: Color = Color::Rgb(226, 232, 240); // surface_hover
pub const LIGHT_BORDER: Color = Color::Rgb(200, 208, 219); // border
pub const LIGHT_BORDER_MUTED: Color = Color::Rgb(221, 226, 233); // border_muted
pub const LIGHT_TEXT: Color = Color::Rgb(23, 32, 44); // text_primary
pub const LIGHT_MUTED: Color = Color::Rgb(79, 93, 112); // text_secondary
pub const LIGHT_DIM: Color = Color::Rgb(86, 99, 120); // text_muted (clears 4.5 on surface_raised)
pub const LIGHT_ACCENT: Color = Color::Rgb(23, 105, 204); // accent
pub const LIGHT_ACCENT_2: Color = Color::Rgb(7, 120, 132); // info
pub const LIGHT_ACCENT_SOFT: Color = Color::Rgb(220, 235, 252); // accent_soft
pub const LIGHT_AGENT: Color = Color::Rgb(112, 72, 200); // agent
pub const LIGHT_OK: Color = Color::Rgb(35, 122, 59); // success
pub const LIGHT_WARN: Color = Color::Rgb(153, 101, 0); // warning
pub const LIGHT_DANGER: Color = Color::Rgb(198, 49, 44); // error
pub const LIGHT_TOOL: Color = Color::Rgb(8, 126, 139);
pub const LIGHT_CURSOR: Color = TEXT;
pub const LIGHT_SELECTION: Color = Color::Rgb(201, 225, 252); // selection
pub const LIGHT_SELECTION_FG: Color = LIGHT_TEXT;
pub const LIGHT_TAG: Color = Color::Rgb(80, 92, 112);
pub const LIGHT_DIFF_ADD: Color = Color::Rgb(221, 244, 228);
pub const LIGHT_DIFF_REMOVE: Color = Color::Rgb(251, 225, 227);
pub const LIGHT_DIFF_HUNK: Color = LIGHT_ACCENT_SOFT;
pub const LIGHT_SEARCH_MATCH_BG: Color = Color::Rgb(255, 244, 200);

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
    Style::default().bg(palette(active()).canvas)
}

/// Blend a color toward a translucent dark overlay tone, approximating a
/// ~62%-opacity dark panel sitting on top of it. Ratatui cells have no alpha
/// channel, so this is a one-shot blend rather than true compositing;
/// non-`Rgb` colors (e.g. `Reset`) pass through unchanged.
fn dim_toward_overlay(color: Color) -> Color {
    const ALPHA: f32 = 0.62;
    const OVERLAY: (f32, f32, f32) = (9.0, 12.0, 17.0);
    match color {
        Color::Rgb(r, g, b) => {
            let blend = |c: u8, d: f32| ((c as f32) * (1.0 - ALPHA) + d * ALPHA).round() as u8;
            Color::Rgb(
                blend(r, OVERLAY.0),
                blend(g, OVERLAY.1),
                blend(b, OVERLAY.2),
            )
        }
        other => other,
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
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = dim_toward_overlay(cell.fg);
                cell.bg = dim_toward_overlay(cell.bg);
            }
        }
    }
}

pub fn panel_alt_bg() -> Color {
    palette(active()).panel_alt
}

pub fn accent_soft_bg() -> Color {
    palette(active()).accent_soft
}

pub fn syntax_theme() -> forge_syntax::HighlightTheme {
    match active() {
        Theme::Light => forge_syntax::HighlightTheme::light(),
        Theme::Dark | Theme::System => forge_syntax::HighlightTheme::default(),
    }
}

pub fn brand() -> Style {
    Style::default()
        .fg(palette(active()).accent)
        .add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(palette(active()).muted)
}

pub fn dim() -> Style {
    Style::default().fg(palette(active()).dim)
}

pub fn text() -> Style {
    Style::default().fg(palette(active()).text)
}

pub fn ok() -> Style {
    Style::default().fg(palette(active()).ok)
}

pub fn warn() -> Style {
    Style::default().fg(palette(active()).warn)
}

pub fn danger() -> Style {
    Style::default().fg(palette(active()).danger)
}

pub fn info() -> Style {
    Style::default().fg(palette(active()).info)
}

pub fn agent() -> Style {
    Style::default().fg(palette(active()).agent)
}

pub fn surface_hover() -> Style {
    Style::default().bg(palette(active()).surface_hover)
}

pub fn tool() -> Style {
    Style::default().fg(palette(active()).tool)
}

pub fn code_punctuation() -> Style {
    Style::default().fg(palette(active()).muted)
}

pub fn border() -> Style {
    Style::default().fg(palette(active()).border)
}

pub fn border_muted() -> Style {
    Style::default().fg(palette(active()).border_muted)
}

pub fn panel() -> Style {
    Style::default().bg(palette(active()).panel)
}

pub fn panel_alt() -> Style {
    Style::default().bg(palette(active()).panel_alt)
}

pub fn user_message() -> Style {
    Style::default().bg(palette(active()).user_bg)
}

pub fn assistant_message() -> Style {
    Style::default().bg(palette(active()).response_bg)
}

pub fn search_match() -> Style {
    let p = palette(active());
    Style::default().fg(p.warn).bg(p.search_match)
}

pub fn diff_add() -> Style {
    let p = palette(active());
    Style::default().fg(p.ok).bg(p.diff_add)
}

pub fn diff_remove() -> Style {
    let p = palette(active());
    Style::default().fg(p.danger).bg(p.diff_remove)
}

pub fn diff_context() -> Style {
    let p = palette(active());
    Style::default().fg(p.muted).bg(p.panel_alt)
}

pub fn diff_hunk() -> Style {
    let p = palette(active());
    Style::default().fg(p.info).bg(p.diff_hunk)
}

// Transcript roles. Keep these semantic so widgets do not need to know the
// palette and basic ANSI terminals still get hierarchy from modifiers/symbols.
pub fn user_message_style() -> Style {
    user_message().fg(palette(active()).text)
}

#[cfg(test)]
pub fn user_message_gutter_style() -> Style {
    user_message_gutter_style_for(active())
}

pub fn user_message_gutter_style_for(theme: Theme) -> Style {
    user_message().fg(palette(theme).user_message_gutter)
}

pub fn user_gutter_active_style_for(theme: Theme) -> Style {
    user_message().fg(palette(theme).user_gutter_active)
}

pub fn assistant_answer_style() -> Style {
    assistant_message()
        .fg(palette(active()).text)
        .add_modifier(Modifier::BOLD)
}

pub fn progress_style() -> Style {
    muted().add_modifier(Modifier::ITALIC)
}

pub fn tool_running_style() -> Style {
    warn().add_modifier(Modifier::BOLD)
}

pub fn tool_success_style() -> Style {
    ok().add_modifier(Modifier::BOLD)
}

pub fn metadata_style() -> Style {
    muted()
}

pub fn selection_active() -> Style {
    selected_row()
}

pub fn selection_inactive() -> Style {
    surface_hover().fg(palette(active()).text)
}

pub fn directory() -> Style {
    brand()
}

pub fn file_default() -> Style {
    text()
}

pub fn file_source() -> Style {
    info()
}

pub fn file_config() -> Style {
    warn()
}

pub fn file_document() -> Style {
    text()
}

pub fn file_data() -> Style {
    tool()
}

pub fn file_image() -> Style {
    ok()
}

pub fn file_binary() -> Style {
    dim()
}

pub fn symlink() -> Style {
    Style::default()
        .fg(palette(active()).accent)
        .add_modifier(Modifier::ITALIC)
}

pub fn git_added() -> Style {
    ok()
}

pub fn git_modified() -> Style {
    info()
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
    let p = palette(active());
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
    let p = palette(active());
    let style = Style::default().fg(p.tag);
    if selected {
        style.bg(p.selection)
    } else {
        style
    }
}

/// Input block cursor: solid inverted cell (bg fills the whole character cell).
pub fn caret() -> Style {
    let p = palette(active());
    Style::default()
        .fg(p.panel)
        .bg(p.cursor)
        .add_modifier(Modifier::BOLD)
}

/// History-recalled input (subtle highlight of the whole field text).
pub fn history_active() -> Style {
    let p = palette(active());
    Style::default().fg(p.text).bg(p.selection)
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
    pub user_message_gutter: Color,
    pub user_gutter_active: Color,
    pub border: Color,
    pub border_muted: Color,
    pub search_match: Color,
    /// Foreground for text painted on top of `selection` (see [`SELECTION_FG`]).
    pub selection_fg: Color,
    /// "current" / "connected" tag label color (see [`TAG`]).
    pub tag: Color,
    /// Editor/input cursor color (see [`caret`]).
    pub cursor: Color,
}

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => Palette {
            canvas: CANVAS,
            text: TEXT,
            muted: MUTED,
            dim: DIM,
            accent: ACCENT,
            accent_soft: ACCENT_SOFT,
            agent: AGENT,
            ok: OK,
            warn: WARN,
            danger: DANGER,
            info: ACCENT_2,
            tool: TOOL,
            selection: SELECTED_BG,
            diff_add: DIFF_ADD_BG,
            diff_remove: DIFF_REMOVE_BG,
            diff_hunk: DIFF_HUNK_BG,
            panel: PANEL,
            panel_alt: PANEL_ALT,
            surface_hover: SURFACE_HOVER,
            user_bg: USER_BG,
            response_bg: RESPONSE_BG,
            user_message_gutter: USER_MESSAGE_GUTTER_DARK,
            user_gutter_active: USER_GUTTER_ACTIVE_DARK,
            border: BORDER,
            border_muted: BORDER_MUTED,
            search_match: DARK_SEARCH_MATCH_BG,
            selection_fg: SELECTION_FG,
            tag: TAG,
            cursor: CURSOR,
        },
        Theme::Light => Palette {
            canvas: LIGHT_CANVAS,
            text: LIGHT_TEXT,
            muted: LIGHT_MUTED,
            dim: LIGHT_DIM,
            accent: LIGHT_ACCENT,
            accent_soft: LIGHT_ACCENT_SOFT,
            agent: LIGHT_AGENT,
            ok: LIGHT_OK,
            warn: LIGHT_WARN,
            danger: LIGHT_DANGER,
            info: LIGHT_ACCENT_2,
            tool: LIGHT_TOOL,
            selection: LIGHT_SELECTION,
            diff_add: LIGHT_DIFF_ADD,
            diff_remove: LIGHT_DIFF_REMOVE,
            diff_hunk: LIGHT_DIFF_HUNK,
            panel: LIGHT_PANEL,
            panel_alt: LIGHT_PANEL_ALT,
            surface_hover: LIGHT_SURFACE_HOVER,
            user_bg: LIGHT_CANVAS,
            response_bg: LIGHT_CANVAS,
            user_message_gutter: USER_MESSAGE_GUTTER_LIGHT,
            user_gutter_active: USER_GUTTER_ACTIVE_LIGHT,
            border: LIGHT_BORDER,
            border_muted: LIGHT_BORDER_MUTED,
            search_match: LIGHT_SEARCH_MATCH_BG,
            selection_fg: LIGHT_SELECTION_FG,
            tag: LIGHT_TAG,
            cursor: LIGHT_CURSOR,
        },
        Theme::System => Palette {
            canvas: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            dim: Color::Reset,
            accent: Color::Cyan,
            accent_soft: Color::Reset,
            agent: Color::Magenta,
            ok: Color::Green,
            warn: Color::Yellow,
            danger: Color::Red,
            info: Color::Blue,
            tool: Color::Cyan,
            selection: Color::Reset,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            diff_hunk: Color::Blue,
            panel: Color::Reset,
            panel_alt: Color::Reset,
            surface_hover: Color::Reset,
            user_bg: Color::Reset,
            response_bg: Color::Reset,
            user_message_gutter: Color::Blue,
            user_gutter_active: Color::LightBlue,
            border: Color::Reset,
            border_muted: Color::Reset,
            search_match: Color::Reset,
            selection_fg: Color::White,
            tag: Color::Gray,
            cursor: Color::White,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_config::Theme;

    #[test]
    fn tokens_are_distinct() {
        assert_ne!(ACCENT, OK);
        assert_ne!(WARN, DANGER);
        assert_ne!(brand().fg, Some(MUTED));
    }

    #[test]
    fn selected_and_caret_use_background() {
        assert_eq!(selected_row().bg, Some(SELECTED_BG));
        assert_eq!(caret().bg, Some(CURSOR));
        assert_eq!(selected_row().fg, Some(SELECTION_FG));
    }

    #[test]
    fn tag_style_matches_row_selection_state() {
        assert_eq!(tag_style(false).fg, Some(TAG));
        assert_eq!(tag_style(false).bg, None);
        assert_eq!(tag_style(true).fg, Some(TAG));
        assert_eq!(tag_style(true).bg, Some(SELECTED_BG));
    }

    #[test]
    fn conversation_backgrounds_use_palette_roles() {
        set_active(Theme::Dark);
        assert_eq!(user_message().bg, Some(USER_BG));
        assert_eq!(assistant_message().bg, Some(RESPONSE_BG));
        set_active(Theme::Light);
        assert_eq!(user_message().bg, Some(LIGHT_CANVAS));
        assert_eq!(assistant_message().bg, Some(LIGHT_CANVAS));
        set_active(Theme::Dark);
    }

    #[test]
    fn borders_follow_active_palette() {
        set_active(Theme::Dark);
        assert_eq!(border().fg, Some(BORDER));
        assert_eq!(border_muted().fg, Some(BORDER_MUTED));
        set_active(Theme::Light);
        assert_eq!(border().fg, Some(LIGHT_BORDER));
        assert_eq!(border_muted().fg, Some(LIGHT_BORDER_MUTED));
        set_active(Theme::Dark);
    }

    #[test]
    fn canvas_style_uses_palette_background() {
        set_active(Theme::Light);
        assert_eq!(canvas().bg, Some(LIGHT_CANVAS));
        set_active(Theme::Dark);
        assert_eq!(canvas().bg, Some(CANVAS));
    }

    #[test]
    fn danger_and_warn_are_different() {
        assert_ne!(danger().fg, warn().fg);
    }

    #[test]
    fn info_uses_accent_2() {
        assert_eq!(info().fg, Some(ACCENT_2));
    }

    #[test]
    fn diff_styles_use_background() {
        assert!(diff_add().bg.is_some());
        assert!(diff_remove().bg.is_some());
    }

    #[test]
    fn user_message_gutter_style_uses_semantic_token() {
        assert_eq!(
            user_message_gutter_style().fg,
            Some(USER_MESSAGE_GUTTER_DARK)
        );
    }

    #[test]
    fn user_message_gutter_uses_accent() {
        let dark = palette(Theme::Dark);
        assert_eq!(
            dark.user_message_gutter, dark.accent,
            "user gutter marker uses accent"
        );
        assert_ne!(dark.user_message_gutter, dark.info);
    }

    #[test]
    fn user_gutter_active_is_distinct_from_submitted() {
        let dark = palette(Theme::Dark);
        assert_ne!(dark.user_gutter_active, dark.user_message_gutter);
    }

    #[test]
    fn light_palette_snapshot() {
        let p = palette(Theme::Light);
        assert_eq!(p.text, LIGHT_TEXT);
        assert_eq!(p.canvas, LIGHT_CANVAS);
        assert_eq!(p.panel_alt, LIGHT_PANEL_ALT);
        assert_eq!(p.dim, LIGHT_DIM);
        assert_ne!(p.dim, p.muted);
    }

    #[test]
    fn light_diff_snapshot() {
        let p = palette(Theme::Light);
        assert_eq!(p.diff_add, LIGHT_DIFF_ADD);
        assert_eq!(p.diff_remove, LIGHT_DIFF_REMOVE);
    }

    #[test]
    fn fill_paints_every_cell() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        set_active(Theme::Light);
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(area);
        fill(area, &mut buf, canvas());
        for y in 0..2 {
            for x in 0..4 {
                assert_eq!(buf[(x, y)].style().bg, Some(LIGHT_CANVAS));
            }
        }
        set_active(Theme::Dark);
    }

    #[test]
    fn set_active_switches_palette() {
        set_active(Theme::Light);
        assert_eq!(text().fg, Some(LIGHT_TEXT));
        set_active(Theme::Dark);
        assert_eq!(text().fg, Some(TEXT));
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

    fn assert_aa(role: &str, fg: Color, bg_label: &str, bg: Color) {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= 4.5,
            "{role} on {bg_label} only has {ratio:.2}:1 contrast (need >= 4.5:1)"
        );
    }

    #[test]
    fn dark_text_roles_meet_wcag_aa() {
        let p = palette(Theme::Dark);
        for (bg_label, bg) in [
            ("canvas", p.canvas),
            ("panel", p.panel),
            ("panel_alt", p.panel_alt),
        ] {
            assert_aa("text", p.text, bg_label, bg);
            assert_aa("muted", p.muted, bg_label, bg);
            assert_aa("accent", p.accent, bg_label, bg);
            assert_aa("ok", p.ok, bg_label, bg);
            assert_aa("danger", p.danger, bg_label, bg);
            assert_aa("info", p.info, bg_label, bg);
            assert_aa("tag", p.tag, bg_label, bg);
        }
        // dim and danger are additionally used on panel_alt (sidebar inspector).
        assert_aa("dim", p.dim, "panel_alt", p.panel_alt);
        assert_aa("selection_fg", p.selection_fg, "selection", p.selection);
        assert_aa("tag on selection", p.tag, "selection", p.selection);
    }

    #[test]
    fn light_text_roles_meet_wcag_aa() {
        let p = palette(Theme::Light);
        for (bg_label, bg) in [("canvas", p.canvas), ("panel_alt", p.panel_alt)] {
            assert_aa("text", p.text, bg_label, bg);
            assert_aa("muted", p.muted, bg_label, bg);
            assert_aa("tag", p.tag, bg_label, bg);
        }
        assert_aa("accent", p.accent, "canvas", p.canvas);
        assert_aa("ok", p.ok, "canvas", p.canvas);
        assert_aa("danger", p.danger, "canvas", p.canvas);
        assert_aa("info", p.info, "canvas", p.canvas);
        assert_aa("dim", p.dim, "panel_alt", p.panel_alt);
        assert_aa("danger", p.danger, "panel_alt", p.panel_alt);
        assert_aa("selection_fg", p.selection_fg, "selection", p.selection);
        assert_aa("tag on selection", p.tag, "selection", p.selection);
    }
}
