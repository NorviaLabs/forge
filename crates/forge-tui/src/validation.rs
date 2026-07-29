use std::time::{Duration, SystemTime};

use forge_config::CommandConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationStatus {
    NotConfigured,
    NotRun,
    Running,
    Passed,
    Failed,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_not_configured() {
        let snapshot = ValidationSnapshot::default();
        assert_eq!(snapshot.display_status(), "Not configured");
    }

    #[test]
    fn configured_is_not_run() {
        let snapshot = ValidationSnapshot::configured(CommandConfig {
            executable: "cargo".into(),
            args: vec!["test".into()],
        });
        assert_eq!(snapshot.display_status(), "Not run");
    }

    #[test]
    fn completion_and_staleness_work() {
        let mut snapshot = ValidationSnapshot::configured(CommandConfig {
            executable: "cargo".into(),
            args: vec!["test".into()],
        });
        let started = SystemTime::now();
        snapshot.start(1, Some("Terminal".into()), started);
        snapshot.mark_completed(ValidationOutcome::Passed, started);
        assert_eq!(snapshot.display_status(), "Passed");
        snapshot.update_staleness(2);
        assert_eq!(snapshot.display_status(), "Stale");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationOutcome {
    Passed,
    Failed(i32),
    Cancelled,
    SpawnFailed(String),
    WaitFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationSnapshot {
    pub command: Option<CommandConfig>,
    pub status: ValidationStatus,
    pub started_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub duration: Option<Duration>,
    pub exit_code: Option<i32>,
    pub stale: bool,
    pub repo_generation: u64,
    pub output_ref: Option<String>,
}

impl Default for ValidationSnapshot {
    fn default() -> Self {
        Self {
            command: None,
            status: ValidationStatus::NotConfigured,
            started_at: None,
            completed_at: None,
            duration: None,
            exit_code: None,
            stale: false,
            repo_generation: 0,
            output_ref: None,
        }
    }
}

impl ValidationSnapshot {
    pub fn configured(command: CommandConfig) -> Self {
        Self {
            command: Some(command),
            status: ValidationStatus::NotRun,
            ..Self::default()
        }
    }

    pub fn start(
        &mut self,
        repo_generation: u64,
        output_ref: Option<String>,
        started_at: SystemTime,
    ) {
        self.status = ValidationStatus::Running;
        self.started_at = Some(started_at);
        self.completed_at = None;
        self.duration = None;
        self.exit_code = None;
        self.stale = false;
        self.repo_generation = repo_generation;
        self.output_ref = output_ref;
    }

    pub fn finish(
        &mut self,
        status: ValidationStatus,
        exit_code: Option<i32>,
        completed_at: SystemTime,
    ) {
        self.status = status;
        self.completed_at = Some(completed_at);
        self.duration = self
            .started_at
            .and_then(|start| completed_at.duration_since(start).ok());
        self.exit_code = exit_code;
    }

    pub fn mark_stale(&mut self, repo_generation: u64) {
        if matches!(
            self.status,
            ValidationStatus::Passed | ValidationStatus::Failed | ValidationStatus::Cancelled
        ) && self.repo_generation != repo_generation
        {
            self.stale = true;
        }
    }

    pub fn update_staleness(&mut self, repo_generation: u64) {
        self.mark_stale(repo_generation);
    }

    pub fn mark_completed(&mut self, outcome: ValidationOutcome, completed_at: SystemTime) {
        match outcome {
            ValidationOutcome::Passed => {
                self.finish(ValidationStatus::Passed, Some(0), completed_at)
            }
            ValidationOutcome::Failed(code) => {
                self.finish(ValidationStatus::Failed, Some(code), completed_at)
            }
            ValidationOutcome::Cancelled => {
                self.finish(ValidationStatus::Cancelled, None, completed_at)
            }
            ValidationOutcome::SpawnFailed(_) | ValidationOutcome::WaitFailed(_) => {
                self.finish(ValidationStatus::Failed, None, completed_at)
            }
        }
    }

    pub fn display_status(&self) -> &'static str {
        if self.status == ValidationStatus::NotConfigured {
            return "Not configured";
        }
        if self.stale {
            return "Stale";
        }
        match self.status {
            ValidationStatus::NotRun => "Not run",
            ValidationStatus::Running => "Running",
            ValidationStatus::Passed => "Passed",
            ValidationStatus::Failed => "Failed",
            ValidationStatus::Cancelled => "Cancelled",
            ValidationStatus::NotConfigured => "Not configured",
        }
    }
}

pub fn validation_command_text(command: &CommandConfig) -> String {
    std::iter::once(command.executable.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}
