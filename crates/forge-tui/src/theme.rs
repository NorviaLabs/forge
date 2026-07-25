//! Color tokens from ui.md design language (TUI-01).

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(61, 214, 198); // teal
pub const ACCENT_2: Color = Color::Rgb(110, 168, 254); // blue info
pub const OK: Color = Color::Rgb(63, 185, 80);
pub const WARN: Color = Color::Rgb(227, 179, 65);
pub const DANGER: Color = Color::Rgb(248, 81, 73);
pub const TOOL: Color = Color::Rgb(210, 168, 255);
pub const MUTED: Color = Color::Rgb(139, 155, 176);
pub const DIM: Color = Color::Rgb(92, 107, 126);
pub const TEXT: Color = Color::Rgb(230, 237, 243);
pub const BORDER: Color = Color::Rgb(38, 48, 62);
pub const PANEL: Color = Color::Rgb(16, 22, 29);
pub const PANEL_ALT: Color = Color::Rgb(20, 28, 37);
pub const HISTORY_BG: Color = Color::Rgb(27, 38, 52);
pub const USER_BG: Color = Color::Rgb(14, 19, 25);
pub const RESPONSE_BG: Color = Color::Rgb(17, 24, 32);
pub const THINKING_BG: Color = Color::Rgb(19, 28, 39);

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

pub fn code_keyword() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn code_string() -> Style {
    Style::default().fg(WARN)
}

pub fn code_number() -> Style {
    Style::default().fg(ACCENT_2)
}

pub fn code_comment() -> Style {
    Style::default().fg(OK).add_modifier(Modifier::ITALIC)
}

pub fn code_punctuation() -> Style {
    Style::default().fg(MUTED)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
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

pub fn thinking_message() -> Style {
    Style::default().bg(THINKING_BG)
}

/// Full-row selection (suggestion list, palette, connect picker).
/// Explicit bg — bare REVERSED is unreliable across terminals.
pub fn selected_row() -> Style {
    Style::default()
        .fg(PANEL)
        .bg(ACCENT)
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
        assert_eq!(selected_row().bg, Some(ACCENT));
        assert_eq!(caret().bg, Some(TEXT));
        assert_eq!(selected_row().fg, Some(PANEL));
    }

    #[test]
    fn conversation_backgrounds_are_distinct() {
        assert_ne!(user_message().bg, assistant_message().bg);
        assert_ne!(assistant_message().bg, thinking_message().bg);
    }
}
