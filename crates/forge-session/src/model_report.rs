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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use forge_config::Config;
use std::sync::Arc;

use forge_durable::{TurnRecord, TurnRecordFilter, TurnRecordView};
use forge_types::TaskKind;
use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::workspace_journals::{JournalListing, WorkspaceJournals};
pub use crate::workspace_journals::{SkippedJournal, UntrustedWorkspace};

/// Fewer turns than this and a group is listed but not ranked.
pub const DEFAULT_MIN_TURNS: usize = 5;

/// The completion reason of a turn a restart interrupted.
const INTERRUPTED_BY_RESTART: &str = "interrupted_by_restart";

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

// ------------------------------------------------------------- the report

/// The provider profile a stored id names today: an old route id
/// (`anthropic-api`) or an old model prefix (`openai-codex`, `grok`) reads as
/// its profile; anything else as itself.
fn provider_name(stored: &str) -> String {
    let stored = stored.trim();
    let routed = forge_connect::profile_id_for_stored_route(stored);
    if routed != stored {
        return routed.to_string();
    }
    forge_connect::normalize_saved_model_id(&format!("{stored}/_"))
        .split_once('/')
        .map(|(profile, _)| profile.to_string())
        .unwrap_or_else(|| stored.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    model: String,
    provider: String,
    effort: Option<String>,
}

impl GroupKey {
    fn of(model: &str, profile_id: &str, effort: Option<&str>) -> Self {
        Self {
            model: forge_connect::normalize_saved_model_id(model),
            provider: provider_name(profile_id),
            effort: effort
                .map(str::trim)
                .filter(|effort| !effort.is_empty())
                .map(str::to_string),
        }
    }
}

/// What a turn's signals say, for the measures.
#[derive(Debug, Default, Clone, Copy)]
struct SignalCounts {
    verification_passed: bool,
    verification_failed: bool,
    approvals_granted: usize,
    approvals_denied: usize,
    queued: bool,
}

impl SignalCounts {
    fn count<'k>(kinds: impl IntoIterator<Item = &'k str>) -> Self {
        let mut counts = Self::default();
        for kind in kinds {
            match kind {
                "verification_passed" => counts.verification_passed = true,
                "verification_failed" => counts.verification_failed = true,
                "approval_granted" => counts.approvals_granted += 1,
                "approval_denied" => counts.approvals_denied += 1,
                "user_message_queued" => counts.queued = true,
                _ => {}
            }
        }
        counts
    }

    /// From a stored signals array, reading only each signal's `kind`.
    fn parse(json: &str) -> Result<Self, serde_json::Error> {
        #[derive(serde::Deserialize)]
        struct Kind<'a> {
            #[serde(borrow)]
            kind: std::borrow::Cow<'a, str>,
        }
        let kinds: Vec<Kind<'_>> = serde_json::from_str(json)?;
        Ok(Self::count(kinds.iter().map(|signal| signal.kind.as_ref())))
    }
}

/// What one turn contributes to the report: from a stored record, or from a
/// row scanned out of a journal.
struct TurnFacts<'a> {
    kind: &'a TaskKind,
    ended_at: DateTime<Utc>,
    model: &'a str,
    profile_id: &'a str,
    effort: Option<&'a str>,
    completed: bool,
    interrupted_by_restart: bool,
    escalated: bool,
    tokens: Option<u64>,
    active_ms: Option<u64>,
    signals: SignalCounts,
}

fn tokens(prompt: Option<u64>, completion: Option<u64>) -> Option<u64> {
    Some(prompt?.saturating_add(completion?))
}

/// Time not spent waiting on the user; unknown waiting is none.
fn active_ms(duration: Option<u64>, waiting: Option<u64>) -> Option<u64> {
    Some(duration?.saturating_sub(waiting.unwrap_or(0)))
}

impl<'a> TurnFacts<'a> {
    fn of_record(record: &'a TurnRecord) -> Self {
        let kinds = record
            .signals
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|signal| signal.get("kind").and_then(|kind| kind.as_str()));
        Self {
            kind: &record.task_kind,
            ended_at: record.ended_at,
            model: &record.model,
            profile_id: &record.profile_id,
            effort: record.reasoning_effort.as_deref(),
            completed: record.outcome == "completed",
            interrupted_by_restart: record.completion_reason == INTERRUPTED_BY_RESTART,
            escalated: record.escalated,
            tokens: tokens(record.prompt_tokens, record.completion_tokens),
            active_ms: active_ms(record.duration_ms, record.waiting_ms),
            signals: SignalCounts::count(kinds),
        }
    }

    fn of_view(view: &TurnRecordView<'a>) -> Result<Self, serde_json::Error> {
        Ok(Self {
            kind: view.task_kind,
            ended_at: view.ended_at,
            model: view.model,
            profile_id: view.profile_id,
            effort: view.reasoning_effort,
            completed: view.outcome == "completed",
            interrupted_by_restart: view.completion_reason == INTERRUPTED_BY_RESTART,
            escalated: view.escalated,
            tokens: tokens(view.prompt_tokens, view.completion_tokens),
            active_ms: active_ms(view.duration_ms, view.waiting_ms),
            signals: SignalCounts::parse(view.signals)?,
        })
    }
}

/// What a group's turns add up to. Only the two numbers per turn the
/// medians need are kept; everything else is a count.
#[derive(Debug, Default, Clone)]
struct Tally {
    turns: usize,
    completed: usize,
    escalated: usize,
    verification_passed: usize,
    verification_failed: usize,
    approvals_granted: usize,
    approvals_denied: usize,
    queued_turns: usize,
    tokens: Vec<f64>,
    active_ms: Vec<f64>,
}

impl Tally {
    fn add(&mut self, turn: &TurnFacts<'_>) {
        self.turns += 1;
        self.completed += usize::from(turn.completed);
        self.escalated += usize::from(turn.escalated);
        if turn.signals.verification_failed {
            self.verification_failed += 1;
        } else if turn.signals.verification_passed {
            self.verification_passed += 1;
        }
        self.approvals_granted += turn.signals.approvals_granted;
        self.approvals_denied += turn.signals.approvals_denied;
        self.queued_turns += usize::from(turn.signals.queued);
        if let Some(tokens) = turn.tokens {
            self.tokens.push(tokens as f64);
        }
        if let Some(active) = turn.active_ms {
            self.active_ms.push(active as f64);
        }
    }

    fn merge(&mut self, other: Tally) {
        self.turns += other.turns;
        self.completed += other.completed;
        self.escalated += other.escalated;
        self.verification_passed += other.verification_passed;
        self.verification_failed += other.verification_failed;
        self.approvals_granted += other.approvals_granted;
        self.approvals_denied += other.approvals_denied;
        self.queued_turns += other.queued_turns;
        self.tokens.extend(other.tokens);
        self.active_ms.extend(other.active_ms);
    }

    fn into_model(self, key: GroupKey) -> RankedModel {
        let turns = self.turns.max(1) as f64;
        let ratio = |part: usize, whole: usize| (whole > 0).then(|| part as f64 / whole as f64);
        RankedModel {
            rank: None,
            status: RankStatus::InsufficientData,
            score: None,
            ranks: None,
            model: key.model,
            provider: key.provider,
            effort: key.effort,
            turns: self.turns,
            completed: self.completed,
            completion_rate: self.completed as f64 / turns,
            escalated: self.escalated,
            escalation_rate: self.escalated as f64 / turns,
            verification_passed: self.verification_passed,
            verification_failed: self.verification_failed,
            verification_pass_rate: ratio(
                self.verification_passed,
                self.verification_passed + self.verification_failed,
            ),
            approvals_granted: self.approvals_granted,
            approvals_denied: self.approvals_denied,
            approval_rate: ratio(
                self.approvals_granted,
                self.approvals_granted + self.approvals_denied,
            ),
            queued_turns: self.queued_turns,
            queued_rate: self.queued_turns as f64 / turns,
            median_tokens: median(self.tokens),
            median_active_ms: median(self.active_ms),
        }
    }
}

/// The middle value; the mean of the middle two for an even count.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// Values this close are a tie (rates computed from different counts, such
/// as 1/6 and 2/12, may differ in the last bit).
const TIE: f64 = 1e-9;

/// The rank of each value (1 = best), `None` values getting the neutral
/// rank `(n + 1) / 2`. Tied values share the mean of their positions.
fn fractional_ranks(values: &[Option<f64>], higher_is_better: bool) -> Vec<f64> {
    let n = values.len();
    let neutral = (n as f64 + 1.0) / 2.0;
    let mut ranks = vec![neutral; n];
    let mut present: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.map(|value| (i, value)))
        .collect();
    present.sort_by(|a, b| {
        let order = a.1.total_cmp(&b.1);
        if higher_is_better {
            order.reverse()
        } else {
            order
        }
    });
    let mut start = 0;
    while start < present.len() {
        let mut end = start + 1;
        while end < present.len() && (present[end].1 - present[start].1).abs() < TIE {
            end += 1;
        }
        // Positions start+1 ..= end share their mean.
        let shared = (start + 1 + end) as f64 / 2.0;
        for (index, _) in &present[start..end] {
            ranks[*index] = shared;
        }
        start = end;
    }
    ranks
}

/// Rank the groups with enough turns and order the kind: ranked best first,
/// then the rest by turns and name.
fn rank_kind(kind: TaskKind, groups: BTreeMap<GroupKey, Tally>, min_turns: usize) -> KindReport {
    let models = groups
        .into_iter()
        .map(|(key, tally)| tally.into_model(key))
        .collect();
    rank_models(kind, models, min_turns)
}

/// Rank groups whose numbers are worked out (not yet ranked): the ones with
/// enough turns best first, then the rest by turns and name.
fn rank_models(kind: TaskKind, models: Vec<RankedModel>, min_turns: usize) -> KindReport {
    let turns = models.iter().map(|model| model.turns).sum();
    let (mut ranked, mut unranked): (Vec<RankedModel>, Vec<RankedModel>) = models
        .into_iter()
        .partition(|model| model.turns >= min_turns);

    let metric = |pick: fn(&RankedModel) -> Option<f64>, higher_is_better: bool| {
        let values: Vec<Option<f64>> = ranked.iter().map(pick).collect();
        fractional_ranks(&values, higher_is_better)
    };
    let completion = metric(|m| Some(m.completion_rate), true);
    let escalation = metric(|m| Some(m.escalation_rate), false);
    let verification = metric(|m| m.verification_pass_rate, true);
    let approval = metric(|m| m.approval_rate, true);
    let queued = metric(|m| Some(m.queued_rate), false);
    let tokens = metric(|m| m.median_tokens, false);
    let active_time = metric(|m| m.median_active_ms, false);
    for (i, model) in ranked.iter_mut().enumerate() {
        let ranks = MetricRanks {
            completion: completion[i],
            escalation: escalation[i],
            verification: verification[i],
            approval: approval[i],
            queued: queued[i],
            tokens: tokens[i],
            active_time: active_time[i],
        };
        let sum = ranks.completion
            + ranks.escalation
            + ranks.verification
            + ranks.approval
            + ranks.queued
            + ranks.tokens
            + ranks.active_time;
        model.score = Some(sum / 7.0);
        model.ranks = Some(ranks);
        model.status = RankStatus::Ranked;
    }

    let by_turns_then_name = |a: &RankedModel, b: &RankedModel| {
        b.turns
            .cmp(&a.turns)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.effort.cmp(&b.effort))
    };
    ranked.sort_by(|a, b| {
        let (sa, sb) = (a.score.unwrap_or(f64::MAX), b.score.unwrap_or(f64::MAX));
        if (sa - sb).abs() < TIE {
            by_turns_then_name(a, b)
        } else {
            sa.total_cmp(&sb)
        }
    });
    for (i, model) in ranked.iter_mut().enumerate() {
        model.rank = Some(i + 1);
    }
    unranked.sort_by(by_turns_then_name);
    ranked.extend(unranked);
    KindReport {
        kind,
        turns,
        models: ranked,
    }
}

/// Distinct raw (model, provider, effort) names remembered with their
/// normalized group, so names are normalized once, not once per turn. Past
/// this many, further names are normalized each time (no unbounded growth).
const KEY_CACHE: usize = 64;

/// Running totals: turns are added one at a time, as they are read, and
/// totals from separate journals merged.
#[derive(Debug, Default)]
pub(crate) struct Aggregator {
    kinds: BTreeMap<TaskKind, BTreeMap<GroupKey, Tally>>,
    keys: Vec<(String, String, Option<String>, GroupKey)>,
}

impl Aggregator {
    fn add(&mut self, turn: &TurnFacts<'_>, options: &ReportOptions) {
        if turn.interrupted_by_restart
            || options.since.is_some_and(|since| turn.ended_at < since)
            || options.kind.as_ref().is_some_and(|kind| kind != turn.kind)
        {
            return;
        }
        let cached = self.keys.iter().position(|(model, profile, effort, _)| {
            model == turn.model && profile == turn.profile_id && effort.as_deref() == turn.effort
        });
        let key = match cached {
            Some(index) => self.keys[index].3.clone(),
            None => {
                let key = GroupKey::of(turn.model, turn.profile_id, turn.effort);
                if self.keys.len() < KEY_CACHE {
                    self.keys.push((
                        turn.model.to_string(),
                        turn.profile_id.to_string(),
                        turn.effort.map(str::to_string),
                        key.clone(),
                    ));
                }
                key
            }
        };
        if !self.kinds.contains_key(turn.kind) {
            self.kinds.insert(turn.kind.clone(), BTreeMap::new());
        }
        if let Some(groups) = self.kinds.get_mut(turn.kind) {
            groups.entry(key).or_default().add(turn);
        }
    }

    /// Add `other`'s totals, leaving it as it is.
    pub(crate) fn merge_from(&mut self, other: &Aggregator) {
        for (kind, groups) in &other.kinds {
            let mine = self.kinds.entry(kind.clone()).or_default();
            for (key, tally) in groups {
                mine.entry(key.clone()).or_default().merge(tally.clone());
            }
        }
    }

    fn merge(&mut self, other: Aggregator) {
        for (kind, groups) in other.kinds {
            let mine = self.kinds.entry(kind).or_default();
            for (key, tally) in groups {
                mine.entry(key).or_default().merge(tally);
            }
        }
    }

    fn finish(self, min_turns: usize) -> Vec<KindReport> {
        self.kinds
            .into_iter()
            .map(|(kind, groups)| rank_kind(kind, groups, min_turns))
            .collect()
    }
}

/// The report for `records`: one entry per kind with turns, in kind order.
pub fn build_report(
    records: impl IntoIterator<Item = TurnRecord>,
    options: &ReportOptions,
) -> Vec<KindReport> {
    let mut totals = Aggregator::default();
    for record in records {
        totals.add(&TurnFacts::of_record(&record), options);
    }
    totals.finish(options.min_turns)
}

// ---------------------------------------------------------------- snapshots

/// A group of turns as a snapshot names it: normalized model and provider,
/// and effort (`None`: the provider's default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupRef<'a> {
    pub model: &'a str,
    pub provider: &'a str,
    pub effort: Option<&'a str>,
}

/// Every kind's groups with their numbers worked out (rates, medians), ready
/// to be ranked over any set of them — what choosing a subagent's model
/// reads, built once and ranked per spawn. The numbers are worked out when
/// the snapshot is built, so ranking costs the same whatever the history's
/// length. Ranking a kind over all its groups gives the report's result.
#[derive(Debug, Default)]
pub struct RankingSnapshot {
    kinds: BTreeMap<TaskKind, Vec<RankedModel>>,
}

impl RankingSnapshot {
    /// The totals of `records` that ended at or after `since` (restart-
    /// interrupted turns left out, as in the report).
    pub fn from_records(
        records: impl IntoIterator<Item = TurnRecord>,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        let options = ReportOptions {
            since,
            ..ReportOptions::default()
        };
        let mut totals = Aggregator::default();
        for record in records {
            totals.add(&TurnFacts::of_record(&record), &options);
        }
        Self::from_totals(totals)
    }

    pub(crate) fn from_totals(totals: Aggregator) -> Self {
        Self {
            kinds: totals
                .kinds
                .into_iter()
                .map(|(kind, groups)| {
                    let models = groups
                        .into_iter()
                        .map(|(key, tally)| tally.into_model(key))
                        .collect();
                    (kind, models)
                })
                .collect(),
        }
    }

    /// `kind` ranked over the groups `allow` accepts alone (groups with
    /// fewer than `min_turns` turns listed, not ranked); `None` when no
    /// accepted group has turns.
    pub fn rank(
        &self,
        kind: &TaskKind,
        allow: impl Fn(&GroupRef<'_>) -> bool,
        min_turns: usize,
    ) -> Option<KindReport> {
        let models: Vec<RankedModel> = self
            .kinds
            .get(kind)?
            .iter()
            .filter(|model| {
                allow(&GroupRef {
                    model: &model.model,
                    provider: &model.provider,
                    effort: model.effort.as_deref(),
                })
            })
            .cloned()
            .collect();
        (!models.is_empty()).then(|| rank_models(kind.clone(), models, min_turns))
    }
}

/// The snapshot of every session journal in `journal_dir`, turns that ended
/// at or after `since`. Read like the report: without changing or creating
/// anything; journals that cannot be read are left out.
pub async fn ranking_snapshot_in(
    journal_dir: &Path,
    since: Option<DateTime<Utc>>,
) -> anyhow::Result<RankingSnapshot> {
    let options = ReportOptions {
        since,
        ..ReportOptions::default()
    };
    let (totals, skipped) =
        totals_in(&WorkspaceJournals::at(journal_dir.to_path_buf()), &options).await?;
    for journal in &skipped {
        tracing::debug!(path = %journal.path.display(), error = %journal.error, "journal left out of the ranking");
    }
    Ok(RankingSnapshot::from_totals(totals))
}

// ------------------------------------------------------------ the journals

/// The report for every session journal in `journal_dir`. A missing
/// directory is an empty report (and is not created); a journal that cannot
/// be read is listed in `skipped`, and so is one that is a symbolic link:
/// Forge writes journals as files, and following a link could pull in
/// another project's journal, or any file, as this one's.
pub async fn model_report_in(
    journal_dir: &Path,
    options: &ReportOptions,
) -> anyhow::Result<ModelReport> {
    report_of(&WorkspaceJournals::at(journal_dir.to_path_buf()), options).await
}

/// The report for `journals`, refused as their listing is.
async fn report_of(
    journals: &WorkspaceJournals,
    options: &ReportOptions,
) -> anyhow::Result<ModelReport> {
    let (totals, skipped) = totals_in(journals, options).await?;
    Ok(ModelReport {
        journal_dir: journals.dir().to_path_buf(),
        since: options.since,
        min_turns: options.min_turns,
        kinds: totals.finish(options.min_turns),
        skipped,
    })
}

/// The running totals of every session journal `journals` may read, and
/// the journals skipped (see [`WorkspaceJournals::list`]) or unreadable.
async fn totals_in(
    journals: &WorkspaceJournals,
    options: &ReportOptions,
) -> anyhow::Result<(Aggregator, Vec<SkippedJournal>)> {
    let JournalListing {
        journals,
        mut skipped,
    } = journals.list().await?;
    let mut totals = Aggregator::default();
    for (path, read) in read_journals(journals, options).await? {
        match read {
            Ok(journal) => totals.merge(journal),
            Err(error) => skipped.push(SkippedJournal { path, error }),
        }
    }
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((totals, skipped))
}

/// Each of `journals`' totals, read [`CONCURRENT_JOURNALS`] at a time; an
/// error says why a journal could not be read.
pub(crate) async fn read_journals(
    journals: Vec<PathBuf>,
    options: &ReportOptions,
) -> anyhow::Result<Vec<(PathBuf, Result<Aggregator, String>)>> {
    let permits = Arc::new(Semaphore::new(CONCURRENT_JOURNALS));
    let mut reads = JoinSet::new();
    for path in journals {
        let permit = permits.clone().acquire_owned().await?;
        let options = options.clone();
        reads.spawn(async move {
            let totals = read_journal(&path, &options).await;
            drop(permit);
            (path, totals)
        });
    }
    let mut results = Vec::new();
    while let Some(read) = reads.join_next().await {
        results.push(read?);
    }
    Ok(results)
}

/// Journals read at once. Not tied to the number of CPUs: a journal's read
/// is mostly waiting — sqlx runs each connection on a thread of its own and
/// every query is a hand-off to it and back, slow across cores — so more
/// reads in flight than cores still helps. Measured on 10,000 journals,
/// 8 at a time was fastest on 1, 2 and 4 CPUs (3.5 s, 4.5 s, 4.4 s against
/// 4.1 s, 8.2 s, 8.4 s one at a time), for the same CPU time on 1–2 CPUs
/// and about 1.7 times as much on 4.
const CONCURRENT_JOURNALS: usize = 8;

/// One journal's totals. Streamed: only the running totals are held. A
/// journal with a row whose values the report uses do not decode counts for
/// nothing — its totals are dropped, not merged — and the error says why.
/// Columns the report does not read (task id, start time, tier, cache
/// tokens, model calls) are not checked: a bad value there cannot change a
/// number in the report, and does not cost the session its other turns.
/// [`forge_durable::read_turn_records`] checks every column.
async fn read_journal(path: &Path, options: &ReportOptions) -> Result<Aggregator, String> {
    let filter = TurnRecordFilter {
        ended_since: options.since,
        task_kind: options.kind.clone(),
    };
    let mut totals = Aggregator::default();
    let mut bad_row: Option<String> = None;
    let scanned = forge_durable::scan_turn_records(path, &filter, |view| {
        if bad_row.is_some() {
            return;
        }
        match TurnFacts::of_view(&view) {
            Ok(turn) => totals.add(&turn, options),
            Err(error) => bad_row = Some(format!("signals are not valid: {error}")),
        }
    })
    .await;
    match (scanned, bad_row) {
        (Err(error), _) => Err(error.to_string()),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(totals),
    }
}

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

// ------------------------------------------------------------------ since

/// `--since`: a date (`2025-06-01`, midnight UTC) or a time before `now`
/// (`7d`, `24h`).
pub fn parse_since(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let invalid =
        || format!("`{text}` is not a date (YYYY-MM-DD) or a number of days or hours (7d, 24h)");
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        // Exactly `YYYY-MM-DD`: chrono also accepts unpadded fields.
        if text.len() == 10 {
            return Ok(date.and_time(chrono::NaiveTime::MIN).and_utc());
        }
        return Err(invalid());
    }
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let count: i64 = number.parse().map_err(|_| invalid())?;
    let span = match unit {
        "d" => chrono::Duration::try_days(count),
        "h" => chrono::Duration::try_hours(count),
        _ => None,
    }
    .ok_or_else(invalid)?;
    now.checked_sub_signed(span).ok_or_else(invalid)
}
