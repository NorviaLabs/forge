//! Auto for the main session: the model and effort of each turn are chosen
//! for the user.
//!
//! Each turn starts from the ranked best model and effort for main-session
//! work (from the session's [`AutoModelAdvisor`]) or, when the evidence is
//! thin, the fallback the user set, and then climbs the **step-up ladder**
//! by the session's current position:
//!
//! 1. the start itself;
//! 2. each higher effort on the same model;
//! 3. the provider's strong-tier model at its effort, then its higher
//!    efforts — only once the start model is at its highest effort, or has
//!    no effort setting.
//!
//! After a failed turn the next turn starts one step higher (the failed turn
//! is never re-run: the user may not want its tool calls repeated); after
//! two successful turns in a row at a raised step, one step back down. A
//! turn that says nothing about the model — a provider error, a denied
//! approval, a cancellation, a turn on a tier the user asked for — leaves
//! the ladder alone. Each turn's model, effort and why are recorded as an
//! [`AutoDecision`] and an `auto_model` turn signal; Auto itself, its
//! position and the decisions are journaled, so a restart keeps them.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{ChildModel, ModelChoiceEvidence, ModelExecutionSelection};

/// Successful turns in a row at a raised step that take the ladder one step
/// back down.
pub const SUCCESSES_TO_STEP_DOWN: usize = 2;

/// The model and effort Auto falls back on when the evidence is thin: the
/// model the user had when they chose Auto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoFallback {
    pub model: String,
    pub profile_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

impl AutoFallback {
    pub fn of(selection: &ModelExecutionSelection) -> Self {
        Self {
            model: selection.model.clone(),
            profile_id: selection.profile_id.clone(),
            reasoning_effort: selection.reasoning_effort.clone(),
        }
    }
}

/// A model's reasoning efforts, lowest first, and the one it runs at when
/// none is named. No options: the model has no effort setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEfforts {
    pub options: Vec<String>,
    pub default: Option<String>,
}

/// A move along the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LadderMove {
    Up,
    Down,
}

/// What a finished turn says about the model that ran it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The work failed (the completion evaluator's verdict).
    Failed,
    Succeeded,
    /// Nothing about the model: a provider error, a denied approval, a
    /// cancellation, a turn on a tier the user asked for.
    Neutral,
}

/// Auto, on: its fallback and where it stands on the ladder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoModel {
    pub fallback: AutoFallback,
    /// Steps above the start; 0 is the start.
    pub position: usize,
    /// Successful turns in a row since the last move or failure.
    pub successes: usize,
    /// The last move, to say why a raised step is where it is.
    #[serde(default)]
    pub last_move: Option<LadderMove>,
}

impl AutoModel {
    pub fn new(fallback: AutoFallback) -> Self {
        Self {
            fallback,
            position: 0,
            successes: 0,
            last_move: None,
        }
    }

    /// The step to run on, on a ladder whose top step is `top`.
    pub fn step_on(&self, top: usize) -> usize {
        self.position.min(top)
    }

    /// Move for a turn that ended `end` on a ladder whose top step is `top`.
    pub fn after_turn(&mut self, end: TurnEnd, top: usize) -> Option<LadderMove> {
        self.position = self.position.min(top);
        match end {
            TurnEnd::Failed => {
                self.successes = 0;
                if self.position >= top {
                    return None;
                }
                self.position += 1;
                self.last_move = Some(LadderMove::Up);
                Some(LadderMove::Up)
            }
            TurnEnd::Succeeded => {
                self.successes += 1;
                if self.position == 0 || self.successes < SUCCESSES_TO_STEP_DOWN {
                    return None;
                }
                self.position -= 1;
                self.successes = 0;
                self.last_move = Some(LadderMove::Down);
                Some(LadderMove::Down)
            }
            TurnEnd::Neutral => None,
        }
    }
}

/// Where an Auto turn's model came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoSource {
    /// The ranked best for main-session work.
    Ranked,
    /// The fallback: thin evidence, or no ranking.
    Fallback,
    /// Above the start, after a failed turn.
    SteppedUp,
    /// Above the start, but lower than before, after successful turns.
    SteppedDown,
}

impl AutoSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ranked => "ranked",
            Self::Fallback => "fallback",
            Self::SteppedUp => "stepped_up",
            Self::SteppedDown => "stepped_down",
        }
    }
}

impl fmt::Display for AutoSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The model and effort Auto chose for one turn, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDecision {
    pub task_id: u64,
    /// Which of the user's messages (1 = the first) the turn answers: where
    /// the page shows the decision.
    pub user_message: usize,
    pub model: String,
    pub profile_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub source: AutoSource,
    /// The step run on (0 = the start), of `steps`.
    pub step: usize,
    pub steps: usize,
    pub reason: String,
}

/// Where a turn starts: the ranked best, or the fallback, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoStart {
    pub model: ChildModel,
    /// The ranking chose it (else it is the fallback).
    pub ranked: bool,
    pub reason: String,
    pub evidence: Option<ModelChoiceEvidence>,
}

/// Chooses where Auto turns start, and knows models' efforts. Installed by
/// the session's owner. Answers at once: a turn must never wait on it.
pub trait AutoModelAdvisor: Send + Sync {
    fn start(&self, fallback: &AutoFallback) -> AutoStart;
    fn efforts(&self, model: &str, profile_id: &str) -> ModelEfforts;
}

/// The efforts above `model`'s (its own, or its default when it names
/// none). An effort the model does not list: only its highest is above.
fn higher_efforts(model: &ChildModel, efforts: &ModelEfforts) -> Vec<ChildModel> {
    let Some(highest) = efforts.options.last() else {
        return Vec::new();
    };
    let current = model.reasoning_effort.as_ref().or(efforts.default.as_ref());
    let above: Vec<&String> =
        match current.and_then(|c| efforts.options.iter().position(|o| o == c)) {
            Some(index) => efforts.options[index + 1..].iter().collect(),
            None if current == Some(highest) => Vec::new(),
            None => vec![highest],
        };
    above
        .into_iter()
        .map(|effort| ChildModel {
            reasoning_effort: Some(effort.clone()),
            ..model.clone()
        })
        .collect()
}

/// The step-up ladder from `start`: the start, its higher efforts, then the
/// stronger model (`strong`, when it is another model) and its higher
/// efforts.
pub fn ladder(
    start: &ChildModel,
    start_efforts: &ModelEfforts,
    strong: Option<(&ChildModel, &ModelEfforts)>,
) -> Vec<ChildModel> {
    let mut rungs = vec![start.clone()];
    rungs.extend(higher_efforts(start, start_efforts));
    if let Some((strong, strong_efforts)) = strong {
        if strong.model != start.model {
            rungs.push(strong.clone());
            rungs.extend(higher_efforts(strong, strong_efforts));
        }
    }
    rungs
}

/// Auto as the journal last recorded it: `None` when never set, or turned
/// off (a `null` state), or unreadable.
pub(crate) fn auto_from_journal(state: Option<&serde_json::Value>) -> Option<AutoModel> {
    state
        .filter(|state| !state.is_null())
        .and_then(|state| serde_json::from_value(state.clone()).ok())
}

/// The decisions the journal recorded, in order (unreadable ones left out).
pub(crate) fn decisions_from_journal(decisions: &[serde_json::Value]) -> Vec<AutoDecision> {
    decisions
        .iter()
        .filter_map(|decision| serde_json::from_value(decision.clone()).ok())
        .collect()
}

/// `model @ effort`, the model's default effort as `default`.
fn named(model: &ChildModel) -> String {
    format!(
        "{} @ {}",
        model.model,
        model.reasoning_effort.as_deref().unwrap_or("default")
    )
}

impl crate::AgentSession {
    /// Install the advisor that chooses where Auto turns start.
    pub fn set_auto_model_advisor(&mut self, advisor: std::sync::Arc<dyn AutoModelAdvisor>) {
        self.models.auto_advisor = Some(advisor);
    }

    /// Turn Auto on, falling back on `fallback` when the evidence is thin.
    /// Journaled, so it survives a restart. Starts at the bottom of the
    /// ladder.
    pub async fn set_auto_model(&mut self, fallback: AutoFallback) -> Result<(), crate::LoopError> {
        let auto = AutoModel::new(fallback);
        self.journal
            .append_auto_model(self.session_id, serde_json::json!({ "state": auto }))
            .await?;
        self.models.auto = Some(auto);
        Ok(())
    }

    /// Turn Auto off (the user picked a model). Journaled.
    pub async fn clear_auto_model(&mut self) -> Result<(), crate::LoopError> {
        if self.models.auto.is_none() {
            return Ok(());
        }
        self.journal
            .append_auto_model(self.session_id, serde_json::json!({ "state": null }))
            .await?;
        self.models.auto = None;
        Ok(())
    }

    /// Auto, when it is on.
    pub fn auto_model(&self) -> Option<&AutoModel> {
        self.models.auto.as_ref()
    }

    /// The model and effort Auto chose for each of its turns, and why.
    pub fn auto_decisions(&self) -> &[AutoDecision] {
        &self.models.auto_decisions
    }

    /// A model's efforts, from the advisor (none without one).
    fn efforts_of(&self, model: &ChildModel) -> ModelEfforts {
        self.models
            .auto_advisor
            .as_ref()
            .map(|advisor| advisor.efforts(&model.model, &model.profile_id))
            .unwrap_or_default()
    }

    /// At the start of a main-session turn with Auto on: work out the start
    /// (ranked, or the fallback), the ladder from it, and run the turn on the
    /// current step. Recorded as an `auto_model` signal and a journaled
    /// decision.
    pub(crate) async fn choose_auto_model(&mut self) -> Result<(), crate::LoopError> {
        if self.is_subagent() {
            return Ok(());
        }
        let Some(auto) = self.models.auto.clone() else {
            return Ok(());
        };
        let start = match &self.models.auto_advisor {
            Some(advisor) => advisor.start(&auto.fallback),
            None => AutoStart {
                model: ChildModel {
                    model: auto.fallback.model.clone(),
                    profile_id: auto.fallback.profile_id.clone(),
                    reasoning_effort: auto.fallback.reasoning_effort.clone(),
                    context_window: None,
                    output_reserve: None,
                    image_input: None,
                },
                ranked: false,
                reason: "no ranking of models is available to this session".into(),
                evidence: None,
            },
        };
        let start_selection = ModelExecutionSelection {
            model: start.model.model.clone(),
            profile_id: start.model.profile_id.clone(),
            reasoning_effort: start.model.reasoning_effort.clone(),
            thinking_enabled: self.thinking_enabled,
        };
        let strong = self.resolve_model_tier(forge_types::ModelTier::Strong, &start_selection);
        let strong = (strong.basis != crate::TierBasis::Unavailable).then_some(strong.model);
        let strong_efforts = strong.as_ref().map(|model| self.efforts_of(model));
        let rungs = ladder(
            &start.model,
            &self.efforts_of(&start.model),
            strong.as_ref().zip(strong_efforts.as_ref()),
        );
        let top = rungs.len() - 1;
        let step = auto.step_on(top);
        let rung = rungs[step].clone();
        let (source, reason) = match (step, auto.last_move) {
            (0, _) if start.ranked => (AutoSource::Ranked, start.reason.clone()),
            (0, _) => (AutoSource::Fallback, start.reason.clone()),
            (_, Some(LadderMove::Down)) => (
                AutoSource::SteppedDown,
                format!(
                    "step {step} of {top} above the start ({}), lowered after \
                     {SUCCESSES_TO_STEP_DOWN} successful turns",
                    named(&start.model)
                ),
            ),
            _ => (
                AutoSource::SteppedUp,
                format!(
                    "step {step} of {top} above the start ({}), after a failed turn",
                    named(&start.model)
                ),
            ),
        };
        self.apply_resolved_model(&rung);
        let decision = AutoDecision {
            task_id: self.active_task.task_id.0,
            user_message: self.canonical_user_messages.len(),
            model: rung.model.clone(),
            profile_id: rung.profile_id.clone(),
            reasoning_effort: rung.reasoning_effort.clone(),
            source,
            step,
            steps: rungs.len(),
            reason: reason.clone(),
        };
        self.tracking.telemetry.auto_steps = Some(rungs.len());
        self.record_signal(
            crate::TurnSignalKind::AutoModel,
            Some(format!("{source}: {reason}")),
        );
        self.journal
            .append_auto_model(self.session_id, serde_json::json!({ "decision": decision }))
            .await?;
        self.models.auto_decisions.push(decision);
        Ok(())
    }

    /// At the end of a turn Auto chose the model of: move along the ladder
    /// for what the turn says about the model. A turn the user put on a tier
    /// says nothing about Auto's choice. Journaled when Auto changed; never
    /// fails the turn.
    pub(crate) async fn move_auto_model(&mut self, to: forge_types::TaskLifecycle) {
        let Some(steps) = self.tracking.telemetry.auto_steps.take() else {
            return;
        };
        let on_a_tier = self
            .turn_signals()
            .iter()
            .any(|signal| signal.kind == crate::TurnSignalKind::TierApplied);
        let end = match to {
            _ if on_a_tier => TurnEnd::Neutral,
            forge_types::TaskLifecycle::Completed => TurnEnd::Succeeded,
            forge_types::TaskLifecycle::Failed if self.tracking.telemetry.work_failed => {
                TurnEnd::Failed
            }
            _ => TurnEnd::Neutral,
        };
        let Some(auto) = self.models.auto.as_mut() else {
            return;
        };
        let before = auto.clone();
        auto.after_turn(end, steps.saturating_sub(1));
        if *auto == before {
            return;
        }
        let state = serde_json::json!({ "state": auto });
        if let Err(error) = self.journal.append_auto_model(self.session_id, state).await {
            tracing::warn!(%error, session = %self.session_id, "could not record Auto's step");
        }
    }
}
