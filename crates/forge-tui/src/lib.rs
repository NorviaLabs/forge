//! Forge terminal UI — slash commands + Phase 4 shell, conversation, sidebar (TUI-01–03).

mod commands;
mod conversation;
mod layout;
mod sidebar;
mod theme;
mod widgets;

pub use commands::{help_text, parse_slash, CommandError, SlashCommand, WorktreeAction};
pub use conversation::{ChatItem, ConversationModel, ToolCardState};
pub use layout::{is_too_small, split_areas, LayoutRegions, MIN_HEIGHT, MIN_WIDTH};
pub use sidebar::SidebarModel;
pub use widgets::{FooterModel, InputModel, StatusModel};

/// Headless / process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Failed = 1,
    AwaitingHitl = 2,
    Canceled = 3,
    ConfigError = 4,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_design() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::Failed.code(), 1);
        assert_eq!(ExitCode::AwaitingHitl.code(), 2);
        assert_eq!(ExitCode::Canceled.code(), 3);
        assert_eq!(ExitCode::ConfigError.code(), 4);
    }
}
