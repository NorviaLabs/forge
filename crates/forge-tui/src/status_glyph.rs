use ratatui::style::Modifier;
use ratatui::text::Span;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Render semantic status as a compact animated word rather than a symbolic glyph.
/// Cycling dots provide motion while the word keeps the meaning explicit.
pub fn status_indicator(status: Status, millis: u128) -> Span<'static> {
    let (label, style) = match status {
        Status::Success => ("OK", theme::tool_success_style()),
        Status::Warning => ("WAIT", theme::warn().add_modifier(Modifier::BOLD)),
        Status::Error => ("ERR", theme::danger().add_modifier(Modifier::BOLD)),
        Status::Info => ("INFO", theme::info().add_modifier(Modifier::BOLD)),
        Status::Modified => ("MOD", theme::git_modified().add_modifier(Modifier::BOLD)),
        Status::Added => ("ADD", theme::git_added().add_modifier(Modifier::BOLD)),
        Status::Deleted => ("DEL", theme::git_deleted().add_modifier(Modifier::BOLD)),
        Status::Untracked => ("NEW", theme::git_untracked().add_modifier(Modifier::BOLD)),
        Status::Ignored => ("SKIP", theme::git_ignored().add_modifier(Modifier::BOLD)),
        Status::Conflicted => ("MERGE", theme::git_deleted().add_modifier(Modifier::BOLD)),
    };
    let dots = ["   ", ".  ", ".. ", "..."][(millis / 140) as usize % 4];
    Span::styled(format!("{label}{dots}"), style)
}

pub fn status_indicator_now(status: Status) -> Span<'static> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    status_indicator(status, millis)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // Moved here from `git_status` when that module became
    // `forge-workspace`: it asserts how a status is *drawn*, which is
    // this module's job, not the workspace crate's.
    #[test]
    fn status_indicator_follows_theme_semantics() {
        use ratatui::style::Modifier;
        use GitStatusKind::*;

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
