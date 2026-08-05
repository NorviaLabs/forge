use std::borrow::Cow;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use forge_config::CommandConfig;
use forge_types::ExecutionOutcome;

const MAX_PARSE_BYTES: usize = 128_000;
const MAX_FAILED_TESTS: usize = 64;
pub const MAX_FAILED_DISPLAY: usize = 5;
const MAX_TEST_NAME_CHARS: usize = 200;

/// Pre-terminal validation-run states. Once a run finishes, its result lives
/// in `ValidationSnapshot::outcome: Option<ExecutionOutcome>` instead of a
/// parallel terminal-state enum, so pass/fail/spawn-failed/denied/cancelled/
/// timed-out can never collapse into one another again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationStatus {
    NotConfigured,
    NotRun,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationParseState {
    NotApplicable,
    Parsed,
    Partial,
    Unrecognised,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CargoTestCounts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub measured: u64,
    pub filtered_out: u64,
    pub suites: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CargoTestSummary {
    pub parse_state: ValidationParseState,
    pub counts: CargoTestCounts,
    pub failed_tests: Vec<String>,
    pub hidden_failed_tests: usize,
    pub duration_secs: Option<f64>,
    pub truncated: bool,
}

impl Default for CargoTestSummary {
    fn default() -> Self {
        Self {
            parse_state: ValidationParseState::NotApplicable,
            counts: CargoTestCounts::default(),
            failed_tests: Vec::new(),
            hidden_failed_tests: 0,
            duration_secs: None,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationSnapshot {
    pub command: Option<CommandConfig>,
    pub status: ValidationStatus,
    /// Terminal result of the most recent run. `None` while `status` is
    /// pre-terminal (`NotConfigured`/`NotRun`/`Running`).
    pub outcome: Option<ExecutionOutcome>,
    pub started_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub duration: Option<Duration>,
    pub stale: bool,
    pub repo_generation: u64,
    pub output_ref: Option<String>,
    pub cargo_summary: CargoTestSummary,
}

impl Default for ValidationSnapshot {
    fn default() -> Self {
        Self {
            command: None,
            status: ValidationStatus::NotConfigured,
            outcome: None,
            started_at: None,
            completed_at: None,
            duration: None,
            stale: false,
            repo_generation: 0,
            output_ref: None,
            cargo_summary: CargoTestSummary::default(),
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
        self.outcome = None;
        self.started_at = Some(started_at);
        self.completed_at = None;
        self.duration = None;
        self.stale = false;
        self.repo_generation = repo_generation;
        self.output_ref = output_ref;
        self.cargo_summary = CargoTestSummary::default();
    }

    /// Exit code of the most recent run, when the outcome carries one.
    pub fn exit_code(&self) -> Option<i32> {
        match &self.outcome {
            Some(ExecutionOutcome::Success) => Some(0),
            Some(ExecutionOutcome::Failed { exit_code }) => *exit_code,
            _ => None,
        }
    }

    pub fn mark_stale(&mut self, repo_generation: u64) {
        if self.outcome.is_some() && self.repo_generation != repo_generation {
            self.stale = true;
        }
    }

    pub fn update_staleness(&mut self, repo_generation: u64) {
        self.mark_stale(repo_generation);
    }

    pub fn mark_completed(&mut self, outcome: ExecutionOutcome, completed_at: SystemTime) {
        self.status = ValidationStatus::NotRun;
        self.outcome = Some(outcome);
        self.completed_at = Some(completed_at);
        self.duration = self
            .started_at
            .and_then(|start| completed_at.duration_since(start).ok());
    }

    pub fn display_status(&self) -> &'static str {
        if self.status == ValidationStatus::NotConfigured {
            return "Not configured";
        }
        if self.stale {
            return "Stale";
        }
        match self.status {
            ValidationStatus::NotConfigured => "Not configured",
            ValidationStatus::NotRun => match &self.outcome {
                None => "Not run",
                Some(ExecutionOutcome::Success) => "Passed",
                Some(ExecutionOutcome::Failed { .. }) => "Failed",
                Some(ExecutionOutcome::SpawnFailed { .. }) => "Failed to start",
                Some(ExecutionOutcome::Denied { .. }) => "Skipped",
                Some(ExecutionOutcome::Cancelled) => "Cancelled",
                Some(ExecutionOutcome::TimedOut) => "Timed out",
                // `ExecutionOutcome` is `#[non_exhaustive]`; an outcome this
                // build doesn't recognise must never read as "Passed".
                Some(_) => "Failed",
            },
            ValidationStatus::Running => "Running",
        }
    }

    pub fn parse_cargo_output(&mut self, raw_output: &str, output_truncated: bool) {
        self.cargo_summary =
            parse_cargo_test_output(self.command.as_ref(), raw_output, output_truncated);
    }
}

pub fn validation_command_text(command: &CommandConfig) -> String {
    std::iter::once(command.executable.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_cargo_test_command(command: &CommandConfig) -> bool {
    let exe = std::path::Path::new(&command.executable)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&command.executable);
    if exe != "cargo" && exe != "cargo.exe" {
        return false;
    }
    let mut args = command.args.iter().map(String::as_str);
    let first = match args.next() {
        Some(first) => first,
        None => return false,
    };
    if first.starts_with('+') {
        return matches!(args.next(), Some("test"));
    }
    first == "test"
}

pub fn parse_cargo_test_output(
    command: Option<&CommandConfig>,
    raw_output: &str,
    output_truncated: bool,
) -> CargoTestSummary {
    if !command.is_some_and(is_cargo_test_command) {
        return CargoTestSummary::default();
    }

    let normalized = normalize_for_parse(raw_output, MAX_PARSE_BYTES);
    let text = normalized.as_ref();
    let mut counts = CargoTestCounts::default();
    let mut durations = Vec::new();
    let mut parse_state = ValidationParseState::Unrecognised;
    let mut failed_tests = Vec::new();
    let mut seen_failed = HashSet::new();
    let mut in_failures_block = false;
    let mut saw_failure_header = false;

    for line in text.lines() {
        let trimmed = line.trim_end();
        let compact = trimmed.trim();
        if compact == "failures:" {
            in_failures_block = true;
            saw_failure_header = true;
            continue;
        }
        if compact.starts_with("test result:") {
            in_failures_block = false;
            if let Some((suite_counts, duration)) = parse_summary_line(compact) {
                counts.passed += suite_counts.passed;
                counts.failed += suite_counts.failed;
                counts.ignored += suite_counts.ignored;
                counts.measured += suite_counts.measured;
                counts.filtered_out += suite_counts.filtered_out;
                counts.suites += 1;
                if let Some(duration) = duration {
                    durations.push(duration);
                }
                parse_state = if output_truncated {
                    ValidationParseState::Partial
                } else {
                    ValidationParseState::Parsed
                };
            } else if matches!(parse_state, ValidationParseState::Unrecognised) {
                parse_state = ValidationParseState::Malformed;
            }
            continue;
        }
        if !in_failures_block {
            continue;
        }
        if compact.is_empty() {
            continue;
        }
        if compact.starts_with("---- ") || compact.starts_with("thread '") {
            continue;
        }
        if let Some(name) = parse_failed_test_name(compact) {
            let name = bounded_name(name);
            if seen_failed.insert(name.clone()) {
                failed_tests.push(name);
            }
        }
    }

    let total_failed = failed_tests.len();
    let shown = failed_tests
        .into_iter()
        .take(MAX_FAILED_TESTS)
        .collect::<Vec<_>>();
    let hidden_failed_tests = total_failed.saturating_sub(shown.len());

    if output_truncated && matches!(parse_state, ValidationParseState::Parsed) {
        parse_state = ValidationParseState::Partial;
    }
    if counts.suites == 0 && saw_failure_header {
        parse_state = ValidationParseState::Malformed;
    }

    CargoTestSummary {
        parse_state,
        counts,
        failed_tests: shown,
        hidden_failed_tests,
        duration_secs: if durations.is_empty() {
            None
        } else {
            Some(durations.into_iter().sum())
        },
        truncated: output_truncated,
    }
}

fn normalize_for_parse(raw: &str, max_bytes: usize) -> Cow<'_, str> {
    let stripped = strip_ansi(raw);
    let mut text = stripped.replace('\r', "\n");
    if text.len() > max_bytes {
        text.truncate(max_bytes);
    }
    Cow::Owned(text)
}

fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_summary_line(line: &str) -> Option<(CargoTestCounts, Option<f64>)> {
    let mut counts = CargoTestCounts::default();
    let mut duration = None;
    let mut saw_passed = false;
    let mut saw_failed = false;
    let (_, tail) = line.split_once('.')?;
    for part in tail.split(';').map(str::trim) {
        if let Some(value) = part.strip_suffix(" passed") {
            counts.passed = value.parse().ok()?;
            saw_passed = true;
        } else if let Some(value) = part.strip_suffix(" failed") {
            counts.failed = value.parse().ok()?;
            saw_failed = true;
        } else if let Some(value) = part.strip_suffix(" ignored") {
            counts.ignored = value.parse().ok()?;
        } else if let Some(value) = part.strip_suffix(" measured") {
            counts.measured = value.parse().ok()?;
        } else if let Some(value) = part.strip_suffix(" filtered out") {
            counts.filtered_out = value.parse().ok()?;
        } else if let Some(value) = part.strip_prefix("finished in ") {
            duration = parse_duration_secs(value);
        }
    }
    if !saw_passed || !saw_failed {
        return None;
    }
    Some((counts, duration))
}

fn parse_duration_secs(value: &str) -> Option<f64> {
    let value = value.strip_suffix('s')?;
    value.parse::<f64>().ok()
}

fn parse_failed_test_name(line: &str) -> Option<&str> {
    if line.starts_with("failures:")
        || line.starts_with("----")
        || line.contains(" at ")
        || line.ends_with(":")
    {
        return None;
    }
    let trimmed = line.trim();
    if trimmed.chars().any(char::is_whitespace) && !trimmed.contains("::") {
        return None;
    }
    Some(trimmed)
}

fn bounded_name(name: &str) -> String {
    name.chars().take(MAX_TEST_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cargo_test_command(args: &[&str]) -> CommandConfig {
        CommandConfig {
            executable: "cargo".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn cargo_command_identification() {
        assert!(is_cargo_test_command(&cargo_test_command(&["test"])));
        assert!(is_cargo_test_command(&cargo_test_command(&[
            "test",
            "--workspace"
        ])));
        assert!(is_cargo_test_command(&cargo_test_command(&[
            "+stable", "test"
        ])));
        assert!(is_cargo_test_command(&CommandConfig {
            executable: "/usr/bin/cargo".into(),
            args: vec!["test".into()]
        }));
        assert!(!is_cargo_test_command(&cargo_test_command(&["check"])));
        assert!(!is_cargo_test_command(&cargo_test_command(&["clippy"])));
        assert!(!is_cargo_test_command(&CommandConfig {
            executable: "nextest".into(),
            args: vec!["run".into()]
        }));
    }

    #[test]
    fn default_is_not_configured() {
        let snapshot = ValidationSnapshot::default();
        assert_eq!(snapshot.display_status(), "Not configured");
    }

    #[test]
    fn configured_is_not_run() {
        let snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        assert_eq!(snapshot.display_status(), "Not run");
    }

    #[test]
    fn completion_and_staleness_work() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        let started = SystemTime::now();
        snapshot.start(1, Some("Terminal".into()), started);
        snapshot.mark_completed(ExecutionOutcome::Success, started);
        assert_eq!(snapshot.display_status(), "Passed");
        snapshot.update_staleness(2);
        assert_eq!(snapshot.display_status(), "Stale");
    }

    #[test]
    fn mark_completed_success_yields_passed() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(ExecutionOutcome::Success, SystemTime::now());
        assert_eq!(snapshot.display_status(), "Passed");
        assert_eq!(snapshot.exit_code(), Some(0));
    }

    #[test]
    fn mark_completed_failed_exit_code_preserved() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(
            ExecutionOutcome::Failed {
                exit_code: Some(101),
            },
            SystemTime::now(),
        );
        assert_eq!(snapshot.display_status(), "Failed");
        assert_eq!(snapshot.exit_code(), Some(101));
    }

    #[test]
    fn mark_completed_spawn_failed_is_distinct_from_generic_failed() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(
            ExecutionOutcome::SpawnFailed {
                reason: "command not found".into(),
            },
            SystemTime::now(),
        );
        assert_eq!(snapshot.display_status(), "Failed to start");
        assert_ne!(snapshot.display_status(), "Failed");
    }

    #[test]
    fn mark_completed_denied_yields_skipped() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(
            ExecutionOutcome::Denied {
                reason: "denied by ACL".into(),
            },
            SystemTime::now(),
        );
        assert_eq!(snapshot.display_status(), "Skipped");
    }

    #[test]
    fn mark_completed_cancelled_yields_cancelled() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(ExecutionOutcome::Cancelled, SystemTime::now());
        assert_eq!(snapshot.display_status(), "Cancelled");
    }

    #[test]
    fn mark_completed_timed_out_yields_timed_out() {
        let mut snapshot = ValidationSnapshot::configured(cargo_test_command(&["test"]));
        snapshot.start(1, None, SystemTime::now());
        snapshot.mark_completed(ExecutionOutcome::TimedOut, SystemTime::now());
        assert_eq!(snapshot.display_status(), "Timed out");
    }

    #[test]
    fn parses_one_successful_suite() {
        let summary = parse_cargo_test_output(
            Some(&cargo_test_command(&["test"])),
            "test result: ok. 12 passed; 0 failed; 3 ignored; 0 measured; 2 filtered out; finished in 0.18s\n",
            false,
        );
        assert_eq!(summary.parse_state, ValidationParseState::Parsed);
        assert_eq!(summary.counts.passed, 12);
        assert_eq!(summary.counts.ignored, 3);
        assert_eq!(summary.duration_secs, Some(0.18));
    }

    #[test]
    fn parses_failed_names_list() {
        let output = "failures:\n    parser::tests::handles_unicode\n    tui::tests::renders_narrow_layout\n\ntest result: FAILED. 10 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.42s\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.failed_tests.len(), 2);
        assert_eq!(summary.failed_tests[0], "parser::tests::handles_unicode");
    }

    #[test]
    fn deduplicates_failed_names() {
        let output = "failures:\n    a::test\n    a::test\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.failed_tests, vec!["a::test"]);
    }

    #[test]
    fn carriage_return_output_parses() {
        let output = "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\r\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.counts.passed, 2);
    }

    #[test]
    fn malformed_summary_is_malformed() {
        let output = "test result: FAILED. nope\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.parse_state, ValidationParseState::Malformed);
    }

    #[test]
    fn long_failed_name_is_bounded() {
        let long_name = format!("suite::{}", "x".repeat(400));
        let output = format!(
            "failures:\n    {long_name}\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
        );
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), &output, false);
        assert!(summary.failed_tests[0].chars().count() <= MAX_TEST_NAME_CHARS);
    }

    #[test]
    fn bounded_failed_collection() {
        let mut output = String::from("failures:\n");
        for index in 0..80 {
            output.push_str(&format!("    suite::test_{index}\n"));
        }
        output.push_str("\ntest result: FAILED. 0 passed; 80 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), &output, false);
        assert_eq!(summary.failed_tests.len(), MAX_FAILED_TESTS);
        assert_eq!(summary.hidden_failed_tests, 16);
    }

    #[test]
    fn aggregates_multiple_summaries() {
        let output = "test result: ok. 10 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s\n\ntest result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.counts.passed, 30);
        assert_eq!(summary.counts.failed, 1);
        assert_eq!(summary.counts.suites, 3);
    }

    #[test]
    fn ansi_and_unicode_parse() {
        let output = "\u{1b}[32mtest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\u{1b}[0m\nfailures:\n    uni::🦀\n";
        let summary = parse_cargo_test_output(Some(&cargo_test_command(&["test"])), output, false);
        assert_eq!(summary.counts.passed, 1);
    }

    #[test]
    fn truncated_output_is_partial() {
        let summary = parse_cargo_test_output(
            Some(&cargo_test_command(&["test"])),
            "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s",
            true,
        );
        assert_eq!(summary.parse_state, ValidationParseState::Partial);
    }

    #[test]
    fn arbitrary_non_cargo_output_is_not_applicable() {
        let summary = parse_cargo_test_output(
            Some(&CommandConfig {
                executable: "bash".into(),
                args: vec!["-lc".into(), "cargo test".into()],
            }),
            "hello",
            false,
        );
        assert_eq!(summary.parse_state, ValidationParseState::NotApplicable);
    }
}
