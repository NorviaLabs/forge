//! Slash commands — Phase 1 + Phase 2 + Phase 6 `/connect`.

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("unknown command `/{0}`")]
    Unknown(String),
    #[error("usage: {0}")]
    Usage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// Open the help overlay — the same one the empty composer's `?`
    /// shortcut opens, just reachable without knowing that shortcut exists.
    Help,
    ResumeList,
    Resume {
        session_id: Uuid,
    },
    /// Open the interactive model picker.
    Model,
    Quit,
    Compact,
    /// Open the interactive route connection picker.
    Connect,
    /// Clear the visible transcript without deleting model context.
    Clear,
    /// Disconnect from the current provider and clear stored credentials.
    Disconnect {
        profile_id: Option<String>,
    },
    /// Refresh the file explorer's git status cache.
    Refresh,
    /// Open the active file in the external editor.
    Edit,
    /// Attach the current active file to the next user message.
    ContextFile,
    /// Switch presentation theme (`dark`, `light`, `system`).
    Theme {
        name: Option<String>,
    },
    /// Show session status overlay.
    Status,
}

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
        "help" | "?" => Ok(SlashCommand::Help),
        "resume" => match parts.next() {
            None => Ok(SlashCommand::ResumeList),
            Some(id) => {
                let session_id = Uuid::parse_str(id)
                    .map_err(|_| CommandError::Usage("/resume <uuid session_id>".into()))?;
                Ok(SlashCommand::Resume { session_id })
            }
        },
        "model" => {
            if parts.next().is_some() {
                Err(CommandError::Usage("/model".into()))
            } else {
                Ok(SlashCommand::Model)
            }
        }
        "quit" | "exit" => Ok(SlashCommand::Quit),
        "disconnect" => Ok(SlashCommand::Disconnect {
            profile_id: parts.next().map(|s| s.to_string()),
        }),
        "compact" => Ok(SlashCommand::Compact),
        "connect" => {
            if parts.next().is_some() {
                Err(CommandError::Usage("/connect".into()))
            } else {
                Ok(SlashCommand::Connect)
            }
        }
        "clear" => Ok(SlashCommand::Clear),
        "refresh" => Ok(SlashCommand::Refresh),
        "edit" => Ok(SlashCommand::Edit),
        "context-file" | "context_file" | "cf" => Ok(SlashCommand::ContextFile),
        "theme" => Ok(SlashCommand::Theme {
            name: parts.next().map(|s| s.to_string()),
        }),
        "status" => Ok(SlashCommand::Status),
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phase1_commands() {
        assert!(parse_slash("/tools").unwrap().is_err());
        assert!(parse_slash("/journal").unwrap().is_err());
    }

    #[test]
    fn model_is_an_argument_free_picker_command() {
        assert_eq!(parse_slash("/model").unwrap().unwrap(), SlashCommand::Model);
        assert!(matches!(
            parse_slash("/model openai/gpt-4.1-mini").unwrap(),
            Err(CommandError::Usage(_))
        ));
    }

    #[test]
    fn parses_phase2_commands() {
        assert_eq!(parse_slash("/clear").unwrap().unwrap(), SlashCommand::Clear);
        assert!(parse_slash("/reset").unwrap().is_err());
        assert!(parse_slash("/context").unwrap().is_err());
        assert!(parse_slash("/worktree merge").unwrap().is_err());
        assert_eq!(
            parse_slash("/disconnect").unwrap().unwrap(),
            SlashCommand::Disconnect { profile_id: None }
        );
    }

    #[test]
    fn removed_commands_are_unknown() {
        for command in [
            "/cost", "/diff", "/appove", "/approve", "/deny", "/effort", "/sync", "/copy", "/file",
            "/files", "/open",
        ] {
            assert!(matches!(
                parse_slash(command).unwrap().unwrap_err(),
                CommandError::Unknown(_)
            ));
        }
    }

    #[test]
    fn bare_resume_lists_sessions() {
        assert_eq!(
            parse_slash("/resume").unwrap().unwrap(),
            SlashCommand::ResumeList
        );
    }

    #[test]
    fn no_queue_slash_commands() {
        assert!(parse_slash("/queue").unwrap().is_err());
        assert!(parse_slash("/dequeue").unwrap().is_err());
    }

    #[test]
    fn parses_edit() {
        assert_eq!(parse_slash("/edit").unwrap().unwrap(), SlashCommand::Edit);
    }

    #[test]
    fn parses_context_file() {
        assert_eq!(
            parse_slash("/context-file").unwrap().unwrap(),
            SlashCommand::ContextFile
        );
        assert_eq!(
            parse_slash("/context_file").unwrap().unwrap(),
            SlashCommand::ContextFile
        );
        assert_eq!(
            parse_slash("/cf").unwrap().unwrap(),
            SlashCommand::ContextFile
        );
    }

    #[test]
    fn unknown_command() {
        assert!(matches!(
            parse_slash("/nope").unwrap().unwrap_err(),
            CommandError::Unknown(_)
        ));
    }

    #[test]
    fn non_slash_is_none() {
        assert!(parse_slash("hello").is_none());
    }

    #[test]
    fn parses_status() {
        assert_eq!(
            parse_slash("/status").unwrap().unwrap(),
            SlashCommand::Status
        );
    }

    #[test]
    fn parses_theme() {
        assert_eq!(
            parse_slash("/theme").unwrap().unwrap(),
            SlashCommand::Theme { name: None }
        );
        assert_eq!(
            parse_slash("/theme light").unwrap().unwrap(),
            SlashCommand::Theme {
                name: Some("light".into())
            }
        );
    }

    #[test]
    fn parses_connect_commands() {
        assert_eq!(
            parse_slash("/connect").unwrap().unwrap(),
            SlashCommand::Connect
        );
        assert_eq!(
            parse_slash("/connect xai").unwrap().unwrap_err(),
            CommandError::Usage("/connect".into())
        );
    }
}
