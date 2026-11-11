//! Candidate-context construction and validation.
//!
//! Compaction is transactional (§15): this module only ever *proposes* a new
//! model-visible context. Nothing here mutates a session. The caller installs
//! a returned [`CompactionPlan`] or discards it, and discarding it leaves the
//! active context byte-identical to what it was.

use forge_types::{Message, MessageRole};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::checkpoint::{Checkpoint, CheckpointError, CHECKPOINT_ROOT};
use super::facts::ProtectedFact;
use super::policy::{CompactionPolicy, CompactionTrigger};
use super::prompt::checkpoint_message;
use super::tail::{
    messages_tokens, select_tail, select_tail_in_turn, tool_calls_are_paired,
    tool_results_are_complete,
};

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompactionError {
    #[error("compaction is already in progress")]
    AlreadyRunning,
    #[error("nothing to compact: the context has no conversation yet")]
    NothingToCompact,
    #[error("provider call failed: {0}")]
    Provider(String),
    /// The journal write that makes a compaction durable failed. Raised
    /// before the in-memory swap, so the projection is left untouched rather
    /// than becoming something resume could not reproduce.
    #[error("could not journal the compaction: {0}")]
    Journal(String),
    #[error("invalid checkpoint: {0}")]
    Checkpoint(#[from] CheckpointError),
    #[error("checkpoint dropped every user constraint recorded in this session")]
    MissingProtectedFacts,
    #[error("compacted context ({new} tokens) is not smaller than the current one ({old} tokens)")]
    NotSmaller { old: usize, new: usize },
    #[error(
        "compacted context ({new} tokens) is still too close to the compaction \
         threshold ({limit} tokens) to be worth breaking the cached prefix"
    )]
    StillOversized { new: usize, limit: usize },
    #[error("compacted context is structurally invalid: {0}")]
    InvalidStructure(&'static str),
}

impl CompactionError {
    /// Stable, low-cardinality label for telemetry.
    pub fn category(&self) -> &'static str {
        match self {
            Self::AlreadyRunning => "already_running",
            Self::NothingToCompact => "nothing_to_compact",
            Self::Provider(_) => "provider_error",
            Self::Journal(_) => "journal_error",
            Self::Checkpoint(_) => "invalid_checkpoint",
            Self::MissingProtectedFacts => "missing_protected_facts",
            Self::NotSmaller { .. } => "not_smaller",
            Self::StillOversized { .. } => "oversized_result",
            Self::InvalidStructure(_) => "invalid_structure",
        }
    }
}

/// A validated replacement context, ready to install.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// The complete new model-visible context.
    pub messages: Vec<Message>,
    pub checkpoint: Checkpoint,
    /// Index into the pre-compaction context where the retained tail began.
    pub tail_start: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub checkpoint_tokens: usize,
    pub tail_tokens: usize,
    pub retained_messages: usize,
}

/// One compaction attempt's measurements, for telemetry and the status line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub trigger: CompactionTrigger,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub checkpoint_tokens: usize,
    pub retained_tail_tokens: usize,
    pub retained_messages: usize,
    pub context_window: usize,
    pub utilization_before: f64,
    pub utilization_after: f64,
    pub epoch_before: u64,
    pub epoch_after: u64,
    pub duration_ms: u64,
    /// `None` on success; a [`CompactionError::category`] label on failure.
    pub failure_reason: Option<String>,
}

impl CompactionRecord {
    pub fn succeeded(&self) -> bool {
        self.failure_reason.is_none()
    }
}

/// Cumulative compaction telemetry for one session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompactionTelemetry {
    pub compaction_count: u64,
    pub failure_count: u64,
    pub last: Option<CompactionRecord>,
}

impl CompactionTelemetry {
    pub fn record(&mut self, record: CompactionRecord) {
        if record.succeeded() {
            self.compaction_count = self.compaction_count.saturating_add(1);
        } else {
            self.failure_count = self.failure_count.saturating_add(1);
        }
        self.last = Some(record);
    }
}

/// Persisted context-projection state. Deliberately does not duplicate
/// canonical history: it records how the projection was derived, not what it
/// contains.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionContextState {
    /// Cache epoch the projection belongs to. Bumped once per compaction.
    pub epoch: u64,
    /// Structured state installed by the most recent compaction.
    pub checkpoint: Option<Checkpoint>,
    /// Index, in the pre-compaction context, where the retained raw tail began.
    pub tail_start_message_index: Option<usize>,
    /// User-authored statements that must survive every future compaction.
    pub protected_facts: Vec<ProtectedFact>,
}

impl SessionContextState {}

/// True when `message` is an installed checkpoint rather than a real system
/// prompt. A previous checkpoint is replaced by the new one, never stacked.
pub fn is_checkpoint_message(message: &Message) -> bool {
    message.role == MessageRole::System && message.content.contains(&format!("<{CHECKPOINT_ROOT}"))
}

/// Build and validate the replacement context.
///
/// Tries the policy's tail target first and shrinks it if the result would
/// still sit too close to the compaction threshold (§17). Never touches
/// canonical history: `context` is read-only here.
pub fn plan_compaction(
    context: &[Message],
    checkpoint: &Checkpoint,
    policy: &CompactionPolicy,
    protected_facts: &[ProtectedFact],
) -> Result<CompactionPlan, CompactionError> {
    checkpoint.validate()?;
    if !protected_facts.is_empty() && !checkpoint.has_user_constraints() {
        return Err(CompactionError::MissingProtectedFacts);
    }

    let body_start = context
        .iter()
        .position(|message| !(message.role == MessageRole::System))
        .unwrap_or(context.len());
    if body_start >= context.len() {
        return Err(CompactionError::NothingToCompact);
    }
    // Everything before the conversation is the stable prefix, minus any
    // checkpoint a previous compaction installed — that one is replaced, so
    // repeated compaction merges state instead of stacking summaries.
    let preserved: Vec<Message> = context[..body_start]
        .iter()
        .filter(|message| !is_checkpoint_message(message))
        .cloned()
        .collect();
    let body = &context[body_start..];
    let source_body_complete = tool_results_are_complete(body);

    let tokens_before = messages_tokens(context);
    let checkpoint_message = checkpoint_message(checkpoint);
    let checkpoint_tokens = messages_tokens(std::slice::from_ref(&checkpoint_message));
    let runway_limit = policy.runway_limit();
    let preserved_tokens = messages_tokens(&preserved);

    let target = policy.tail_target_tokens();
    let attempts = [target, target / 2, target / 4, 0];
    let mut last: Option<CompactionPlan> = None;

    for attempt in attempts {
        let mut selection = select_tail(body, attempt);
        let mut tail = selection.messages(body);
        let standard_after = preserved_tokens + checkpoint_tokens + messages_tokens(&tail);
        // A turn too long to keep whole (or a target that left nothing, which
        // would drop the user's request) is cut inside the turn instead.
        if standard_after > runway_limit || tail.is_empty() {
            if let Some(in_turn) = select_tail_in_turn(body, attempt) {
                let candidate = in_turn.messages(body);
                let candidate_after =
                    preserved_tokens + checkpoint_tokens + messages_tokens(&candidate);
                if tail.is_empty() || candidate_after < standard_after {
                    selection = in_turn;
                    tail = candidate;
                }
            }
        }
        validate_structure(&preserved, &tail, source_body_complete)?;

        let mut messages = Vec::with_capacity(preserved.len() + 1 + tail.len());
        messages.extend(preserved.iter().cloned());
        messages.push(checkpoint_message.clone());
        messages.extend(tail.iter().cloned());
        let tokens_after = messages_tokens(&messages);

        let plan = CompactionPlan {
            messages,
            checkpoint: checkpoint.clone(),
            tail_start: body_start + selection.start,
            tokens_before,
            tokens_after,
            checkpoint_tokens,
            tail_tokens: selection.tokens,
            retained_messages: tail.len(),
        };
        if tokens_after <= runway_limit {
            return finish(plan);
        }
        let shrank = last
            .as_ref()
            .is_none_or(|previous| plan.tokens_after < previous.tokens_after);
        if shrank {
            last = Some(plan);
        }
        if selection.start == 0 && selection.anchor.is_none() {
            // The tail already covers the whole body and no cut inside the
            // current turn was possible; a smaller target cannot help.
            break;
        }
    }

    // Nothing hit the runway target. Accept the smallest candidate only if it
    // still buys real headroom below the trigger; otherwise refuse rather
    // than break the cached prefix for a marginal gain.
    let plan = last.ok_or(CompactionError::NothingToCompact)?;
    let threshold = policy.trigger_threshold();
    if plan.tokens_after >= threshold {
        return Err(CompactionError::StillOversized {
            new: plan.tokens_after,
            limit: runway_limit,
        });
    }
    finish(plan)
}

fn finish(plan: CompactionPlan) -> Result<CompactionPlan, CompactionError> {
    if plan.tokens_after >= plan.tokens_before {
        return Err(CompactionError::NotSmaller {
            old: plan.tokens_before,
            new: plan.tokens_after,
        });
    }
    Ok(plan)
}

/// Provider-independent structural checks on a candidate context.
fn validate_structure(
    preserved: &[Message],
    tail: &[Message],
    source_body_complete: bool,
) -> Result<(), CompactionError> {
    if preserved.is_empty() {
        return Err(CompactionError::InvalidStructure(
            "context has no system prompt to preserve",
        ));
    }
    if !preserved.iter().all(|m| m.role == MessageRole::System) {
        return Err(CompactionError::InvalidStructure(
            "preserved prefix contains a non-system message",
        ));
    }
    if !tool_calls_are_paired(tail) {
        return Err(CompactionError::InvalidStructure(
            "retained tail opens on a tool result with no matching call",
        ));
    }
    if source_body_complete && !tool_results_are_complete(tail) {
        return Err(CompactionError::InvalidStructure(
            "retained tail drops the result of a tool call it keeps",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::facts::ProtectedFactKind;
    use forge_types::ToolCall;

    fn checkpoint() -> Checkpoint {
        Checkpoint::parse(
            "<forge_checkpoint version=\"1\"><objective>ship compaction</objective>\
             <user_constraints>do not change the public API</user_constraints>\
             <next_action>write tests</next_action></forge_checkpoint>",
        )
        .unwrap()
    }

    fn bare_checkpoint() -> Checkpoint {
        Checkpoint::parse(
            "<forge_checkpoint version=\"1\"><objective>o</objective>\
             <next_action>n</next_action></forge_checkpoint>",
        )
        .unwrap()
    }

    fn fact(text: &str) -> ProtectedFact {
        ProtectedFact {
            kind: ProtectedFactKind::UserConstraint,
            text: text.into(),
            source_message_index: 1,
        }
    }

    fn content_of(messages: &[Message]) -> Vec<&str> {
        messages.iter().map(|m| m.content.as_str()).collect()
    }

    fn big(tag: &str, chars: usize) -> String {
        format!("{tag}{}", "x".repeat(chars))
    }

    /// A long conversation: system prompt plus alternating turns, each ~250
    /// tokens, for ~10K tokens total.
    fn long_context(turns: usize) -> Vec<Message> {
        let mut messages = vec![Message::new(MessageRole::System, "system prompt")];
        for i in 0..turns {
            messages.push(Message::new(
                MessageRole::User,
                big(&format!("u{i}"), 1_000),
            ));
            messages.push(Message::new(
                MessageRole::Assistant,
                big(&format!("a{i}"), 1_000),
            ));
        }
        messages
    }

    fn policy() -> CompactionPolicy {
        CompactionPolicy::for_window(30_000, Some(2_000))
    }

    /// One long turn: the request, then `steps` tool-calling rounds of ~250
    /// tokens each, ending with a result that awaits the model's next step.
    fn long_turn(request: &str, steps: usize) -> Vec<Message> {
        let mut messages = vec![Message::new(MessageRole::User, request.to_string())];
        for i in 0..steps {
            let mut call = Message::new(MessageRole::Assistant, big(&format!("s{i}"), 500));
            call.tool_calls = vec![ToolCall {
                id: format!("c{i}"),
                name: "bash".into(),
                arguments: serde_json::json!({"command": format!("step {i}")}),
            }];
            messages.push(call);
            let mut result = Message::new(MessageRole::Tool, big(&format!("r{i}"), 500));
            result.tool_call_id = Some(format!("c{i}"));
            messages.push(result);
        }
        messages
    }

    fn pairs_are_intact(messages: &[Message]) -> bool {
        tool_calls_are_paired(messages) && tool_results_are_complete(messages)
    }

    /// A current turn bigger than the whole budget used to be kept whole
    /// (never shrinking) or dropped with its request. It is cut inside the
    /// turn: the request stays, then the latest steps.
    #[test]
    fn a_long_tool_turn_is_cut_inside_the_turn_and_keeps_its_request() {
        let mut context = long_context(6);
        context.extend(long_turn("fix the failing build", 120));
        let plan = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();

        assert!(plan.tokens_after <= policy().runway_limit(), "{plan:?}");
        let kept = &plan.messages[2..];
        assert_eq!(
            kept[0].content, "fix the failing build",
            "the request leads the tail"
        );
        assert_eq!(kept[0].role, MessageRole::User);
        assert!(kept.len() > 1, "recent steps are kept");
        assert_eq!(
            kept.last().unwrap().content,
            context.last().unwrap().content,
            "the latest result is kept"
        );
        assert!(pairs_are_intact(kept), "every call keeps its result");
        assert_eq!(
            kept[1].role,
            MessageRole::Assistant,
            "the cut opens on a step"
        );
    }

    /// The same turn as a session's first: there is no earlier history for
    /// the ordinary selector to snap to, so it could never compact at all.
    #[test]
    fn a_long_first_turn_can_be_compacted() {
        let mut context = vec![Message::new(MessageRole::System, "system prompt")];
        context.extend(long_turn("build the whole feature", 120));
        let plan = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();

        assert!(plan.tokens_after < plan.tokens_before);
        assert!(plan.tokens_after <= policy().runway_limit(), "{plan:?}");
        let kept = &plan.messages[2..];
        assert_eq!(kept[0].content, "build the whole feature");
        assert!(pairs_are_intact(kept));
    }

    /// A turn with nothing the selector can cut at keeps the user's request
    /// rather than installing a context with no conversation in it.
    #[test]
    fn compaction_never_installs_a_context_without_the_users_request() {
        let mut context = long_context(30);
        context.extend(long_turn("the current request", 3));
        // A target of zero (the planner's last resort) used to leave nothing.
        let selection = select_tail_in_turn(&context[1..], 0).expect("a user message exists");
        let kept = selection.messages(&context[1..]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].content, "the current request");
        assert!(select_tail_in_turn(&[Message::new(MessageRole::Assistant, "a")], 0).is_none());
    }

    #[test]
    fn plan_preserves_the_system_prompt_and_installs_the_checkpoint_second() {
        let context = long_context(40);
        let plan = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();
        assert_eq!(plan.messages[0].content, context[0].content);
        assert!(is_checkpoint_message(&plan.messages[1]));
        assert_eq!(plan.messages[2..].len(), plan.retained_messages);
        assert!(plan.tokens_after < plan.tokens_before);
        assert!(plan.tokens_after <= policy().runway_limit());
    }

    #[test]
    fn plan_keeps_the_most_recent_turns_verbatim() {
        let context = long_context(40);
        let plan = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();
        assert_eq!(
            plan.messages.last().unwrap().content,
            context.last().unwrap().content
        );
        assert!(plan.retained_messages > 0, "a raw tail must be retained");
        assert_eq!(
            content_of(&context[plan.tail_start..]),
            content_of(&plan.messages[2..]),
            "the retained tail must be the source messages, unedited"
        );
    }

    #[test]
    fn repeated_compaction_replaces_the_previous_checkpoint_instead_of_stacking() {
        let context = long_context(40);
        let first = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();

        // Grow the compacted context back up, then compact again.
        let mut grown = first.messages.clone();
        for i in 0..40 {
            grown.push(Message::new(
                MessageRole::User,
                big(&format!("v{i}"), 1_000),
            ));
            grown.push(Message::new(
                MessageRole::Assistant,
                big(&format!("b{i}"), 1_000),
            ));
        }
        let second = plan_compaction(&grown, &checkpoint(), &policy(), &[]).unwrap();
        assert_eq!(
            second
                .messages
                .iter()
                .filter(|m| is_checkpoint_message(m))
                .count(),
            1,
            "a second compaction must replace the first checkpoint, not stack another one"
        );
        assert_eq!(second.messages[0].content, context[0].content);
    }

    #[test]
    fn a_checkpoint_that_drops_every_user_constraint_is_rejected() {
        let context = long_context(40);
        let error = plan_compaction(
            &context,
            &bare_checkpoint(),
            &policy(),
            &[fact("do not change the public API")],
        )
        .unwrap_err();
        assert_eq!(error, CompactionError::MissingProtectedFacts);
        assert_eq!(error.category(), "missing_protected_facts");
    }

    #[test]
    fn a_context_too_small_to_shrink_is_rejected_rather_than_installed() {
        let context = vec![
            Message::new(MessageRole::System, "system prompt"),
            Message::new(MessageRole::User, "hi"),
            Message::new(MessageRole::Assistant, "hello"),
        ];
        let error = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap_err();
        assert!(
            matches!(error, CompactionError::NotSmaller { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_context_with_no_conversation_has_nothing_to_compact() {
        let context = vec![Message::new(MessageRole::System, "system prompt")];
        assert_eq!(
            plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap_err(),
            CompactionError::NothingToCompact
        );
    }

    #[test]
    fn the_retained_tail_never_orphans_a_tool_result() {
        let mut context = vec![Message::new(MessageRole::System, "system prompt")];
        for i in 0..40 {
            context.push(Message::new(MessageRole::User, big(&format!("u{i}"), 400)));
            let mut call = Message::new(MessageRole::Assistant, "running");
            call.tool_calls = vec![ToolCall {
                id: format!("c{i}"),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls"}),
            }];
            context.push(call);
            let mut result = Message::new(MessageRole::Tool, big(&format!("r{i}"), 1_500));
            result.tool_call_id = Some(format!("c{i}"));
            context.push(result);
        }
        let plan = plan_compaction(&context, &checkpoint(), &policy(), &[]).unwrap();
        let tail = &plan.messages[2..];
        assert!(tool_calls_are_paired(tail));
        assert!(tool_results_are_complete(tail));
        assert_ne!(tail[0].role, MessageRole::Tool);
    }

    #[test]
    fn telemetry_separates_successes_from_failures() {
        let mut telemetry = CompactionTelemetry::default();
        let base = CompactionRecord {
            trigger: CompactionTrigger::Automatic,
            tokens_before: 100,
            tokens_after: 40,
            checkpoint_tokens: 10,
            retained_tail_tokens: 30,
            retained_messages: 4,
            context_window: 200,
            utilization_before: 0.5,
            utilization_after: 0.2,
            epoch_before: 0,
            epoch_after: 1,
            duration_ms: 12,
            failure_reason: None,
        };
        telemetry.record(base.clone());
        telemetry.record(CompactionRecord {
            failure_reason: Some("provider_error".into()),
            ..base
        });
        assert_eq!(telemetry.compaction_count, 1);
        assert_eq!(telemetry.failure_count, 1);
        assert!(!telemetry.last.unwrap().succeeded());
    }
}
