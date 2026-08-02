//! Runtime-owned completion evaluation.
//!
//! Forge must never mark a turn `Completed` because the model *said* it was
//! done. This module is the single place that turns runtime-observed
//! [`ExecutionEvidence`] into a final [`TaskLifecycle`] for a turn, given what
//! the turn was expected to accomplish (a [`TaskExpectation`]). It has no
//! `async`, no filesystem access, and no dependency on the model's own text —
//! callers (see `AgentSession::apply_model_response` in `lib.rs`) are
//! responsible for collecting evidence from real tool results and filesystem
//! state before calling [`CompletionEvaluator::evaluate`].
//!
//! `TaskLifecycle` (from `forge_types`) is reused as the decision's state
//! instead of introducing a parallel `TaskState` enum.

use forge_types::TaskLifecycle;

/// What a turn was expected to accomplish, derived from the tool calls the
/// model actually issued this turn (see `classify_turn` in `lib.rs`) — not
/// from free-text intent inference over the user's request.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskExpectation {
    /// No filesystem/git/tool side effects required; a valid final answer is
    /// sufficient.
    ReadOnly,
    /// One or more search operations (file-search, content-search) must
    /// finish, regardless of match count.
    Search { required_operations: usize },
    /// One or more external commands must run and succeed.
    ToolExecution {
        required_tools: Vec<ToolExpectation>,
    },
    /// One or more filesystem mutations must be verified against actual
    /// filesystem state.
    FileEdit {
        expected_effects: Vec<FileEffectExpectation>,
    },
    /// One or more git commands must succeed, with their repository effect
    /// verified where practical.
    GitOperation {
        expected_effects: Vec<GitEffectExpectation>,
    },
    /// The runtime could not confidently classify this turn. Fail-safe: this
    /// never resolves to `Completed`.
    Unclassifiable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExpectation {
    /// Ties this expectation to the `ToolCall.id` that must satisfy it.
    pub operation_id: String,
    pub tool_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEffectKind {
    Created,
    Modified,
    Deleted,
    /// Source path expected gone, destination path expected present. No
    /// builtin tool produces this today (Forge has no rename tool); kept for
    /// spec completeness and unit coverage.
    Renamed,
    DirectoryCreated,
    PatchApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffectExpectation {
    /// Ties this expectation to the `ToolCall.id` that attempted it.
    pub operation_id: String,
    pub path: String,
    pub kind: FileEffectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitEffectKind {
    CommitCreated,
    Staged,
    BranchChanged,
    Restored,
    /// Effect isn't practical to verify for this subcommand (e.g. `push`,
    /// `fetch`); a successful command exit is sufficient.
    CommandOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEffectExpectation {
    /// Ties this expectation to the `ToolCall.id` that issued it.
    pub operation_id: String,
    pub command: String,
    pub effect: GitEffectKind,
}

/// A factual, runtime-observed execution outcome. Never derived from model
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEvent {
    AssistantResponseProduced,
    SearchFinished,
    SearchFailed,
    ToolStarted,
    ToolFinished,
    ToolFailed,
    FileCreated,
    FileWritten,
    PatchApplied,
    PatchRejected,
    FileRenamed,
    FileDeleted,
    DirectoryCreated,
    GitCommandSucceeded,
    GitCommandFailed,
    RuntimeFailed,
    UserCancelled,
    WaitingForUser,
}

/// One observed fact, with enough metadata to associate it with the
/// expectation it satisfies (or fails to satisfy).
///
/// For file-effect evidence, `checksum_after` follows one convention:
/// `Some(hash)` means the runtime confirmed the path exists with that content
/// hash; `None` means the runtime confirmed the path does *not* exist. It is
/// never used as an "unknown / not checked" placeholder — callers that can't
/// verify simply omit the entry's involvement in a `FileEdit`/`GitOperation`
/// match, which the evaluator treats as unverified (fails safe).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvidenceEntry {
    pub operation_id: Option<String>,
    pub tool_name: Option<String>,
    pub path: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub checksum_before: Option<u64>,
    pub checksum_after: Option<u64>,
    pub git_command: Option<String>,
    /// `Some(true)`/`Some(false)` when the runtime attempted to verify the
    /// requested git repository effect; `None` when verification wasn't
    /// practical for this subcommand (still counts as satisfied, per "verify
    /// where practical").
    pub git_effect_verified: Option<bool>,
    /// Result-count metadata (e.g. search matches). Not used to gate
    /// success/failure, only to build a human summary.
    pub count: Option<usize>,
    pub seq: u64,
    event: Option<ExecutionEvent>,
}

impl EvidenceEntry {
    pub fn new(event: ExecutionEvent) -> Self {
        Self {
            event: Some(event),
            ..Default::default()
        }
    }

    pub fn event(&self) -> ExecutionEvent {
        self.event
            .expect("EvidenceEntry::new always sets `event`; field is private to keep it required")
    }

    pub fn operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }
    pub fn tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }
    pub fn error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }
    pub fn checksum_before(mut self, hash: Option<u64>) -> Self {
        self.checksum_before = hash;
        self
    }
    pub fn checksum_after(mut self, hash: Option<u64>) -> Self {
        self.checksum_after = hash;
        self
    }
    pub fn git_command(mut self, command: impl Into<String>) -> Self {
        self.git_command = Some(command.into());
        self
    }
    pub fn git_effect_verified(mut self, verified: Option<bool>) -> Self {
        self.git_effect_verified = verified;
        self
    }
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }
    pub fn seq(mut self, seq: u64) -> Self {
        self.seq = seq;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionEvidence(pub Vec<EvidenceEntry>);

impl ExecutionEvidence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: EvidenceEntry) {
        self.0.push(entry);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn find(&self, operation_id: &str, path: Option<&str>) -> Option<&EvidenceEntry> {
        self.0.iter().find(|e| {
            e.operation_id.as_deref() == Some(operation_id)
                && (path.is_none() || e.path.as_deref() == path)
        })
    }

    fn any(&self, event: ExecutionEvent) -> bool {
        self.0.iter().any(|e| e.event() == event)
    }
}

/// Machine-readable reason for a [`CompletionDecision`]. Also doubles as the
/// `category` fed to `AgentSession::finalize_turn_failure`, so it must stay
/// short, snake_case, and free of raw payload text (see
/// [`CompletionReason::as_category`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionReason {
    NoChangesRequired,
    SearchSucceeded,
    SearchExecutionFailed,
    ToolSucceeded,
    ToolExitedNonZero,
    ToolNotInvoked,
    EditVerified,
    NoEditEvidence,
    EditVerificationFailed,
    PartialFailure,
    PatchRejected,
    GitEffectVerified,
    GitCommandFailed,
    GitEffectNotObserved,
    RuntimeFailure,
    ExpectationUnclassifiable,
    Cancelled,
    AwaitingApproval,
    /// The model's final text contains what looks like an unparsed/malformed
    /// tool-call attempt (a real tool name used in call-shaped syntax,
    /// e.g. inside a JSON-ish blob) rather than a genuine final answer, and
    /// no structured tool call actually executed. Never resolves to
    /// `Completed` — see `AgentSession::apply_model_response` in `lib.rs`.
    DanglingToolCallText,
}

impl CompletionReason {
    pub fn as_category(&self) -> &'static str {
        match self {
            Self::NoChangesRequired => "no_changes_required",
            Self::SearchSucceeded => "search_succeeded",
            Self::SearchExecutionFailed => "search_failed",
            Self::ToolSucceeded => "tool_succeeded",
            Self::ToolExitedNonZero => "tool_exited_nonzero",
            Self::ToolNotInvoked => "tool_not_invoked",
            Self::EditVerified => "edit_verified",
            Self::NoEditEvidence => "no_edit_evidence",
            Self::EditVerificationFailed => "edit_verification_failed",
            Self::PartialFailure => "partial_failure",
            Self::PatchRejected => "patch_rejected",
            Self::GitEffectVerified => "git_effect_verified",
            Self::GitCommandFailed => "git_command_failed",
            Self::GitEffectNotObserved => "git_effect_not_observed",
            Self::RuntimeFailure => "runtime_failure",
            Self::ExpectationUnclassifiable => "expectation_unclassifiable",
            Self::Cancelled => "cancelled",
            Self::AwaitingApproval => "awaiting_approval",
            Self::DanglingToolCallText => "dangling_tool_call_text",
        }
    }
}

/// A structured, pre-rendered explanation the UI can show without
/// re-deriving anything from assistant text or raw tool payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceSummary {
    /// Short machine strings, e.g. `"write_file:src/a.rs"`.
    pub succeeded: Vec<String>,
    pub failed: Vec<String>,
    /// The one line the UI shows, e.g.
    /// "Updated 3 files, but 2 required edits failed."
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionDecision {
    pub state: TaskLifecycle,
    pub reason: CompletionReason,
    pub evidence_summary: EvidenceSummary,
}

pub trait CompletionEvaluator {
    fn evaluate(
        &self,
        expectation: &TaskExpectation,
        evidence: &ExecutionEvidence,
    ) -> CompletionDecision;
}

pub struct DefaultCompletionEvaluator;

impl CompletionEvaluator for DefaultCompletionEvaluator {
    fn evaluate(
        &self,
        expectation: &TaskExpectation,
        evidence: &ExecutionEvidence,
    ) -> CompletionDecision {
        // Runtime facts that override any category-specific reasoning.
        if evidence.any(ExecutionEvent::RuntimeFailed) {
            return decision(
                TaskLifecycle::Failed,
                CompletionReason::RuntimeFailure,
                "Forge hit a runtime error before finishing this turn.",
            );
        }
        if evidence.any(ExecutionEvent::UserCancelled) {
            return decision(
                TaskLifecycle::Interrupted,
                CompletionReason::Cancelled,
                "Cancelled by user.",
            );
        }
        if evidence.any(ExecutionEvent::WaitingForUser) {
            return decision(
                TaskLifecycle::Waiting,
                CompletionReason::AwaitingApproval,
                "Waiting for approval.",
            );
        }

        match expectation {
            TaskExpectation::Unclassifiable => decision(
                TaskLifecycle::Failed,
                CompletionReason::ExpectationUnclassifiable,
                "Could not confidently determine what this task required, so it was not marked complete.",
            ),
            TaskExpectation::ReadOnly => evaluate_read_only(evidence),
            TaskExpectation::Search { required_operations } => {
                evaluate_search(*required_operations, evidence)
            }
            TaskExpectation::ToolExecution { required_tools } => {
                evaluate_tool_execution(required_tools, evidence)
            }
            TaskExpectation::FileEdit { expected_effects } => {
                evaluate_file_edit(expected_effects, evidence)
            }
            TaskExpectation::GitOperation { expected_effects } => {
                evaluate_git_operation(expected_effects, evidence)
            }
        }
    }
}

fn decision(state: TaskLifecycle, reason: CompletionReason, detail: &str) -> CompletionDecision {
    CompletionDecision {
        state,
        reason,
        evidence_summary: EvidenceSummary {
            succeeded: Vec::new(),
            failed: Vec::new(),
            detail: detail.to_string(),
        },
    }
}

fn evaluate_read_only(evidence: &ExecutionEvidence) -> CompletionDecision {
    if evidence.any(ExecutionEvent::AssistantResponseProduced) {
        decision(
            TaskLifecycle::Completed,
            CompletionReason::NoChangesRequired,
            "No changes required.",
        )
    } else {
        decision(
            TaskLifecycle::Failed,
            CompletionReason::NoEditEvidence,
            "Forge couldn't complete this turn.",
        )
    }
}

fn evaluate_search(required_operations: usize, evidence: &ExecutionEvidence) -> CompletionDecision {
    let finished: Vec<&EvidenceEntry> = evidence
        .0
        .iter()
        .filter(|e| e.event() == ExecutionEvent::SearchFinished)
        .collect();
    let failed_count = evidence
        .0
        .iter()
        .filter(|e| e.event() == ExecutionEvent::SearchFailed)
        .count();

    if failed_count > 0 {
        return decision(
            TaskLifecycle::Failed,
            CompletionReason::SearchExecutionFailed,
            "Search execution failed.",
        );
    }
    if required_operations == 0 || finished.len() < required_operations {
        return decision(
            TaskLifecycle::Failed,
            CompletionReason::SearchExecutionFailed,
            "Search did not complete.",
        );
    }
    let matches: usize = finished.iter().filter_map(|e| e.count).sum();
    decision(
        TaskLifecycle::Completed,
        CompletionReason::SearchSucceeded,
        &format!(
            "Search completed with {matches} match{}.",
            if matches == 1 { "" } else { "es" }
        ),
    )
}

fn evaluate_tool_execution(
    required_tools: &[ToolExpectation],
    evidence: &ExecutionEvidence,
) -> CompletionDecision {
    if required_tools.is_empty() {
        return decision(
            TaskLifecycle::Failed,
            CompletionReason::ToolNotInvoked,
            "No required command ran.",
        );
    }
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for expectation in required_tools {
        match evidence.find(&expectation.operation_id, None) {
            Some(entry)
                if entry.event() == ExecutionEvent::ToolFinished && entry.exit_code == Some(0) =>
            {
                ok.push(expectation.tool_name.clone());
            }
            Some(entry)
                if matches!(
                    entry.event(),
                    ExecutionEvent::ToolFinished | ExecutionEvent::ToolFailed
                ) =>
            {
                // `exit_code` is only ever `None` for calls the runtime
                // refused to execute at all (HITL/ACL denial) — nothing ever
                // ran, so "exited with code unknown" is actively misleading
                // (it implies a process started and its result is merely
                // unknown). Say plainly that it didn't run instead.
                failed.push(match entry.exit_code {
                    Some(code) => format!("{} exited with code {code}", expectation.tool_name),
                    None => format!("{} was not run (denied or blocked)", expectation.tool_name),
                });
            }
            _ => failed.push(format!("{} did not run", expectation.tool_name)),
        }
    }

    let total = required_tools.len();
    if failed.is_empty() {
        decision_with_lists(
            TaskLifecycle::Completed,
            CompletionReason::ToolSucceeded,
            format!(
                "{} command{} completed successfully.",
                total,
                if total == 1 { "" } else { "s" }
            ),
            ok,
            failed,
        )
    } else if ok.is_empty() {
        let reason = if failed.iter().all(|f| f.ends_with("did not run")) {
            CompletionReason::ToolNotInvoked
        } else {
            CompletionReason::ToolExitedNonZero
        };
        decision_with_lists(
            TaskLifecycle::Failed,
            reason,
            failed.join("; ") + ".",
            ok,
            failed,
        )
    } else {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::PartialFailure,
            format!(
                "{} of {total} required commands succeeded; {} failed.",
                ok.len(),
                failed.join(", ")
            ),
            ok,
            failed,
        )
    }
}

fn file_effect_verified(kind: FileEffectKind, entry: &EvidenceEntry) -> bool {
    match kind {
        FileEffectKind::Created
        | FileEffectKind::PatchApplied
        | FileEffectKind::DirectoryCreated => {
            entry.event() != ExecutionEvent::PatchRejected && entry.checksum_after.is_some()
        }
        // Covers both genuine modification (before/after both present and
        // different) and creation via `write_file`/patch Add-File, where
        // there is no "before" at all (`checksum_before` is `None`) — the
        // comparison `None != Some(_)` already treats that as changed.
        FileEffectKind::Modified => {
            entry.event() != ExecutionEvent::PatchRejected
                && entry.checksum_after.is_some()
                && entry.checksum_before != entry.checksum_after
        }
        FileEffectKind::Deleted => entry.checksum_after.is_none(),
        FileEffectKind::Renamed => {
            entry.checksum_before.is_none() && entry.checksum_after.is_some()
        }
    }
}

fn evaluate_file_edit(
    expected_effects: &[FileEffectExpectation],
    evidence: &ExecutionEvidence,
) -> CompletionDecision {
    if expected_effects.is_empty() {
        return decision(
            TaskLifecycle::Failed,
            CompletionReason::NoEditEvidence,
            "No file modifications were successfully applied.",
        );
    }

    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut any_patch_rejected = None;
    for expectation in expected_effects {
        match evidence.find(&expectation.operation_id, Some(&expectation.path)) {
            Some(entry) if file_effect_verified(expectation.kind, entry) => {
                ok.push(expectation.path.clone());
            }
            Some(entry) => {
                if entry.event() == ExecutionEvent::PatchRejected {
                    any_patch_rejected.get_or_insert(expectation.path.clone());
                }
                failed.push(expectation.path.clone());
            }
            None => failed.push(expectation.path.clone()),
        }
    }

    let total = expected_effects.len();
    if failed.is_empty() {
        decision_with_lists(
            TaskLifecycle::Completed,
            CompletionReason::EditVerified,
            format!("Updated {total} file{}.", if total == 1 { "" } else { "s" }),
            ok,
            failed,
        )
    } else if let Some(path) = any_patch_rejected.filter(|_| ok.is_empty() && failed.len() == 1) {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::PatchRejected,
            format!("Patch could not be applied to {path}."),
            ok,
            failed,
        )
    } else if ok.is_empty() {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::NoEditEvidence,
            "No file modifications were successfully applied.",
            ok,
            failed,
        )
    } else {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::PartialFailure,
            format!(
                "Updated {} file{}, but {} required edit{} failed.",
                ok.len(),
                if ok.len() == 1 { "" } else { "s" },
                failed.len(),
                if failed.len() == 1 { "" } else { "s" }
            ),
            ok,
            failed,
        )
    }
}

fn evaluate_git_operation(
    expected_effects: &[GitEffectExpectation],
    evidence: &ExecutionEvidence,
) -> CompletionDecision {
    if expected_effects.is_empty() {
        return decision(
            TaskLifecycle::Failed,
            CompletionReason::GitCommandFailed,
            "No git operation ran.",
        );
    }

    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut not_observed = false;
    for expectation in expected_effects {
        match evidence.find(&expectation.operation_id, None) {
            Some(entry) if entry.event() == ExecutionEvent::GitCommandSucceeded => {
                if entry.git_effect_verified == Some(false) {
                    not_observed = true;
                    failed.push(expectation.command.clone());
                } else {
                    ok.push(expectation.command.clone());
                }
            }
            _ => failed.push(expectation.command.clone()),
        }
    }

    let total = expected_effects.len();
    if failed.is_empty() {
        decision_with_lists(
            TaskLifecycle::Completed,
            CompletionReason::GitEffectVerified,
            format!(
                "Completed {total} git operation{}.",
                if total == 1 { "" } else { "s" }
            ),
            ok,
            failed,
        )
    } else if ok.is_empty() && not_observed && failed.len() == 1 {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::GitEffectNotObserved,
            format!(
                "git {} exited successfully, but the expected repository effect did not occur.",
                failed[0]
            ),
            ok,
            failed,
        )
    } else if ok.is_empty() {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::GitCommandFailed,
            format!("git {} failed.", failed.join(", ")),
            ok,
            failed,
        )
    } else {
        decision_with_lists(
            TaskLifecycle::Failed,
            CompletionReason::PartialFailure,
            format!(
                "{} of {total} required git operations succeeded; {} failed.",
                ok.len(),
                failed.join(", ")
            ),
            ok,
            failed,
        )
    }
}

fn decision_with_lists(
    state: TaskLifecycle,
    reason: CompletionReason,
    detail: impl Into<String>,
    succeeded: Vec<String>,
    failed: Vec<String>,
) -> CompletionDecision {
    CompletionDecision {
        state,
        reason,
        evidence_summary: EvidenceSummary {
            succeeded,
            failed,
            detail: detail.into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expectation: &TaskExpectation, evidence: &ExecutionEvidence) -> CompletionDecision {
        DefaultCompletionEvaluator.evaluate(expectation, evidence)
    }

    // 1. Model says "Done" but no write occurred: FileEdit expectation, only
    // an assistant-response entry in evidence, no matching file evidence.
    #[test]
    fn file_edit_with_no_matching_evidence_fails() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/a.rs".into(),
                kind: FileEffectKind::Modified,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(EvidenceEntry::new(
            ExecutionEvent::AssistantResponseProduced,
        ));
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::NoEditEvidence);
    }

    // 2. Patch tool returns failure.
    #[test]
    fn patch_rejected_fails() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/auth.rs".into(),
                kind: FileEffectKind::PatchApplied,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::PatchRejected)
                .operation_id("call-1")
                .path("src/auth.rs")
                .error("hunk did not match"),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::PatchRejected);
        assert_eq!(
            d.evidence_summary.detail,
            "Patch could not be applied to src/auth.rs."
        );
    }

    // 3. File write reports success but expected file does not exist
    // afterward: checksum_after is None (runtime confirmed absence).
    #[test]
    fn write_reported_success_but_file_missing_fails() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/new.rs".into(),
                kind: FileEffectKind::Created,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::FileCreated)
                .operation_id("call-1")
                .path("src/new.rs")
                .checksum_after(None),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
    }

    // 4. File contents remain unchanged after an edit task.
    #[test]
    fn unchanged_content_on_modify_fails() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/a.rs".into(),
                kind: FileEffectKind::Modified,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::FileWritten)
                .operation_id("call-1")
                .path("src/a.rs")
                .checksum_before(Some(42))
                .checksum_after(Some(42)),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        // Zero of one verified -> NoEditEvidence, not PartialFailure.
        assert_eq!(d.reason, CompletionReason::NoEditEvidence);
    }

    // 5. Search succeeds with zero matches.
    #[test]
    fn zero_match_search_succeeds() {
        let expectation = TaskExpectation::Search {
            required_operations: 1,
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::SearchFinished)
                .operation_id("call-1")
                .count(0),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Completed);
        assert_eq!(d.reason, CompletionReason::SearchSucceeded);
        assert_eq!(
            d.evidence_summary.detail,
            "Search completed with 0 matches."
        );
    }

    // 6. Tool exits non-zero.
    #[test]
    fn nonzero_exit_fails_with_code_in_message() {
        let expectation = TaskExpectation::ToolExecution {
            required_tools: vec![ToolExpectation {
                operation_id: "call-1".into(),
                tool_name: "cargo test".into(),
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::ToolFinished)
                .operation_id("call-1")
                .exit_code(101),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::ToolExitedNonZero);
        assert!(d
            .evidence_summary
            .detail
            .contains("cargo test exited with code 101"));
    }

    /// F-RECOVERY-02: a denied (never-executed) call left `exit_code: None`,
    /// which previously rendered as "{tool} exited with code unknown" —
    /// contradicting the model's own honest "was not run because denied"
    /// message shown right next to it. `exit_code: None` must read as "did
    /// not run", not as an indeterminate exit from a process that started.
    #[test]
    fn denied_call_reads_as_not_run_not_exited_with_unknown_code() {
        let expectation = TaskExpectation::ToolExecution {
            required_tools: vec![ToolExpectation {
                operation_id: "call-1".into(),
                tool_name: "bash".into(),
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::ToolFailed)
                .operation_id("call-1")
                .error("HITL denied by tui"),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert!(
            d.evidence_summary.detail.contains("bash was not run"),
            "{}",
            d.evidence_summary.detail
        );
        assert!(
            !d.evidence_summary.detail.contains("exited with code"),
            "{}",
            d.evidence_summary.detail
        );
    }

    // 7. Tool succeeds but was unrelated to the expected operation (no
    // evidence entry for the required operation_id).
    #[test]
    fn unrelated_tool_success_does_not_satisfy_expectation() {
        let expectation = TaskExpectation::ToolExecution {
            required_tools: vec![ToolExpectation {
                operation_id: "call-cargo".into(),
                tool_name: "cargo test".into(),
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::ToolFinished)
                .operation_id("call-unrelated")
                .exit_code(0),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::ToolNotInvoked);
    }

    // 8. Three of five required edits succeed -> Failed, never Completed.
    #[test]
    fn partial_edit_success_fails() {
        let expected_effects: Vec<FileEffectExpectation> = (1..=5)
            .map(|i| FileEffectExpectation {
                operation_id: format!("call-{i}"),
                path: format!("src/f{i}.rs"),
                kind: FileEffectKind::Created,
            })
            .collect();
        let expectation = TaskExpectation::FileEdit { expected_effects };
        let mut evidence = ExecutionEvidence::new();
        for i in 1..=3 {
            evidence.push(
                EvidenceEntry::new(ExecutionEvent::FileCreated)
                    .operation_id(format!("call-{i}"))
                    .path(format!("src/f{i}.rs"))
                    .checksum_after(Some(1)),
            );
        }
        for i in 4..=5 {
            evidence.push(
                EvidenceEntry::new(ExecutionEvent::FileCreated)
                    .operation_id(format!("call-{i}"))
                    .path(format!("src/f{i}.rs"))
                    .checksum_after(None),
            );
        }
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::PartialFailure);
        assert_eq!(
            d.evidence_summary.detail,
            "Updated 3 files, but 2 required edits failed."
        );
    }

    // 9. User cancels while a tool is running: evidence-level proof that
    // `UserCancelled` never routes to `Completed` (real cancellation in
    // `AgentSession` goes through `mark_cancelled`, bypassing this evaluator
    // entirely — see the integration test `cancel_never_completes`).
    #[test]
    fn user_cancelled_evidence_never_completes() {
        let expectation = TaskExpectation::ToolExecution {
            required_tools: vec![ToolExpectation {
                operation_id: "call-1".into(),
                tool_name: "bash".into(),
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(EvidenceEntry::new(ExecutionEvent::UserCancelled).operation_id("call-1"));
        let d = eval(&expectation, &evidence);
        assert_ne!(d.state, TaskLifecycle::Completed);
        assert_eq!(d.state, TaskLifecycle::Interrupted);
    }

    // 10. Runtime crashes after one successful edit.
    #[test]
    fn runtime_failure_fails_even_with_prior_success() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/a.rs".into(),
                kind: FileEffectKind::Created,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::FileCreated)
                .operation_id("call-1")
                .path("src/a.rs")
                .checksum_after(Some(1)),
        );
        evidence.push(EvidenceEntry::new(ExecutionEvent::RuntimeFailed));
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::RuntimeFailure);
    }

    // 11 / read-only edge case: read-only response completes without any
    // tool call.
    #[test]
    fn read_only_completes_without_tools() {
        let mut evidence = ExecutionEvidence::new();
        evidence.push(EvidenceEntry::new(
            ExecutionEvent::AssistantResponseProduced,
        ));
        let d = eval(&TaskExpectation::ReadOnly, &evidence);
        assert_eq!(d.state, TaskLifecycle::Completed);
        assert_eq!(d.reason, CompletionReason::NoChangesRequired);
    }

    // 12. Task waits for approval.
    #[test]
    fn waiting_for_user_yields_awaiting_hitl() {
        let mut evidence = ExecutionEvidence::new();
        evidence.push(EvidenceEntry::new(ExecutionEvent::WaitingForUser));
        let d = eval(&TaskExpectation::ReadOnly, &evidence);
        assert_eq!(d.state, TaskLifecycle::Waiting);
        assert_ne!(d.state, TaskLifecycle::Completed);
    }

    // 13. Rename source disappears but destination is missing.
    #[test]
    fn rename_with_missing_destination_fails() {
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/new_name.rs".into(),
                kind: FileEffectKind::Renamed,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::FileRenamed)
                .operation_id("call-1")
                .path("src/new_name.rs")
                .checksum_before(None) // source confirmed gone
                .checksum_after(None), // destination NOT found either
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
    }

    // 14. Git command exits successfully but the expected effect did not
    // occur (e.g. `git commit` with nothing staged).
    #[test]
    fn git_success_without_observed_effect_fails() {
        let expectation = TaskExpectation::GitOperation {
            expected_effects: vec![GitEffectExpectation {
                operation_id: "call-1".into(),
                command: "commit".into(),
                effect: GitEffectKind::CommitCreated,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::GitCommandSucceeded)
                .operation_id("call-1")
                .git_command("commit")
                .git_effect_verified(Some(false)),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::GitEffectNotObserved);
    }

    #[test]
    fn git_command_failure_fails() {
        let expectation = TaskExpectation::GitOperation {
            expected_effects: vec![GitEffectExpectation {
                operation_id: "call-1".into(),
                command: "push".into(),
                effect: GitEffectKind::CommandOnly,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::GitCommandFailed)
                .operation_id("call-1")
                .git_command("push")
                .error("rejected"),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::GitCommandFailed);
    }

    #[test]
    fn git_effect_verified_completes() {
        let expectation = TaskExpectation::GitOperation {
            expected_effects: vec![GitEffectExpectation {
                operation_id: "call-1".into(),
                command: "commit".into(),
                effect: GitEffectKind::CommitCreated,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        evidence.push(
            EvidenceEntry::new(ExecutionEvent::GitCommandSucceeded)
                .operation_id("call-1")
                .git_command("commit")
                .git_effect_verified(Some(true)),
        );
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Completed);
        assert_eq!(d.reason, CompletionReason::GitEffectVerified);
    }

    #[test]
    fn unclassifiable_never_completes() {
        let mut evidence = ExecutionEvidence::new();
        evidence.push(EvidenceEntry::new(
            ExecutionEvent::AssistantResponseProduced,
        ));
        let d = eval(&TaskExpectation::Unclassifiable, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
        assert_eq!(d.reason, CompletionReason::ExpectationUnclassifiable);
    }

    #[test]
    fn assistant_text_alone_cannot_complete_file_edit() {
        // The exact invariant: a turn with abundant "narration" evidence
        // (AssistantResponseProduced) but zero matching FileEdit evidence
        // must still fail.
        let expectation = TaskExpectation::FileEdit {
            expected_effects: vec![FileEffectExpectation {
                operation_id: "call-1".into(),
                path: "src/a.rs".into(),
                kind: FileEffectKind::Created,
            }],
        };
        let mut evidence = ExecutionEvidence::new();
        for _ in 0..5 {
            evidence.push(EvidenceEntry::new(
                ExecutionEvent::AssistantResponseProduced,
            ));
        }
        let d = eval(&expectation, &evidence);
        assert_eq!(d.state, TaskLifecycle::Failed);
    }
}
