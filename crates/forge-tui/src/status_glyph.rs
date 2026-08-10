use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::theme;
use crate::widgets::FeedbackSeverity;
use forge_workspace::git_status::GitStatusKind;

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

    // Moved here from `git_status` when that module became
    // `forge-workspace`: it asserts how a status is *drawn*, which is
    // this module's job, not the workspace crate's.
    #[test]
    fn status_glyph_follows_theme_semantics() {
        use ratatui::style::Modifier;
        use GitStatusKind::*;

        assert_eq!(
            status_glyph(Modified.into()).style,
            crate::theme::git_modified().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Added.into()).style,
            crate::theme::git_added().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Deleted.into()).style,
            crate::theme::git_deleted().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Untracked.into()).style,
            crate::theme::git_untracked().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Ignored.into()).style,
            crate::theme::git_ignored().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            status_glyph(Conflicted.into()).style,
            crate::theme::git_deleted().add_modifier(Modifier::BOLD)
        );
    }
}
