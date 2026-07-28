//! Forge terminal UI — slash commands, full-screen ratatui, input history (Phase 7).

mod activity;
mod app;
mod commands;
mod conversation;
mod editor;
mod effort;
mod file_context;
mod file_explorer;
mod git_status;
mod history;
mod layout;
mod msg_queue;
mod overlays;
mod sidebar;
mod source_viewer;
mod terminal;
mod theme;
#[cfg(test)]
mod visual_test;
mod widgets;

pub use activity::{ActivityFeed, ActivityItem, ActivityKind};
pub use app::{run_tui, ExitSummary, TuiApp, TuiError, TuiRuntimeConfig};
pub use commands::{parse_slash, CommandError, SlashCommand};
pub use conversation::{
    BannerKind, ChatItem, ConversationModel, ConversationViewOpts, StreamWaitPhase, ToolCardState,
};
pub use effort::ReasoningEffort;
pub use file_explorer::{FileExplorer, FileExplorerWidget};
pub use history::InputHistory;
pub use layout::{
    is_too_small, split_areas, split_areas_ex, split_areas_full, LayoutRegions, MIN_HEIGHT,
    MIN_WIDTH,
};
pub use msg_queue::MessageQueue;
pub use overlays::{
    default_models, default_palette_items, filter_palette, handle_overlay_key, models_from_catalog,
    Key as OverlayKey, Overlay, OverlayAction,
};
pub use sidebar::SidebarModel;
pub use widgets::{
    classify_operator_error, session_chrome_lines, BusyPhase, FeedbackModel, FeedbackSeverity,
    FooterModel, InputModel, StatusModel,
};

/// Process exit codes.
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
