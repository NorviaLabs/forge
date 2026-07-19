//! Slash commands — Phase 1 + Phase 2 + Phase 6 `/connect`.

use forge_connect::ConnectAction;
use thiserror::Error;
use uuid::Uuid;

use crate::effort::ReasoningEffort;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("unknown command `/{0}`")]
    Unknown(String),
    #[error("usage: {0}")]
    Usage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Status,
    Resume {
        session_id: Uuid,
    },
    /// Switch model. `id` is a LiteLLM string (`openai/gpt-4.1`) or prefix+name.
    /// `refresh` re-fetches remote catalogs for connected providers.
    Model {
        /// Full model id or provider prefix (when `model` is set).
        provider: Option<String>,
        model: Option<String>,
        /// `/model refresh` — re-pull live catalogs.
        refresh: bool,
    },
    /// Set model reasoning effort for subsequent calls.
    Effort {
        level: Option<ReasoningEffort>,
    },
    Quit,
    // Phase 2
    Approve,
    Deny,
    Compact,
    /// Phase 6 — provider connect flow
    Connect(ConnectAction),
    /// Session file/tool change summary
    Diff,
    /// Copy last assistant message to clipboard (best-effort)
    Copy,
    /// Clear the visible transcript without deleting model context.
    Clear,
    /// Disconnect from the current provider and clear stored credentials.
    Disconnect {
        profile_id: Option<String>,
    },
    /// Stage all changes, generate a commit message from the changeset, commit, and push.
    Sync,
    /// Configure STT: `/stt` status or `/stt speed fast|normal|slow`.
    Stt {
        action: SttAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttAction {
    /// Show current STT settings.
    Status,
    /// Set speed preset (affects local whisper model size for transcription).
    Speed(String),
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
        "status" => Ok(SlashCommand::Status),
        "resume" => {
            let id = parts
                .next()
                .ok_or_else(|| CommandError::Usage("/resume <session_id>".into()))?;
            let session_id = Uuid::parse_str(id)
                .map_err(|_| CommandError::Usage("/resume <uuid session_id>".into()))?;
            Ok(SlashCommand::Resume { session_id })
        }
        "model" => {
            let a = parts.next().map(|s| s.to_string());
            let b = parts.next().map(|s| s.to_string());
            if a.as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("refresh"))
            {
                Ok(SlashCommand::Model {
                    provider: None,
                    model: None,
                    refresh: true,
                })
            } else {
                Ok(SlashCommand::Model {
                    provider: a,
                    model: b,
                    refresh: false,
                })
            }
        }
        "effort" => {
            let level = parts
                .next()
                .map(|value| {
                    value.parse().map_err(|_| {
                        CommandError::Usage(format!("/effort {}", ReasoningEffort::USAGE))
                    })
                })
                .transpose()?;
            Ok(SlashCommand::Effort { level })
        }
        "quit" | "exit" => Ok(SlashCommand::Quit),
        "disconnect" => Ok(SlashCommand::Disconnect {
            profile_id: parts.next().map(|s| s.to_string()),
        }),
        "approve" => Ok(SlashCommand::Approve),
        "deny" => Ok(SlashCommand::Deny),
        "compact" => Ok(SlashCommand::Compact),
        "connect" => {
            let rest: Vec<&str> = parts.collect();
            let args = rest.join(" ");
            forge_connect::parse_connect_args(&args)
                .map(SlashCommand::Connect)
                .map_err(|e| CommandError::Usage(e.to_string()))
        }
        "diff" => Ok(SlashCommand::Diff),
        "copy" => Ok(SlashCommand::Copy),
        "clear" => Ok(SlashCommand::Clear),
        "sync" => Ok(SlashCommand::Sync),
        "stt" => {
            let sub = parts.next().unwrap_or("status").to_ascii_lowercase();
            match sub.as_str() {
                "status" | "show" => Ok(SlashCommand::Stt {
                    action: SttAction::Status,
                }),
                "speed" => {
                    let v = parts
                        .next()
                        .ok_or_else(|| CommandError::Usage("/stt speed fast|normal|slow".into()))?
                        .to_string();
                    Ok(SlashCommand::Stt {
                        action: SttAction::Speed(v),
                    })
                }
                other => Err(CommandError::Usage(format!(
                    "/stt status|speed, got {other}"
                ))),
            }
        }
        other => Err(CommandError::Unknown(other.to_string())),
    }
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
        assert!(parse_slash("/tools").unwrap().is_err());
        assert!(parse_slash("/journal").unwrap().is_err());
    }

    #[test]
    fn parses_model_refresh_and_id() {
        assert_eq!(
            parse_slash("/model refresh").unwrap().unwrap(),
            SlashCommand::Model {
                provider: None,
                model: None,
                refresh: true,
            }
        );
        assert_eq!(
            parse_slash("/model openai/gpt-4.1-mini").unwrap().unwrap(),
            SlashCommand::Model {
                provider: Some("openai/gpt-4.1-mini".into()),
                model: None,
                refresh: false,
            }
        );
        assert_eq!(
            parse_slash("/model openai gpt-4.1").unwrap().unwrap(),
            SlashCommand::Model {
                provider: Some("openai".into()),
                model: Some("gpt-4.1".into()),
                refresh: false,
            }
        );
    }

    #[test]
    fn parses_effort_level_and_query() {
        assert_eq!(
            parse_slash("/effort high").unwrap().unwrap(),
            SlashCommand::Effort {
                level: Some(ReasoningEffort::High)
            }
        );
        assert_eq!(
            parse_slash("/effort").unwrap().unwrap(),
            SlashCommand::Effort { level: None }
        );
    }

    #[test]
    fn parses_phase2_commands() {
        assert_eq!(
            parse_slash("/approve").unwrap().unwrap(),
            SlashCommand::Approve
        );
        assert_eq!(parse_slash("/deny").unwrap().unwrap(), SlashCommand::Deny);
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
    fn resume_requires_uuid() {
        assert!(matches!(
            parse_slash("/resume").unwrap().unwrap_err(),
            CommandError::Usage(_)
        ));
    }

    #[test]
    fn no_queue_slash_commands() {
        assert!(parse_slash("/queue").unwrap().is_err());
        assert!(parse_slash("/dequeue").unwrap().is_err());
    }

    #[test]
    fn parses_stt() {
        assert_eq!(
            parse_slash("/stt").unwrap().unwrap(),
            SlashCommand::Stt {
                action: SttAction::Status
            }
        );
        assert_eq!(
            parse_slash("/stt speed fast").unwrap().unwrap(),
            SlashCommand::Stt {
                action: SttAction::Speed("fast".into())
            }
        );
        assert!(parse_slash("/listen").unwrap().is_err());
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
