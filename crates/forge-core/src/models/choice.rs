//! How a subagent started without a tier or a model gets its model.
//!
//! The session's owner installs a [`SubagentModelAdvisor`] (in Forge, the
//! ranking of models per kind of task, `forge-session::subagent_models`).
//! When `spawn_agent` names no tier, model or effort, the session asks it
//! for the child's kind of task and the parent's model: it answers with a
//! model to use, or none (the parent's model, as without an advisor), and
//! always with the reason — a [`ModelChoice`], journaled with the child's
//! profile, returned to the parent by `spawn_agent`, and recorded as a
//! `model_chosen` signal on each of the child's turns.

use std::fmt;

use forge_types::TaskKind;
use serde::{Deserialize, Serialize};

use crate::{ChildModel, ModelExecutionSelection};

/// Where a child's model came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelChoiceSource {
    /// The leader of the ranking for the child's kind of task.
    Ranking,
    /// The parent's model, kept on purpose although the ranking had a
    /// leader, so the other models keep being tried.
    Exploration,
    /// The parent's model: not enough evidence, or ranking not in use.
    Fallback,
}

impl ModelChoiceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ranking => "ranking",
            Self::Exploration => "exploration",
            Self::Fallback => "fallback",
        }
    }
}

impl fmt::Display for ModelChoiceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A (model, provider, effort) as the ranking placed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedCandidate {
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub effort: Option<String>,
    pub turns: usize,
    /// Mean rank over the measures; lower is better.
    pub score: f64,
    pub rank: usize,
}

/// What the ranking showed when the choice was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChoiceEvidence {
    /// How far back the turns go, in days.
    pub window_days: u32,
    /// Turns of this kind on the candidate models.
    pub turns: usize,
    /// Candidates with enough turns to be ranked.
    pub ranked_groups: usize,
    #[serde(default)]
    pub leader: Option<RankedCandidate>,
    #[serde(default)]
    pub runner_up: Option<RankedCandidate>,
    /// The thresholds the evidence was held to.
    pub min_group_turns: usize,
    pub min_kind_turns: usize,
    pub min_ranked_groups: usize,
}

/// Why a child runs on the model it runs on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub source: ModelChoiceSource,
    /// One sentence, for people.
    pub reason: String,
    pub kind: TaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ModelChoiceEvidence>,
}

/// What the advisor is asked.
#[derive(Debug, Clone, Copy)]
pub struct ModelAdviceRequest<'a> {
    /// The child's kind of task: `subagent` or `custom:<name>`.
    pub kind: &'a TaskKind,
    /// The model the parent runs on (its own choice, not a turn's tier).
    pub parent: &'a ModelExecutionSelection,
}

/// The advisor's answer: a model, or none (the parent's), and why.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAdvice {
    pub pick: Option<ChildModel>,
    pub choice: ModelChoice,
}

/// Chooses the model of a subagent started without a tier or a model.
/// Answers at once: a spawn must never wait on it.
pub trait SubagentModelAdvisor: Send + Sync {
    fn advise(&self, request: &ModelAdviceRequest<'_>) -> ModelAdvice;
}
