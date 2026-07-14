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
    /// Switch model. `id` is a LiteLLM string (`openai/gpt-4.1`) or prefix+name.
    /// `refresh` re-fetches remote catalogs for connected providers.
    Model {
        /// Full model id or provider prefix (when `model` is set).
        provider: Option<String>,
        model: Option<String>,
        /// `/model refresh` — re-pull live catalogs.
        refresh: bool,
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
    /// Session file/tool change summary
    Diff,
    /// Copy last assistant message to clipboard (best-effort)
    Copy,
    /// Clear chat banners / notices / soft reset of UI chrome
    Clear,
    /// Toggle compact density
    Density,
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
        "model" => {
            let a = parts.next().map(|s| s.to_string());
            let b = parts.next().map(|s| s.to_string());
            if a.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("refresh")) {
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
        "diff" => Ok(SlashCommand::Diff),
        "copy" => Ok(SlashCommand::Copy),
        "clear" => Ok(SlashCommand::Clear),
        "density" => Ok(SlashCommand::Density),
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
                        .ok_or_else(|| {
                            CommandError::Usage("/stt speed fast|normal|slow".into())
                        })?
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

pub fn help_text() -> &'static str {
    "Commands:\n\
     /help [cmd]     List or detail commands\n\
     /status         Session status\n\
     /resume <id>    Resume session from journal\n\
     /cancel         Soft-cancel current turn (Esc)\n\
     /model [id]     Switch model (LiteLLM id) · /model refresh for catalogs\n\
     /connect …      Connect (xai | opencode_go | opencode_zen | openai | anthropic | ollama)\n\
     /diff           Tools & file changes this session\n\
     /sync           Stage, commit (message from changeset), push\n\
     /stt [speed …]  STT status/speed · hold Ctrl+Space to dictate\n\
     /copy           Copy last assistant answer (clipboard)\n\
     /clear          Clear banners / notices\n\
     /density        Toggle compact layout\n\
     /journal [n]    Tail journal events\n\
     /tools          List tools\n\
     /cost           Session token usage (prompt/completion/context)\n\
     /reset          Force context handoff reset\n\
     /compact        Alias → /reset\n\
     /approve        Approve pending HITL (a)\n\
     /deny           Deny pending HITL (d)\n\
     /worktree …     status|merge|discard [--yes]\n\
     /quit           Exit\n\
     \n\
     Keys: Enter send · ⇧Enter newline · Ctrl+T thinking · Ctrl+O tool ·\n\
           Ctrl+B sidebar · Ctrl+K commands · Esc interrupt/clear · Ctrl+C quit\n"
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
