use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::git_status::GitStatusKind;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Warning,
    Error,
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

pub fn status_glyph(status: Status) -> Span<'static> {
    let (glyph, style) = match status {
        Status::Success => ("✓", theme::tool_success_style()),
        Status::Warning => ("!", theme::warn().add_modifier(Modifier::BOLD)),
        Status::Error => ("✗", theme::danger().add_modifier(Modifier::BOLD)),
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
}
