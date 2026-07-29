use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use forge_config::CommandConfig;
use serde::{Deserialize, Serialize};

pub const MAX_RECENT_RUNS: usize = 30;
pub const RUN_HISTORY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunHistoryFile {
    pub version: u32,
    pub repository_or_workspace_id: String,
    pub recent: Vec<RunRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunExecutionMode {
    Direct,
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunEnvironmentChange {
    Set { name: String, value: String },
    Remove { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDraft {
    pub command_input: String,
    pub working_directory: PathBuf,
    pub environment_delta: Vec<RunEnvironmentChange>,
    pub execution_mode: RunExecutionMode,
    pub source_record_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInvocation {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub environment_delta: Vec<RunEnvironmentChange>,
    pub execution_mode: RunExecutionMode,
    pub shell_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunProvenance {
    Manual,
    Recent,
    LegacyValidation,
    RepositoryShared,
    Discovered,
    AgentSuggested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    StartFailed,
    CaptureFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunFreshness {
    Current,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub invocation: RunInvocation,
    pub provenance: RunProvenance,
    pub state: RunState,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
    pub duration: Option<Duration>,
    pub exit_status: Option<i32>,
    pub spawn_error: Option<String>,
    pub output_reference: Option<String>,
    pub session_id: Option<String>,
    pub repository_or_workspace_id: String,
    pub freshness: RunFreshness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunParseError {
    EmptyCommand,
    MalformedQuoting,
    ShellSyntax(String),
}

impl std::fmt::Display for RunParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCommand => write!(f, "enter a command"),
            Self::MalformedQuoting => write!(f, "fix quoting or switch to Shell mode"),
            Self::ShellSyntax(op) => write!(
                f,
                "Direct mode does not evaluate shell syntax `{op}`; switch to Shell mode"
            ),
        }
    }
}

impl RunDraft {
    pub fn manual(working_directory: PathBuf) -> Self {
        Self {
            command_input: String::new(),
            working_directory,
            environment_delta: Vec::new(),
            execution_mode: RunExecutionMode::Direct,
            source_record_id: None,
        }
    }

    pub fn invocation(&self) -> Result<RunInvocation, RunParseError> {
        match self.execution_mode {
            RunExecutionMode::Direct => {
                let parts = parse_direct_command(&self.command_input)?;
                let mut parts = parts.into_iter();
                let executable = parts.next().ok_or(RunParseError::EmptyCommand)?;
                Ok(RunInvocation {
                    executable,
                    arguments: parts.collect(),
                    working_directory: self.working_directory.clone(),
                    environment_delta: self.environment_delta.clone(),
                    execution_mode: self.execution_mode,
                    shell_command: None,
                })
            }
            RunExecutionMode::Shell => {
                let command = self.command_input.trim();
                if command.is_empty() {
                    return Err(RunParseError::EmptyCommand);
                }
                let (shell, flag) = default_shell();
                Ok(RunInvocation {
                    executable: shell,
                    arguments: vec![flag.into(), command.into()],
                    working_directory: self.working_directory.clone(),
                    environment_delta: self.environment_delta.clone(),
                    execution_mode: self.execution_mode,
                    shell_command: Some(command.into()),
                })
            }
        }
    }
}

impl RunInvocation {
    pub fn summary(&self) -> String {
        match self.execution_mode {
            RunExecutionMode::Direct => std::iter::once(self.executable.as_str())
                .chain(self.arguments.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            RunExecutionMode::Shell => self.shell_command.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunStateModel {
    pub draft: RunDraft,
    pub current: Option<RunRecord>,
    pub recent: VecDeque<RunRecord>,
    pub legacy: Vec<RunRecord>,
    pub editing: bool,
    pub editing_directory: bool,
    pub error: Option<String>,
    next_id: u64,
}

impl RunStateModel {
    pub fn new(working_directory: PathBuf, legacy_command: Option<CommandConfig>) -> Self {
        let mut model = Self {
            draft: RunDraft::manual(working_directory.clone()),
            current: None,
            recent: VecDeque::new(),
            legacy: Vec::new(),
            editing: false,
            editing_directory: false,
            error: None,
            next_id: 1,
        };
        if let Some(command) = legacy_command {
            let record = model.record(
                RunInvocation {
                    executable: command.executable,
                    arguments: command.args,
                    working_directory,
                    environment_delta: Vec::new(),
                    execution_mode: RunExecutionMode::Direct,
                    shell_command: None,
                },
                RunProvenance::LegacyValidation,
                None,
            );
            model.legacy.push(record);
        }
        model
    }

    pub fn record(
        &mut self,
        invocation: RunInvocation,
        provenance: RunProvenance,
        session_id: Option<String>,
    ) -> RunRecord {
        let id = self.next_id.to_string();
        self.next_id += 1;
        RunRecord {
            id,
            repository_or_workspace_id: invocation.working_directory.display().to_string(),
            invocation,
            provenance,
            state: RunState::Queued,
            started_at: None,
            finished_at: None,
            duration: None,
            exit_status: None,
            spawn_error: None,
            output_reference: Some("Terminal".into()),
            session_id,
            freshness: RunFreshness::Current,
        }
    }

    pub fn remember(&mut self, record: RunRecord) {
        self.recent.push_front(record);
        self.recent.truncate(MAX_RECENT_RUNS);
    }
}

pub fn command_text(invocation: &RunInvocation) -> String {
    invocation.summary()
}

pub fn legacy_command_text(command: &CommandConfig) -> String {
    std::iter::once(command.executable.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_direct_command(input: &str) -> Result<Vec<String>, RunParseError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote = None;
    let mut token_started = false;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    token_started = true;
                }
            }
            Some(_) => {
                current.push(ch);
                token_started = true;
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                token_started = true;
            }
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    token_started = true;
                }
            }
            None if ch.is_whitespace() => {
                if token_started {
                    reject_shell_syntax(&current)?;
                    args.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            None => {
                current.push(ch);
                token_started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(RunParseError::MalformedQuoting);
    }
    if token_started {
        reject_shell_syntax(&current)?;
        args.push(current);
    }
    if args.is_empty() {
        Err(RunParseError::EmptyCommand)
    } else {
        Ok(args)
    }
}

fn reject_shell_syntax(token: &str) -> Result<(), RunParseError> {
    for op in ["&&", "||", ">>", "$(", "|", ">", "<", ";", "`"] {
        if token.contains(op) {
            return Err(RunParseError::ShellSyntax(op.into()));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn default_shell() -> (String, &'static str) {
    (
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()),
        "/C",
    )
}

#[cfg(not(windows))]
pub fn default_shell() -> (String, &'static str) {
    (
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        "-c",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(input: &str) -> Result<Vec<String>, RunParseError> {
        parse_direct_command(input)
    }

    #[test]
    fn parses_direct_commands() {
        assert_eq!(direct("cargo test").unwrap(), ["cargo", "test"]);
        assert_eq!(
            direct("cmd 'path with spaces' \"x y\"").unwrap(),
            ["cmd", "path with spaces", "x y"]
        );
        assert_eq!(direct("cmd ''").unwrap(), ["cmd", ""]);
        assert_eq!(
            direct("cmd \"a \\\"quote\\\"\"").unwrap(),
            ["cmd", "a \"quote\""]
        );
        assert_eq!(direct("cmd café").unwrap(), ["cmd", "café"]);
        assert_eq!(direct("cmd /tmp/a\\ b").unwrap(), ["cmd", "/tmp/a b"]);
        assert_eq!(
            direct(&format!("cmd {}", "x".repeat(4096))).unwrap()[1].len(),
            4096
        );
    }

    #[test]
    fn rejects_bad_direct_commands() {
        assert_eq!(direct("   ").unwrap_err(), RunParseError::EmptyCommand);
        assert_eq!(
            direct("cmd 'unterminated").unwrap_err(),
            RunParseError::MalformedQuoting
        );
        assert!(matches!(
            direct("echo hi | wc"),
            Err(RunParseError::ShellSyntax(_))
        ));
        assert!(matches!(
            direct("echo $(date)"),
            Err(RunParseError::ShellSyntax(_))
        ));
    }

    #[test]
    fn shell_mode_preserves_command_string() {
        let mut draft = RunDraft::manual(PathBuf::from("/repo"));
        draft.execution_mode = RunExecutionMode::Shell;
        draft.command_input = "echo hi | wc".into();
        let invocation = draft.invocation().unwrap();
        assert_eq!(invocation.shell_command.as_deref(), Some("echo hi | wc"));
        assert_eq!(invocation.arguments[1], "echo hi | wc");
    }

    #[test]
    fn recent_history_is_bounded_and_distinct() {
        let mut model = RunStateModel::new(PathBuf::from("/repo"), None);
        for _ in 0..35 {
            let invocation = RunInvocation {
                executable: "echo".into(),
                arguments: vec!["hi".into()],
                working_directory: PathBuf::from("/repo"),
                environment_delta: vec![RunEnvironmentChange::Set {
                    name: "A".into(),
                    value: "B".into(),
                }],
                execution_mode: RunExecutionMode::Direct,
                shell_command: None,
            };
            let record = model.record(invocation, RunProvenance::Manual, Some("s".into()));
            model.remember(record);
        }
        assert_eq!(model.recent.len(), MAX_RECENT_RUNS);
        assert_ne!(model.recent[0].id, model.recent[1].id);
        assert_eq!(model.recent[0].invocation.environment_delta.len(), 1);
    }
}
