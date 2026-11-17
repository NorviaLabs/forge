//! Turns into running totals: each turn's facts, grouped by kind and by
//! (model, provider, effort), added as they are read and merged across
//! journals.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use forge_durable::{TurnRecord, TurnRecordView};
use forge_types::{TaskKind, TurnOutcome, TurnSignalKind};

use super::rank::rank_kind;
use super::{KindReport, RankStatus, RankedModel, ReportOptions};

/// The completion reason of a turn a restart interrupted.
const INTERRUPTED_BY_RESTART: &str = TurnSignalKind::InterruptedByRestart.as_str();
/// The outcome of a turn the model finished.
const COMPLETED: &str = TurnOutcome::Completed.as_str();

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
pub(super) struct GroupKey {
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
        for kind in kinds.into_iter().filter_map(TurnSignalKind::from_name) {
            match kind {
                TurnSignalKind::VerificationPassed => counts.verification_passed = true,
                TurnSignalKind::VerificationFailed => counts.verification_failed = true,
                TurnSignalKind::ApprovalGranted => counts.approvals_granted += 1,
                TurnSignalKind::ApprovalDenied => counts.approvals_denied += 1,
                TurnSignalKind::UserMessageQueued => counts.queued = true,
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
pub(super) struct TurnFacts<'a> {
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
    pub(super) fn of_record(record: &'a TurnRecord) -> Self {
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
            completed: record.outcome == COMPLETED,
            interrupted_by_restart: record.completion_reason == INTERRUPTED_BY_RESTART,
            escalated: record.escalated,
            tokens: tokens(record.prompt_tokens, record.completion_tokens),
            active_ms: active_ms(record.duration_ms, record.waiting_ms),
            signals: SignalCounts::count(kinds),
        }
    }

    pub(super) fn of_view(view: &TurnRecordView<'a>) -> Result<Self, serde_json::Error> {
        Ok(Self {
            kind: view.task_kind,
            ended_at: view.ended_at,
            model: view.model,
            profile_id: view.profile_id,
            effort: view.reasoning_effort,
            completed: view.outcome == COMPLETED,
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
pub(super) struct Tally {
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

    pub(super) fn into_model(self, key: GroupKey) -> RankedModel {
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

/// Distinct raw (model, provider, effort) names remembered with their
/// normalized group, so names are normalized once, not once per turn. Past
/// this many, further names are normalized each time (no unbounded growth).
const KEY_CACHE: usize = 64;

/// Running totals: turns are added one at a time, as they are read, and
/// totals from separate journals merged.
#[derive(Debug, Default)]
pub(crate) struct Aggregator {
    pub(super) kinds: BTreeMap<TaskKind, BTreeMap<GroupKey, Tally>>,
    keys: Vec<(String, String, Option<String>, GroupKey)>,
}

impl Aggregator {
    pub(super) fn add(&mut self, turn: &TurnFacts<'_>, options: &ReportOptions) {
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

    pub(super) fn merge(&mut self, other: Aggregator) {
        for (kind, groups) in other.kinds {
            let mine = self.kinds.entry(kind).or_default();
            for (key, tally) in groups {
                mine.entry(key).or_default().merge(tally);
            }
        }
    }

    pub(super) fn finish(self, min_turns: usize) -> Vec<KindReport> {
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
