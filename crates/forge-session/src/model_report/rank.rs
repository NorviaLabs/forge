//! Ranking a kind's groups against each other.

use std::collections::BTreeMap;

use forge_types::TaskKind;

use super::aggregate::{GroupKey, Tally};
use super::{KindReport, MetricRanks, RankStatus, RankedModel};

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
pub(super) fn rank_kind(
    kind: TaskKind,
    groups: BTreeMap<GroupKey, Tally>,
    min_turns: usize,
) -> KindReport {
    let models = groups
        .into_iter()
        .map(|(key, tally)| tally.into_model(key))
        .collect();
    rank_models(kind, models, min_turns)
}

/// Rank groups whose numbers are worked out (not yet ranked): the ones with
/// enough turns best first, then the rest by turns and name.
pub(super) fn rank_models(
    kind: TaskKind,
    models: Vec<RankedModel>,
    min_turns: usize,
) -> KindReport {
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
