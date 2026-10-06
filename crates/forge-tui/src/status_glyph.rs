//! 2026 design-system state markers and text roles.
//!
//! Lifecycle grammar (`[ ] [>] [✓] [!] [-] [?] [|`), each exactly
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
    /// Three-cell marker.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::Active => "[>]",
            Self::Complete => "[✓]",
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
        Status::Success => Span::styled("[✓]", theme::tool_success_style()),
        Status::Warning => Span::styled("[?]", theme::warn().add_modifier(Modifier::BOLD)),
        Status::Error => Span::styled("[!]", theme::danger().add_modifier(Modifier::BOLD)),
        Status::Info => Span::styled("[|]", theme::info().add_modifier(Modifier::BOLD)),
        _ => unreachable!("git statuses handled above"),
    }
}

pub fn status_indicator_now(status: Status) -> Span<'static> {
    status_indicator(status, 0)
}
