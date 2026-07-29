//! Color tokens from the Forge TUI design system.

use ratatui::style::{Color, Modifier, Style};

pub const CANVAS: Color = Color::Rgb(24, 23, 22);
pub const CANVAS_DEEP: Color = Color::Rgb(17, 17, 16);
pub const ACCENT: Color = Color::Rgb(83, 214, 227);
pub const ACCENT_2: Color = Color::Rgb(125, 168, 245);
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

pub fn brand() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn ok() -> Style {
    Style::default().fg(OK)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn danger() -> Style {
    Style::default().fg(DANGER)
}

pub fn info() -> Style {
    Style::default().fg(ACCENT_2)
}

pub fn tool() -> Style {
    Style::default().fg(TOOL)
}

pub fn code_punctuation() -> Style {
    Style::default().fg(MUTED)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}

pub fn border_muted() -> Style {
    Style::default().fg(BORDER_MUTED)
}

pub fn panel() -> Style {
    Style::default().bg(PANEL)
}

pub fn panel_alt() -> Style {
    Style::default().bg(PANEL_ALT)
}

pub fn user_message() -> Style {
    Style::default().bg(USER_BG)
}

pub fn assistant_message() -> Style {
    Style::default().bg(RESPONSE_BG)
}

pub fn diff_add() -> Style {
    Style::default().fg(OK).bg(DIFF_ADD_BG)
}

pub fn diff_remove() -> Style {
    Style::default().fg(DANGER).bg(DIFF_REMOVE_BG)
}

pub fn diff_context() -> Style {
    Style::default().fg(MUTED).bg(PANEL_ALT)
}

pub fn diff_hunk() -> Style {
    Style::default().fg(ACCENT_2).bg(DIFF_HUNK_BG)
}

// Transcript roles. Keep these semantic so widgets do not need to know the
// palette and basic ANSI terminals still get hierarchy from modifiers/symbols.
pub fn user_message_style() -> Style {
    user_message().fg(TEXT)
}

pub fn assistant_answer_style() -> Style {
    assistant_message().fg(TEXT).add_modifier(Modifier::BOLD)
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

pub fn tool_failure_style() -> Style {
    danger().add_modifier(Modifier::BOLD)
}

pub fn metadata_style() -> Style {
    muted()
}

pub fn focused_selection_style() -> Style {
    selected_row()
}

/// Full-row selection (suggestion list, palette, connect picker).
/// Explicit bg — bare REVERSED is unreliable across terminals.
pub fn selected_row() -> Style {
    Style::default()
        .fg(TEXT_STRONG)
        .bg(SELECTED_BG)
        .add_modifier(Modifier::BOLD)
}

/// Input block cursor: solid inverted cell (bg fills the whole character cell).
pub fn caret() -> Style {
    Style::default()
        .fg(PANEL)
        .bg(TEXT)
        .add_modifier(Modifier::BOLD)
}

/// History-recalled input (subtle highlight of the whole field text).
pub fn history_active() -> Style {
    Style::default().fg(TEXT).bg(HISTORY_BG)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_ne!(user_message().bg, assistant_message().bg);
        assert_ne!(assistant_message().bg, Some(PANEL));
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
}
