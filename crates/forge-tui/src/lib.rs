//! TUI commands (tui-commands.md) and lightweight surface helpers (surfaces.md Phase 1).

mod commands;

pub use commands::{help_text, parse_slash, CommandError, SlashCommand};

/// Headless exit codes (surfaces.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Failed = 1,
    // 2 reserved for Phase 2 awaiting_hitl
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
        assert_eq!(ExitCode::Canceled.code(), 3);
        assert_eq!(ExitCode::ConfigError.code(), 4);
    }
}
