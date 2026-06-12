//! Phase 1 slash command catalog (tui-commands.md).

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("unknown command `/{0}`")]
    Unknown(String),
    #[error("usage: {0}")]
    Usage(String),
    #[error("command `/{0}` requires Phase 2")]
    RequiresPhase2(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help { cmd: Option<String> },
    Status,
    Resume { session_id: Uuid },
    Cancel,
    Model { provider: Option<String>, model: Option<String> },
    Journal { tail: Option<usize> },
    Tools,
    Quit,
}

/// Parse a line that may start with `/`. Returns None if not a slash command.
pub fn parse_slash(line: &str) -> Option<Result<SlashCommand, CommandError>> {
    let line = line.trim();
    if !line.starts_with('/') {
        return None;
    }
    Some(parse_slash_inner(line))
}

fn parse_slash_inner(line: &str) -> Result<SlashCommand, CommandError> {
    let rest = line.trim_start_matches('/');
    let mut parts = rest.split_whitespace();
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
    match cmd.as_str() {
        "help" => {
            let cmd = parts.next().map(|s| s.to_string());
            Ok(SlashCommand::Help { cmd })
        }
        "status" => Ok(SlashCommand::Status),
        "resume" => {
            let id = parts
                .next()
                .ok_or_else(|| CommandError::Usage("/resume <session_id>".into()))?;
            let session_id = Uuid::parse_str(id)
                .map_err(|_| CommandError::Usage("/resume <uuid session_id>".into()))?;
            Ok(SlashCommand::Resume { session_id })
        }
        "cancel" => Ok(SlashCommand::Cancel),
        "model" => {
            let provider = parts.next().map(|s| s.to_string());
            let model = parts.next().map(|s| s.to_string());
            Ok(SlashCommand::Model { provider, model })
        }
        "journal" => {
            let tail = parts.next().and_then(|s| s.parse::<usize>().ok());
            Ok(SlashCommand::Journal { tail })
        }
        "tools" => Ok(SlashCommand::Tools),
        "quit" | "exit" => Ok(SlashCommand::Quit),
        // Phase 2 commands — explicit error, not silent no-op
        "approve" | "deny" | "reset" | "compact" | "cost" | "worktree" => {
            Err(CommandError::RequiresPhase2(cmd))
        }
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

pub fn help_text() -> &'static str {
    "Phase 1 commands:\n\
     /help [cmd]     List or detail commands\n\
     /status         Session status\n\
     /resume <id>    Resume session from journal\n\
     /cancel         Cancel current turn\n\
     /model [p] [m]  Switch provider/model (config)\n\
     /journal [n]    Tail journal events\n\
     /tools          List tools\n\
     /quit           Exit TUI\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phase1_commands() {
        assert_eq!(
            parse_slash("/status").unwrap().unwrap(),
            SlashCommand::Status
        );
        assert_eq!(
            parse_slash("/tools").unwrap().unwrap(),
            SlashCommand::Tools
        );
        assert_eq!(
            parse_slash("  /quit ").unwrap().unwrap(),
            SlashCommand::Quit
        );
        assert!(matches!(
            parse_slash("/help status").unwrap().unwrap(),
            SlashCommand::Help { cmd: Some(_) }
        ));
    }

    #[test]
    fn resume_requires_uuid() {
        let err = parse_slash("/resume").unwrap().unwrap_err();
        assert!(matches!(err, CommandError::Usage(_)));
        let err = parse_slash("/resume not-a-uuid").unwrap().unwrap_err();
        assert!(matches!(err, CommandError::Usage(_)));
    }

    #[test]
    fn resume_ok() {
        let id = Uuid::new_v4();
        let cmd = parse_slash(&format!("/resume {id}")).unwrap().unwrap();
        assert_eq!(cmd, SlashCommand::Resume { session_id: id });
    }

    #[test]
    fn phase2_commands_error() {
        for c in ["/approve", "/deny", "/reset", "/worktree", "/cost"] {
            let err = parse_slash(c).unwrap().unwrap_err();
            assert!(matches!(err, CommandError::RequiresPhase2(_)), "{c}");
        }
    }

    #[test]
    fn unknown_command() {
        let err = parse_slash("/nope").unwrap().unwrap_err();
        assert!(matches!(err, CommandError::Unknown(_)));
    }

    #[test]
    fn non_slash_is_none() {
        assert!(parse_slash("hello").is_none());
    }

    #[test]
    fn model_args() {
        let cmd = parse_slash("/model xai grok").unwrap().unwrap();
        assert_eq!(
            cmd,
            SlashCommand::Model {
                provider: Some("xai".into()),
                model: Some("grok".into()),
            }
        );
    }
}
