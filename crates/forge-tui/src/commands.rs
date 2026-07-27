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
    ResumeList,
    Resume {
        session_id: Uuid,
    },
    /// Switch model. `id` is a provider/model string (`openai/gpt-4.1`) or prefix+name.
    Model {
        /// Full model id or provider prefix (when `model` is set).
        provider: Option<String>,
        model: Option<String>,
    },
    Quit,
    Compact,
    /// Phase 6 — provider connect flow
    Connect(ConnectAction),
    /// Copy last assistant message to clipboard (best-effort)
    Copy,
    /// Clear the visible transcript without deleting model context.
    Clear,
    /// Browse and read a single workspace file in readonly mode.
    File {
        path: Option<String>,
    },
    /// Disconnect from the current provider and clear stored credentials.
    Disconnect {
        profile_id: Option<String>,
    },
    /// Stage all changes, generate a commit message from the changeset, commit, and push.
    Sync,
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
        "resume" => match parts.next() {
            None => Ok(SlashCommand::ResumeList),
            Some(id) => {
                let session_id = Uuid::parse_str(id)
                    .map_err(|_| CommandError::Usage("/resume <uuid session_id>".into()))?;
                Ok(SlashCommand::Resume { session_id })
            }
        },
        "model" => {
            let a = parts.next().map(|s| s.to_string());
            let b = parts.next().map(|s| s.to_string());
            Ok(SlashCommand::Model {
                provider: a,
                model: b,
            })
        }
        "quit" | "exit" => Ok(SlashCommand::Quit),
        "disconnect" => Ok(SlashCommand::Disconnect {
            profile_id: parts.next().map(|s| s.to_string()),
        }),
        "compact" => Ok(SlashCommand::Compact),
        "connect" => {
            let rest: Vec<&str> = parts.collect();
            let args = rest.join(" ");
            forge_connect::parse_connect_args(&args)
                .map(SlashCommand::Connect)
                .map_err(|e| CommandError::Usage(e.to_string()))
        }
        "copy" => Ok(SlashCommand::Copy),
        "clear" => Ok(SlashCommand::Clear),
        "file" | "files" | "open" => Ok(SlashCommand::File {
            path: parts.next().map(|s| s.to_string()),
        }),
        "sync" => Ok(SlashCommand::Sync),
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
    fn parses_model_id() {
        assert_eq!(
            parse_slash("/model openai/gpt-4.1-mini").unwrap().unwrap(),
            SlashCommand::Model {
                provider: Some("openai/gpt-4.1-mini".into()),
                model: None,
            }
        );
        assert_eq!(
            parse_slash("/model openai gpt-4.1").unwrap().unwrap(),
            SlashCommand::Model {
                provider: Some("openai".into()),
                model: Some("gpt-4.1".into()),
            }
        );
    }

    #[test]
    fn parses_phase2_commands() {
        assert_eq!(parse_slash("/clear").unwrap().unwrap(), SlashCommand::Clear);
        assert_eq!(
            parse_slash("/file README.md").unwrap().unwrap(),
            SlashCommand::File {
                path: Some("README.md".into())
            }
        );
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
        for command in ["/cost", "/diff", "/appove", "/approve", "/deny", "/effort"] {
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
    fn parses_sync() {
        assert_eq!(parse_slash("/sync").unwrap().unwrap(), SlashCommand::Sync);
        assert!(parse_slash("/commit").unwrap().is_err());
        assert!(parse_slash("/push").unwrap().is_err());
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
