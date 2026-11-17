//! Where an Auto turn of the main session starts
//! (`forge_core::AutoModelAdvisor`): the ranked best model and effort for
//! `main` work among the models on the fallback's provider that the account
//! can run — the same ranking, thresholds and rules as choosing a subagent's
//! model (`subagent_models`), without exploration — or, when the evidence is
//! thin, the fallback the user set. And the efforts the step-up ladder
//! climbs, from the provider and the catalog.

use std::sync::Arc;
use std::time::Duration;

use forge_core::{
    AutoFallback, AutoModelAdvisor, AutoStart, ChildModel, ChildModelRequest, ChildModelResolver,
    ModelEfforts, ModelExecutionSelection,
};
use forge_types::TaskKind;

use crate::model_ranking::{
    first_accepted, provider_of, standing, RankingCache, RankingSource, RunnableModels,
    SharedRanking, Standing,
};

/// A model's efforts.
pub trait EffortLookup: Send + Sync {
    fn efforts(&self, model: &str, profile_id: &str) -> ModelEfforts;
}

/// How Auto's advisor behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoSettings {
    /// Whether the user trusts the workspace; an untrusted one's journals
    /// are not read.
    pub trusted: bool,
    pub stale_after: Duration,
}

/// The ranking-based advisor for Auto.
pub struct RankedAutoModels {
    ranking: SharedRanking,
    runnable: Arc<dyn RunnableModels>,
    resolver: Arc<dyn ChildModelResolver>,
    efforts: Arc<dyn EffortLookup>,
    settings: AutoSettings,
}

impl RankedAutoModels {
    /// An advisor with a ranking of its own, not built yet.
    pub fn new(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        efforts: Arc<dyn EffortLookup>,
        settings: AutoSettings,
    ) -> Arc<Self> {
        Self::with_cache(
            source,
            runnable,
            resolver,
            efforts,
            settings,
            Arc::new(RankingCache::default()),
        )
    }

    pub(crate) fn with_cache(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        efforts: Arc<dyn EffortLookup>,
        settings: AutoSettings,
        cache: Arc<RankingCache>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ranking: SharedRanking::new(source, cache, settings.stale_after),
            runnable,
            resolver,
            efforts,
            settings,
        })
    }

    /// Build the ranking now, and wait for it (nothing for an untrusted
    /// workspace).
    pub async fn refresh(&self) -> anyhow::Result<()> {
        if !self.settings.trusted {
            return Ok(());
        }
        self.ranking.refresh().await
    }

    /// Build in the background if there is no ranking yet, or it is stale.
    pub fn warm(&self) {
        if self.settings.trusted {
            self.ranking.warm();
        }
    }

    /// The fallback as a model to run on, with what the resolver knows of
    /// it (its limits), and why it is used.
    fn fallback(&self, fallback: &AutoFallback, reason: String) -> AutoStart {
        let selection = ModelExecutionSelection {
            model: fallback.model.clone(),
            profile_id: fallback.profile_id.clone(),
            reasoning_effort: fallback.reasoning_effort.clone(),
            thinking_enabled: true,
        };
        let request = ChildModelRequest {
            model: Some(&fallback.model),
            reasoning_effort: fallback.reasoning_effort.as_deref(),
            parent: &selection,
        };
        let model = self.resolver.resolve(&request).unwrap_or(ChildModel {
            model: fallback.model.clone(),
            profile_id: fallback.profile_id.clone(),
            reasoning_effort: fallback.reasoning_effort.clone(),
            context_window: None,
            output_reserve: None,
            image_input: None,
        });
        AutoStart {
            model,
            ranked: false,
            reason,
            evidence: None,
        }
    }
}

impl AutoModelAdvisor for RankedAutoModels {
    fn start(&self, fallback: &AutoFallback) -> AutoStart {
        let kind = TaskKind::Main;
        if !self.settings.trusted {
            return self.fallback(fallback, crate::model_ranking::UNTRUSTED_WORKSPACE.into());
        }
        let snapshot = match self.ranking.current(&kind) {
            Ok(snapshot) => snapshot,
            Err(reason) => return self.fallback(fallback, reason),
        };
        let selection = ModelExecutionSelection {
            model: fallback.model.clone(),
            profile_id: fallback.profile_id.clone(),
            reasoning_effort: fallback.reasoning_effort.clone(),
            thinking_enabled: true,
        };
        let provider = provider_of(&selection);
        match standing(
            &snapshot,
            &kind,
            provider,
            &fallback.model,
            self.runnable.as_ref(),
        ) {
            Standing::Thin { reason, evidence } => AutoStart {
                evidence,
                ..self.fallback(fallback, reason)
            },
            Standing::Ranked { ranked, evidence } => {
                match first_accepted(&ranked, self.resolver.as_ref(), &selection, provider, &kind) {
                    Ok((model, reason)) => AutoStart {
                        model,
                        ranked: true,
                        reason,
                        evidence: Some(evidence),
                    },
                    Err(reason) => AutoStart {
                        evidence: Some(evidence),
                        ..self.fallback(fallback, reason)
                    },
                }
            }
        }
    }

    fn efforts(&self, model: &str, profile_id: &str) -> ModelEfforts {
        self.efforts.efforts(model, profile_id)
    }
}
