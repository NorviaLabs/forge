//! Slash commands — Phase 1 + Phase 2 + Phase 6 `/connect`.

use forge_connect::ConnectAction;
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
    Help { cmd: Option<String> },
    Status,
    Resume { session_id: Uuid },
    Cancel,
    Model {
        provider: Option<String>,
        model: Option<String>,
    },
    Journal { tail: Option<usize> },
    Tools,
    Quit,
    // Phase 2
    Approve,
    Deny,
    Reset,
    Compact,
    Cost,
    Worktree { action: WorktreeAction },
    /// Phase 6 — provider connect flow
    Connect(ConnectAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeAction {
    Status,
    Merge,
    Discard { confirm: bool },
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
        "help" => Ok(SlashCommand::Help {
            cmd: parts.next().map(|s| s.to_string()),
        }),
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
        "model" => Ok(SlashCommand::Model {
            provider: parts.next().map(|s| s.to_string()),
            model: parts.next().map(|s| s.to_string()),
        }),
        "journal" => Ok(SlashCommand::Journal {
            tail: parts.next().and_then(|s| s.parse().ok()),
        }),
        "tools" => Ok(SlashCommand::Tools),
        "quit" | "exit" => Ok(SlashCommand::Quit),
        "approve" => Ok(SlashCommand::Approve),
        "deny" => Ok(SlashCommand::Deny),
        "reset" => Ok(SlashCommand::Reset),
        "compact" => Ok(SlashCommand::Compact),
        "cost" => Ok(SlashCommand::Cost),
        "worktree" => {
            let sub = parts.next().unwrap_or("status").to_ascii_lowercase();
            match sub.as_str() {
                "status" => Ok(SlashCommand::Worktree {
                    action: WorktreeAction::Status,
                }),
                "merge" => Ok(SlashCommand::Worktree {
                    action: WorktreeAction::Merge,
                }),
                "discard" => {
                    let confirm = parts.any(|p| p == "--yes" || p == "-y");
                    Ok(SlashCommand::Worktree {
                        action: WorktreeAction::Discard { confirm },
                    })
                }
                other => Err(CommandError::Usage(format!(
                    "/worktree status|merge|discard, got {other}"
                ))),
            }
        }
        "connect" => {
            let rest: Vec<&str> = parts.collect();
            let args = rest.join(" ");
            forge_connect::parse_connect_args(&args)
                .map(SlashCommand::Connect)
                .map_err(|e| CommandError::Usage(e.to_string()))
        }
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

pub fn help_text() -> &'static str {
    "Commands:\n\
     /help [cmd]     List or detail commands\n\
     /status         Session status\n\
     /resume <id>    Resume session from journal\n\
     /cancel         Cancel current turn\n\
     /model [p] [m]  Switch provider/model (config)\n\
     /connect …      Connect provider (xai | opencode_go | list | status) (Phase 6)\n\
     (TUI) type /cmd in the main box + Enter; Ctrl+K opens command list (Phase 8)\n\
     /journal [n]    Tail journal events\n\
     /tools          List tools\n\
     /cost           Context usage ratio (Phase 2)\n\
     /reset          Force context handoff reset (Phase 2)\n\
     /compact        Alias guidance → /reset (Phase 2)\n\
     /approve        Approve pending HITL (Phase 2)\n\
     /deny           Deny pending HITL (Phase 2)\n\
     /worktree …     status|merge|discard [--yes] (Phase 2)\n\
     /quit           Exit\n"
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
        assert_eq!(parse_slash("/tools").unwrap().unwrap(), SlashCommand::Tools);
    }

    #[test]
    fn parses_phase2_commands() {
        assert_eq!(
            parse_slash("/approve").unwrap().unwrap(),
            SlashCommand::Approve
        );
        assert_eq!(parse_slash("/deny").unwrap().unwrap(), SlashCommand::Deny);
        assert_eq!(parse_slash("/reset").unwrap().unwrap(), SlashCommand::Reset);
        assert_eq!(parse_slash("/cost").unwrap().unwrap(), SlashCommand::Cost);
        assert_eq!(
            parse_slash("/worktree merge").unwrap().unwrap(),
            SlashCommand::Worktree {
                action: WorktreeAction::Merge
            }
        );
        assert_eq!(
            parse_slash("/worktree discard --yes").unwrap().unwrap(),
            SlashCommand::Worktree {
                action: WorktreeAction::Discard { confirm: true }
            }
        );
    }

    #[test]
    fn resume_requires_uuid() {
        assert!(matches!(
            parse_slash("/resume").unwrap().unwrap_err(),
            CommandError::Usage(_)
        ));
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
    fn parses_connect_commands() {
        use forge_connect::ConnectAction;
        assert_eq!(
            parse_slash("/connect").unwrap().unwrap(),
            SlashCommand::Connect(ConnectAction::Open)
        );
        assert_eq!(
            parse_slash("/connect list").unwrap().unwrap(),
            SlashCommand::Connect(ConnectAction::List)
        );
        assert_eq!(
            parse_slash("/connect status").unwrap().unwrap(),
            SlashCommand::Connect(ConnectAction::Status)
        );
        assert_eq!(
            parse_slash("/connect xai").unwrap().unwrap(),
            SlashCommand::Connect(ConnectAction::Connect {
                profile_id: "xai".into(),
                api_key: None,
                oauth_fixture: false,
            })
        );
        assert_eq!(
            parse_slash("/connect disconnect xai").unwrap().unwrap(),
            SlashCommand::Connect(ConnectAction::Disconnect {
                profile_id: Some("xai".into())
            })
        );
    }
}
