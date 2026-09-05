//! Shared pane chrome (2026 design system, DESIGN-004).
//!
//! One focus grammar for every pane: the focused title carries the blue bold
//! `>` marker, inactive titles stay neutral, and an open modal suppresses
//! background markers so exactly one keyboard owner is visible. Pane content
//! and input routing are untouched — this is paint, not focus state.

use crate::theme;

/// Effective pane focus: a pane shows its focus marker only when it owns
/// input *and* no modal is open. Closing the modal restores the previous
/// valid owner through the existing `FocusState` transitions; this helper
/// only decides what is painted while it is open.
pub fn background_focused(pane_focused: bool, modal_open: bool) -> bool {
    pane_focused && !modal_open
}

/// Shared title row for a background pane, with modal suppression built in.
pub fn title(pane_focused: bool, modal_open: bool, label: &str) -> ratatui::text::Line<'static> {
    theme::pane_title(background_focused(pane_focused, modal_open), label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_marker_suppressed_only_by_open_modal() {
        assert!(background_focused(true, false));
        assert!(!background_focused(true, true));
        assert!(!background_focused(false, false));
        assert!(!background_focused(false, true));
    }

    #[test]
    fn title_uses_shared_marker_grammar() {
        let focused = title(true, false, "Terminal").to_string();
        assert!(focused.starts_with("> Terminal"), "{focused:?}");
        let suppressed = title(true, true, "Terminal").to_string();
        assert!(!suppressed.contains('>'), "{suppressed:?}");
        let inactive = title(false, false, "Terminal").to_string();
        assert!(!inactive.contains('>'), "{inactive:?}");
    }
}
