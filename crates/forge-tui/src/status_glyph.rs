//! 2026 design-system state markers and text roles.
//!
//! ASCII lifecycle grammar (`[ ] [>] [x] [!] [-] [?] [|`), each exactly
//! three cells in supported monospace fonts. No emoji or Nerd Font
//! dependency; every state stays legible in monochrome via glyph shape plus
//! an adjacent text label at call sites.

use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::theme;
use crate::widgets::FeedbackSeverity;
use forge_workspace::git_status::GitStatusKind;

/// Agent/tool lifecycle states (2026 glyph vocabulary).
///
/// Plan projection uses Pending/Active/Complete today; Failed/Cancelled/
/// Warning/Blocked wire into truthful tool rows in DESIGN-008.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Pending,
    Active,
    Complete,
    Failed,
    Cancelled,
    Warning,
    Blocked,
}

impl Lifecycle {
    /// Three-cell ASCII marker.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::Active => "[>]",
            Self::Complete => "[x]",
            Self::Failed => "[!]",
            Self::Cancelled => "[-]",
            Self::Warning => "[?]",
            Self::Blocked => "[|]",
        }
    }

    pub fn style(self) -> ratatui::style::Style {
        match self {
            Self::Pending => theme::muted(),
            Self::Active => theme::activity().add_modifier(Modifier::BOLD),
            // Neutral in history; call sites apply `theme::ok()` only for a
            // confirmed successful result glyph.
            Self::Complete => theme::muted(),
            Self::Failed => theme::danger().add_modifier(Modifier::BOLD),
            Self::Cancelled => theme::text_secondary(),
            Self::Warning | Self::Blocked => theme::warn().add_modifier(Modifier::BOLD),
        }
    }
}

/// Lifecycle marker span (exactly three cells).
pub fn lifecycle_marker(state: Lifecycle) -> Span<'static> {
    Span::styled(state.marker(), state.style())
}

/// Typed tool-kind labels live in the projection that consumes them for
/// standalone tool rows ([`forge_transcript::tool_kind_label`], DESIGN-008).
/// Unknown/MCP tools keep their registered name instead of being guessed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Warning,
    Error,
    Info,
    Modified,
    Added,
    Deleted,
    Untracked,
    Ignored,
    Conflicted,
}

impl From<GitStatusKind> for Status {
    fn from(status: GitStatusKind) -> Self {
        match status {
            GitStatusKind::Modified => Self::Modified,
            GitStatusKind::Added => Self::Added,
            GitStatusKind::Deleted => Self::Deleted,
            GitStatusKind::Untracked => Self::Untracked,
            GitStatusKind::Ignored => Self::Ignored,
            GitStatusKind::Conflicted => Self::Conflicted,
        }
    }
}

impl From<FeedbackSeverity> for Status {
    fn from(severity: FeedbackSeverity) -> Self {
        match severity {
            FeedbackSeverity::Ok => Self::Success,
            FeedbackSeverity::Warn => Self::Warning,
            FeedbackSeverity::Error => Self::Error,
            FeedbackSeverity::Info => Self::Info,
        }
    }
}

/// Git single-letter code with existing semantics: `M A D ? ! U`.
pub fn git_code(status: Status) -> Option<&'static str> {
    match status {
        Status::Modified => Some("M"),
        Status::Added => Some("A"),
        Status::Deleted => Some("D"),
        Status::Untracked => Some("?"),
        Status::Ignored => Some("!"),
        Status::Conflicted => Some("U"),
        _ => None,
    }
}

/// Static semantic indicator. `millis` is retained for call-site
/// compatibility but no longer drives animation: motion lives only in the
/// single active live row, never in settled markers.
pub fn status_indicator(status: Status, _millis: u128) -> Span<'static> {
    if let Some(code) = git_code(status) {
        let style = match status {
            Status::Modified => theme::git_modified().add_modifier(Modifier::BOLD),
            Status::Added => theme::git_added().add_modifier(Modifier::BOLD),
            Status::Deleted => theme::git_deleted().add_modifier(Modifier::BOLD),
            Status::Untracked => theme::git_untracked().add_modifier(Modifier::BOLD),
            Status::Ignored => theme::git_ignored().add_modifier(Modifier::BOLD),
            Status::Conflicted => theme::git_deleted().add_modifier(Modifier::BOLD),
            _ => unreachable!("git_code returned Some for non-git status"),
        };
        return Span::styled(code, style);
    }
    match status {
        Status::Success => Span::styled("[x]", theme::tool_success_style()),
        Status::Warning => Span::styled("[?]", theme::warn().add_modifier(Modifier::BOLD)),
        Status::Error => Span::styled("[!]", theme::danger().add_modifier(Modifier::BOLD)),
        Status::Info => Span::styled("[|]", theme::info().add_modifier(Modifier::BOLD)),
        _ => unreachable!("git statuses handled above"),
    }
}

pub fn status_indicator_now(status: Status) -> Span<'static> {
    status_indicator(status, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_markers_are_three_ascii_cells() {
        for state in [
            Lifecycle::Pending,
            Lifecycle::Active,
            Lifecycle::Complete,
            Lifecycle::Failed,
            Lifecycle::Cancelled,
            Lifecycle::Warning,
            Lifecycle::Blocked,
        ] {
            let marker = state.marker();
            assert_eq!(marker.len(), 3, "{state:?} marker must be 3 cells");
            assert!(marker.is_ascii(), "{state:?} marker must be ASCII");
            assert!(marker.starts_with('[') && marker.ends_with(']'));
        }
        assert_eq!(Lifecycle::Pending.marker(), "[ ]");
        assert_eq!(Lifecycle::Active.marker(), "[>]");
        assert_eq!(Lifecycle::Complete.marker(), "[x]");
        assert_eq!(Lifecycle::Failed.marker(), "[!]");
        assert_eq!(Lifecycle::Cancelled.marker(), "[-]");
        assert_eq!(Lifecycle::Warning.marker(), "[?]");
        assert_eq!(Lifecycle::Blocked.marker(), "[|]");
    }

    #[test]
    fn active_marker_uses_activity_token_not_warning() {
        assert_eq!(Lifecycle::Active.style().fg, Some(theme::activity_color()));
    }

    #[test]
    fn tool_kind_labels_cover_vocabulary_and_preserve_unknown() {
        use forge_transcript::tool_kind_label;
        assert_eq!(tool_kind_label("read_file"), "Read");
        assert_eq!(tool_kind_label("search"), "Search");
        assert_eq!(tool_kind_label("exec_command"), "Shell");
        assert_eq!(tool_kind_label("git_status"), "Git");
        assert_eq!(tool_kind_label("edit_file"), "Edit");
        assert_eq!(tool_kind_label("run_tests"), "Check");
        assert_eq!(tool_kind_label("web_fetch"), "Web");
        assert_eq!(tool_kind_label("update_plan"), "Plan");
        // Unknown/MCP tools keep their registered name.
        assert_eq!(tool_kind_label("mcp__custom_tool"), "mcp__custom_tool");
    }

    #[test]
    fn status_indicators_use_ascii_grammar_without_animation() {
        // `millis` must not change settled markers.
        assert_eq!(
            status_indicator(Status::Success, 0).content,
            status_indicator(Status::Success, 9999).content
        );
        assert_eq!(status_indicator(Status::Success, 0).content, "[x]");
        assert_eq!(status_indicator(Status::Warning, 0).content, "[?]");
        assert_eq!(status_indicator(Status::Error, 0).content, "[!]");
    }

    #[test]
    fn git_statuses_render_single_letter_codes() {
        assert_eq!(status_indicator(Status::Modified, 0).content, "M");
        assert_eq!(status_indicator(Status::Added, 0).content, "A");
        assert_eq!(status_indicator(Status::Deleted, 0).content, "D");
        assert_eq!(status_indicator(Status::Untracked, 0).content, "?");
        assert_eq!(status_indicator(Status::Ignored, 0).content, "!");
        assert_eq!(status_indicator(Status::Conflicted, 0).content, "U");
    }

    #[test]
    fn status_indicators_use_semantic_bold_styles() {
        assert_eq!(
            status_indicator(Status::Success, 0).style,
            theme::tool_success_style()
        );
        assert_eq!(
            status_indicator(Status::Modified, 0).style,
            theme::git_modified().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Status::Error, 0).style,
            theme::danger().add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn feedback_severity_maps_to_status() {
        assert_eq!(Status::from(FeedbackSeverity::Ok), Status::Success);
        assert_eq!(Status::from(FeedbackSeverity::Warn), Status::Warning);
        assert_eq!(Status::from(FeedbackSeverity::Error), Status::Error);
        assert_eq!(Status::from(FeedbackSeverity::Info), Status::Info);
    }
}

#[cfg(test)]
mod git_theme_tests {
    use super::*;
    use GitStatusKind::*;

    // Asserts how a status is *drawn*, which is this module's job.
    #[test]
    fn status_indicator_follows_theme_semantics() {
        use ratatui::style::Modifier;

        assert_eq!(
            status_indicator(Modified.into(), 0).style,
            crate::theme::git_modified().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Added.into(), 0).style,
            crate::theme::git_added().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Deleted.into(), 0).style,
            crate::theme::git_deleted().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Untracked.into(), 0).style,
            crate::theme::git_untracked().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Ignored.into(), 0).style,
            crate::theme::git_ignored().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_indicator(Conflicted.into(), 0).style,
            crate::theme::git_deleted().add_modifier(Modifier::BOLD)
        );
    }
}
