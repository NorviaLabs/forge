//! Session-level compaction tests: the transactional install, the automatic
//! trigger, cache-epoch behaviour, canonical-history preservation, and
//! persistence across resume.
//!
//! The pure pieces (policy arithmetic, checkpoint parsing, tail selection,
//! candidate validation) are covered in `forge_context::compaction`. What is
//! tested here is everything that needs a real session: a model client, a
//! journal, and the prompt-wire snapshots the cache diagnostics compare.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use forge_model::{ModelClient, ModelError, ModelRequest, SharedMessages, StreamEventTx};
use forge_tools::ToolRegistry;
use forge_types::{Message, MessageRole, ModelResponse, ModelStreamEvent};
use tempfile::tempdir;

use crate::{AgentSession, CompactionTrigger, LoopConfig, LoopError};

const COMPACTION_MARKER: &str = "[forge:compaction]";

fn checkpoint_text() -> String {
    r#"<forge_checkpoint version="1">
<objective>
Wire compaction into the session.
</objective>
<user_constraints>
Do not change the public API.
</user_constraints>
<current_work>
Installing the candidate context.
</current_work>
<next_action>
Run the test suite.
</next_action>
</forge_checkpoint>"#
        .to_string()
}

/// A model that answers ordinary turns with a fixed reply and compaction
/// requests with a scripted checkpoint, and keeps the compaction request so
/// tests can assert on the exact bytes that were sent.
struct CheckpointModel {
    checkpoint: Mutex<Vec<Result<String, String>>>,
    reply: String,
    compaction_requests: Mutex<Vec<SharedMessages>>,
}

impl CheckpointModel {
    fn new(checkpoints: Vec<Result<String, String>>) -> Self {
        Self {
            checkpoint: Mutex::new(checkpoints),
            reply: "ok".into(),
            compaction_requests: Mutex::new(Vec::new()),
        }
    }

    /// Always returns a valid checkpoint, however many times it is asked.
    fn valid() -> Arc<Self> {
        Arc::new(Self::new(vec![]))
    }

    fn is_compaction(req: &ModelRequest) -> bool {
        req.messages
            .last()
            .is_some_and(|message| message.content.starts_with(COMPACTION_MARKER))
    }

    fn next_checkpoint(&self) -> Result<String, String> {
        let mut scripted = self.checkpoint.lock().unwrap();
        if scripted.is_empty() {
            return Ok(checkpoint_text());
        }
        scripted.remove(0)
    }

    fn compaction_requests(&self) -> Vec<SharedMessages> {
        self.compaction_requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelClient for CheckpointModel {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        if Self::is_compaction(&req) {
            self.compaction_requests
                .lock()
                .unwrap()
                .push(req.messages.clone());
            let text = self.next_checkpoint().map_err(ModelError::Provider)?;
            return Ok(ModelResponse {
                text,
                tool_calls: vec![],
                usage: None,
                thinking: None,
            });
        }
        Ok(ModelResponse {
            text: self.reply.clone(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        })
    }

    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let response = self.complete(req).await?;
        if let Some(tx) = tx {
            let _ = tx.send(ModelStreamEvent::TextDelta {
                text: response.text.clone(),
            });
            let _ = tx.send(ModelStreamEvent::MessageEnd);
        }
        Ok(response)
    }

    fn clear_provider_env(&self) {}
}

fn cfg(dir: &std::path::Path) -> LoopConfig {
    LoopConfig {
        max_turns: 5,
        workspace: dir.to_path_buf(),
        journal_dir: dir.join("j"),
        enable_context_lifecycle: true,
        enable_governance: true,
        ..Default::default()
    }
}

async fn session_with(model: Arc<dyn ModelClient>, dir: &std::path::Path) -> AgentSession {
    AgentSession::create(cfg(dir), model, ToolRegistry::new())
        .await
        .unwrap()
}

/// ~2_500 tokens under the ~4-chars/token estimate.
fn padding(tag: &str) -> String {
    format!("{tag} {}", "x".repeat(10_000))
}

/// Drive `turns` real user turns through the session so canonical history is
/// genuinely journaled, not just pushed onto the in-memory transcript.
async fn grow_context(session: &mut AgentSession, turns: usize) {
    for i in 0..turns {
        session
            .run_user_message(&padding(&format!("request {i}:")))
            .await
            .unwrap();
    }
}

/// Grow the conversation without the automatic trigger firing partway
/// through: a fixture needs to *reach* pressure, not be rescued from it.
async fn grow_without_pressure(session: &mut AgentSession, turns: usize) {
    let policy = *session.compaction_policy();
    session.set_context_window(4_000_000, Some(policy.max_output_reserve));
    grow_context(session, turns).await;
    session.set_context_window(policy.context_window, Some(policy.max_output_reserve));
}

/// Window these fixtures compact against.
///
/// Pinned rather than inherited from `CompactionPolicy::default()`: the
/// product default moved 200K -> 500K, which pushed the trigger from 170K to
/// 425K and silently left every fixture below the boundary — the suite then
/// failed on its own "must actually be under context pressure" guard rather
/// than on anything about compaction. These tests are about behaviour at the
/// boundary, so they set the boundary themselves.
const FIXTURE_CONTEXT_WINDOW: usize = 200_000;

/// A session whose context sits above the automatic compaction boundary.
async fn pressured_session(model: Arc<CheckpointModel>, dir: &std::path::Path) -> AgentSession {
    let mut session = session_with(model, dir).await;
    session.set_context_window(FIXTURE_CONTEXT_WINDOW, None);
    // 200K window: the trigger sits at 170K, and each turn is ~2.5K tokens.
    grow_without_pressure(&mut session, 70).await;
    assert!(
        session.context_pressure_reached(),
        "test fixture must actually be under context pressure ({} tokens)",
        session.context_tokens()
    );
    session
}

// ---------------------------------------------------------------- triggers

#[tokio::test]
async fn automatic_compaction_runs_when_the_next_turn_would_cross_the_boundary() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let before = session.context_tokens();
    session.prepare_model_step(0).await.unwrap();

    assert_eq!(session.compaction_telemetry().compaction_count, 1);
    let record = session.compaction_telemetry().last.clone().unwrap();
    assert_eq!(record.trigger, CompactionTrigger::Automatic);
    assert_eq!(record.tokens_before, before);
    assert!(record.tokens_after < record.tokens_before);
    assert!(
        record.utilization_after <= 0.40,
        "compaction must buy real runway, not shave a few percent: {:.2}",
        record.utilization_after
    );
    assert!(session
        .events
        .iter()
        .any(|event| event.kind == "context_compacted"));
}

#[tokio::test]
async fn automatic_compaction_does_not_run_below_the_boundary() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = session_with(model.clone(), dir.path()).await;
    grow_context(&mut session, 2).await;

    assert!(!session.context_pressure_reached());
    let messages_before = session.messages.len();
    session.prepare_model_step(0).await.unwrap();

    assert_eq!(session.compaction_telemetry().compaction_count, 0);
    assert!(model.compaction_requests().is_empty());
    assert_eq!(session.messages.len(), messages_before);
    assert!(!session
        .events
        .iter()
        .any(|event| event.kind == "context_compacted"));
}

#[tokio::test]
async fn manual_compaction_runs_below_the_automatic_threshold() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = session_with(model.clone(), dir.path()).await;
    // Well under the 170K trigger, but big enough that a checkpoint is smaller.
    grow_context(&mut session, 30).await;
    assert!(!session.context_pressure_reached());

    let record = session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    assert_eq!(record.trigger, CompactionTrigger::Manual);
    assert!(record.tokens_after < record.tokens_before);
    assert_eq!(session.installed_checkpoint_count(), 1);
}

#[tokio::test]
async fn manual_and_automatic_compaction_share_one_pipeline() {
    let dir = tempdir().unwrap();
    let manual_dir = tempdir().unwrap();

    let mut automatic = pressured_session(CheckpointModel::valid(), dir.path()).await;
    automatic.prepare_model_step(0).await.unwrap();

    let mut manual = pressured_session(CheckpointModel::valid(), manual_dir.path()).await;
    manual
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let a = automatic.compaction_telemetry().last.clone().unwrap();
    let m = manual.compaction_telemetry().last.clone().unwrap();
    assert_eq!(a.tokens_after, m.tokens_after);
    assert_eq!(a.retained_messages, m.retained_messages);
    assert_eq!(a.checkpoint_tokens, m.checkpoint_tokens);
    assert_ne!(a.trigger, m.trigger, "only the trigger label differs");
}

// ------------------------------------------------- cache-friendly request

#[tokio::test]
async fn the_compaction_request_is_the_live_context_plus_one_appended_message() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let live: Vec<String> = session
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect();
    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let requests = model.compaction_requests();
    assert_eq!(requests.len(), 1);
    let sent: Vec<String> = requests[0]
        .iter()
        .map(|message| message.content.clone())
        .collect();
    assert_eq!(
        sent.len(),
        live.len() + 1,
        "the compaction request must append exactly one message"
    );
    assert_eq!(
        &sent[..live.len()],
        live.as_slice(),
        "everything before the instruction must be byte-identical to the live \
         context, or the provider cannot serve it from its cached prefix"
    );
    assert!(sent.last().unwrap().starts_with(COMPACTION_MARKER));
    assert_eq!(requests[0].last().unwrap().role, MessageRole::User);
}

#[tokio::test]
async fn compaction_opens_a_new_cache_epoch_and_appending_resumes_after_it() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let epoch_before = session.compaction_policy().context_window; // touch, keeps policy read live
    let _ = epoch_before;
    let record = session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(
        record.epoch_after,
        record.epoch_before + 1,
        "a successful compaction opens exactly one new epoch"
    );
    assert_eq!(session.context_state().epoch, record.epoch_after);

    // Append-only caching resumes immediately: the next two requests inside
    // the new epoch must be byte prefixes of one another.
    session.prepare_model_step(0).await.unwrap();
    let (_, first) = session.last_prompt_snapshot_for_tests();
    session
        .messages
        .push(Message::new(MessageRole::User, "next turn"));
    session.prepare_model_step(1).await.unwrap();
    let (_, second) = session.last_prompt_snapshot_for_tests();

    assert!(!first.is_empty());
    assert_eq!(
        forge_model::common_prefix_len(&first, &second),
        first.len(),
        "post-compaction turns must stay append-only"
    );
}

// -------------------------------------------------------- what survives

#[tokio::test]
async fn canonical_history_survives_compaction_intact() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let journal = forge_durable::Journal::open(session.journal_dir(), session.session_id)
        .await
        .unwrap();
    let before = journal.replay(session.session_id).await.unwrap();

    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let after = journal.replay(session.session_id).await.unwrap();
    assert_eq!(
        after.user_messages, before.user_messages,
        "every canonical user message must remain in the journal"
    );
    assert_eq!(after.model_responses.len(), before.model_responses.len());
    assert_eq!(after.tool_results.len(), before.tool_results.len());
    assert!(
        after.events.len() > before.events.len(),
        "compaction is additive: it appends an event, it never rewrites history"
    );
    // The projection did shrink, even though canonical history did not.
    assert!(after.messages.len() < before.messages.len());
    assert_eq!(after.compaction_count, 1);
}

#[tokio::test]
async fn the_retained_tail_is_the_most_recent_conversation_verbatim() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let last_two: Vec<String> = session
        .messages
        .iter()
        .rev()
        .take(2)
        .map(|message| message.content.clone())
        .collect();
    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let tail: Vec<String> = session
        .messages
        .iter()
        .rev()
        .take(2)
        .map(|message| message.content.clone())
        .collect();
    assert_eq!(tail, last_two, "the newest turns are kept unedited");

    let record = session.compaction_telemetry().last.clone().unwrap();
    assert!(record.retained_messages >= 2);
    assert!(
        record.retained_tail_tokens >= 16_000,
        "a 200K window asks for a ~24K raw tail, not a token or two: got {}",
        record.retained_tail_tokens
    );
    // System prompt, checkpoint, then the raw tail.
    assert_eq!(session.messages[0].role, MessageRole::System);
    assert_eq!(session.messages[1].role, MessageRole::System);
    assert_eq!(session.installed_checkpoint_count(), 1);
}

#[tokio::test]
async fn a_tool_call_and_its_result_are_never_split_by_the_tail_boundary() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    // Interleave tool exchanges into the tail region.
    for i in 0..12 {
        let mut call = Message::new(MessageRole::Assistant, "running");
        call.tool_calls = vec![forge_types::ToolCall {
            id: format!("call-{i}"),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "ls"}),
        }];
        session.messages.push(call);
        let mut result = Message::new(MessageRole::Tool, padding("tool output:"));
        result.tool_call_id = Some(format!("call-{i}"));
        result.name = Some("bash".into());
        session.messages.push(result);
    }

    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let tail: Vec<Message> = session.messages[2..].to_vec();
    assert!(forge_context::compaction::tool_calls_are_paired(&tail));
    assert!(forge_context::compaction::tool_results_are_complete(&tail));
    assert_ne!(tail[0].role, MessageRole::Tool);
}

#[tokio::test]
async fn user_constraints_are_supplied_to_the_prompt_and_required_in_the_result() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    let instruction = model.compaction_requests()[0]
        .last()
        .unwrap()
        .content
        .clone();
    assert!(
        instruction.contains("request 69:"),
        "the most recent user constraints must be listed verbatim"
    );
    assert!(instruction.contains("explicit user constraints"));
    assert_eq!(
        session.context_state().protected_facts.len(),
        70,
        "every canonical user turn is retained as a protected fact, even though \
         only the most recent window is supplied to any one prompt"
    );
    assert!(!session.context_state().protected_facts.is_empty());
    assert_eq!(
        session
            .context_state()
            .checkpoint
            .as_ref()
            .unwrap()
            .section("user_constraints"),
        Some("Do not change the public API.")
    );
}

// ------------------------------------------------------ repeated compaction

#[tokio::test]
async fn repeated_compaction_merges_state_instead_of_nesting_summaries() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();
    let system_prompt = session.messages[0].content.clone();

    grow_without_pressure(&mut session, 60).await;
    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();

    assert_eq!(
        session.installed_checkpoint_count(),
        1,
        "a second compaction replaces the first checkpoint rather than stacking one"
    );
    assert_eq!(session.messages[0].content, system_prompt);
    assert_eq!(session.compaction_telemetry().compaction_count, 2);

    // The second request must ask for a merge, not a summary of a summary.
    let requests = model.compaction_requests();
    assert_eq!(requests.len(), 2);
    let second = requests[1].last().unwrap().content.clone();
    assert!(second.contains("Do not narratively summarise the previous checkpoint"));
    assert!(!requests[0]
        .last()
        .unwrap()
        .content
        .contains("Do not narratively summarise the previous checkpoint"));
}

// ------------------------------------------------------------- failures

/// Every failure mode must leave the projection, the epoch, and canonical
/// history exactly as they were.
async fn assert_rollback(checkpoints: Vec<Result<String, String>>, expected: &str) {
    let dir = tempdir().unwrap();
    let model = Arc::new(CheckpointModel::new(checkpoints));
    let mut session = pressured_session(model.clone(), dir.path()).await;

    let before: Vec<String> = session
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect();
    let epoch_before = session.context_state().epoch;
    let journal = forge_durable::Journal::open(session.journal_dir(), session.session_id)
        .await
        .unwrap();
    let canonical_before = journal.replay(session.session_id).await.unwrap();

    let error = session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap_err();
    let LoopError::Compaction(error) = error else {
        panic!("expected a compaction error, got {error}");
    };
    assert_eq!(error.category(), expected);

    let after: Vec<String> = session
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect();
    assert_eq!(
        after, before,
        "a failed compaction must not touch the context"
    );
    assert_eq!(session.context_state().epoch, epoch_before);
    assert!(session.context_state().checkpoint.is_none());
    assert_eq!(session.installed_checkpoint_count(), 0);
    assert_eq!(session.compaction_telemetry().compaction_count, 0);
    assert_eq!(session.compaction_telemetry().failure_count, 1);
    assert_eq!(
        session
            .compaction_telemetry()
            .last
            .as_ref()
            .unwrap()
            .failure_reason
            .as_deref(),
        Some(expected)
    );

    let canonical_after = journal.replay(session.session_id).await.unwrap();
    assert_eq!(canonical_after.compaction_count, 0);
    assert_eq!(
        canonical_after.user_messages,
        canonical_before.user_messages
    );
}

#[tokio::test]
async fn a_provider_failure_leaves_the_context_unchanged() {
    assert_rollback(vec![Err("upstream unavailable".into())], "provider_error").await;
}

#[tokio::test]
async fn a_malformed_checkpoint_leaves_the_context_unchanged() {
    assert_rollback(
        vec![Ok("I have summarised the conversation for you.".into())],
        "invalid_checkpoint",
    )
    .await;
}

#[tokio::test]
async fn a_truncated_checkpoint_leaves_the_context_unchanged() {
    assert_rollback(
        vec![Ok(
            "<forge_checkpoint version=\"1\">\n<objective>partial".into()
        )],
        "invalid_checkpoint",
    )
    .await;
}

#[tokio::test]
async fn a_checkpoint_that_drops_user_constraints_leaves_the_context_unchanged() {
    assert_rollback(
        vec![Ok(
            "<forge_checkpoint version=\"1\"><objective>o</objective>\
                 <next_action>n</next_action></forge_checkpoint>"
                .into(),
        )],
        "missing_protected_facts",
    )
    .await;
}

#[tokio::test]
async fn an_oversized_checkpoint_leaves_the_context_unchanged() {
    // A "checkpoint" the size of the conversation buys no runway.
    let bloated = format!(
        "<forge_checkpoint version=\"1\"><objective>{}</objective>\
         <user_constraints>keep the API stable</user_constraints>\
         <next_action>n</next_action></forge_checkpoint>",
        "z".repeat(900_000)
    );
    assert_rollback(vec![Ok(bloated)], "oversized_result").await;
}

#[tokio::test]
async fn a_failed_automatic_compaction_still_lets_the_turn_proceed() {
    let dir = tempdir().unwrap();
    let model = Arc::new(CheckpointModel::new(vec![Err("provider down".into())]));
    let mut session = pressured_session(model.clone(), dir.path()).await;
    let before = session.messages.len();

    // `prepare_model_step` must succeed: the old context is still valid.
    let request = session.prepare_model_step(0).await.unwrap();

    assert_eq!(session.messages.len(), before);
    assert_eq!(request.messages.len(), before);
    assert_eq!(session.compaction_telemetry().failure_count, 1);
    assert!(session
        .events
        .iter()
        .any(|event| event.kind == "compaction_failed"));
}

// ---------------------------------------------------------- persistence

#[tokio::test]
async fn a_compacted_session_reconstructs_the_same_context_on_resume() {
    let dir = tempdir().unwrap();
    let model = CheckpointModel::valid();
    let mut session = pressured_session(model.clone(), dir.path()).await;

    session
        .compact_context(CompactionTrigger::Manual)
        .await
        .unwrap();
    let session_id = session.session_id;
    let expected: Vec<String> = session
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect();
    let checkpoint = session.context_state().checkpoint.clone().unwrap();
    let tail_start = session.context_state().tail_start_message_index;
    let facts = session.context_state().protected_facts.len();

    let resumed = AgentSession::resume(
        cfg(dir.path()),
        CheckpointModel::valid(),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();

    let restored: Vec<String> = resumed
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect();
    assert_eq!(
        restored, expected,
        "resume must rebuild the same logical compacted context"
    );
    assert_eq!(
        resumed.context_state().checkpoint.as_ref(),
        Some(&checkpoint)
    );
    assert_eq!(resumed.context_state().tail_start_message_index, tail_start);
    assert_eq!(
        resumed.context_state().protected_facts.len(),
        facts,
        "protected facts are rebuilt from canonical user turns, not from the projection"
    );
    assert_eq!(resumed.installed_checkpoint_count(), 1);
}

#[tokio::test]
async fn an_uncompacted_session_resumes_with_no_checkpoint() {
    let dir = tempdir().unwrap();
    let mut session = session_with(CheckpointModel::valid(), dir.path()).await;
    grow_context(&mut session, 2).await;
    let session_id = session.session_id;

    let resumed = AgentSession::resume(
        cfg(dir.path()),
        CheckpointModel::valid(),
        ToolRegistry::new(),
        session_id,
    )
    .await
    .unwrap();
    assert!(resumed.context_state().checkpoint.is_none());
    assert_eq!(resumed.context_state().protected_facts.len(), 2);
}

// --------------------------------------------------------------- policy

#[tokio::test]
async fn the_active_model_window_drives_the_trigger() {
    let dir = tempdir().unwrap();
    let mut session = session_with(CheckpointModel::valid(), dir.path()).await;
    grow_context(&mut session, 4).await;
    assert!(!session.context_pressure_reached());

    // A small-window model puts the same conversation under pressure.
    session.set_context_window(24_000, Some(4_000));
    assert_eq!(session.compaction_policy().context_window, 24_000);
    assert!(session.context_pressure_reached());
    assert_eq!(session.token_usage_report().context_capacity, 24_000);

    // A zero window is metadata Forge does not have, not a claim of no room.
    session.set_context_window(0, None);
    assert_eq!(session.compaction_policy().context_window, 24_000);
}
