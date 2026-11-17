//! The model report: which model, provider and effort does best at each kind
//! of task, from the turn records in a workspace's session journals.
//!
//! Turns are grouped by task kind (`main`, `subagent`, `custom:<name>`) and
//! by (model, provider, effort). Each group is scored on seven metrics —
//! completion rate, escalation rate, verification pass rate, approval rate,
//! the rate of turns the user queued a message during, median tokens and
//! median active time — and the groups of a kind are ranked against each
//! other:
//!
//! - a turn a restart interrupted is left out entirely (the model did not
//!   end it);
//! - only groups with at least `min_turns` turns are ranked; the rest are
//!   listed with their numbers as not having enough data;
//! - on each metric the ranked groups are ranked best first; tied values
//!   share the mean of their positions, and a group without data for the
//!   metric (no verification commands run, no approvals asked) gets the
//!   neutral rank `(n + 1) / 2`;
//! - a group's score is the mean of its seven ranks, lower is better; ties
//!   go to more turns, then model, provider and effort by name.
//!
//! Reading never changes a journal: journals are opened read-only, the
//! journal directory is found without creating it, and a journal that
//! cannot be read is skipped and listed rather than failing the report.
//!
//! The workspace's own journals are read only when the user trusts the
//! workspace. Forge runs no session in an untrusted workspace, so journals
//! found there are someone else's — committed to the repository, say — and
//! would both fake the ranking and hand crafted SQLite files to the reader.
//! A journal directory named explicitly (an absolute path) is the user's own
//! choice and needs no trust.
//!
//! Layout: `aggregate` (turns into running totals), `rank` (a kind's groups
//! ranked), `snapshot` (totals worked out once, ranked on demand — what
//! choosing a model reads), `journals` (reading a listing's journals),
//! `since` (the `--since` argument); the workspace-level entry points are
//! here.

mod aggregate;
mod journals;
mod rank;
mod since;
mod snapshot;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use forge_config::Config;
use forge_types::TaskKind;
use serde::Serialize;

pub use aggregate::build_report;
pub(crate) use aggregate::Aggregator;
pub use journals::model_report_in;
pub(crate) use journals::read_journals;
use journals::report_of;
pub use since::parse_since;
pub use snapshot::{ranking_snapshot_in, GroupRef, RankingSnapshot};

use crate::workspace_journals::WorkspaceJournals;
pub use crate::workspace_journals::{SkippedJournal, UntrustedWorkspace};

/// Fewer turns than this and a group is listed but not ranked.
pub const DEFAULT_MIN_TURNS: usize = 5;

/// What to report on.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportOptions {
    /// Only this kind; every kind when `None`.
    pub kind: Option<TaskKind>,
    /// Only turns that ended at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Groups with fewer turns are not ranked.
    pub min_turns: usize,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            kind: None,
            since: None,
            min_turns: DEFAULT_MIN_TURNS,
        }
    }
}

/// Whether a group was ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankStatus {
    Ranked,
    /// Fewer than `min_turns` turns.
    InsufficientData,
}

/// A ranked group's rank on each metric (1 = best), the score's inputs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricRanks {
    pub completion: f64,
    pub escalation: f64,
    pub verification: f64,
    pub approval: f64,
    pub queued: f64,
    pub tokens: f64,
    pub active_time: f64,
}

/// One (model, provider, effort) within a kind: its numbers and, when it
/// has enough turns, its rank.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RankedModel {
    /// 1 = best; `None` when not ranked.
    pub rank: Option<usize>,
    pub status: RankStatus,
    /// The mean of `ranks`; lower is better.
    pub score: Option<f64>,
    pub ranks: Option<MetricRanks>,
    /// `<provider>/<model>`, old provider prefixes read as today's.
    pub model: String,
    /// The provider profile id, old route ids read as today's.
    pub provider: String,
    /// `None`: the provider's default effort.
    pub effort: Option<String>,
    /// Turns counted (restart-interrupted turns are not).
    pub turns: usize,
    pub completed: usize,
    pub completion_rate: f64,
    pub escalated: usize,
    pub escalation_rate: f64,
    /// Turns whose verification commands passed (none failed).
    pub verification_passed: usize,
    /// Turns with a failed verification command.
    pub verification_failed: usize,
    /// `passed / (passed + failed)`; `None` when no turn ran one.
    pub verification_pass_rate: Option<f64>,
    pub approvals_granted: usize,
    pub approvals_denied: usize,
    /// `granted / (granted + denied)`; `None` when none was asked.
    pub approval_rate: Option<f64>,
    /// Turns during which the user queued a message.
    pub queued_turns: usize,
    pub queued_rate: f64,
    /// Median prompt + completion tokens, over turns where both are known.
    pub median_tokens: Option<f64>,
    /// Median time not spent waiting on the user, over turns with a known
    /// duration.
    pub median_active_ms: Option<f64>,
}

/// One kind's groups, ranked first (best first), then the unranked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindReport {
    pub kind: TaskKind,
    /// Turns counted across every group of the kind.
    pub turns: usize,
    pub models: Vec<RankedModel>,
}

/// The report for a journal directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelReport {
    pub journal_dir: PathBuf,
    pub since: Option<DateTime<Utc>>,
    pub min_turns: usize,
    /// Kinds in order: main, subagent, then custom agents by name.
    pub kinds: Vec<KindReport>,
    pub skipped: Vec<SkippedJournal>,
}

// ------------------------------------------------------- the workspace

/// Where the sessions of `cfg`'s workspace keep their journals — the
/// directory [`crate::resolve_journal_dir`] gives them — found without
/// creating anything (no `.forge` directory, no Git exclude entry). When the
/// repository-local directory does not exist but the application-data
/// fallback does (the repository could not be written to), that is where
/// the sessions went.
pub fn report_journal_dir(cfg: &Config) -> PathBuf {
    crate::journal_dir(cfg, crate::JournalDirFor::Reading).0
}

/// The report for `cfg`'s workspace. Refused with [`UntrustedWorkspace`]
/// when the workspace's own journals are asked for and the user has not
/// trusted it.
pub async fn model_report(cfg: &Config, options: &ReportOptions) -> anyhow::Result<ModelReport> {
    model_report_with_trust_store(cfg, options, None).await
}

/// [`model_report`] with the trust store redirected. Only a test has a
/// reason to pass anything but `None`.
pub async fn model_report_with_trust_store(
    cfg: &Config,
    options: &ReportOptions,
    trust_store: Option<PathBuf>,
) -> anyhow::Result<ModelReport> {
    report_of(&WorkspaceJournals::for_config(cfg, trust_store), options).await
}

/// The models for one kind in `cfg`'s workspace, with their turn counts:
/// ranked best first, then those without enough turns. Empty when the kind
/// has no turns. Refused like [`model_report`] for an untrusted workspace.
pub async fn ranked_models(
    cfg: &Config,
    kind: &TaskKind,
    options: &ReportOptions,
) -> anyhow::Result<Vec<RankedModel>> {
    ranked_models_with_trust_store(cfg, kind, options, None).await
}

/// [`ranked_models`] with the trust store redirected. Only a test has a
/// reason to pass anything but `None`.
pub async fn ranked_models_with_trust_store(
    cfg: &Config,
    kind: &TaskKind,
    options: &ReportOptions,
    trust_store: Option<PathBuf>,
) -> anyhow::Result<Vec<RankedModel>> {
    let options = ReportOptions {
        kind: Some(kind.clone()),
        ..options.clone()
    };
    let report = model_report_with_trust_store(cfg, &options, trust_store).await?;
    Ok(report
        .kinds
        .into_iter()
        .find(|report| &report.kind == kind)
        .map(|report| report.models)
        .unwrap_or_default())
}
