//! How a session's models are chosen, kept together: what its turns are
//! recorded as (its task kind), the advisors its owner installs, and Auto.
//! One place builds it — for a new session, and for one reopened from its
//! journal — so every way a session is made agrees. The owner installs the
//! services that choose models in one go ([`ModelServices`]).

use std::sync::Arc;

use forge_types::TaskKind;

use crate::auto_model::{AutoDecision, AutoModel, AutoModelAdvisor};
use crate::custom_agents::ChildModelResolver;
use crate::model_choice::SubagentModelAdvisor;
use crate::tiers::ModelTierResolver;

pub(crate) struct SessionModels {
    /// What this session's turns are recorded as: `main` for a top-level
    /// session, `subagent` or `custom:<name>` for a child. Set when the
    /// session is built or switched to another journal.
    pub(crate) task_kind: TaskKind,
    /// Chooses the model of a child started without a tier or a model.
    /// Installed by the session's owner; a child has none (it cannot spawn).
    pub(crate) subagent_advisor: Option<Arc<dyn SubagentModelAdvisor>>,
    /// Chooses where Auto turns start (the session's owner installs it).
    pub(crate) auto_advisor: Option<Arc<dyn AutoModelAdvisor>>,
    /// Auto, when the user chose it for this (top-level) session.
    pub(crate) auto: Option<AutoModel>,
    /// The model and effort Auto chose for each of its turns, and why.
    pub(crate) auto_decisions: Vec<AutoDecision>,
}

impl SessionModels {
    /// A new session of `task_kind`: no advisors yet, Auto off.
    pub(crate) fn new(task_kind: TaskKind) -> Self {
        Self {
            task_kind,
            subagent_advisor: None,
            auto_advisor: None,
            auto: None,
            auto_decisions: Vec::new(),
        }
    }

    /// A session reopened from its journal: the journal's agent profile,
    /// Auto state and decisions (see [`Self::reopen_from`]).
    pub(crate) fn reopened(
        agent_profile: Option<&serde_json::Value>,
        auto: Option<&serde_json::Value>,
        auto_decisions: &[serde_json::Value],
        is_child: bool,
    ) -> Self {
        let mut models = Self::new(TaskKind::Main);
        models.reopen_from(agent_profile, auto, auto_decisions, is_child);
        models
    }

    /// Switch to a replayed journal: the kind it shows
    /// (`forge_durable::kind_from_journal`), and Auto as it last recorded it
    /// — never for a child, which does not run Auto. The installed advisors
    /// are kept.
    pub(crate) fn reopen_from(
        &mut self,
        agent_profile: Option<&serde_json::Value>,
        auto: Option<&serde_json::Value>,
        auto_decisions: &[serde_json::Value],
        is_child: bool,
    ) {
        self.task_kind = crate::custom_agents::kind_of_journaled_session(agent_profile, is_child);
        if is_child {
            self.auto = None;
            self.auto_decisions = Vec::new();
        } else {
            self.auto = crate::auto_model::auto_from_journal(auto);
            self.auto_decisions = crate::auto_model::decisions_from_journal(auto_decisions);
        }
    }
}

/// The services that choose a session's models, which its owner installs:
/// each one given is installed, each one left out (`None`) keeps what is
/// installed. Without any, a session runs on the model it was given.
#[derive(Clone, Default)]
pub struct ModelServices {
    /// Validates a model or effort `spawn_agent` asks for; without it only
    /// the request's shape is checked.
    pub child_models: Option<Arc<dyn ChildModelResolver>>,
    /// Resolves model tiers; shared with the subagents the session spawns,
    /// which need it to escalate.
    pub tiers: Option<Arc<dyn ModelTierResolver>>,
    /// Chooses the model of a child started without a tier or a model.
    pub subagent_advisor: Option<Arc<dyn SubagentModelAdvisor>>,
    /// Chooses where Auto turns start, and knows models' efforts.
    pub auto_advisor: Option<Arc<dyn AutoModelAdvisor>>,
}

impl crate::AgentSession {
    /// Install the services that choose this session's models (see
    /// [`ModelServices`]).
    pub fn install_model_services(&mut self, services: ModelServices) {
        let ModelServices {
            child_models,
            tiers,
            subagent_advisor,
            auto_advisor,
        } = services;
        if child_models.is_some() {
            self.child_model_resolver = child_models;
        }
        if tiers.is_some() {
            self.tier_resolver = tiers;
        }
        if subagent_advisor.is_some() {
            self.models.subagent_advisor = subagent_advisor;
        }
        if auto_advisor.is_some() {
            self.models.auto_advisor = auto_advisor;
        }
    }

    /// The services installed.
    #[cfg(test)]
    pub(crate) fn model_services(&self) -> ModelServices {
        ModelServices {
            child_models: self.child_model_resolver.clone(),
            tiers: self.tier_resolver.clone(),
            subagent_advisor: self.models.subagent_advisor.clone(),
            auto_advisor: self.models.auto_advisor.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_model::{AutoFallback, AutoStart, ModelEfforts};
    use crate::{ModelAdvice, ModelAdviceRequest};

    struct NoAdvice;
    impl SubagentModelAdvisor for NoAdvice {
        fn advise(&self, _request: &ModelAdviceRequest<'_>) -> ModelAdvice {
            unreachable!("not asked")
        }
    }
    impl AutoModelAdvisor for NoAdvice {
        fn start(&self, _fallback: &AutoFallback) -> AutoStart {
            unreachable!("not asked")
        }
        fn efforts(&self, _model: &str, _profile_id: &str) -> ModelEfforts {
            ModelEfforts::default()
        }
    }

    fn fallback() -> AutoFallback {
        AutoFallback {
            model: "anthropic/claude-sonnet-4-5".into(),
            profile_id: "anthropic".into(),
            reasoning_effort: Some("medium".into()),
        }
    }

    fn with_advisors(kind: TaskKind) -> SessionModels {
        let mut models = SessionModels::new(kind);
        models.subagent_advisor = Some(Arc::new(NoAdvice));
        models.auto_advisor = Some(Arc::new(NoAdvice));
        models
    }

    #[test]
    fn a_new_session_has_its_kind_and_nothing_else() {
        let models = SessionModels::new(TaskKind::Subagent);
        assert_eq!(models.task_kind, TaskKind::Subagent);
        assert!(models.subagent_advisor.is_none());
        assert!(models.auto_advisor.is_none());
        assert!(models.auto.is_none());
        assert!(models.auto_decisions.is_empty());
    }

    /// Switching to another journal takes the kind and Auto from it; the
    /// owner's advisors stay installed.
    #[test]
    fn reopening_takes_kind_and_auto_from_the_journal_and_keeps_the_advisors() {
        let mut models = with_advisors(TaskKind::Main);
        let auto = AutoModel::new(fallback());
        let state = serde_json::json!(auto);
        models.reopen_from(None, Some(&state), &[], false);
        assert_eq!(models.task_kind, TaskKind::Main);
        assert_eq!(models.auto, Some(auto));
        assert!(models.subagent_advisor.is_some());
        assert!(models.auto_advisor.is_some());

        // Auto turned off in the journal (a `null` state).
        models.reopen_from(None, Some(&serde_json::Value::Null), &[], false);
        assert_eq!(models.auto, None);
    }

    /// A child's kind comes from its journaled profile, and it never runs
    /// Auto, whatever its journal holds.
    #[test]
    fn a_reopened_child_has_its_profiles_kind_and_no_auto() {
        let profile = serde_json::json!({ "agent": "reviewer" });
        let auto = serde_json::json!(AutoModel::new(fallback()));
        let models = SessionModels::reopened(Some(&profile), Some(&auto), &[], true);
        assert_eq!(models.task_kind, "custom:reviewer".parse().unwrap());
        assert_eq!(models.auto, None);
        assert!(models.auto_decisions.is_empty());

        let plain = SessionModels::reopened(None, None, &[], true);
        assert_eq!(plain.task_kind, TaskKind::Subagent);
    }
}
