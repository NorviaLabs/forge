//! The numbers of every group, worked out once and ranked on demand — what
//! choosing a model reads.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use forge_durable::TurnRecord;
use forge_types::TaskKind;

use super::aggregate::{Aggregator, TurnFacts};
use super::journals::totals_in;
use super::rank::rank_models;
use super::{KindReport, RankedModel, ReportOptions};
use crate::workspace_journals::WorkspaceJournals;

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
