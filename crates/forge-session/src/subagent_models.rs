//! Choosing the model of a subagent started without a tier or a model, from
//! the ranking of models for its kind of task (`model_report`).
//!
//! - **Candidates** are the models on the parent's provider that the account
//!   can run; the kind (`subagent`, or a custom agent's own
//!   `custom:<name>`, never borrowed from another) is ranked over those
//!   alone, from the turns of the last [`WINDOW_DAYS`] days.
//! - **Enough evidence** is at least [`MIN_KIND_TURNS`] such turns and at
//!   least [`MIN_RANKED_GROUPS`] candidates with enough turns to be ranked.
//!   The leader is then picked — model and effort — if the child-model
//!   resolver accepts it; else the next ranked candidate.
//! - **Otherwise** — thin evidence, the switch set to `inherit`, a workspace
//!   the user does not trust (its journals could have come from anyone, and
//!   are not read), no ranking built yet — the child keeps the parent's
//!   model, as before.
//! - **Exploration**: about one spawn in ten keeps the parent's model even
//!   with enough evidence, so every model keeps being tried.
//! - **Never waiting**: the ranking is built in the background and rebuilt
//!   when older than [`STALE_AFTER`]; a spawn uses what is there.
//!
//! Every answer carries its reason (`forge_core::ModelChoice`).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forge_config::{Config, SubagentModelSelection};
use forge_core::{
    ChildModelResolver, ModelAdvice, ModelAdviceRequest, ModelChoice, ModelChoiceEvidence,
    ModelChoiceSource, SubagentModelAdvisor,
};
use forge_types::TaskKind;

use crate::model_ranking::{
    first_accepted, named, provider_of, standing, RankingCache, SharedRanking, Standing,
};
/// The ranking this advisor reads, where it lived first.
pub use crate::model_ranking::{
    JournalRanking, RankingSource, RunnableModels, SnapshotFuture, UnknownRunnableModels,
    FULL_REBUILD_EVERY, MIN_KIND_TURNS, MIN_RANKED_GROUPS, STALE_AFTER, WINDOW_DAYS,
};

/// About one spawn in this many keeps the parent's model anyway.
pub const EXPLORE_ONE_IN: u32 = 10;

/// Whether, and how often, to keep the parent's model despite a leader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exploration {
    Never,
    Always,
    /// About one in `n`, drawn from a generator seeded with `seed`.
    OneIn {
        n: u32,
        seed: u64,
    },
}

/// How an advisor behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorSettings {
    pub mode: SubagentModelSelection,
    /// Whether the user trusts the workspace; an untrusted one's journals
    /// are not read.
    pub trusted: bool,
    pub exploration: Exploration,
    pub stale_after: Duration,
}

/// The settings for a session of `cfg`'s workspace: its switch, the user's
/// trust (`trust_store`: `None` for the user's own; only a test passes
/// another), exploring one spawn in [`EXPLORE_ONE_IN`].
pub fn settings_for(cfg: &Config, trust_store: Option<&Path>) -> AdvisorSettings {
    AdvisorSettings {
        mode: cfg.subagents.model_selection,
        trusted: crate::workspace_journals::is_workspace_trusted(trust_store, cfg.workspace_root()),
        exploration: Exploration::OneIn {
            n: EXPLORE_ONE_IN,
            seed: uuid::Uuid::new_v4().as_u64_pair().0,
        },
        stale_after: STALE_AFTER,
    }
}

/// The ranking-based advisor.
pub struct RankedSubagentModels {
    ranking: SharedRanking,
    runnable: Arc<dyn RunnableModels>,
    resolver: Arc<dyn ChildModelResolver>,
    settings: AdvisorSettings,
    /// The exploration generator's state.
    draws: Mutex<u64>,
}

impl RankedSubagentModels {
    /// An advisor with a ranking of its own, not built yet.
    pub fn new(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        settings: AdvisorSettings,
    ) -> Arc<Self> {
        Self::with_cache(
            source,
            runnable,
            resolver,
            settings,
            Arc::new(RankingCache::default()),
        )
    }

    pub(crate) fn with_cache(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        settings: AdvisorSettings,
        cache: Arc<RankingCache>,
    ) -> Arc<Self> {
        let seed = match settings.exploration {
            Exploration::OneIn { seed, .. } => seed,
            _ => 0,
        };
        Arc::new(Self {
            ranking: SharedRanking::new(source, cache, settings.stale_after),
            runnable,
            resolver,
            settings,
            draws: Mutex::new(seed),
        })
    }

    /// Whether this advisor reads the workspace's journals at all.
    fn reads_journals(&self) -> bool {
        self.settings.mode == SubagentModelSelection::Ranked && self.settings.trusted
    }

    /// Build the ranking now, and wait for it. Does nothing when the
    /// journals are not to be read.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        if !self.reads_journals() {
            return Ok(());
        }
        self.ranking.refresh().await
    }

    /// Build the ranking in the background if there is none yet, or it is
    /// stale: what a session does when it opens.
    pub fn warm(&self) {
        if self.reads_journals() {
            self.ranking.warm();
        }
    }

    /// Start building the ranking, unless one is being built (or the
    /// journals are not to be read, or there is no runtime to build it on).
    /// Returns at once.
    pub fn refresh_in_background(&self) {
        if self.reads_journals() {
            self.ranking.refresh_in_background();
        }
    }

    /// Whether this spawn explores: about one in `n`.
    fn explores(&self) -> bool {
        match self.settings.exploration {
            Exploration::Never => false,
            Exploration::Always => true,
            Exploration::OneIn { n, .. } => {
                let mut state = self
                    .draws
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // splitmix64: small, fast, and good enough to spread draws.
                *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = *state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                n > 0 && z.is_multiple_of(u64::from(n))
            }
        }
    }
}

fn fallback(kind: &TaskKind, reason: String, evidence: Option<ModelChoiceEvidence>) -> ModelAdvice {
    ModelAdvice {
        pick: None,
        choice: ModelChoice {
            source: ModelChoiceSource::Fallback,
            reason,
            kind: kind.clone(),
            evidence,
        },
    }
}

impl SubagentModelAdvisor for RankedSubagentModels {
    fn advise(&self, request: &ModelAdviceRequest<'_>) -> ModelAdvice {
        let kind = request.kind;
        let parent = request.parent;
        if self.settings.mode == SubagentModelSelection::Inherit {
            return fallback(
                kind,
                "subagent model selection is set to `inherit`: the parent's model is kept".into(),
                None,
            );
        }
        if !self.settings.trusted {
            return fallback(kind, crate::model_ranking::UNTRUSTED_WORKSPACE.into(), None);
        }
        let snapshot = match self.ranking.current(kind) {
            Ok(snapshot) => snapshot,
            Err(reason) => return fallback(kind, reason, None),
        };
        let provider = provider_of(parent);
        let (ranked, evidence) = match standing(
            &snapshot,
            kind,
            provider,
            &parent.model,
            self.runnable.as_ref(),
        ) {
            Standing::Thin { reason, evidence } => return fallback(kind, reason, evidence),
            Standing::Ranked { ranked, evidence } => (ranked, evidence),
        };
        if self.explores() {
            let leader = &ranked[0];
            return ModelAdvice {
                pick: None,
                choice: ModelChoice {
                    source: ModelChoiceSource::Exploration,
                    reason: format!(
                        "kept the parent's model {} to keep trying it (about one spawn in \
                         {EXPLORE_ONE_IN}); the ranking's leader is {}",
                        named(&parent.model, parent.reasoning_effort.as_deref()),
                        named(&leader.model, leader.effort.as_deref())
                    ),
                    kind: kind.clone(),
                    evidence: Some(evidence),
                },
            };
        }
        match first_accepted(&ranked, self.resolver.as_ref(), parent, provider, kind) {
            Ok((pick, reason)) => ModelAdvice {
                pick: Some(pick),
                choice: ModelChoice {
                    source: ModelChoiceSource::Ranking,
                    reason,
                    kind: kind.clone(),
                    evidence: Some(evidence),
                },
            },
            Err(reason) => fallback(kind, reason, Some(evidence)),
        }
    }
}
