use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::git_status::GitStatusKind;
use crate::theme;
use crate::widgets::FeedbackSeverity;

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

pub fn status_glyph(status: Status) -> Span<'static> {
    let (glyph, style) = match status {
        Status::Success => ("✓", theme::tool_success_style()),
        Status::Warning => ("!", theme::warn().add_modifier(Modifier::BOLD)),
        Status::Error => ("✗", theme::danger().add_modifier(Modifier::BOLD)),
        Status::Info => ("i", theme::info().add_modifier(Modifier::BOLD)),
        Status::Modified => ("M", theme::git_modified().add_modifier(Modifier::BOLD)),
        Status::Added => ("A", theme::git_added().add_modifier(Modifier::BOLD)),
        Status::Deleted => ("D", theme::git_deleted().add_modifier(Modifier::BOLD)),
        Status::Untracked => ("?", theme::git_untracked().add_modifier(Modifier::BOLD)),
        Status::Ignored => ("!", theme::git_ignored().add_modifier(Modifier::BOLD)),
        Status::Conflicted => ("U", theme::git_deleted().add_modifier(Modifier::BOLD)),
    };
    Span::styled(glyph, style)
}

/// Compact colored dot for list rows where a full letter/checkmark glyph
/// would be too wide (e.g. one entry per line in a narrow sidebar list).
pub fn status_dot(status: Status) -> Span<'static> {
    let style = match status {
        Status::Success | Status::Added => theme::ok(),
        Status::Warning | Status::Modified => theme::warn(),
        Status::Error | Status::Deleted | Status::Conflicted => theme::danger(),
        Status::Info => theme::info(),
        Status::Untracked | Status::Ignored => theme::muted(),
    };
    Span::styled("●", style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_glyphs_use_semantic_bold_styles() {
        assert_eq!(
            status_glyph(Status::Success).style,
            theme::tool_success_style()
        );
        assert_eq!(
            status_glyph(Status::Modified).style,
            theme::git_modified().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Status::Error).style,
            theme::danger().add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn status_dot_uses_plain_semantic_colors() {
        assert_eq!(status_dot(Status::Success).content, "●");
        assert_eq!(status_dot(Status::Success).style, theme::ok());
        assert_eq!(status_dot(Status::Warning).style, theme::warn());
        assert_eq!(status_dot(Status::Error).style, theme::danger());
        assert_eq!(status_dot(Status::Info).style, theme::info());
    }

    #[test]
    fn feedback_severity_maps_to_status() {
        assert_eq!(Status::from(FeedbackSeverity::Ok), Status::Success);
        assert_eq!(Status::from(FeedbackSeverity::Warn), Status::Warning);
        assert_eq!(Status::from(FeedbackSeverity::Error), Status::Error);
        assert_eq!(Status::from(FeedbackSeverity::Info), Status::Info);
    }
}
