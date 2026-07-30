//! Color tokens from the Forge TUI design system.

use forge_config::Theme;
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

pub const CANVAS: Color = Color::Rgb(24, 23, 22);
pub const CANVAS_DEEP: Color = Color::Rgb(17, 17, 16);
pub const ACCENT: Color = Color::Rgb(83, 214, 227);
pub const ACCENT_2: Color = Color::Rgb(125, 168, 245);
pub const USER_MESSAGE_GUTTER_DARK: Color = Color::Rgb(96, 145, 220);
pub const USER_MESSAGE_GUTTER_LIGHT: Color = Color::Rgb(37, 99, 175);
pub const USER_GUTTER_ACTIVE_DARK: Color = Color::Rgb(112, 168, 245);
pub const USER_GUTTER_ACTIVE_LIGHT: Color = Color::Rgb(25, 82, 155);
pub const OK: Color = Color::Rgb(97, 207, 139);
pub const WARN: Color = Color::Rgb(229, 185, 79);
pub const DANGER: Color = Color::Rgb(239, 112, 120);
pub const TOOL: Color = Color::Rgb(132, 231, 239);
pub const MUTED: Color = Color::Rgb(145, 139, 130);
pub const DIM: Color = Color::Rgb(111, 106, 99);
pub const TEXT: Color = Color::Rgb(242, 239, 232);
pub const TEXT_STRONG: Color = Color::Rgb(255, 255, 255);
pub const BORDER: Color = Color::Rgb(69, 65, 60);
pub const BORDER_MUTED: Color = Color::Rgb(53, 50, 47);
pub const PANEL: Color = Color::Rgb(35, 33, 31);
pub const PANEL_ALT: Color = Color::Rgb(44, 41, 38);
pub const SELECTED_BG: Color = Color::Rgb(42, 58, 60);
pub const HISTORY_BG: Color = SELECTED_BG;
pub const USER_BG: Color = CANVAS_DEEP;
pub const RESPONSE_BG: Color = CANVAS;
pub const DIFF_ADD_BG: Color = Color::Rgb(41, 75, 55);
pub const DIFF_REMOVE_BG: Color = Color::Rgb(91, 44, 49);
pub const DIFF_HUNK_BG: Color = Color::Rgb(45, 63, 97);

pub const LIGHT_CANVAS: Color = Color::Rgb(253, 250, 242);
pub const LIGHT_TEXT: Color = Color::Rgb(32, 29, 26);
pub const LIGHT_MUTED: Color = Color::Rgb(119, 113, 102);
pub const LIGHT_SELECTION: Color = Color::Rgb(203, 225, 245);
pub const LIGHT_DIFF_ADD: Color = Color::Rgb(209, 240, 223);
pub const LIGHT_DIFF_REMOVE: Color = Color::Rgb(255, 223, 223);
pub const LIGHT_DIFF_HUNK: Color = Color::Rgb(220, 230, 255);

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

pub fn tool() -> Style {
    Style::default().fg(palette(active()).tool)
}

pub fn code_punctuation() -> Style {
    Style::default().fg(palette(active()).muted)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}

pub fn border_muted() -> Style {
    Style::default().fg(BORDER_MUTED)
}

pub fn panel() -> Style {
    Style::default().bg(palette(active()).panel)
}

pub fn panel_alt() -> Style {
    Style::default().bg(palette(active()).panel_alt)
}

pub fn user_message() -> Style {
    Style::default()
}

pub fn assistant_message() -> Style {
    Style::default()
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

pub fn user_message_gutter_style() -> Style {
    user_message_gutter_style_for(active())
}

pub fn user_message_gutter_style_for(theme: Theme) -> Style {
    user_message().fg(palette(theme).user_message_gutter)
}

pub fn user_gutter_active_style() -> Style {
    user_gutter_active_style_for(active())
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
    info().add_modifier(Modifier::BOLD)
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
    let p = palette(active());
    Style::default().fg(p.text).bg(p.panel_alt)
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
        .fg(p.text_strong)
        .bg(p.selection)
        .add_modifier(Modifier::BOLD)
}

/// Input block cursor: solid inverted cell (bg fills the whole character cell).
pub fn caret() -> Style {
    let p = palette(active());
    Style::default()
        .fg(p.panel)
        .bg(p.text)
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
    pub user_bg: Color,
    pub response_bg: Color,
    pub text_strong: Color,
    pub user_message_gutter: Color,
    pub user_gutter_active: Color,
}

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => Palette {
            canvas: CANVAS,
            text: TEXT,
            muted: MUTED,
            dim: DIM,
            accent: ACCENT,
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
            user_bg: USER_BG,
            response_bg: RESPONSE_BG,
            text_strong: TEXT_STRONG,
            user_message_gutter: USER_MESSAGE_GUTTER_DARK,
            user_gutter_active: USER_GUTTER_ACTIVE_DARK,
        },
        Theme::Light => Palette {
            canvas: LIGHT_CANVAS,
            text: LIGHT_TEXT,
            muted: LIGHT_MUTED,
            dim: LIGHT_MUTED,
            accent: ACCENT,
            ok: OK,
            warn: WARN,
            danger: DANGER,
            info: ACCENT_2,
            tool: TOOL,
            selection: LIGHT_SELECTION,
            diff_add: LIGHT_DIFF_ADD,
            diff_remove: LIGHT_DIFF_REMOVE,
            diff_hunk: LIGHT_DIFF_HUNK,
            panel: LIGHT_CANVAS,
            panel_alt: LIGHT_SELECTION,
            user_bg: LIGHT_CANVAS,
            response_bg: LIGHT_CANVAS,
            text_strong: LIGHT_TEXT,
            user_message_gutter: USER_MESSAGE_GUTTER_LIGHT,
            user_gutter_active: USER_GUTTER_ACTIVE_LIGHT,
        },
        Theme::System => Palette {
            canvas: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            dim: Color::Reset,
            accent: Color::Cyan,
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
            user_bg: Color::Reset,
            response_bg: Color::Reset,
            text_strong: Color::White,
            user_message_gutter: Color::Blue,
            user_gutter_active: Color::LightBlue,
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
        assert_eq!(caret().bg, Some(TEXT));
        assert_eq!(selected_row().fg, Some(TEXT_STRONG));
    }

    #[test]
    fn conversation_background_is_distinct_from_panel() {
        assert_eq!(user_message().bg, None);
        assert_eq!(assistant_message().bg, None);
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
    fn user_gutter_active_is_distinct_from_submitted() {
        let dark = palette(Theme::Dark);
        assert_ne!(dark.user_gutter_active, dark.user_message_gutter);
        assert_ne!(dark.user_gutter_active, dark.accent);
    }

    #[test]
    fn user_message_gutter_is_distinct_from_info() {
        let dark = palette(Theme::Dark);
        assert_ne!(dark.user_message_gutter, dark.info);
        assert_ne!(dark.user_message_gutter, dark.accent);
    }

    #[test]
    fn light_palette_snapshot() {
        let p = palette(Theme::Light);
        assert_eq!(p.text, LIGHT_TEXT);
        assert_eq!(p.canvas, LIGHT_CANVAS);
    }

    #[test]
    fn light_diff_snapshot() {
        let p = palette(Theme::Light);
        assert_eq!(p.diff_add, LIGHT_DIFF_ADD);
        assert_eq!(p.diff_remove, LIGHT_DIFF_REMOVE);
    }

    #[test]
    fn set_active_switches_palette() {
        set_active(Theme::Light);
        assert_eq!(text().fg, Some(LIGHT_TEXT));
        set_active(Theme::Dark);
        assert_eq!(text().fg, Some(TEXT));
    }
}
