//! Model tiers inside a session: asking for `fast`, `default` or `strong`
//! instead of a model, and escalating a failed subagent turn.
//!
//! Tiers are resolved by a [`ModelTierResolver`] the session's owner installs
//! (`forge-session` derives them from models.dev, see
//! `forge_connect::tiers`). Without one, a tier changes nothing.
//!
//! Three uses:
//!
//! - **Top-level session**: the model calls `set_model_tier` when the user's
//!   message asks for a faster or stronger model. The tier applies from the
//!   next model step until the turn ends; the session then returns to the
//!   model the user selected. An explicit model switch by the user drops it.
//! - **Subagent**: `spawn_agent`'s `tier` picks the child's model for its
//!   whole life (journaled in its `ChildProfile`).
//! - **Escalation**: when the completion evaluator fails a subagent turn,
//!   the turn is re-run once on the strong tier, in the same child and
//!   worktree (see `subagent::drive_subagent`).

use forge_types::ModelTier;
use serde::Serialize;

use crate::{AgentSession, ChildModel, CompactionPolicy, ModelExecutionSelection};

/// How a tier was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TierBasis {
    /// A model chosen for the tier from the provider's catalog.
    Catalog,
    /// The current model, at the tier's effort: the provider had no tier
    /// ladder to choose from.
    EffortOnly,
    /// Nothing to change: no resolver, or the model has no adjustable effort
    /// and no ladder.
    Unavailable,
}

/// A request to resolve `tier` for a session currently on `current`.
#[derive(Debug, Clone, Copy)]
pub struct TierRequest<'a> {
    pub tier: ModelTier,
    pub current: &'a ModelExecutionSelection,
}

/// What a tier resolves to: a model with its capabilities, and how it was
/// chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct TierResolution {
    pub tier: ModelTier,
    pub model: ChildModel,
    pub basis: TierBasis,
}

impl TierResolution {
    /// `tier` resolved to `current`, unchanged.
    pub fn unavailable(tier: ModelTier, current: &ModelExecutionSelection) -> Self {
        Self {
            tier,
            model: ChildModel {
                model: current.model.clone(),
                profile_id: current.profile_id.clone(),
                reasoning_effort: current.reasoning_effort.clone(),
                context_window: None,
                output_reserve: None,
                image_input: None,
            },
            basis: TierBasis::Unavailable,
        }
    }

    /// Whether running on this would change nothing from `selection`.
    pub fn matches(&self, selection: &ModelExecutionSelection) -> bool {
        self.model.model == selection.model
            && self.model.reasoning_effort == selection.reasoning_effort
    }

    pub(crate) fn describe(&self) -> String {
        match &self.model.reasoning_effort {
            Some(effort) => format!("{}: {} @ {effort}", self.tier, self.model.model),
            None => format!("{}: {}", self.tier, self.model.model),
        }
    }
}

/// Resolves tiers against the providers and catalog. Installed by the
/// session's owner and shared with the subagents it spawns.
pub trait ModelTierResolver: Send + Sync {
    fn resolve_tier(&self, request: &TierRequest<'_>) -> TierResolution;
}

/// The model state a turn-scoped tier replaced, restored when the turn ends.
#[derive(Debug, Clone)]
pub(crate) struct SelectionRestore {
    selection: ModelExecutionSelection,
    compaction_policy: CompactionPolicy,
    capacity_tokens: usize,
    image_input: bool,
}

/// A tier in force for the current turn only.
#[derive(Debug, Clone)]
pub(crate) struct TierOverride {
    pub(crate) tier: ModelTier,
    restore: SelectionRestore,
}

/// A failed subagent turn waiting to be re-run on the strong tier.
#[derive(Debug, Clone)]
pub(crate) struct EscalationPlan {
    pub(crate) resolution: TierResolution,
    /// Why the turn failed, for the retry's instructions.
    pub(crate) failure: String,
}

/// How the strong-tier retry's note begins. `[REPAIR TASK` is the prefix
/// frontends use to tell a runtime-authored repair from the operator
/// speaking.
const ESCALATION_NOTE_PREFIX: &str = "[REPAIR TASK: strong-tier retry on ";

/// The user-role note that starts the strong-tier retry. It names the model
/// and effort the retry runs on, so a retry a restart interrupts is still
/// recorded as one (see [`parse_escalation_note`]): the note is the turn's
/// first journaled message.
pub(crate) fn escalation_note(failure: &str, resolution: &TierResolution) -> String {
    let failure = failure.trim();
    let failure = if failure.is_empty() {
        "the completion check did not pass"
    } else {
        failure
    };
    let target = match &resolution.model.reasoning_effort {
        Some(effort) => format!("{} @ {effort}", resolution.model.model),
        None => resolution.model.model.clone(),
    };
    format!(
        "{ESCALATION_NOTE_PREFIX}{target}] Your previous attempt at this task did not \
         complete: {failure}\n\nYou are now running on a stronger model. Finish the original \
         task. Check the current state of the workspace first: the previous attempt may have \
         left partial changes."
    )
}

/// The model and effort a strong-tier retry's note names; `None` for any
/// other message.
pub(crate) fn parse_escalation_note(text: &str) -> Option<(String, Option<String>)> {
    let rest = text.strip_prefix(ESCALATION_NOTE_PREFIX)?;
    let (target, _) = rest.split_once(']')?;
    let (model, effort) = match target.split_once(" @ ") {
        Some((model, effort)) => (model, Some(effort.to_string())),
        None => (target, None),
    };
    (!model.trim().is_empty()).then(|| (model.trim().to_string(), effort))
}

impl AgentSession {
    /// Install the resolver that turns tiers into models.
    pub fn set_model_tier_resolver(&mut self, resolver: std::sync::Arc<dyn ModelTierResolver>) {
        self.tier_resolver = Some(resolver);
    }

    /// Resolve `tier` against `current` without applying it.
    pub fn resolve_model_tier(
        &self,
        tier: ModelTier,
        current: &ModelExecutionSelection,
    ) -> TierResolution {
        match &self.tier_resolver {
            Some(resolver) => resolver.resolve_tier(&TierRequest { tier, current }),
            None => TierResolution::unavailable(tier, current),
        }
    }

    /// The tier in force for this turn or, for a subagent started with one,
    /// for its life.
    pub fn active_model_tier(&self) -> Option<ModelTier> {
        self.tracking
            .tier_override
            .as_ref()
            .map(|active| active.tier)
            .or_else(|| self.child_profile.as_ref().and_then(|profile| profile.tier))
    }

    /// The selection the user (or the spawning call) chose, ignoring any
    /// turn-scoped tier in force.
    pub(crate) fn base_model_selection(&self) -> ModelExecutionSelection {
        match &self.tracking.tier_override {
            Some(active) => active.restore.selection.clone(),
            None => self.model_selection(),
        }
    }

    /// Put the session on `model` and its capabilities, without touching a
    /// turn-scoped tier.
    pub(crate) fn apply_resolved_model(&mut self, model: &ChildModel) {
        let mut selection = self.model_selection();
        selection.model = model.model.clone();
        selection.profile_id = model.profile_id.clone();
        selection.reasoning_effort = model.reasoning_effort.clone();
        self.set_model_selection_keeping_tier(selection);
        if let Some(window) = model.context_window {
            self.set_context_window(window, model.output_reserve);
        }
        if let Some(image_input) = model.image_input {
            self.set_image_input_supported(image_input);
        }
    }

    /// Run the rest of this turn on `resolution`. The model state it
    /// replaces is kept (once per turn) and restored when the turn ends.
    pub(crate) fn apply_turn_tier(&mut self, resolution: &TierResolution) {
        let restore = match self.tracking.tier_override.take() {
            Some(active) => active.restore,
            None => SelectionRestore {
                selection: self.model_selection(),
                compaction_policy: self.compaction_policy,
                capacity_tokens: self.context.config.capacity_tokens,
                image_input: self.image_input_supported(),
            },
        };
        self.apply_resolved_model(&resolution.model);
        self.tracking.tier_override = Some(TierOverride {
            tier: resolution.tier,
            restore,
        });
    }

    /// Change the thinking flag the session returns to after a turn-scoped
    /// tier.
    pub(crate) fn set_base_thinking_enabled(&mut self, enabled: bool) {
        if let Some(active) = self.tracking.tier_override.as_mut() {
            active.restore.selection.thinking_enabled = enabled;
        }
    }

    /// Return to the model state a turn-scoped tier replaced. Runs when a
    /// turn ends.
    pub(crate) fn restore_turn_tier(&mut self) {
        let Some(active) = self.tracking.tier_override.take() else {
            return;
        };
        let restore = active.restore;
        self.set_model_selection_keeping_tier(restore.selection);
        self.compaction_policy = restore.compaction_policy;
        self.context.config.capacity_tokens = restore.capacity_tokens;
        self.set_image_input_supported(restore.image_input);
    }

    /// Resolve `tier` for the rest of this turn and apply it, recording what
    /// happened in the turn's signals.
    pub(crate) fn switch_turn_tier(&mut self, tier: ModelTier) -> TierResolution {
        let base = self.base_model_selection();
        let resolution = self.resolve_model_tier(tier, &base);
        if resolution.basis == TierBasis::Unavailable {
            self.record_signal(
                crate::TurnSignalKind::TierUnavailable,
                Some(format!(
                    "{tier}: no tier model or effort for {}",
                    base.model
                )),
            );
        } else {
            self.apply_turn_tier(&resolution);
            self.record_signal(
                crate::TurnSignalKind::TierApplied,
                Some(resolution.describe()),
            );
        }
        resolution
    }

    /// Decide, as a subagent turn fails, whether it will be re-run on the
    /// strong tier. Only the completion evaluator's failures qualify (not a
    /// model call error, not the user's repeated denials), and only once: a
    /// retry that fails again is not retried.
    pub(crate) fn plan_escalation(&mut self, category: &str, failure: &str) {
        if !self.is_subagent()
            || category == "hitl_denied"
            || self.tracking.telemetry.escalation_retry
            || self.tracking.pending_escalation.is_some()
        {
            return;
        }
        let current = self.model_selection();
        let base = self.base_model_selection();
        let resolution = self.resolve_model_tier(ModelTier::Strong, &base);
        let skipped = match resolution.basis {
            TierBasis::Unavailable => Some("no strong tier is available for this provider"),
            _ if resolution.matches(&current) => Some("the turn already ran on the strong tier"),
            _ => None,
        };
        if let Some(reason) = skipped {
            self.record_signal(
                crate::TurnSignalKind::EscalationSkipped,
                Some(reason.to_string()),
            );
            return;
        }
        self.record_signal(
            crate::TurnSignalKind::Escalated,
            Some(resolution.describe()),
        );
        self.tracking.telemetry.escalated = true;
        self.tracking.pending_escalation = Some(EscalationPlan {
            resolution,
            failure: failure.to_string(),
        });
    }

    /// Take the strong-tier re-run planned for the turn that just failed.
    pub(crate) fn take_pending_escalation(&mut self) -> Option<EscalationPlan> {
        self.tracking.pending_escalation.take()
    }

    /// On resume, before a turn found running is recorded as interrupted:
    /// if that turn was a strong-tier retry (its first message is the
    /// retry's note), record it as one, on the model it ran on.
    pub(crate) fn recognize_interrupted_escalation_retry(&mut self) {
        let Some((model, reasoning_effort)) = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == forge_types::MessageRole::User)
            .and_then(|message| parse_escalation_note(&message.content))
        else {
            return;
        };
        let profile_id = model
            .split_once('/')
            .map(|(profile, _)| profile.to_string())
            .unwrap_or_default();
        self.apply_turn_tier(&TierResolution {
            tier: ModelTier::Strong,
            model: ChildModel {
                model,
                profile_id,
                reasoning_effort,
                context_window: None,
                output_reserve: None,
                image_input: None,
            },
            basis: TierBasis::Catalog,
        });
        self.tracking.telemetry.escalation_retry = true;
    }

    /// Put the session on the strong tier for the next turn, which is
    /// recorded as the escalation's retry.
    pub(crate) fn begin_escalation_retry(&mut self, plan: &EscalationPlan) {
        self.apply_turn_tier(&plan.resolution);
        self.tracking.next_turn_is_escalation_retry = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strong(model: &str, effort: Option<&str>) -> TierResolution {
        TierResolution {
            tier: ModelTier::Strong,
            model: ChildModel {
                model: model.into(),
                profile_id: "x".into(),
                reasoning_effort: effort.map(str::to_string),
                context_window: None,
                output_reserve: None,
                image_input: None,
            },
            basis: TierBasis::Catalog,
        }
    }

    #[test]
    fn the_retry_note_is_a_repair_task_that_carries_the_failure_and_model() {
        let note = escalation_note(
            "cargo test did not finish successfully",
            &strong("x/big", Some("high")),
        );
        assert!(note.starts_with("[REPAIR TASK"));
        assert!(note.contains("cargo test did not finish successfully"));
        assert_eq!(
            parse_escalation_note(&note),
            Some(("x/big".to_string(), Some("high".to_string())))
        );
        let plain = escalation_note("  ", &strong("x/big", None));
        assert!(plain.contains("the completion check did not pass"));
        assert_eq!(
            parse_escalation_note(&plain),
            Some(("x/big".to_string(), None))
        );
        assert_eq!(parse_escalation_note("[REPAIR TASK 1] fix the test"), None);
        assert_eq!(
            parse_escalation_note("please retry on a strong model"),
            None
        );
    }

    #[test]
    fn an_unavailable_tier_is_the_current_selection() {
        let current = ModelExecutionSelection {
            model: "x/m".into(),
            profile_id: "x".into(),
            reasoning_effort: Some("medium".into()),
            thinking_enabled: true,
        };
        let resolution = TierResolution::unavailable(ModelTier::Strong, &current);
        assert!(resolution.matches(&current));
        assert_eq!(resolution.basis, TierBasis::Unavailable);
        assert_eq!(resolution.describe(), "strong: x/m @ medium");
    }
}
