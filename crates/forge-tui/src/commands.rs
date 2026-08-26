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
    /// Show the token-budget breakdown by category (system prompt, tool
    /// schemas, messages) — the detail `/status` deliberately omits.
    Context,
    /// Open (and focus) the terminal panel. The `Ctrl+\`` chord is the fast
    /// path, but it is an unusual key to guess and appears only in the help
    /// overlay — without a palette entry the terminal is unreachable for
    /// anyone who hasn't memorised it.
    Terminal,
    /// Review changes in the workspace pane. `source` is `None` for the
    /// working tree and `Some(LastTurn)` for `/diff turn`.
    Diff {
        source: crate::diff_view::DiffSource,
    },
    /// Toggle the workspace symbol graph (`find_definition`/`find_references`)
    /// on or off. `None` reports current status without changing it.
    Graph {
        on: Option<bool>,
    },
}

impl SlashCommand {
    /// Commands that can run while a foreground model/tool turn owns the
    /// session. Lifecycle, provider, or terminal-ownership changes wait until
    /// the turn is finished (or interrupted); view-only commands stay usable.
    pub fn available_while_busy(&self) -> bool {
        !matches!(
            self,
            Self::ResumeList
                | Self::Resume { .. }
                | Self::Model
                | Self::Compact
                | Self::Connect
                | Self::Disconnect { .. }
                | Self::Edit
                | Self::Graph { .. }
        )
    }
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
        "context" | "ctx" => Ok(SlashCommand::Context),
        "diff" | "d" => match parts.next() {
            None => Ok(SlashCommand::Diff {
                source: crate::diff_view::DiffSource::WorkingTree,
            }),
            Some(arg) if arg.eq_ignore_ascii_case("turn") => Ok(SlashCommand::Diff {
                source: crate::diff_view::DiffSource::LastTurn,
            }),
            Some(_) => Err(CommandError::Usage("/diff [turn]".into())),
        },
        "terminal" | "term" | "shell" => Ok(SlashCommand::Terminal),
        "graph" => match parts.next() {
            None => Ok(SlashCommand::Graph { on: None }),
            Some(arg) if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("enable") => {
                Ok(SlashCommand::Graph { on: Some(true) })
            }
            Some(arg) if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("disable") => {
                Ok(SlashCommand::Graph { on: Some(false) })
            }
            Some(_) => Err(CommandError::Usage("/graph [on|off]".into())),
        },
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
    fn busy_turns_reject_session_mutations_but_keep_ui_commands_available() {
        for command in [
            SlashCommand::ResumeList,
            SlashCommand::Model,
            SlashCommand::Compact,
            SlashCommand::Connect,
            SlashCommand::Disconnect { profile_id: None },
            SlashCommand::Edit,
            SlashCommand::Graph { on: None },
        ] {
            assert!(!command.available_while_busy(), "{command:?}");
        }
        for command in [
            SlashCommand::Help,
            SlashCommand::Quit,
            SlashCommand::Clear,
            SlashCommand::Refresh,
            SlashCommand::ContextFile,
            SlashCommand::Theme { name: None },
            SlashCommand::Status,
            SlashCommand::Terminal,
        ] {
            assert!(command.available_while_busy(), "{command:?}");
        }
    }

    #[test]
    fn graph_parses_bare_status_and_on_off_synonyms() {
        assert_eq!(
            parse_slash("/graph").unwrap().unwrap(),
            SlashCommand::Graph { on: None }
        );
        for line in ["/graph on", "/graph enable", "/graph ON"] {
            assert_eq!(
                parse_slash(line).unwrap().unwrap(),
                SlashCommand::Graph { on: Some(true) },
                "{line}"
            );
        }
        for line in ["/graph off", "/graph disable"] {
            assert_eq!(
                parse_slash(line).unwrap().unwrap(),
                SlashCommand::Graph { on: Some(false) },
                "{line}"
            );
        }
        assert!(matches!(
            parse_slash("/graph bogus").unwrap(),
            Err(CommandError::Usage(_))
        ));
    }

    #[test]
    fn terminal_is_reachable_by_name_and_common_synonyms() {
        // `Ctrl+\`` is the fast path but an unusual chord to guess, and it
        // appears only in the help overlay — without these the terminal is
        // undiscoverable from the command palette.
        for line in ["/terminal", "/term", "/shell"] {
            assert_eq!(
                parse_slash(line).unwrap().unwrap(),
                SlashCommand::Terminal,
                "{line}"
            );
        }
    }

    #[test]
    fn every_palette_entry_parses() {
        // The palette advertises commands; a listed command that doesn't parse
        // is a dead end the user can only find by trying it.
        for item in crate::overlays::default_palette_items() {
            let parsed = parse_slash(&item.cmd)
                .unwrap_or_else(|| panic!("{} is not a slash command", item.cmd));
            assert!(
                parsed.is_ok(),
                "palette advertises unparseable {}",
                item.cmd
            );
        }
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
        assert!(parse_slash("/worktree merge").unwrap().is_err());
        assert_eq!(
            parse_slash("/disconnect").unwrap().unwrap(),
            SlashCommand::Disconnect { profile_id: None }
        );
    }

    #[test]
    fn removed_commands_are_unknown() {
        // `/diff` is deliberately absent from this list: it came back as the
        // workspace pane's review mode.
        for command in [
            "/cost", "/appove", "/approve", "/deny", "/effort", "/sync", "/copy", "/file",
            "/files", "/open",
        ] {
            assert!(matches!(
                parse_slash(command).unwrap().unwrap_err(),
                CommandError::Unknown(_)
            ));
        }
    }

    #[test]
    fn diff_parses_bare_and_with_a_source() {
        use crate::diff_view::DiffSource;
        assert_eq!(
            parse_slash("/diff").unwrap().unwrap(),
            SlashCommand::Diff {
                source: DiffSource::WorkingTree
            }
        );
        assert_eq!(
            parse_slash("/d").unwrap().unwrap(),
            SlashCommand::Diff {
                source: DiffSource::WorkingTree
            }
        );
        assert_eq!(
            parse_slash("/diff turn").unwrap().unwrap(),
            SlashCommand::Diff {
                source: DiffSource::LastTurn
            }
        );
        assert!(matches!(
            parse_slash("/diff main").unwrap(),
            Err(CommandError::Usage(_))
        ));
    }

    #[test]
    fn diff_stays_usable_while_a_turn_is_running() {
        // Reviewing what the agent just wrote, while it is still writing, is
        // the whole point of not being a full-screen modal.
        assert!(SlashCommand::Diff {
            source: crate::diff_view::DiffSource::WorkingTree
        }
        .available_while_busy());
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
    fn parses_context() {
        assert_eq!(
            parse_slash("/context").unwrap().unwrap(),
            SlashCommand::Context
        );
        assert_eq!(parse_slash("/ctx").unwrap().unwrap(), SlashCommand::Context);
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
