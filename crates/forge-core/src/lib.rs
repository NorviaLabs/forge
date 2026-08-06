//! Agent loop — Phase 1 base + Phase 2 hooks (context, HITL, governance).

mod background;
mod completion;
mod lifecycle;
mod persistence;
mod queue;
mod resume;
mod stream;
mod subagent;
mod task_runtime;
mod turn;
pub(crate) mod turn_state;

pub use stream::{
    accumulate_stream_event, merge_streamed_response, observe_stream_event, stream_turn_event,
    ModelStepAccumulator,
};

pub use background::{
    BackgroundTaskHandle, BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus,
};
pub use completion::{
    CompletionDecision, CompletionEvaluator, CompletionReason, DefaultCompletionEvaluator,
    EvidenceEntry, EvidenceSummary, ExecutionEvent, ExecutionEvidence, FileEffectExpectation,
    FileEffectKind, GitEffectExpectation, GitEffectKind, TaskExpectation, ToolExpectation,
};
pub use lifecycle::{ActiveTaskState, TransitionError, TransitionReason};
pub use queue::{QueuedTask, TaskQueue};
pub use subagent::{SubagentOutcome, SubagentSpec};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use forge_config::WebSearchConfig;
use forge_context::{estimate_messages_tokens, estimate_tokens, ContextEngine};
use forge_durable::{new_session_id, Journal};
use forge_governance::{AuditEvent, Governance};
use forge_model::{ModelClient, ModelRequest, StreamEventTx};
use forge_tools::{
    default_builtins_with_web_search, ToolContext, ToolError, ToolRegistry, ValidationBudget,
};
use forge_types::{
    BackgroundTaskId, ExecutionOutcome, HitlDecision, HitlPayload, Message, MessageRole,
    ModelResponse, PolicyDecision, SessionId, SideEffectClass, TaskId, TaskLifecycle, ToolCall,
    ToolOutput, Usage, WaitReason,
};
use serde_json::json;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::persistence::SessionPersistence;
use crate::task_runtime::TaskRuntime;
use crate::turn::TurnCoordinator;
use crate::turn_state::TurnState;

const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

/// Discovery stage of progressive disclosure (issue #226): a skill with
/// frontmatter contributes only its `name` + `description` here — its full
/// `SKILL.md` body is fetched on demand via the `load_skill` tool once the
/// model judges the description relevant. A skill without frontmatter has no
/// `description` to show instead, so its whole body is injected eagerly,
/// matching pre-#226 behavior.
fn assemble_system_prompt(agents_md: &str, skills: &[forge_context::SkillManifest]) -> String {
    let mut prompt = SYSTEM_PROMPT.trim_end().to_owned();

    if !agents_md.trim().is_empty() {
        prompt.push_str("\n\n# Project Instructions\n\nAGENTS.md:\n");
        prompt.push_str(agents_md);
    }

    if !skills.is_empty() {
        prompt.push_str(
            "\n\n# Skills\n\nEach skill below is listed by name and description. When a task \
matches a skill's description, call the `load_skill` tool with that name to load its full \
instructions before proceeding.",
        );
        for skill in skills {
            prompt.push_str("\n\n## ");
            prompt.push_str(&skill.name);
            prompt.push_str("\n\n");
            if skill.has_frontmatter {
                prompt.push_str(skill.description.trim());
            } else {
                prompt.push_str(skill.body.trim());
            }
        }
    }

    prompt
}

/// Durable marker for a terminal turn failure summary in session messages.
/// Presentation maps this to TurnFailure; it is never a user-facing answer.
pub const TURN_FAILED_MARKER: &str = "[forge.turn_failed]";

// --- Completion-evidence helpers -------------------------------------------
//
// These are pure/near-pure helpers used to classify a turn's expectation and
// to build `EvidenceEntry` values from real tool calls and filesystem state.
// None of them read the model's own text.

/// Lightweight, local mirror of `forge_tools::builtins::GitArgs` so this
/// module doesn't need to depend on that crate's private argument shape —
/// just enough to recover the subcommand and its arguments from a `ToolCall`.
#[derive(serde::Deserialize)]
struct GitCallArgsLite {
    subcommand: String,
    #[serde(default)]
    args: Vec<String>,
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

fn search_result_count(content: &str) -> usize {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(hits) = value.get("hits").and_then(|hits| hits.as_array()) {
            return hits.len();
        }
        if value
            .get("message")
            .and_then(|message| message.as_str())
            .is_some_and(|message| message.contains("no matches found"))
        {
            return 0;
        }
    }
    if content.trim() == "no matches found" || content.contains("no matches found") {
        0
    } else {
        content.lines().count()
    }
}

/// Content hash of a workspace file. `None` means the path does not exist
/// (or isn't readable) — the convention `EvidenceEntry` documents.
async fn hash_file(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = tokio::fs::read(path).await.ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// Parse `*** Add/Update/Delete File: <path>` header lines out of an
/// `apply_patch` call's own `patch` argument. Deliberately does not
/// duplicate the tool's hunk-application logic — only enough to know which
/// paths a patch touched and whether the file should end up present or gone.
fn parse_patch_paths(patch: &str) -> Vec<(String, FileEffectKind)> {
    patch
        .lines()
        .filter_map(|line| {
            for (prefix, kind) in [
                ("*** Add File: ", FileEffectKind::Modified),
                ("*** Update File: ", FileEffectKind::Modified),
                ("*** Delete File: ", FileEffectKind::Deleted),
            ] {
                if let Some(p) = line.strip_prefix(prefix) {
                    return Some((p.to_string(), kind));
                }
            }
            None
        })
        .collect()
}

/// A short label for a bash call, e.g. `"cargo test"`, used both to classify
/// the turn and to name the operation in evidence/user-facing messages.
fn bash_label(arguments: &serde_json::Value) -> String {
    let command = arguments
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("bash")
        .trim();
    let first_line = command.lines().next().unwrap_or(command);
    truncate(first_line, 60)
}

fn git_effect_kind(subcommand: &str) -> GitEffectKind {
    match subcommand {
        "commit" => GitEffectKind::CommitCreated,
        "add" => GitEffectKind::Staged,
        "checkout" | "switch" => GitEffectKind::BranchChanged,
        "restore" => GitEffectKind::Restored,
        _ => GitEffectKind::CommandOnly,
    }
}

/// Collapse repeated attempts at the same target down to the last one, order
/// otherwise unspecified. A model that retries a failed write/command/git
/// call until it succeeds should only be judged on the final attempt, not
/// penalized for the earlier failures — this is what makes that distinction
/// from "5 required edits, 3 succeeded" (genuinely distinct targets).
fn dedup_keep_last<T: Clone>(items: Vec<T>, key_fn: impl Fn(&T) -> String) -> Vec<T> {
    let mut map: HashMap<String, T> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for item in items {
        let key = key_fn(&item);
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.insert(key, item);
    }
    order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect()
}

/// True when `text` contains what looks like an unparsed tool-call attempt —
/// a real, registered tool name used in call-shaped syntax (a JSON object
/// key, or a bare `{"tool_name", ...}` element) — rather than genuine prose.
/// This is exactly what a model emits when it tries to invoke a tool as plain
/// text instead of through the real structured tool-calling wire format (most
/// often a smaller/local model that doesn't reliably follow the function-call
/// API shape): the response has zero real `ToolCall`s, so without this check
/// it looks identical to a legitimate no-op chat answer and would otherwise
/// be marked `Completed` under `TaskExpectation::ReadOnly`.
///
/// Deliberately conservative: prose that merely *mentions* a tool by name
/// (e.g. "you can use the write_file tool") does not match, because the quoted
/// name isn't immediately followed by `:` or `,` the way a JSON key or a bare
/// call argument would be.
fn looks_like_dangling_tool_call(text: &str, tool_names: &[String]) -> bool {
    for name in tool_names {
        let needle = format!("\"{name}\"");
        let mut search_from = 0;
        while let Some(offset) = text[search_from..].find(needle.as_str()) {
            let match_end = search_from + offset + needle.len();
            let next_non_space = text[match_end..]
                .find(|c: char| !c.is_whitespace())
                .map(|i| match_end + i);
            if let Some(i) = next_non_space {
                if matches!(text.as_bytes()[i], b':' | b',') {
                    return true;
                }
            }
            search_from = search_from + offset + 1;
        }
    }
    false
}

/// Classify a finished turn's expectation from the tool calls the model
/// actually issued — not from natural-language intent inference over the
/// user's request. Precedence (a turn can only be one category):
/// `GitOperation > FileEdit > ToolExecution > Search > ReadOnly`.
fn classify_turn(calls: &[ToolCall]) -> TaskExpectation {
    let mut git_items: Vec<(String, String, GitEffectKind)> = Vec::new();
    let mut file_items: Vec<(String, String, FileEffectKind)> = Vec::new();
    let mut tool_items: Vec<(String, String)> = Vec::new();
    let mut search_count = 0usize;

    for call in calls {
        match call.name.as_str() {
            "git" => {
                if let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) {
                    let sub = a.subcommand.trim().to_ascii_lowercase();
                    git_items.push((call.id.clone(), sub.clone(), git_effect_kind(&sub)));
                }
            }
            "write_file" => {
                if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                    file_items.push((call.id.clone(), path.to_string(), FileEffectKind::Modified));
                }
            }
            "apply_patch" => {
                if let Some(patch) = call.arguments.get("patch").and_then(|v| v.as_str()) {
                    for (path, kind) in parse_patch_paths(patch) {
                        file_items.push((call.id.clone(), path, kind));
                    }
                }
            }
            "bash" => tool_items.push((call.id.clone(), bash_label(&call.arguments))),
            "fffind" | "ffgrep" => search_count += 1,
            _ => {}
        }
    }

    if !git_items.is_empty() {
        let deduped = dedup_keep_last(git_items, |(_, sub, _)| sub.clone());
        return TaskExpectation::GitOperation {
            expected_effects: deduped
                .into_iter()
                .map(|(operation_id, command, effect)| GitEffectExpectation {
                    operation_id,
                    command,
                    effect,
                })
                .collect(),
        };
    }
    if !file_items.is_empty() {
        let deduped = dedup_keep_last(file_items, |(_, path, _)| path.clone());
        return TaskExpectation::FileEdit {
            expected_effects: deduped
                .into_iter()
                .map(|(operation_id, path, kind)| FileEffectExpectation {
                    operation_id,
                    path,
                    kind,
                })
                .collect(),
        };
    }
    if !tool_items.is_empty() {
        let deduped = dedup_keep_last(tool_items, |(_, label)| label.clone());
        return TaskExpectation::ToolExecution {
            required_tools: deduped
                .into_iter()
                .map(|(operation_id, tool_name)| ToolExpectation {
                    operation_id,
                    tool_name,
                })
                .collect(),
        };
    }
    if search_count > 0 {
        return TaskExpectation::Search {
            required_operations: search_count,
        };
    }
    TaskExpectation::ReadOnly
}

/// Pre-call git state needed to verify a subcommand's repository effect
/// afterward. `None`-shaped variants mean "not practical to verify" per
/// subcommand.
#[derive(Clone)]
enum GitPre {
    Head(Option<String>),
    Branch(Option<String>),
    RestorePath(Option<String>),
    NotVerified,
}

/// Remove structural protocol control markers from final-answer text before
/// persistence. Not phrase filtering — only known control envelopes.
fn strip_protocol_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\\confidence{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "\\confidence{".len()..];
        if let Some(end) = after.find('}') {
            rest = &after[end + 1..];
        } else {
            // Unterminated marker, e.g. model output truncated mid-annotation.
            // Rewind to the marker so the tail is emitted exactly once: the
            // prefix was already pushed above, so leaving `rest` untouched
            // would duplicate it.
            rest = &rest[start..];
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Reconstruct the `WaitReason` a restored session was blocked on, from the
/// raw HITL payload the journal replayed. Only `Approval` is ever produced
/// today — the sole wait reason with a real runtime producer.
fn restored_wait_reason(pending_hitl: &Option<serde_json::Value>) -> Option<WaitReason> {
    let payload: HitlPayload = serde_json::from_value(pending_hitl.clone()?).ok()?;
    Some(WaitReason::Approval {
        request_id: payload.call_id.clone(),
        payload,
    })
}

/// Map forge-durable's lightweight replay mirror into forge-core's
/// `QueuedTask`, attaching the session id (not itself journaled per item).
fn restored_queue_items(
    session_id: SessionId,
    items: Vec<forge_durable::RestoredQueueItem>,
) -> Vec<QueuedTask> {
    items
        .into_iter()
        .map(|item| QueuedTask {
            id: item.id,
            session_id,
            text: item.text,
            created_at: item.created_at,
            status: item.status,
        })
        .collect()
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoopError {
    #[error(transparent)]
    Journal(#[from] forge_durable::JournalError),
    #[error(transparent)]
    Model(#[from] forge_model::ModelError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Context(#[from] forge_context::ContextError),
    #[error("session awaiting HITL; call resolve_hitl first")]
    AwaitingHitl,
    #[error("no pending HITL")]
    NoPendingHitl,
    #[error(transparent)]
    Transition(#[from] TransitionError),
    /// A `cancel_token` (subagent cancellation) fired mid-step — distinct
    /// from `Other` so callers can react without string-matching an error
    /// message.
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_turns: u32,
    pub workspace: PathBuf,
    pub journal_dir: PathBuf,
    pub enable_context_lifecycle: bool,
    pub enable_governance: bool,
    /// Phase 9 — controls registration of `web_search` (WEB-01).
    pub web_search: WebSearchConfig,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 128,
            workspace: PathBuf::from("."),
            journal_dir: PathBuf::from(".forge/sessions"),
            enable_context_lifecycle: true,
            enable_governance: true,
            web_search: WebSearchConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnEvent {
    pub kind: String,
    pub detail: String,
}

/// Cumulative API-reported token usage for a session (not $ cost).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTokenUsage {
    /// Sum of provider-reported prompt/input tokens across model calls.
    pub prompt_tokens: u64,
    /// Sum of provider-reported completion/output tokens across model calls.
    pub completion_tokens: u64,
    /// Number of model complete/stream calls that reported usage.
    pub model_calls_with_usage: u32,
    /// Model steps applied (with or without usage metadata).
    pub model_steps: u32,
    /// Estimated thinking/reasoning tokens (from thinking text, ~4 chars/token).
    pub thinking_tokens_est: u64,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
}

impl SessionTokenUsage {
    pub fn total_api_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }

    pub fn record_response(&mut self, usage: Option<&Usage>, thinking: Option<&str>) {
        self.model_steps = self.model_steps.saturating_add(1);
        if let Some(u) = usage {
            self.prompt_tokens = self.prompt_tokens.saturating_add(u.prompt_tokens as u64);
            self.completion_tokens = self
                .completion_tokens
                .saturating_add(u.completion_tokens as u64);
            self.prompt_cache_hits = self
                .prompt_cache_hits
                .saturating_add(u.prompt_cache_read_tokens as u64);
            self.prompt_cache_writes = self
                .prompt_cache_writes
                .saturating_add(u.prompt_cache_write_tokens as u64);
            self.model_calls_with_usage = self.model_calls_with_usage.saturating_add(1);
        }
        if let Some(th) = thinking.filter(|t| !t.trim().is_empty()) {
            self.thinking_tokens_est = self
                .thinking_tokens_est
                .saturating_add(estimate_tokens(th) as u64);
        }
    }
}

/// Snapshot of session token metrics for status UIs.
#[derive(Debug, Clone)]
pub struct TokenUsageReport {
    pub api: SessionTokenUsage,
    pub context_tokens_est: usize,
    pub context_capacity: usize,
    pub context_pct: f64,
    pub system_tokens_est: usize,
    pub user_tokens_est: usize,
    pub assistant_tokens_est: usize,
    pub tool_tokens_est: usize,
    pub thinking_in_context_est: usize,
    pub message_count: usize,
    pub tool_message_count: usize,
}

/// Result of applying one model response inside the agent loop.
#[derive(Debug)]
#[non_exhaustive]
pub enum ApplyOutcome {
    /// No tool calls — turn finished.
    Done(ModelResponse),
    /// Tools ran; call the model again.
    Continue,
    /// Paused for human-in-the-loop.
    Hitl(ModelResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    pub last_seq: u64,
    pub model_steps: usize,
    pub tool_results: usize,
    pub incomplete_intents: usize,
}

pub struct AgentSession {
    pub session_id: SessionId,
    pub messages: Vec<Message>,
    pub events: Vec<TurnEvent>,
    /// Authoritative task/attempt lifecycle. The single source of truth for
    /// "what task is active, is Forge working/waiting, and why" — UI code
    /// must read this rather than deriving its own copy from busy/streaming
    /// flags. All mutation goes through `AgentSession::transition`/
    /// `enter_waiting`/`start_new_task`, never a direct field write.
    pub active_task: ActiveTaskState,
    /// Queue and background-task runtime stores. Public access is provided by
    /// read-only accessors so their ownership stays inside the session.
    tasks: TaskRuntime,
    /// Provider/model id for the next completion (empty → client default).
    pub active_model: String,
    /// Wire-level reasoning-effort value for the next completion, or `None`
    /// to omit the field entirely (model doesn't support it, or effort is
    /// Auto). Set via `set_reasoning_effort`, read by `build_model_request`.
    reasoning_effort: Option<String>,
    journal: SessionPersistence,
    /// Shared across a parent session and every subagent spawned from it —
    /// `register` only ever runs during `create`/`resume` setup, so sharing
    /// via `Arc` after that point is a type change, not a behavior change.
    tools: Arc<ToolRegistry>,
    model: Arc<dyn ModelClient>,
    tool_ctx: ToolContext,
    max_turns: u32,
    governance: Governance,
    context: ContextEngine,
    enable_context: bool,
    enable_gov: bool,
    /// `Some` only for a subagent session — flipped by the parent's
    /// `BackgroundTaskRegistry::cancel`, checked in
    /// `run_model_step_with_stream`'s streaming poll loop. `None` for the
    /// top-level/foreground session, which is cancelled via the TUI's own
    /// `cancel_requested` bool instead.
    cancel_token: Option<CancellationToken>,
    /// Cumulative provider token usage for this session.
    pub token_usage: SessionTokenUsage,
    /// Runtime bookkeeping scoped to the current user turn. It is reset when
    /// a new user message starts and is intentionally not persisted as its
    /// own journal state.
    turn: TurnState,
    /// The `CompletionEvaluator` decision for the most recently finished
    /// turn, for callers/tests that want the machine-readable reason behind
    /// `status` without re-deriving it from messages.
    pub last_completion: Option<CompletionDecision>,
    /// Journaled tool results indexed by call id — used to avoid re-execution on resume.
    journaled_tool_results: HashMap<String, forge_durable::ToolResultPayload>,
    /// Memoized `estimate_messages_tokens` total, keyed by `messages.len()`.
    /// Messages only ever grow (append) or are wholesale replaced (context
    /// reset), so a length match means the sum is current. The status bar asks
    /// for this on every frame from an `&self` path; the lock is uncontended and
    /// held only to read/write two words, so it stays cheaper than the O(history)
    /// char scan it avoids.
    ctx_tokens_cache: Mutex<Option<(usize, usize)>>,
}

/// A short, human-readable hint for a resumable session — its first user
/// message, truncated — so a `/resume` list can show more than a raw UUID
/// and timestamp. Cheap: opens and replays only the one session's journal,
/// independent of any live `AgentSession` (no tools/model/governance
/// needed). Returns `None` on any read/replay error or an empty journal —
/// callers should fall back to showing just the id/timestamp in that case,
/// never fail the whole listing over one unreadable session.
pub async fn session_title_hint(
    journal_dir: &Path,
    session_id: forge_types::SessionId,
) -> Option<String> {
    let journal = Journal::open(journal_dir, session_id).await.ok()?;
    let state = journal.replay(session_id).await.ok()?;
    let first = state.user_messages.into_iter().next()?;
    let mut title: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 60;
    if title.chars().count() > MAX_LEN {
        title = title.chars().take(MAX_LEN).collect::<String>() + "…";
    }
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

impl AgentSession {
    /// How many HITL denials in a row within one user turn are tolerated
    /// before the turn is stopped outright (see `consecutive_hitl_denials`).
    const MAX_CONSECUTIVE_HITL_DENIALS: u32 = 2;

    /// Replace the active conversation by replaying another session journal.
    pub async fn resume_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<ResumeReport, LoopError> {
        if session_id == self.session_id {
            return Ok(ResumeReport {
                last_seq: self.events.len() as u64,
                model_steps: self.token_usage.model_steps as usize,
                tool_results: self
                    .messages
                    .iter()
                    .filter(|message| message.role == MessageRole::Tool)
                    .count(),
                incomplete_intents: 0,
            });
        }

        let journal = Journal::open(self.journal.directory(), session_id).await?;
        let state = journal.replay(session_id).await?;
        let mut context = ContextEngine::new(self.context.workspace.clone(), session_id);
        context.config = self.context.config.clone();
        let system_message = Message {
            outcome: Default::default(),
            role: MessageRole::System,
            content: assemble_system_prompt(&context.load_agents_md(), &context.load_skills()),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        };
        let mut messages = state.messages;
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            *first = system_message;
        } else {
            messages.insert(0, system_message);
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume");
        }

        let incomplete = state.incomplete_intents.clone();
        let journaled_tool_results = state.tool_results.clone();
        let active_root = context.workspace.clone();
        let wait_reason = restored_wait_reason(&state.pending_hitl);
        let queue = TaskQueue::from_restored(restored_queue_items(session_id, state.queue_items));
        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let report = ResumeReport {
            last_seq: state.last_seq,
            model_steps: state.model_responses.len(),
            tool_results: state.tool_results.len(),
            incomplete_intents: state.incomplete_intents.len(),
        };
        self.session_id = session_id;
        self.active_task = ActiveTaskState::from_restored(session_id, state.status, wait_reason);
        self.tasks = TaskRuntime::with_queue(queue);
        self.messages = messages;
        self.events = vec![TurnEvent {
            kind: "resume".into(),
            detail: format!("seq={}", state.last_seq),
        }];
        self.journal = SessionPersistence::new(journal);
        self.tool_ctx = ToolContext::new(active_root);
        self.context = context;
        self.token_usage = token_usage;
        self.journaled_tool_results = journaled_tool_results;
        self.reconcile_incomplete_intents(&incomplete).await?;
        // Stale Working without a live executor is Interrupted, not eternal Working.
        self.mark_interrupted_if_stale().await?;
        Ok(report)
    }

    pub async fn create(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
    ) -> Result<Self, LoopError> {
        for t in default_builtins_with_web_search(&loop_cfg.web_search) {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let session_id = new_session_id();
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        journal.append_session_created(session_id).await?;

        let active_root = loop_cfg.workspace.clone();
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let agents = context.load_agents_md();
        let skills = context.load_skills();
        let system = assemble_system_prompt(&agents, &skills);

        Ok(Self {
            session_id,
            messages: vec![Message {
                outcome: Default::default(),
                role: MessageRole::System,
                content: system,
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            }],
            events: vec![],
            active_task: ActiveTaskState::new(session_id),
            tasks: TaskRuntime::new(),
            active_model: String::new(),
            reasoning_effort: None,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            token_usage: SessionTokenUsage::default(),
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: HashMap::new(),
            ctx_tokens_cache: Mutex::new(None),
        })
    }

    pub async fn resume(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
        session_id: SessionId,
    ) -> Result<Self, LoopError> {
        for t in default_builtins_with_web_search(&loop_cfg.web_search) {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        let state = journal.replay(session_id).await?;
        let context = ContextEngine::new(loop_cfg.workspace.clone(), session_id);
        let system = assemble_system_prompt(&context.load_agents_md(), &context.load_skills());
        let system_message = Message {
            outcome: Default::default(),
            role: MessageRole::System,
            content: system,
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        };
        let mut messages = state.messages.clone();
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            *first = system_message;
        } else {
            messages.insert(0, system_message);
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume");
        }

        let incomplete = state.incomplete_intents.clone();
        let active_root = loop_cfg.workspace.clone();

        let wait_reason = restored_wait_reason(&state.pending_hitl);
        let queue = TaskQueue::from_restored(restored_queue_items(session_id, state.queue_items));

        let mut token_usage = SessionTokenUsage::default();
        for response in &state.model_responses {
            token_usage.record_response(response.usage.as_ref(), response.thinking.as_deref());
        }

        let mut session = Self {
            session_id,
            messages,
            events: vec![TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            active_task: ActiveTaskState::from_restored(session_id, state.status, wait_reason),
            tasks: TaskRuntime::with_queue(queue),
            active_model: String::new(),
            reasoning_effort: None,
            journal: SessionPersistence::new(journal),
            tools: Arc::new(tools),
            model,
            tool_ctx: ToolContext::new(active_root),
            max_turns: loop_cfg.max_turns,
            governance: Governance::default(),
            context,
            enable_context: loop_cfg.enable_context_lifecycle,
            enable_gov: loop_cfg.enable_governance,
            cancel_token: None,
            token_usage,
            turn: TurnState::new(),
            last_completion: None,
            journaled_tool_results: state.tool_results.clone(),
            ctx_tokens_cache: Mutex::new(None),
        };
        session.reconcile_incomplete_intents(&incomplete).await?;
        session
            .reconcile_orphaned_background_tasks(
                &state.background_tasks,
                &state.subagent_workspaces,
            )
            .await?;
        // Legacy fallback: Running with no runtime becomes Interrupted (not guessed from text).
        session.mark_interrupted_if_stale().await?;
        Ok(session)
    }

    pub fn set_governance(&mut self, g: Governance) {
        self.governance = g;
    }

    /// Apply a named permission mode in place — preserves the ACL and any
    /// loaded pattern rules, only pre-seeding `hitl_classes` (see
    /// `Governance::apply_mode`). Unlike `set_governance`, this can't
    /// accidentally drop rules a `permissions.toml` load already put in
    /// place.
    pub fn apply_permission_mode(&mut self, mode: forge_governance::PermissionMode) {
        self.governance.apply_mode(mode);
    }

    /// Read-only view of queued future-task instructions.
    pub fn queue(&self) -> &TaskQueue {
        &self.tasks.queue
    }

    /// Read-only view of in-flight and recently-finished background tasks.
    pub fn background(&self) -> &BackgroundTaskRegistry {
        &self.tasks.background
    }

    /// Request cancellation of a background task through the session-owned
    /// runtime. Returns `false` for unknown or already-terminal tasks.
    pub fn cancel_background_task(&mut self, id: BackgroundTaskId) -> bool {
        self.tasks.background.cancel(id)
    }

    /// The HITL payload the active task is waiting on, if any — a
    /// convenience view over `active_task.wait_reason`'s `Approval` variant
    /// (the only wait reason with a real producer today).
    pub fn pending_hitl(&self) -> Option<&HitlPayload> {
        match &self.active_task.wait_reason {
            Some(WaitReason::Approval { payload, .. }) => Some(payload),
            _ => None,
        }
    }

    /// Validate and apply a lifecycle transition, journaling it durably.
    /// The single path through which `active_task.lifecycle` may change
    /// (other than `enter_waiting`/`start_new_task`, which carry their own
    /// payload) — every direct field write this crate used to do goes
    /// through here instead. Rejects illegal transitions rather than
    /// silently coercing state; the current valid state is preserved on
    /// failure since `ActiveTaskState::try_transition` never partially
    /// applies a rejected move.
    async fn transition(
        &mut self,
        to: TaskLifecycle,
        reason: TransitionReason,
    ) -> Result<(), LoopError> {
        let from = self.active_task.lifecycle;
        if let Err(err) = self.active_task.try_transition(to) {
            tracing::warn!(
                from = ?from,
                to = ?to,
                reason = ?reason,
                error = %err,
                "rejected illegal lifecycle transition"
            );
            return Err(LoopError::from(err));
        }
        self.journal.append_status(self.session_id, to).await?;
        tracing::debug!(
            from = ?from,
            to = ?to,
            revision = self.active_task.revision,
            reason = ?reason,
            "lifecycle transition"
        );
        Ok(())
    }

    /// Transition into `Waiting`, atomically attaching the structured reason
    /// a response must correlate against to resume the attempt.
    async fn enter_waiting(
        &mut self,
        wait: WaitReason,
        reason: TransitionReason,
    ) -> Result<(), LoopError> {
        let from = self.active_task.lifecycle;
        if let Err(err) = self.active_task.enter_waiting(wait) {
            tracing::warn!(
                from = ?from,
                to = ?TaskLifecycle::Waiting,
                reason = ?reason,
                error = %err,
                "rejected illegal lifecycle transition"
            );
            return Err(LoopError::from(err));
        }
        self.journal
            .append_status(self.session_id, TaskLifecycle::Waiting)
            .await?;
        tracing::debug!(
            from = ?from,
            to = ?TaskLifecycle::Waiting,
            revision = self.active_task.revision,
            reason = ?reason,
            "lifecycle transition"
        );
        Ok(())
    }

    /// Start a brand-new task (direct dispatch or queue promotion): fresh
    /// task id, attempt reset to 1, `-> Working`. Legal from `Ready` or any
    /// terminal state; rejected while a task is already `Working`/`Waiting`
    /// — at most one active attempt per session.
    async fn transition_to_new_task(&mut self, task_id: TaskId) -> Result<(), LoopError> {
        self.active_task
            .start_new_task(task_id)
            .map_err(LoopError::from)?;
        self.journal
            .append_status(self.session_id, TaskLifecycle::Working)
            .await?;
        Ok(())
    }

    /// Enqueue a future-task instruction. Journals the enqueue before
    /// returning — callers must not report "queued" to the user until this
    /// succeeds.
    pub async fn enqueue_task(&mut self, text: &str) -> Result<QueuedTask, LoopError> {
        let item = self.tasks.queue.enqueue(self.session_id, text);
        self.journal
            .append_queue_enqueued(self.session_id, item.id, &item.text)
            .await?;
        Ok(item)
    }

    /// Cancel a still-queued item by its 1-based visible position (matches
    /// the existing keyboard cancel-by-row UX). Returns `None` if the
    /// position is out of range or the item is no longer cancellable
    /// (already promoting/promoted/removed).
    pub async fn cancel_queued_at(
        &mut self,
        one_based: usize,
    ) -> Result<Option<QueuedTask>, LoopError> {
        let Some(item) = self.tasks.queue.remove_at_visible_position(one_based) else {
            return Ok(None);
        };
        self.journal
            .append_queue_removed(self.session_id, item.id)
            .await?;
        Ok(Some(item))
    }

    /// Atomic promotion of the oldest queued item into a new task, started
    /// `Working`. Returns `None` if the queue is empty. On failure to start
    /// the new task, the item is rolled back to `Queued` (never lost) and
    /// the error propagated.
    pub async fn promote_next_queued(&mut self) -> Result<Option<TaskId>, LoopError> {
        let Some(item) = self.tasks.queue.peek_next_queued().cloned() else {
            return Ok(None);
        };
        if !self.tasks.queue.mark_promoting(item.id) {
            // Lost the race to another caller — nothing to do.
            return Ok(None);
        }
        self.journal
            .append_queue_promoting(self.session_id, item.id)
            .await?;

        match self.append_user_message(&item.text).await {
            Ok(()) => {
                self.tasks.queue.mark_promoted(item.id);
                self.journal
                    .append_queue_promoted(self.session_id, item.id, self.active_task.task_id.0)
                    .await?;
                Ok(Some(self.active_task.task_id))
            }
            Err(err) => {
                self.tasks.queue.revert_promoting(item.id);
                Err(err)
            }
        }
    }

    pub fn journal_dir(&self) -> &std::path::Path {
        self.journal.directory()
    }

    /// Number of project/global skills available to the current session.
    /// This is intentionally a count only: skill contents remain model context.
    pub fn loaded_skills_count(&self) -> usize {
        self.context.load_skills().len()
    }

    /// Names of project/global skills available to the current session.
    pub fn loaded_skill_names(&self) -> Vec<String> {
        self.context
            .load_skills()
            .into_iter()
            .map(|skill| skill.name)
            .collect()
    }

    pub fn loaded_skills(&self) -> Vec<forge_context::SkillManifest> {
        self.context.load_skills()
    }

    pub fn list_tools(&self) -> Vec<String> {
        let desc = self.tools.list_descriptors();
        if self.enable_gov {
            self.governance
                .filter_tools(desc)
                .into_iter()
                .map(|t| t.name)
                .collect()
        } else {
            self.tools.names()
        }
    }

    pub fn context_usage_ratio(&self) -> f64 {
        self.context_tokens_estimate() as f64 / self.context.config.capacity_tokens.max(1) as f64
    }

    /// Estimated in-context tokens for `self.messages`, memoized across frames.
    /// Safe because message transcripts only grow by append or get replaced
    /// wholesale (context reset) — both change the length, so the length key is
    /// a faithful dirty check. See `ctx_tokens_cache`.
    fn context_tokens_estimate(&self) -> usize {
        let mut cache = self
            .ctx_tokens_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((len, total)) = *cache {
            if len == self.messages.len() {
                return total;
            }
        }
        let total = estimate_messages_tokens(&self.messages);
        *cache = Some((self.messages.len(), total));
        total
    }

    pub fn context_reset_ratio(&self) -> f64 {
        self.context.config.reset_usage_ratio
    }

    pub async fn journal_cursor(&self) -> Result<u64, LoopError> {
        Ok(self.journal.last_seq().await?)
    }

    /// Full token-usage report for status UIs (API totals + in-context estimates). No $.
    pub fn token_usage_report(&self) -> TokenUsageReport {
        let mut system_tokens_est = 0usize;
        let mut user_tokens_est = 0usize;
        let mut assistant_tokens_est = 0usize;
        let mut tool_tokens_est = 0usize;
        let mut thinking_in_context_est = 0usize;
        let mut tool_message_count = 0usize;
        for m in &self.messages {
            let n = estimate_tokens(&m.content);
            match m.role {
                MessageRole::System => system_tokens_est = system_tokens_est.saturating_add(n),
                MessageRole::User => user_tokens_est = user_tokens_est.saturating_add(n),
                MessageRole::Assistant => {
                    assistant_tokens_est = assistant_tokens_est.saturating_add(n);
                    if let Some(ref th) = m.thinking {
                        thinking_in_context_est =
                            thinking_in_context_est.saturating_add(estimate_tokens(th));
                    }
                }
                MessageRole::Tool => {
                    tool_tokens_est = tool_tokens_est.saturating_add(n);
                    tool_message_count = tool_message_count.saturating_add(1);
                }
                // `MessageRole` is `#[non_exhaustive]`. Count an unrecognised future role
                // toward the user bucket so the context budget total stays accurate instead
                // of silently under-counting the window.
                _ => user_tokens_est = user_tokens_est.saturating_add(n),
            }
        }
        let context_tokens_est = self
            .context_tokens_estimate()
            .saturating_add(thinking_in_context_est);
        let context_capacity = self.context.config.capacity_tokens.max(1);
        let context_pct = (context_tokens_est as f64 / context_capacity as f64) * 100.0;
        TokenUsageReport {
            api: self.token_usage.clone(),
            context_tokens_est,
            context_capacity,
            context_pct,
            system_tokens_est,
            user_tokens_est,
            assistant_tokens_est,
            tool_tokens_est,
            thinking_in_context_est,
            message_count: self.messages.len(),
            tool_message_count,
        }
    }

    pub fn token_usage_lines(&self) -> Vec<String> {
        let r = self.token_usage_report();
        let api = &r.api;
        let mut lines = vec![
            "Session token usage (not $)".to_string(),
            String::new(),
            "API-reported (cumulative)".to_string(),
            format!("  prompt/input tokens:      {}", api.prompt_tokens),
            format!("  completion/output tokens: {}", api.completion_tokens),
            format!("  total API tokens:         {}", api.total_api_tokens()),
            format!(
                "  model steps:              {} ({} with usage metadata)",
                api.model_steps, api.model_calls_with_usage
            ),
            format!("  thinking tokens (est.):   {}", api.thinking_tokens_est),
            String::new(),
            "In-context estimate (~4 chars/token)".to_string(),
            format!(
                "  total: {} / {}  ({:.1}% of capacity)",
                r.context_tokens_est, r.context_capacity, r.context_pct
            ),
            format!("  system:    {}", r.system_tokens_est),
            format!("  user:      {}", r.user_tokens_est),
            format!("  assistant: {}", r.assistant_tokens_est),
            format!(
                "  tool:      {} ({} tool msgs)",
                r.tool_tokens_est, r.tool_message_count
            ),
            format!("  thinking:  {}", r.thinking_in_context_est),
            format!("  messages:  {}", r.message_count),
        ];
        if api.model_steps > 0 && api.model_calls_with_usage == 0 {
            lines.push(String::new());
            lines.push("Note: provider did not return usage; API totals may stay 0.".into());
        }
        lines
    }

    /// Append a user message to the session (journal + transcript) without calling the model.
    /// Used by the TUI so the YOU bubble can paint before the model run starts.
    pub async fn append_user_message(&mut self, text: &str) -> Result<(), LoopError> {
        if self.active_task.lifecycle == TaskLifecycle::Waiting {
            return Err(LoopError::AwaitingHitl);
        }
        self.journal
            .append_user_message(self.session_id, text)
            .await?;
        self.messages.push(Message {
            outcome: Default::default(),
            role: MessageRole::User,
            content: text.into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        if self.context.goal.is_empty() {
            self.context.goal = text.chars().take(200).collect();
        }
        let next_id = TaskId(self.active_task.task_id.0 + 1);
        self.transition_to_new_task(next_id).await?;
        // Fresh turn-local bookkeeping — a prior turn's tool calls/evidence
        // must never leak into this one's decision.
        self.turn.reset();
        self.last_completion = None;
        Ok(())
    }

    /// Shared model client handle (for streaming from the TUI without holding `&mut self`).
    pub fn model_client(&self) -> Arc<dyn ModelClient> {
        self.model.clone()
    }

    /// Build the next model request from current transcript + tools.
    pub fn build_model_request(&self) -> ModelRequest {
        let mut tools = self.tools.list_descriptors();
        if self.enable_gov {
            tools = self.governance.filter_tools(tools);
        }
        ModelRequest {
            messages: self.messages.clone(),
            tools,
            model: self.active_model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            prompt_cache: true,
        }
    }

    /// Apply a model response: journal, assistant message, then run tools.
    /// Returns `Ok(None)` when the turn is finished (no more tool calls).
    /// Returns `Ok(Some(resp))` when paused for HITL.
    /// Returns `Ok(Some(resp))` with empty tool path... actually:
    /// - finished cleanly → Ok(ApplyOutcome::Done(resp))
    /// - need another model step after tools → Ok(ApplyOutcome::Continue)
    /// - HITL → Ok(ApplyOutcome::Hitl(resp))
    pub async fn apply_model_response(
        &mut self,
        last: ModelResponse,
    ) -> Result<ApplyOutcome, LoopError> {
        self.journal
            .append_model_response(
                self.session_id,
                serde_json::to_value(&last).map_err(|error| LoopError::Other(error.to_string()))?,
            )
            .await?;

        self.token_usage
            .record_response(last.usage.as_ref(), last.thinking.as_deref());

        let has_thinking = last
            .thinking
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        // Final-answer channel is `text` only. Thinking stays internal/progress.
        let final_text = strip_protocol_markers(&last.text);
        if !final_text.is_empty() || has_thinking || !last.tool_calls.is_empty() {
            self.messages.push(Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: final_text.clone(),
                tool_call_id: None,
                name: None,
                thinking: last.thinking.clone().filter(|t| !t.trim().is_empty()),
                thinking_duration_secs: None,
                tool_calls: last.tool_calls.clone(),
            });
            if has_thinking {
                if let Some(ref th) = last.thinking {
                    self.events.push(TurnEvent {
                        kind: "progress".into(),
                        detail: th.clone(),
                    });
                }
            }
            // Durable assistant event only for primary final text without tool calls.
            if !final_text.is_empty() && last.tool_calls.is_empty() {
                self.events.push(TurnEvent {
                    kind: "assistant".into(),
                    detail: final_text.clone(),
                });
            }
        }

        if last.tool_calls.is_empty() {
            // Once a turn has reached a terminal state, a stray extra model
            // step (e.g. a caller re-driving `apply_model_response` outside
            // the normal `run_agent_turns` loop) must never resurrect or
            // overwrite it — a new attempt only starts via a new user
            // message (`append_user_message`), which starts a new task itself.
            if matches!(
                self.active_task.lifecycle,
                TaskLifecycle::Completed
                    | TaskLifecycle::Failed
                    | TaskLifecycle::Cancelled
                    | TaskLifecycle::Interrupted
            ) {
                return Ok(ApplyOutcome::Done(last));
            }
            // No durable final answer *and* the turn already did tool/validation
            // work: a failed terminal state, not silent success. An idle / no-op
            // response with no prior activity still counts as a valid (empty)
            // answer below — unchanged from before this evaluator existed.
            if final_text.is_empty() && self.current_turn_has_tool_activity() {
                self.finalize_turn_failure("Forge couldn't complete this turn.", "no_final_answer")
                    .await?;
                return Ok(ApplyOutcome::Done(last));
            }
            // The model issued zero real tool calls this turn (in this step
            // or any earlier one), but its final text looks like an attempt
            // to invoke one anyway (e.g. a JSON-ish blob naming a real tool).
            // Left unchecked, this is indistinguishable from a legitimate
            // no-op chat answer and falls through to `TaskExpectation::ReadOnly`,
            // which completes on any non-empty text — reporting success while
            // nothing actually happened. Fail explicitly instead.
            if self.turn.calls().is_empty() {
                let tool_names: Vec<String> = self
                    .tools
                    .list_descriptors()
                    .into_iter()
                    .map(|d| d.name)
                    .collect();
                if looks_like_dangling_tool_call(&final_text, &tool_names) {
                    self.finalize_turn_failure(
                        "The model attempted to call a tool but didn't format the call correctly, so no changes were made.",
                        CompletionReason::DanglingToolCallText.as_category(),
                    )
                    .await?;
                    return Ok(ApplyOutcome::Done(last));
                }
            }
            self.turn.push_evidence(EvidenceEntry::new(
                ExecutionEvent::AssistantResponseProduced,
            ));

            // The model's own words never decide this — only the expectation
            // derived from tool calls actually issued this turn, and the
            // evidence those calls produced.
            let expectation = classify_turn(self.turn.calls());
            let mut decision =
                DefaultCompletionEvaluator.evaluate(&expectation, self.turn.evidence());
            // `classify_turn` picks exactly one `TaskExpectation` category per
            // turn (git > file-edit > tool-execution > search > read-only), so
            // a turn that e.g. both writes a file and runs a failing
            // validation command evaluates the file-edit evidence only — the
            // failing bash evidence never gets consulted, and the turn could
            // read `Completed` despite it. Independent of which category the
            // evaluator matched, any failed evidence entry anywhere in this
            // turn must prevent a success framing.
            if decision.state == TaskLifecycle::Completed {
                if let Some(bad) = self.turn.evidence().0.iter().find(|e| e.error.is_some()) {
                    let tool = bad.tool_name.clone().unwrap_or_else(|| "a step".into());
                    decision = CompletionDecision {
                        state: TaskLifecycle::Failed,
                        reason: CompletionReason::PartialFailure,
                        evidence_summary: EvidenceSummary {
                            succeeded: decision.evidence_summary.succeeded,
                            failed: vec![tool.clone()],
                            detail: format!(
                                "{tool} did not finish successfully, so this turn is not complete."
                            ),
                        },
                    };
                }
            }
            tracing::debug!(
                expectation = ?expectation,
                evidence_count = self.turn.evidence().0.len(),
                reason = decision.reason.as_category(),
                state = ?decision.state,
                "turn completion decision"
            );
            match decision.state {
                TaskLifecycle::Completed => {
                    self.transition(
                        TaskLifecycle::Completed,
                        TransitionReason::Completion(decision.reason),
                    )
                    .await?;
                }
                TaskLifecycle::Failed => {
                    self.finalize_turn_failure(
                        &decision.evidence_summary.detail,
                        decision.reason.as_category(),
                    )
                    .await?;
                }
                TaskLifecycle::Waiting | TaskLifecycle::Cancelled | TaskLifecycle::Interrupted => {
                    // The evaluator can, in principle, observe evidence that maps
                    // to one of these states (e.g. a `WaitingForUser`/`UserCancelled`
                    // entry left over from earlier in the same turn) — but only the
                    // runtime's own coordinators may actually author these
                    // transitions (the HITL gate in `run_one_tool`, `mark_cancelled`).
                    // A completion decision alone must never force or re-enter one of
                    // them; this used to fall through to `finalize_turn_failure` and
                    // wrongly mark the turn Failed.
                    tracing::debug!(
                        state = ?decision.state,
                        reason = decision.reason.as_category(),
                        "completion decision observed a non-authoritative state; lifecycle left unchanged"
                    );
                }
                // `TaskLifecycle` is `#[non_exhaustive]`; an unrecognised decision
                // state fails safe rather than silently completing.
                _ => {
                    self.finalize_turn_failure(
                        &decision.evidence_summary.detail,
                        decision.reason.as_category(),
                    )
                    .await?;
                }
            }
            self.last_completion = Some(decision);
            return Ok(ApplyOutcome::Done(last));
        }

        // Budget spans the whole user turn so repeated invalid calls across
        // model steps still exhaust instead of looping forever.
        let mut budget = self.turn.take_validation_budget();
        let tool_result = async {
            for call in &last.tool_calls {
                if let Some(pause) = self.run_one_tool(call, &mut budget).await? {
                    return Ok(ApplyOutcome::Hitl(pause));
                }
                // Retry exhaustion is a terminal failure — do not Continue the loop.
                if self.active_task.lifecycle == TaskLifecycle::Failed {
                    return Ok(ApplyOutcome::Done(last.clone()));
                }
            }
            Ok(ApplyOutcome::Continue)
        }
        .await;
        self.turn.restore_validation_budget(budget);
        tool_result
    }

    /// True when the open user turn already has tool or validation activity.
    fn current_turn_has_tool_activity(&self) -> bool {
        for m in self.messages.iter().rev() {
            match m.role {
                MessageRole::User => return false,
                MessageRole::Tool => return true,
                MessageRole::Assistant if !m.tool_calls.is_empty() => return true,
                _ => {}
            }
        }
        false
    }

    /// Persist a concise terminal failure and mark the session failed.
    pub async fn finalize_turn_failure(
        &mut self,
        summary: &str,
        category: &str,
    ) -> Result<(), LoopError> {
        if self.active_task.lifecycle == TaskLifecycle::Failed {
            // Idempotent: keep the first failure summary.
            if self
                .messages
                .iter()
                .any(|m| m.content.starts_with(TURN_FAILED_MARKER))
            {
                return Ok(());
            }
        }
        let summary = summary.trim();
        let content = format!("{TURN_FAILED_MARKER}{summary}");
        self.messages.push(Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: content.clone(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        self.events.push(TurnEvent {
            kind: "turn_failed".into(),
            detail: format!("{category}: {summary}"),
        });
        self.transition(TaskLifecycle::Failed, TransitionReason::TurnFailure)
            .await?;
        let response = ModelResponse {
            text: content,
            tool_calls: vec![],
            usage: None,
            thinking: None,
        };
        self.journal
            .append_model_response(
                self.session_id,
                serde_json::to_value(&response)
                    .map_err(|error| LoopError::Other(error.to_string()))?,
            )
            .await?;
        Ok(())
    }

    /// Persist operator/system cancellation of the foreground task. A no-op
    /// (not an error) when no attempt is actually active — cancelling a
    /// task that already reached a terminal state, or was never started,
    /// must never overwrite that terminal outcome.
    pub async fn mark_cancelled(&mut self) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working | TaskLifecycle::Waiting => {
                self.transition(TaskLifecycle::Cancelled, TransitionReason::UserCancel)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "cancelled".into(),
                    detail: "foreground task cancelled".into(),
                });
                // Cancelling the foreground task must not leave its
                // subagents/background jobs running unsupervised — flip
                // every still-in-flight child's `CancellationToken` too.
                let child_ids: Vec<_> = self
                    .background()
                    .children_of(self.active_task.task_id)
                    .map(|t| t.id)
                    .collect();
                for id in child_ids {
                    self.tasks.background.cancel(id);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A model/provider request failed before ever producing a
    /// `ModelResponse` (e.g. an HTTP error from `ModelClient::complete_with_stream`,
    /// or a `LoopError` surfacing from `apply_model_response` itself before it
    /// reached its own transition logic). There is no assistant turn for
    /// `apply_model_response`'s evaluator to judge in that case, so nothing
    /// else moves the lifecycle out of `Working` — and because the message
    /// queue's dispatch gate and `start_new_task` both refuse to act while
    /// `Working`, an unhandled error here previously left the session stuck
    /// forever (every later message queuing, never sending) until the whole
    /// process was killed and restarted, even after switching to a healthy
    /// provider. Mirrors `mark_cancelled`'s shape: a lifecycle-only
    /// transition, no synthetic assistant message — the caller (the TUI)
    /// already shows the error to the operator through its own error-banner
    /// mechanism, so duplicating it into the transcript here would just be
    /// noise.
    pub async fn mark_model_call_failed(&mut self, detail: &str) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working | TaskLifecycle::Waiting => {
                self.transition(TaskLifecycle::Failed, TransitionReason::TurnFailure)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "turn_failed".into(),
                    detail: format!("model_call_failed: {detail}"),
                });
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// On resume/reload: a durable Running/AwaitingHitl task with no live runtime
    /// cannot safely continue as Working. HITL remains Waiting; bare Running becomes
    /// Interrupted. Legacy sessions with no terminal metadata stay Interrupted rather
    /// than eternal Working. Completed/Failed/Cancelled are left untouched.
    pub async fn mark_interrupted_if_stale(&mut self) -> Result<(), LoopError> {
        match self.active_task.lifecycle {
            TaskLifecycle::Working => {
                // No active runtime after reload/resume.
                self.transition(TaskLifecycle::Interrupted, TransitionReason::StaleOnResume)
                    .await?;
                self.events.push(TurnEvent {
                    kind: "interrupted".into(),
                    detail: "stale running task has no recoverable runtime".into(),
                });
                Ok(())
            }
            // Waiting is still a recoverable state (operator can decide).
            TaskLifecycle::Ready
            | TaskLifecycle::Waiting
            | TaskLifecycle::Completed
            | TaskLifecycle::Failed
            | TaskLifecycle::Cancelled
            | TaskLifecycle::Interrupted => Ok(()),
            // `TaskLifecycle` is `#[non_exhaustive]`. Leave an unrecognised status untouched
            // rather than forcing it to Interrupted.
            _ => Ok(()),
        }
    }

    /// Run until no tool calls, max turns, or HITL pause.
    pub async fn run_user_message(&mut self, text: &str) -> Result<ModelResponse, LoopError> {
        self.append_user_message(text).await?;
        self.run_agent_turns(None).await
    }

    /// Context-reset (if needed) + journal a model request; returns the request to send.
    pub async fn prepare_model_step(&mut self, turn: u32) -> Result<ModelRequest, LoopError> {
        if self.enable_context && self.context.should_reset(&self.messages) {
            let ws_ref = String::new();
            let system =
                assemble_system_prompt(&self.context.load_agents_md(), &self.context.load_skills());
            let (doc, msgs) = self
                .context
                .handoff_reset(&self.messages, &ws_ref, &system)?;
            self.journal
                .append_context_reset(
                    self.session_id,
                    json!({ "progress": doc, "messages": msgs }),
                )
                .await?;
            self.messages = msgs;
            self.events.push(TurnEvent {
                kind: "context_reset".into(),
                detail: "threshold".into(),
            });
        }

        tracing::debug!(turn, "model step");
        self.journal
            .append_model_request(
                self.session_id,
                json!({ "turn": turn, "messages": self.messages.len() }),
            )
            .await?;
        Ok(self.build_model_request())
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    /// Mark the session failed after exhausting turns.
    pub async fn fail_max_turns(&mut self) -> Result<(), LoopError> {
        self.finalize_turn_failure(
            "Forge couldn't complete this turn within the step limit.",
            "max_turns",
        )
        .await
    }

    /// Drive the agent loop after the user message is already appended.
    /// Optional `stream_tx` receives token deltas during each model complete.
    pub async fn run_agent_turns(
        &mut self,
        stream_tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, LoopError> {
        TurnCoordinator::run(self, stream_tx).await
    }

    async fn hash_workspace_path(&self, relative: &str) -> Option<u64> {
        hash_file(&self.tool_ctx.workspace_root.join(relative)).await
    }

    /// Read-only `git` invocation used only to capture before/after state for
    /// effect verification — never one of the mutating tool-facing commands.
    async fn run_git_readonly(&self, args: &[&str]) -> Option<String> {
        let out = tokio::process::Command::new("git")
            .args(args)
            .current_dir(&self.tool_ctx.workspace_root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Pre-call content hashes for the path(s) a `write_file`/`apply_patch`
    /// call is about to touch. `None` for any other tool.
    async fn pre_edit_snapshot(&self, call: &ToolCall) -> Option<Vec<(String, Option<u64>)>> {
        match call.name.as_str() {
            "write_file" => {
                let path = call.arguments.get("path")?.as_str()?.to_string();
                let hash = self.hash_workspace_path(&path).await;
                Some(vec![(path, hash)])
            }
            "apply_patch" => {
                let patch = call.arguments.get("patch")?.as_str()?;
                let mut out = Vec::new();
                for (path, _kind) in parse_patch_paths(patch) {
                    let hash = self.hash_workspace_path(&path).await;
                    out.push((path, hash));
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Pre-call git state needed to verify the requested effect afterward.
    /// `None` for any non-`git` tool.
    async fn git_pre_state(&self, call: &ToolCall) -> Option<GitPre> {
        if call.name != "git" {
            return None;
        }
        let a: GitCallArgsLite = serde_json::from_value(call.arguments.clone()).ok()?;
        let sub = a.subcommand.trim().to_ascii_lowercase();
        Some(match sub.as_str() {
            "commit" => GitPre::Head(self.run_git_readonly(&["rev-parse", "HEAD"]).await),
            "checkout" | "switch" => GitPre::Branch(
                self.run_git_readonly(&["rev-parse", "--abbrev-ref", "HEAD"])
                    .await,
            ),
            "restore" => {
                GitPre::RestorePath(a.args.iter().rev().find(|s| !s.starts_with('-')).cloned())
            }
            "add" => GitPre::NotVerified, // verified post-hoc from staged state, no pre-check needed
            _ => GitPre::NotVerified,
        })
    }

    async fn pre_tool_state(
        &self,
        call: &ToolCall,
    ) -> (Option<Vec<(String, Option<u64>)>>, Option<GitPre>) {
        (
            self.pre_edit_snapshot(call).await,
            self.git_pre_state(call).await,
        )
    }

    /// Build evidence for a `write_file`/`apply_patch` call from its
    /// pre-call content hashes and the tool's own success/failure report.
    /// Post-call state is re-read from the filesystem — never trusted from
    /// the tool's text output alone.
    async fn push_file_edit_evidence(
        &mut self,
        call: &ToolCall,
        pre: Vec<(String, Option<u64>)>,
        output: &ToolOutput,
    ) {
        match call.name.as_str() {
            "write_file" => {
                let Some((path, pre_hash)) = pre.into_iter().next() else {
                    return;
                };
                let post_hash = self.hash_workspace_path(&path).await;
                let event = if output.is_error {
                    ExecutionEvent::ToolFailed
                } else if pre_hash.is_none() {
                    ExecutionEvent::FileCreated
                } else {
                    ExecutionEvent::FileWritten
                };
                let mut entry = EvidenceEntry::new(event)
                    .operation_id(call.id.clone())
                    .tool_name("write_file")
                    .path(path)
                    .checksum_before(pre_hash)
                    .checksum_after(post_hash);
                if output.is_error {
                    entry = entry.error(truncate(&output.content, 200));
                }
                self.turn.push_evidence(entry);
            }
            "apply_patch" => {
                for (path, pre_hash) in pre {
                    let post_hash = self.hash_workspace_path(&path).await;
                    let event = if output.is_error {
                        ExecutionEvent::PatchRejected
                    } else {
                        ExecutionEvent::PatchApplied
                    };
                    let mut entry = EvidenceEntry::new(event)
                        .operation_id(call.id.clone())
                        .tool_name("apply_patch")
                        .path(path)
                        .checksum_before(pre_hash)
                        .checksum_after(post_hash);
                    if output.is_error {
                        entry = entry.error(truncate(&output.content, 200));
                    }
                    self.turn.push_evidence(entry);
                }
            }
            _ => {}
        }
    }

    /// Build evidence for a `git` call, verifying the subcommand's expected
    /// repository effect where practical (see `GitPre`).
    async fn push_git_evidence(
        &mut self,
        call: &ToolCall,
        pre: Option<GitPre>,
        output: &ToolOutput,
    ) {
        let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) else {
            return;
        };
        let sub = a.subcommand.trim().to_ascii_lowercase();
        let mut entry = EvidenceEntry::new(if output.is_error {
            ExecutionEvent::GitCommandFailed
        } else {
            ExecutionEvent::GitCommandSucceeded
        })
        .operation_id(call.id.clone())
        .tool_name("git")
        .git_command(sub.clone());

        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
            self.turn.push_evidence(entry);
            return;
        }

        let verified = match pre {
            Some(GitPre::Head(pre_head)) => {
                let post_head = self.run_git_readonly(&["rev-parse", "HEAD"]).await;
                Some(pre_head != post_head)
            }
            Some(GitPre::Branch(pre_branch)) => {
                let post_branch = self
                    .run_git_readonly(&["rev-parse", "--abbrev-ref", "HEAD"])
                    .await;
                Some(pre_branch != post_branch)
            }
            Some(GitPre::RestorePath(Some(path))) => {
                let still_dirty = self
                    .run_git_readonly(&["diff", "--name-only", "--", &path])
                    .await
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(true);
                Some(!still_dirty)
            }
            _ if sub == "add" => {
                let staged = self
                    .run_git_readonly(&["diff", "--cached", "--name-only"])
                    .await;
                Some(staged.map(|s| !s.trim().is_empty()).unwrap_or(false))
            }
            _ => None,
        };
        entry = entry.git_effect_verified(verified);
        self.turn.push_evidence(entry);
    }

    fn push_search_evidence(&mut self, call: &ToolCall, output: &ToolOutput) {
        let event = if output.is_error {
            ExecutionEvent::SearchFailed
        } else {
            ExecutionEvent::SearchFinished
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(call.name.clone());
        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
        } else {
            let count = search_result_count(&output.content);
            entry = entry.count(count);
        }
        self.turn.push_evidence(entry);
    }

    /// Ensures every `ToolOutput` reaching a `Message` carries a real
    /// `outcome`, even for `Tool` impls that don't set one explicitly — so
    /// not every tool in `forge-tools` needs an individual update.
    fn backfill_tool_outcome(output: &mut ToolOutput) {
        if output.outcome.is_none() {
            output.outcome = Some(if output.is_error {
                ExecutionOutcome::Failed {
                    exit_code: output.exit_code,
                }
            } else {
                ExecutionOutcome::Success
            });
        }
    }

    fn push_bash_evidence(&mut self, call: &ToolCall, output: &ToolOutput) {
        let event = if output.is_error {
            ExecutionEvent::ToolFailed
        } else {
            ExecutionEvent::ToolFinished
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(bash_label(&call.arguments));
        if let Some(code) = output.exit_code {
            entry = entry.exit_code(code);
        }
        if output.is_error {
            entry = entry.error(truncate(&output.content, 200));
        }
        self.turn.push_evidence(entry);
    }

    /// Dispatches to the right evidence builder for a successfully-dispatched
    /// tool call (the tool itself may still report `is_error`). No-op for
    /// tools with no completion-relevant side effect (e.g. `read_file`).
    async fn push_success_evidence(
        &mut self,
        call: &ToolCall,
        pre_edit: Option<Vec<(String, Option<u64>)>>,
        pre_git: Option<GitPre>,
        output: &ToolOutput,
    ) {
        match call.name.as_str() {
            "write_file" | "apply_patch" => {
                if let Some(pre) = pre_edit {
                    self.push_file_edit_evidence(call, pre, output).await;
                }
            }
            "git" => self.push_git_evidence(call, pre_git, output).await,
            "fffind" | "ffgrep" => self.push_search_evidence(call, output),
            "bash" => self.push_bash_evidence(call, output),
            _ => {}
        }
    }

    /// Evidence for a call the runtime refused to execute at all (ACL denial,
    /// HITL denial) — no filesystem/process ever ran, so there's nothing to
    /// verify beyond recording the refusal.
    fn push_denied_evidence(&mut self, call: &ToolCall, message: &str) {
        let event = match call.name.as_str() {
            "git" => ExecutionEvent::GitCommandFailed,
            "write_file" | "apply_patch" => ExecutionEvent::PatchRejected,
            "fffind" | "ffgrep" => ExecutionEvent::SearchFailed,
            _ => ExecutionEvent::ToolFailed,
        };
        let mut entry = EvidenceEntry::new(event)
            .operation_id(call.id.clone())
            .tool_name(call.name.clone())
            .error(truncate(message, 200));
        if call.name == "git" {
            if let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) {
                entry = entry.git_command(a.subcommand.trim().to_ascii_lowercase());
            }
        }
        self.turn.push_evidence(entry);
    }

    /// Returns Some(response) if paused for HITL.
    async fn run_one_tool(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<Option<ModelResponse>, LoopError> {
        if self.try_serve_journaled_tool(call).await? {
            return Ok(None);
        }
        self.turn.record_call(call.clone());
        let class = self
            .tools
            .get(&call.name)
            .map(|t| t.side_effect_class())
            .unwrap_or(SideEffectClass::Meta);

        if self.enable_gov {
            let decision = self.governance.authorize(call, class);
            let redacted = self.governance.redact_args(&call.arguments);
            self.governance.record_audit(AuditEvent {
                session_id: self.session_id.to_string(),
                principal: self.governance.principal.id.clone(),
                tool: call.name.clone(),
                args_redacted: redacted.clone(),
                decision,
                policy_id: "default".into(),
                result: format!("{decision:?}"),
                duration_ms: 0,
                trace_id: None,
            });
            match decision {
                PolicyDecision::Hitl => {
                    let payload = HitlPayload {
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        args_redacted: redacted,
                        reason: "policy requires human approval".into(),
                    };
                    self.journal
                        .append_hitl_wait(self.session_id, &serde_json::to_value(&payload).unwrap())
                        .await?;
                    let request_id = payload.call_id.clone();
                    self.enter_waiting(
                        WaitReason::Approval {
                            request_id,
                            payload: payload.clone(),
                        },
                        TransitionReason::HitlWait,
                    )
                    .await?;
                    self.events.push(TurnEvent {
                        kind: "hitl_wait".into(),
                        detail: payload.tool.clone(),
                    });
                    self.turn.push_evidence(
                        EvidenceEntry::new(ExecutionEvent::WaitingForUser)
                            .operation_id(call.id.clone())
                            .tool_name(call.name.clone()),
                    );
                    return Ok(Some(ModelResponse {
                        text: format!("Awaiting HITL approval for tool {}", call.name),
                        tool_calls: vec![call.clone()],
                        usage: None,
                        thinking: None,
                    }));
                }
                PolicyDecision::Allow => {}
                // `PolicyDecision` is `#[non_exhaustive]`, so the denial path is the
                // wildcard rather than a named `Deny`: this gate must fail CLOSED. An
                // explicit deny and a decision this build does not recognise are both
                // refused here, so neither can fall through to the execution below.
                _ => {
                    let output = ToolOutput::denied(format!("denied by ACL: {}", call.name));
                    self.push_denied_evidence(call, &output.content);
                    self.journal
                        .append_tool_intent(self.session_id, call)
                        .await?;
                    self.journal
                        .append_tool_result(self.session_id, call, &output)
                        .await?;
                    self.remember_tool_result(call, &output);
                    self.messages.push(Message {
                        outcome: output.effective_outcome(),
                        role: MessageRole::Tool,
                        content: output.content,
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        thinking: None,
                        thinking_duration_secs: None,
                        tool_calls: vec![],
                    });
                    return Ok(None);
                }
            }
        }

        self.journal
            .append_tool_intent(self.session_id, call)
            .await?;

        // `background_run` never reaches `ToolRegistry::call` — it's
        // intercepted here and routed to `spawn_background_shell` instead,
        // so starting it doesn't block this turn. See `background.rs`.
        if call.name == "background_run" {
            return self.dispatch_background_run(call).await;
        }

        let (pre_edit, pre_git) = self.pre_tool_state(call).await;

        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(mut output) => {
                Self::backfill_tool_outcome(&mut output);
                self.push_success_evidence(call, pre_edit, pre_git, &output)
                    .await;
                if self.enable_context {
                    output.content = self.context.maybe_offload_tool_content(output.content)?;
                }
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                self.messages.push(Message {
                    outcome: output.effective_outcome(),
                    role: MessageRole::Tool,
                    content: output.content.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                self.events.push(TurnEvent {
                    kind: "tool".into(),
                    detail: format!("{} -> {} chars", call.name, output.content.len()),
                });
            }
            Err(ToolError::Validation(ve)) => {
                self.journal
                    .append_validation_failed(self.session_id, &call.name, &ve.to_string())
                    .await?;
                // Actionable, schema-derived feedback — no guessed corrected values.
                let msg = format!(
                    "Tool validation error: {ve}. \
                     Do not concatenate fields. Use separate JSON properties with native types \
                     (for example offset: 1, limit: 100 as integers)."
                );
                self.messages.push(Message {
                    outcome: ExecutionOutcome::Failed { exit_code: None },
                    role: MessageRole::Tool,
                    content: msg.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                self.events.push(TurnEvent {
                    kind: "validation".into(),
                    detail: msg,
                });
            }
            Err(e) => {
                let content = e.to_string();
                let is_budget = content.contains("validation retry budget exceeded");
                let outcome = e.as_outcome();
                let output = ToolOutput {
                    outcome: Some(outcome),
                    content: if is_budget {
                        format!(
                            "{content}. Stop retrying this tool with the same invalid argument shape. \
                             Either call it with valid structured JSON types or finish with a final answer."
                        )
                    } else {
                        content
                    },
                    is_error: true,
                    exit_code: None,
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                self.messages.push(Message {
                    outcome: output.effective_outcome(),
                    role: MessageRole::Tool,
                    content: output.content.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
                if is_budget {
                    self.events.push(TurnEvent {
                        kind: "validation_exhausted".into(),
                        detail: output.content.clone(),
                    });
                    // Terminal failure: stop the turn instead of activity-only hang.
                    self.finalize_turn_failure(
                        "Forge couldn't complete this turn after repeated invalid tool calls.",
                        "validation_exhausted",
                    )
                    .await?;
                }
            }
        }
        Ok(None)
    }

    /// DUR-03: resolve pending HITL then optionally execute the tool.
    pub async fn resolve_hitl(
        &mut self,
        decision: HitlDecision,
        actor: &str,
    ) -> Result<(), LoopError> {
        self.resolve_hitl_with_feedback(decision, actor, None).await
    }

    /// Same as [`Self::resolve_hitl`], but a `Deny` can carry a short
    /// message that reaches the agent as the tool result content — context
    /// for what to do differently, folded into this same turn rather than
    /// a bare denial the operator has to re-explain next turn. Modeled on
    /// opencode's `CorrectedError`. Ignored for `Approve`.
    pub async fn resolve_hitl_with_feedback(
        &mut self,
        decision: HitlDecision,
        actor: &str,
        feedback: Option<&str>,
    ) -> Result<(), LoopError> {
        let payload = self
            .pending_hitl()
            .cloned()
            .ok_or(LoopError::NoPendingHitl)?;
        // `HitlDecision` is `#[non_exhaustive]`. Derive approval explicitly rather than
        // testing `== Deny` below: a decision this build does not recognise must never
        // be read as approval and reach execution. Fail closed.
        let (dec, approved) = match decision {
            HitlDecision::Approve => ("approve", true),
            HitlDecision::Deny => ("deny", false),
            _ => ("deny", false),
        };
        self.journal
            .append_hitl_resume(self.session_id, dec, actor)
            .await?;

        if !approved {
            let feedback = feedback.map(str::trim).filter(|f| !f.is_empty());
            let output = ToolOutput::denied(match feedback {
                Some(feedback) => format!("HITL denied by {actor}: {feedback}"),
                None => format!("HITL denied by {actor}"),
            });
            let call = ToolCall {
                id: payload.call_id.clone(),
                name: payload.tool.clone(),
                arguments: payload.args_redacted.clone(),
            };
            self.turn.record_call(call.clone());
            self.push_denied_evidence(&call, &output.content);
            self.journal
                .append_tool_intent(self.session_id, &call)
                .await?;
            self.journal
                .append_tool_result(self.session_id, &call, &output)
                .await?;
            self.remember_tool_result(&call, &output);
            self.messages.push(Message {
                outcome: output.effective_outcome(),
                role: MessageRole::Tool,
                content: output.content,
                tool_call_id: Some(payload.call_id),
                name: Some(payload.tool),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            });
            // Stale evidence from the paused call must not leak into a later
            // completion decision within this same turn (see `apply_model_response`).
            self.turn
                .evidence_mut()
                .0
                .retain(|e| e.event() != ExecutionEvent::WaitingForUser);

            self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
                .await?;

            if self.turn.record_hitl_denial() >= Self::MAX_CONSECUTIVE_HITL_DENIALS {
                // A denial is a strong signal the user does not want this
                // approach pursued at all. Without this, the model would
                // keep autonomously searching for a workaround for up to
                // `max_turns` (128 by default) model steps before yielding
                // control back — expensive, slow, and surprising for what
                // was a single "no". Stop the turn now instead.
                self.finalize_turn_failure(
                    "Forge stopped after repeated denied approvals for this turn.",
                    "hitl_denied",
                )
                .await?;
            }
            return Ok(());
        }

        self.turn.reset_hitl_denials();
        // Re-authorize
        let call = ToolCall {
            id: payload.call_id.clone(),
            name: payload.tool.clone(),
            arguments: payload.args_redacted.clone(),
        };
        let class = self
            .tools
            .get(&call.name)
            .map(|t| t.side_effect_class())
            .unwrap_or(SideEffectClass::Meta);
        if self.enable_gov {
            let d = self.governance.authorize(&call, class);
            // `PolicyDecision` is `#[non_exhaustive]`. Testing `== Deny` let every other
            // verdict through, so an unrecognised one would execute. Decide explicitly:
            // `Hitl` still proceeds because the operator already approved this call and
            // re-requiring approval here would stall the turn; anything unrecognised is
            // refused. Behaviour is unchanged for Allow, Hitl and Deny.
            let refuse = !matches!(d, PolicyDecision::Allow | PolicyDecision::Hitl);
            if refuse {
                self.turn
                    .evidence_mut()
                    .0
                    .retain(|e| e.event() != ExecutionEvent::WaitingForUser);
                self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
                    .await?;
                return Err(LoopError::Other(
                    "policy denies tool after HITL approve".into(),
                ));
            }
        }

        // Restore args from pending — we only have redacted; for tests use redacted as args
        let mut budget = ValidationBudget::with_default_max();
        self.turn
            .evidence_mut()
            .0
            .retain(|e| e.event() != ExecutionEvent::WaitingForUser);
        self.transition(TaskLifecycle::Working, TransitionReason::HitlResolved)
            .await?;
        // Execute with stored args (may be redacted in production; Phase 2 keeps full call in journal intent before wait ideally)
        // Re-fetch from last HitlWait — for approve path re-execute with redacted args is weak;
        // store original args in pending for this implementation:
        self.run_one_tool_exec_only(&call, &mut budget).await?;
        Ok(())
    }

    async fn run_one_tool_exec_only(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<(), LoopError> {
        if self.try_serve_journaled_tool(call).await? {
            return Ok(());
        }
        self.turn.record_call(call.clone());
        self.journal
            .append_tool_intent(self.session_id, call)
            .await?;
        let (pre_edit, pre_git) = self.pre_tool_state(call).await;
        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(mut output) => {
                Self::backfill_tool_outcome(&mut output);
                self.push_success_evidence(call, pre_edit, pre_git, &output)
                    .await;
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
                if call.name == "update_plan" && !output.is_error {
                    // Stateless checklist broadcast — clients replace whatever they
                    // were showing with this payload. Mirrors codex PlanUpdate.
                    self.events.push(TurnEvent {
                        kind: "plan_update".into(),
                        detail: call.arguments.to_string(),
                    });
                }
                self.messages.push(Message {
                    outcome: output.effective_outcome(),
                    role: MessageRole::Tool,
                    content: output.content,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                });
            }
            Err(e) => {
                let outcome = e.as_outcome();
                let output = ToolOutput {
                    outcome: Some(outcome),
                    content: e.to_string(),
                    is_error: true,
                    exit_code: None,
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.remember_tool_result(call, &output);
            }
        }
        Ok(())
    }

    /// Active workspace root.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.tool_ctx.workspace_root
    }

    /// Use this provider/model id on subsequent completions (e.g. after `/connect`).
    pub fn set_active_model(&mut self, model: impl Into<String>) {
        self.active_model = model.into();
    }

    /// Wire-level reasoning-effort value to send on the next completion, or
    /// `None` to omit the field (model doesn't support it, or effort is Auto).
    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    /// Push provider credentials into the model client (OAuth tokens → worker env).
    pub fn apply_provider_env(&self, pairs: &[(String, String)]) {
        self.model.apply_provider_env(pairs);
    }

    /// Clear provider credentials from the model client and recycle the worker.
    pub fn clear_provider_env(&self) {
        self.model.clear_provider_env();
    }
}

impl AgentSession {
    pub async fn force_context_reset_async(&mut self) -> Result<(), LoopError> {
        let ws_ref = String::new();
        let system =
            assemble_system_prompt(&self.context.load_agents_md(), &self.context.load_skills());
        let (doc, msgs) = self
            .context
            .handoff_reset(&self.messages, &ws_ref, &system)?;
        self.journal
            .append_context_reset(
                self.session_id,
                json!({ "progress": doc, "workspace_ref": ws_ref, "messages": msgs }),
            )
            .await?;
        self.messages = msgs;
        self.events.push(TurnEvent {
            kind: "context_reset".into(),
            detail: "handoff written".into(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_governance::AclPolicy;
    use forge_model::MockModelClient;
    use forge_types::{Message, MessageRole, ToolCall, Usage};
    use tempfile::tempdir;

    #[test]
    fn system_prompt_uses_forge_policy() {
        let prompt = assemble_system_prompt("", &[]);
        assert!(prompt.starts_with("You are a coding agent running in the Forge"));
        assert!(prompt.contains("Forge is an open source project led by NorviaLabs."));
        assert!(!prompt.contains("# Project Instructions"));
    }

    #[test]
    fn system_prompt_appends_project_instructions() {
        let prompt = assemble_system_prompt("Run cargo test", &[]);
        assert!(prompt.starts_with("You are a coding agent running in the Forge"));
        assert!(prompt.ends_with("AGENTS.md:\nRun cargo test"));
    }

    fn legacy_skill(name: &str, body: &str) -> forge_context::SkillManifest {
        forge_context::SkillManifest {
            name: name.to_string(),
            description: String::new(),
            dir: std::path::PathBuf::new(),
            body: body.to_string(),
            has_frontmatter: false,
            metadata: None,
            compatibility: None,
            license: None,
        }
    }

    fn manifest_skill(name: &str, description: &str, body: &str) -> forge_context::SkillManifest {
        forge_context::SkillManifest {
            name: name.to_string(),
            description: description.to_string(),
            dir: std::path::PathBuf::new(),
            body: body.to_string(),
            has_frontmatter: true,
            metadata: None,
            compatibility: None,
            license: None,
        }
    }

    /// A skill with no YAML frontmatter has no `description` to show in
    /// discovery, so its full body is injected eagerly — the pre-#226
    /// behavior, preserved for backward compatibility.
    #[test]
    fn system_prompt_appends_skills_without_frontmatter_eagerly() {
        let skills = vec![legacy_skill("ponytail", "# Ponytail\nUse less code.")];
        let prompt = assemble_system_prompt("", &skills);
        assert!(prompt.contains("# Skills"));
        assert!(prompt.contains("## ponytail"));
        assert!(prompt.ends_with("# Ponytail\nUse less code."));
    }

    /// A skill with frontmatter only surfaces name + description (discovery
    /// stage) — its full body must NOT appear in the system prompt, since the
    /// model is expected to fetch it via `load_skill` on demand.
    #[test]
    fn system_prompt_shows_only_name_and_description_for_skills_with_frontmatter() {
        let skills = vec![manifest_skill(
            "reviewer",
            "Reviews pull requests for style issues.",
            "# Reviewer\n\nFull instructions that should stay out of the prompt.",
        )];
        let prompt = assemble_system_prompt("", &skills);
        assert!(prompt.contains("# Skills"));
        assert!(prompt.contains("## reviewer"));
        assert!(prompt.contains("Reviews pull requests for style issues."));
        assert!(!prompt.contains("Full instructions that should stay out of the prompt"));
        assert!(prompt.contains("load_skill"));
    }

    fn base_cfg(dir: &std::path::Path) -> LoopConfig {
        LoopConfig {
            max_turns: 5,
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn session_registers_web_search_with_default_config() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let names = s.list_tools();
        assert!(
            names.iter().any(|n| n == "web_search"),
            "expected web_search in {names:?}"
        );
    }

    #[tokio::test]
    async fn session_omits_web_search_when_disabled() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut cfg = base_cfg(dir.path());
        cfg.web_search.enabled = false;
        let s = AgentSession::create(cfg, model, ToolRegistry::new())
            .await
            .unwrap();
        assert!(!s.list_tools().iter().any(|n| n == "web_search"));
    }

    #[tokio::test]
    async fn build_model_request_carries_reasoning_effort_when_set() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.set_reasoning_effort(Some("high".into()));
        assert_eq!(
            s.build_model_request().reasoning_effort,
            Some("high".to_string())
        );
    }

    #[tokio::test]
    async fn build_model_request_omits_reasoning_effort_when_none() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        assert_eq!(s.build_model_request().reasoning_effort, None);
    }

    #[tokio::test]
    async fn run_model_step_with_stream_merges_stream_usage() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "streamed".into(),
            tool_calls: vec![],
            usage: Some(Usage {
                prompt_tokens: 11,
                completion_tokens: 4,
                ..Default::default()
            }),
            thinking: Some("trace".into()),
        }]));
        let mut session = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        session
            .messages
            .push(Message::new(MessageRole::User, "hello"));
        let response = session.run_model_step_with_stream(0, None).await.unwrap();
        assert_eq!(response.text, "streamed");
        assert_eq!(response.usage.unwrap().prompt_tokens, 11);
        assert_eq!(response.thinking.as_deref(), Some("trace"));
    }

    #[tokio::test]
    async fn loop_text_only_completes() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "all done".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("hello").await.unwrap();
        assert_eq!(r.text, "all done");
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
    }

    #[tokio::test]
    async fn promote_next_queued_starts_a_new_task_and_removes_the_item() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);

        let item = s.enqueue_task("do the next thing").await.unwrap();
        assert_eq!(s.queue().len(), 1);

        let task_id = s.promote_next_queued().await.unwrap().unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
        assert_eq!(s.active_task.task_id, task_id);
        // Promotion removed exactly the one item from the visible queue.
        assert_eq!(s.queue().len(), 0);
        assert!(s.queue().visible().all(|q| q.id != item.id));
        assert!(s.messages.iter().any(|m| m.content == "do the next thing"));
    }

    #[tokio::test]
    async fn promote_next_queued_on_empty_queue_is_a_no_op() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        assert_eq!(s.promote_next_queued().await.unwrap(), None);
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);
    }

    #[tokio::test]
    async fn cancel_queued_at_removes_by_visible_position() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.enqueue_task("a").await.unwrap();
        let b = s.enqueue_task("b").await.unwrap();
        s.enqueue_task("c").await.unwrap();

        let removed = s.cancel_queued_at(2).await.unwrap().unwrap();
        assert_eq!(removed.id, b.id);
        let remaining: Vec<&str> = s.queue().visible().map(|q| q.text.as_str()).collect();
        assert_eq!(remaining, vec!["a", "c"]);
    }

    #[tokio::test]
    async fn queue_items_survive_resume_without_duplication() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.append_user_message("first task").await.unwrap();
        s.enqueue_task("queued one").await.unwrap();
        s.enqueue_task("queued two").await.unwrap();
        assert_eq!(s.queue().len(), 2);

        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();
        assert_eq!(resumed.queue().len(), 2);
        let texts: Vec<&str> = resumed.queue().visible().map(|q| q.text.as_str()).collect();
        assert_eq!(texts, vec!["queued one", "queued two"]);
    }

    /// End-to-end crash-recovery test: a crash between the `QueuePromoting`
    /// journal write and the confirming `QueuePromoted` one — but *after*
    /// the task's user message already landed — must not resurrect the item
    /// as `Queued` on resume. That would let a later `promote_next_queued`
    /// create a second task from the same instruction, violating "a queue
    /// item cannot execute twice."
    #[tokio::test]
    async fn crash_after_task_created_but_before_promoted_confirmation_does_not_duplicate() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let item = s.enqueue_task("do the thing").await.unwrap();

        // Simulate a crash: journal exactly what `promote_next_queued`'s
        // first steps write (mark Promoting, then the task's user message)
        // but never reach the confirming `QueuePromoted` event.
        let journal = forge_durable::Journal::open(s.journal_dir(), s.session_id)
            .await
            .unwrap();
        journal
            .append_queue_promoting(s.session_id, item.id)
            .await
            .unwrap();
        journal
            .append_user_message(s.session_id, &item.text)
            .await
            .unwrap();

        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();

        // Not visible in the queue again — the task was already created.
        assert!(resumed.queue().is_empty());
        assert!(resumed.queue().peek_next_queued().is_none());
        // The task's user message did survive, exactly once.
        assert_eq!(
            resumed
                .messages
                .iter()
                .filter(|m| m.content == "do the thing")
                .count(),
            1
        );
    }

    /// Contrasting case: a crash *before* the task's user message was ever
    /// journaled must return the item to `Queued` so it isn't lost.
    #[tokio::test]
    async fn crash_before_task_created_reverts_item_to_queued() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let queued = s.enqueue_task("not started yet").await.unwrap();
        let journal = forge_durable::Journal::open(s.journal_dir(), s.session_id)
            .await
            .unwrap();
        journal
            .append_queue_promoting(s.session_id, queued.id)
            .await
            .unwrap();
        // No `append_user_message` — the crash happened before the task
        // itself was created.

        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();

        assert_eq!(resumed.queue().len(), 1);
        assert_eq!(
            resumed.queue().peek_next_queued().map(|q| q.text.as_str()),
            Some("not started yet")
        );
    }

    #[tokio::test]
    async fn cancelling_the_active_task_preserves_the_queue() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.append_user_message("first task").await.unwrap();
        s.enqueue_task("queued while busy").await.unwrap();
        assert_eq!(s.queue().len(), 1);

        s.mark_cancelled().await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
        // Cancellation must not silently clear the queue.
        assert_eq!(s.queue().len(), 1);
    }

    #[tokio::test]
    async fn loop_runs_tool_then_finishes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "read ok".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("read it").await.unwrap();
        assert_eq!(r.text, "read ok");
        let assistant = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
            .unwrap();
        assert_eq!(assistant.tool_calls[0].id, "1");
    }

    #[tokio::test]
    async fn malformed_read_file_offset_is_rejected_and_does_not_execute() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello\nworld\n").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "bad".into(),
                    name: "read_file".into(),
                    // Exact observed failure class — composite string must not be salvaged.
                    arguments: json!({"path": "README.md", "offset": "1arglimit\">100"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "ok".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "README.md", "offset": 1, "limit": 100}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "Forge is a Rust workspace.".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("Summarize this codebase").await.unwrap();
        assert_eq!(r.text, "Forge is a Rust workspace.");
        let tool_msgs: Vec<_> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .collect();
        assert!(
            tool_msgs.iter().any(|m| m.content.contains("validation")),
            "expected validation rejection: {tool_msgs:?}"
        );
        assert!(
            tool_msgs.iter().any(|m| m.content.contains("hello")),
            "valid retry should execute and return file content: {tool_msgs:?}"
        );
        // Validation feedback may quote the bad value; execution must not have
        // salvaged it into a successful read of the wrong slice.
        assert!(
            tool_msgs
                .iter()
                .filter(|m| m.content.contains("hello"))
                .all(|m| !m.content.contains("validation")),
            "successful tool result must not be a validation message"
        );
    }

    #[tokio::test]
    async fn repeated_malformed_read_file_exhausts_validation_budget() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "x\n").unwrap();
        let bad = || ToolCall {
            id: "bad".into(),
            name: "read_file".into(),
            arguments: json!({"path": "README.md", "offset": "1arglimit\">50"}),
        };
        // Four invalid attempts across model steps → budget exhausts → terminal failure.
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![bad()],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let _ = s.run_user_message("read it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert!(
            s.messages
                .iter()
                .any(|m| m.content.starts_with(TURN_FAILED_MARKER)),
            "expected durable failure summary: {:?}",
            s.messages
        );
        assert!(
            s.events.iter().any(|e| e.kind == "turn_failed"),
            "expected turn_failed event"
        );
        assert!(
            s.messages.iter().any(|m| {
                m.role == MessageRole::Tool
                    && m.content.contains("validation retry budget exceeded")
            }) || s.events.iter().any(|e| e.kind == "validation_exhausted"),
            "expected budget exhaustion signal"
        );
    }

    #[tokio::test]
    async fn empty_final_after_tools_is_terminal_failure_not_success() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            // Model ends with no answer after tools.
            ModelResponse {
                text: "".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let _ = s.run_user_message("read it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert!(s
            .messages
            .iter()
            .any(|m| m.content.starts_with(TURN_FAILED_MARKER)));
    }

    #[tokio::test]
    async fn resume_restores_conversation_context_and_usage() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    ..Default::default()
                }),
                thinking: Some("inspect".into()),
            },
            ModelResponse {
                text: "read ok".into(),
                tool_calls: vec![],
                usage: Some(Usage {
                    prompt_tokens: 20,
                    completion_tokens: 4,
                    ..Default::default()
                }),
                thinking: None,
            },
        ]));
        let cfg = base_cfg(dir.path());
        let mut session = AgentSession::create(cfg.clone(), model, ToolRegistry::new())
            .await
            .unwrap();
        session.run_user_message("read it").await.unwrap();
        let session_id = session.session_id;
        drop(session);

        let resumed = AgentSession::resume(
            cfg,
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            session_id,
        )
        .await
        .unwrap();
        let roles = resumed
            .messages
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![
                MessageRole::System,
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::Assistant,
            ]
        );
        assert_eq!(resumed.messages[2].tool_calls[0].id, "1");
        assert_eq!(resumed.messages[4].content, "read ok");
        assert_eq!(resumed.token_usage.prompt_tokens, 30);
        assert_eq!(resumed.token_usage.completion_tokens, 6);
        assert_eq!(resumed.token_usage.model_steps, 2);
    }

    #[tokio::test]
    async fn resume_serves_journaled_tool_without_reexecuting() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("read").await.unwrap();
        let session_id = s.session_id;
        std::fs::write(dir.path().join("f.txt"), "second").unwrap();

        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done again".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut resumed =
            AgentSession::resume(base_cfg(dir.path()), model, ToolRegistry::new(), session_id)
                .await
                .unwrap();
        resumed.run_user_message("read again").await.unwrap();
        let tool_messages: Vec<_> = resumed
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .collect();
        assert_eq!(tool_messages.len(), 1);
        assert!(tool_messages[0].content.contains("first"));
        assert!(!tool_messages[0].content.contains("second"));
    }

    #[tokio::test]
    async fn resume_reconciles_non_idempotent_incomplete_intent() {
        use forge_durable::{new_session_id, Journal};

        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("j");
        let sid = new_session_id();
        let journal = Journal::open(&journal_dir, sid).await.unwrap();
        journal.append_session_created(sid).await.unwrap();
        journal.append_user_message(sid, "run").await.unwrap();
        let call = ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: json!({"command": "echo hi"}),
        };
        let response = ModelResponse {
            text: String::new(),
            tool_calls: vec![call.clone()],
            usage: None,
            thinking: None,
        };
        journal
            .append_model_response(sid, serde_json::to_value(&response).unwrap())
            .await
            .unwrap();
        journal.append_tool_intent(sid, &call).await.unwrap();

        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            sid,
        )
        .await
        .unwrap();
        let tool = resumed
            .messages
            .iter()
            .find(|message| message.role == MessageRole::Tool)
            .expect("synthetic tool result");
        assert!(tool.content.contains("not marked idempotent"));
        let state = Journal::open(&journal_dir, sid)
            .await
            .unwrap()
            .replay(sid)
            .await
            .unwrap();
        assert!(state.incomplete_intents.is_empty());
    }

    #[tokio::test]
    async fn resume_retries_idempotent_incomplete_intent() {
        use forge_durable::{new_session_id, Journal};

        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "payload").unwrap();
        let journal_dir = dir.path().join("j");
        let sid = new_session_id();
        let journal = Journal::open(&journal_dir, sid).await.unwrap();
        journal.append_session_created(sid).await.unwrap();
        journal.append_user_message(sid, "read").await.unwrap();
        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "f.txt"}),
        };
        let response = ModelResponse {
            text: String::new(),
            tool_calls: vec![call.clone()],
            usage: None,
            thinking: None,
        };
        journal
            .append_model_response(sid, serde_json::to_value(&response).unwrap())
            .await
            .unwrap();
        journal.append_tool_intent(sid, &call).await.unwrap();

        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            sid,
        )
        .await
        .unwrap();
        let tool = resumed
            .messages
            .iter()
            .find(|message| message.role == MessageRole::Tool)
            .expect("retried tool result");
        assert!(tool.content.contains("payload"));
        let state = Journal::open(&journal_dir, sid)
            .await
            .unwrap()
            .replay(sid)
            .await
            .unwrap();
        assert!(state.incomplete_intents.is_empty());
    }

    /// F-RECOVERY-01: denying one trivial approval used to let the model
    /// keep autonomously retrying for up to `max_turns` (128 by default)
    /// steps before yielding control back — a single "no" shouldn't cost
    /// that much. Two denials in a row within the same turn must now stop
    /// the turn outright instead of continuing to churn.
    #[tokio::test]
    async fn repeated_hitl_denials_stop_the_turn_instead_of_retrying_to_max_turns() {
        let dir = tempdir().unwrap();
        let push = ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git push origin main"}),
        };
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![push.clone()],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "2".into(),
                    ..push.clone()
                }],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("push").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
        assert_eq!(
            s.active_task.lifecycle,
            TaskLifecycle::Working,
            "a single denial must not fail the turn"
        );

        s.run_agent_turns(None).await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
        assert_eq!(
            s.active_task.lifecycle,
            TaskLifecycle::Failed,
            "a second consecutive denial must stop the turn"
        );
        assert!(s
            .messages
            .last()
            .unwrap()
            .content
            .contains("repeated denied approvals"));
    }

    #[tokio::test]
    async fn hitl_pauses_on_git_push() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let r = s.run_user_message("push").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);
        assert!(s.pending_hitl().is_some());
        assert!(r.text.contains("HITL"));
        s.resolve_hitl(HitlDecision::Deny, "test").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
        assert!(s.pending_hitl().is_none());
    }

    /// A deny with feedback folds the operator's note into the same tool
    /// result message the agent sees, so it can act on it this turn instead
    /// of needing to be re-prompted next turn.
    #[tokio::test]
    async fn hitl_deny_with_feedback_reaches_the_agent_as_tool_result_content() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("push").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl_with_feedback(
            HitlDecision::Deny,
            "test",
            Some("use --force-with-lease instead"),
        )
        .await
        .unwrap();

        let tool_message = s
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Tool)
            .expect("a tool result should record the denial");
        assert!(tool_message.content.contains("HITL denied by test"));
        assert!(tool_message
            .content
            .contains("use --force-with-lease instead"));
    }

    /// Whitespace-only feedback is treated the same as no feedback — the
    /// message stays the plain denial rather than trailing an empty colon.
    #[tokio::test]
    async fn hitl_deny_with_blank_feedback_omits_it_from_the_message() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("push").await.unwrap();

        s.resolve_hitl_with_feedback(HitlDecision::Deny, "test", Some("   "))
            .await
            .unwrap();

        let tool_message = s
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert_eq!(tool_message.content, "HITL denied by test");
    }

    /// Before this fix, the `WaitingForUser` evidence pushed at the HITL
    /// pause point lingered in `turn_evidence` for the rest of the turn.
    /// Approving and continuing would then have the completion evaluator see
    /// stale `WaitingForUser` evidence and misroute the next no-tool-calls
    /// model step through `finalize_turn_failure` as if the turn were
    /// waiting/failed, even though the attempt had already resumed Working
    /// and finished cleanly. `resolve_hitl` now strips that stale evidence on
    /// resume, so the turn completes normally instead.
    #[tokio::test]
    async fn resuming_from_hitl_does_not_leak_stale_waiting_evidence_into_completion() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "echo ok"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
        // The stale WaitingForUser entry from the pause must be gone —
        // otherwise the next completion decision would misread it.
        assert!(!s
            .turn
            .evidence()
            .0
            .iter()
            .any(|e| e.event() == ExecutionEvent::WaitingForUser));

        let outcome = s.run_agent_turns(None).await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(outcome.text, "done");
    }

    #[tokio::test]
    async fn acl_hides_denied_tools() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let mut acl = AclPolicy::allow_all();
        acl.deny("bash".into());
        s.set_governance(Governance::default().with_acl(acl));
        let names = s.list_tools();
        assert!(!names.iter().any(|n| n == "bash"));
        assert!(names.iter().any(|n| n == "read_file"));
    }

    /// The ACL denial arm in `run_one_tool` is the trailing wildcard, so this pins the
    /// behaviour: a denied tool is refused at execution time, not merely hidden from the
    /// catalogue. `acl_hides_denied_tools` covers listing; this covers execution.
    #[tokio::test]
    async fn acl_denied_tool_call_is_refused_at_execution_time() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "SENTINEL-CONTENT").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "secret.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let mut acl = AclPolicy::allow_all();
        acl.deny("read_file".into());
        s.set_governance(Governance::default().with_acl(acl));
        s.run_user_message("read it").await.unwrap();

        let tool_contents: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            tool_contents.iter().any(|c| c.contains("denied by ACL")),
            "expected an ACL denial result, got {tool_contents:?}"
        );
        assert!(
            !tool_contents.iter().any(|c| c.contains("SENTINEL-CONTENT")),
            "a denied tool must never execute"
        );
    }

    /// `resolve_hitl` derives approval explicitly rather than testing `== Deny`, so this
    /// pins that an approval does **not** take the denial path. Together with
    /// `hitl_pauses_on_git_push` (which covers deny) both branches are now exercised.
    #[tokio::test]
    async fn hitl_approve_does_not_take_the_denial_path() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    // `bash` is in the default `hitl_tools`, and since #26 that requires
                    // approval for *every* command, so a benign one is enough to reach
                    // the gate. No need to shell out to git.
                    arguments: json!({"command": "echo ok"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl(HitlDecision::Approve, "test").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
        assert!(s.pending_hitl().is_none());

        let tool_contents: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            !tool_contents.iter().any(|c| c.contains("HITL denied")),
            "approval must not be routed through the denial path, got {tool_contents:?}"
        );
        assert!(
            !tool_contents.is_empty(),
            "approval should reach execution and record a tool result"
        );
    }

    #[tokio::test]
    async fn offload_large_tool_output() {
        let dir = tempdir().unwrap();
        // Offloading now routes through the runtime-storage resolver, which
        // falls back to the platform application-data directory outside a
        // Git repository — git-init keeps this test's writes inside the
        // tempdir instead of touching the real host machine.
        init_repo(dir.path()).await;
        let big = "z".repeat(25_000);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "big.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("read big").await.unwrap();
        let tool_msg = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert!(tool_msg.content.contains("offloaded tool output"));
    }

    #[tokio::test]
    async fn accumulates_prompt_cache_tokens_in_session_usage() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "one".into(),
            tool_calls: vec![],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 3,
                prompt_cache_read_tokens: 7,
                prompt_cache_write_tokens: 2,
            }),
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("hi").await.unwrap();
        assert_eq!(s.token_usage.prompt_cache_hits, 7);
        assert_eq!(s.token_usage.prompt_cache_writes, 2);
    }

    #[tokio::test]
    async fn accumulates_api_token_usage_for_cost() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "one".into(),
            tool_calls: vec![],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 3,
                ..Default::default()
            }),
            thinking: Some("hmm".into()),
        }]));
        // Need two responses if we call twice — first call only.
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("hi").await.unwrap();
        assert_eq!(s.token_usage.prompt_tokens, 10);
        assert_eq!(s.token_usage.completion_tokens, 3);
        assert_eq!(s.token_usage.model_steps, 1);
        assert_eq!(s.token_usage.model_calls_with_usage, 1);
        assert!(s.token_usage.thinking_tokens_est >= 1);
        let lines = s.token_usage_lines();
        assert!(lines.iter().any(|l| l.contains("prompt/input")));
        assert!(lines.iter().any(|l| l.contains("completion/output")));
        assert!(lines.iter().any(|l| l.contains("In-context estimate")));
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("$0") || l.contains("USD") || l.contains("price")),
            "should not report dollar cost: {lines:?}"
        );
        let report = s.token_usage_report();
        assert!(report.user_tokens_est >= 1);
        assert!(report.system_tokens_est >= 1);
    }

    #[tokio::test]
    async fn mark_interrupted_if_stale_converts_running() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hi".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Ready);
        s.append_user_message("hi").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Working);
        s.mark_interrupted_if_stale().await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Interrupted);
        // Idempotent on terminal interrupted.
        s.mark_interrupted_if_stale().await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Interrupted);
    }

    #[tokio::test]
    async fn mark_cancelled_persists_terminal_state() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hi".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.append_user_message("hi").await.unwrap();
        s.mark_cancelled().await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
        let s2 = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();
        assert_eq!(s2.active_task.lifecycle, TaskLifecycle::Cancelled);
    }

    #[tokio::test]
    async fn resume_running_session_becomes_interrupted() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "partial".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        // Fresh session journal is Running with no completion event.
        let resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();
        assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Interrupted);
    }

    /// A persisted `Waiting` (HITL) status is a legitimately recoverable
    /// state, not a stale crash — it must restore as `Waiting` with its
    /// `WaitReason::Approval` correlation intact, so the operator's pending
    /// approval can still be resolved after a restart.
    #[tokio::test]
    async fn resume_restores_a_valid_waiting_session_and_can_resolve_it() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "echo hi"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);
        let request_id = s.pending_hitl().unwrap().call_id.clone();

        let mut resumed = AgentSession::resume(
            base_cfg(dir.path()),
            Arc::new(MockModelClient::script(vec![ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            }])),
            ToolRegistry::new(),
            s.session_id,
        )
        .await
        .unwrap();

        assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Waiting);
        assert!(resumed.active_task.is_active_wait(&request_id));
        assert!(resumed.pending_hitl().is_some());

        // The restored wait can actually be resolved — correlation survived.
        resumed
            .resolve_hitl(HitlDecision::Approve, "test")
            .await
            .unwrap();
        assert_eq!(resumed.active_task.lifecycle, TaskLifecycle::Working);
    }

    /// A session with no scripted model turns; enough to exercise state helpers
    /// that never reach the provider.
    async fn idle_session(dir: &std::path::Path) -> AgentSession {
        AgentSession::create(
            base_cfg(dir),
            Arc::new(MockModelClient::script(vec![])),
            ToolRegistry::new(),
        )
        .await
        .unwrap()
    }

    fn assistant_with_tool_call(name: &str) -> Message {
        let mut m = Message::new(MessageRole::Assistant, "calling");
        m.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: json!({}),
        }];
        m
    }

    #[test]
    fn strip_protocol_markers_removes_confidence_annotations() {
        assert_eq!(strip_protocol_markers("done \\confidence{0.9}"), "done");
        assert_eq!(
            strip_protocol_markers("a \\confidence{0.1}b \\confidence{0.2}c"),
            "a b c"
        );
        assert_eq!(strip_protocol_markers("\\confidence{0.5}only"), "only");
        assert_eq!(strip_protocol_markers("  plain  "), "plain");
    }

    /// Regression: an unterminated marker used to duplicate the text before it,
    /// because `rest` was not rewound before breaking out of the scan.
    #[test]
    fn unterminated_confidence_marker_is_kept_verbatim_once() {
        assert_eq!(
            strip_protocol_markers("keep \\confidence{oops"),
            "keep \\confidence{oops"
        );
        assert_eq!(
            strip_protocol_markers("a \\confidence{0.1}b \\confidence{trunc"),
            "a b \\confidence{trunc"
        );
    }

    #[tokio::test]
    async fn current_turn_has_tool_activity_stops_at_the_user_boundary() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        // No messages at all: nothing to scan.
        s.messages.clear();
        assert!(!s.current_turn_has_tool_activity());

        // Only plain assistant text since the last user message.
        s.messages = vec![
            Message::new(MessageRole::User, "hi"),
            Message::new(MessageRole::Assistant, "hello"),
        ];
        assert!(!s.current_turn_has_tool_activity());

        // An assistant turn carrying tool calls counts as activity.
        s.messages = vec![
            Message::new(MessageRole::User, "hi"),
            assistant_with_tool_call("read_file"),
        ];
        assert!(s.current_turn_has_tool_activity());

        // Tool activity from a *previous* turn must not leak into this one:
        // the scan walks backwards and stops at the newer user message.
        s.messages = vec![
            Message::new(MessageRole::User, "first"),
            assistant_with_tool_call("read_file"),
            Message::new(MessageRole::Tool, "contents"),
            Message::new(MessageRole::User, "second"),
            Message::new(MessageRole::Assistant, "plain reply"),
        ];
        assert!(!s.current_turn_has_tool_activity());
    }

    #[tokio::test]
    async fn finalize_turn_failure_keeps_the_first_summary() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.append_user_message("do something").await.unwrap();

        s.finalize_turn_failure("first failure", "cat_a")
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        let after_first = s.messages.len();

        // Idempotent: a second failure must not append another marker message
        // or overwrite the original summary.
        s.finalize_turn_failure("second failure", "cat_b")
            .await
            .unwrap();
        assert_eq!(s.messages.len(), after_first);
        let markers: Vec<&str> = s
            .messages
            .iter()
            .filter(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(markers.len(), 1);
        assert!(markers[0].contains("first failure"));
        assert!(!markers[0].contains("second failure"));
    }

    #[tokio::test]
    async fn fail_max_turns_records_a_step_limit_failure() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.append_user_message("do something").await.unwrap();

        s.fail_max_turns().await.unwrap();

        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        let marker = s
            .messages
            .iter()
            .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .expect("a turn_failed marker should be recorded");
        assert!(marker.content.contains("step limit"));
        assert!(s
            .events
            .iter()
            .any(|e| e.kind == "turn_failed" && e.detail.starts_with("max_turns:")));
    }

    #[tokio::test]
    async fn prepare_model_step_resets_context_when_over_threshold() {
        let dir = tempdir().unwrap();
        // The context-reset handoff writes a progress checkpoint through the
        // runtime-storage resolver, which falls back to the platform
        // application-data directory outside a Git repository — git-init
        // keeps this test's writes inside the tempdir.
        init_repo(dir.path()).await;
        let mut s = idle_session(dir.path()).await;

        // Shrink the window so the messages below cross the reset ratio.
        s.context.config.capacity_tokens = 32;
        s.messages = vec![
            Message::new(MessageRole::User, "x".repeat(400)),
            Message::new(MessageRole::Assistant, "y".repeat(400)),
        ];
        assert!(s.context.should_reset(&s.messages));

        let request = s.prepare_model_step(3).await.unwrap();

        assert!(s
            .events
            .iter()
            .any(|e| e.kind == "context_reset" && e.detail == "threshold"));
        // The handoff replaces the whole conversation with a two-message
        // restart: a system prompt carrying the progress document, then a
        // user turn telling the model to continue.
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, MessageRole::System);
        assert!(s.messages[0].content.contains("# Context Handoff"));
        assert_eq!(s.messages[1].role, MessageRole::User);
        assert!(s.messages[1].content.starts_with("Continue the task."));
        // The padded turns are gone as standalone messages.
        assert!(!s
            .messages
            .iter()
            .any(|m| m.content == "x".repeat(400) || m.content == "y".repeat(400)));
        assert!(!request.messages.is_empty());
    }

    #[tokio::test]
    async fn prepare_model_step_leaves_a_small_context_alone() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.messages = vec![Message::new(MessageRole::User, "short")];

        s.prepare_model_step(1).await.unwrap();

        assert!(!s.events.iter().any(|e| e.kind == "context_reset"));
        assert_eq!(s.messages.len(), 1);
    }

    #[tokio::test]
    async fn context_reset_ratio_and_journal_cursor_are_exposed() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        s.context.config.reset_usage_ratio = 0.75;
        assert!((s.context_reset_ratio() - 0.75).abs() < f64::EPSILON);

        let before = s.journal_cursor().await.unwrap();
        s.append_user_message("advance the journal").await.unwrap();
        let after = s.journal_cursor().await.unwrap();
        assert!(
            after > before,
            "cursor should advance after a journalled append ({before} -> {after})"
        );
    }

    #[tokio::test]
    async fn token_usage_report_buckets_tool_messages_separately() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;

        let mut thinking_turn = Message::new(MessageRole::Assistant, "answer");
        thinking_turn.thinking = Some("pondering at some length".into());

        s.messages = vec![
            Message::new(MessageRole::System, "system preamble"),
            Message::new(MessageRole::User, "a question"),
            thinking_turn,
            Message::new(MessageRole::Tool, "tool output one"),
            Message::new(MessageRole::Tool, "tool output two"),
        ];

        let report = s.token_usage_report();

        assert_eq!(report.tool_message_count, 2);
        assert!(report.tool_tokens_est > 0);
        assert!(report.system_tokens_est > 0);
        assert!(report.user_tokens_est > 0);
        assert!(report.assistant_tokens_est > 0);
        assert!(
            report.thinking_in_context_est > 0,
            "assistant thinking should be counted in the context estimate"
        );
    }

    #[tokio::test]
    async fn exec_only_tool_failure_is_journalled_without_a_tool_message() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        let before = s.messages.len();
        let mut budget = ValidationBudget::default();

        let call = ToolCall {
            id: "c1".into(),
            name: "no_such_tool".into(),
            arguments: json!({}),
        };
        // A failing call must not surface as tool output in the conversation,
        // but it still has to be journalled for replay.
        s.run_one_tool_exec_only(&call, &mut budget).await.unwrap();

        assert_eq!(s.messages.len(), before);
        assert!(s.journal_cursor().await.unwrap() > 0);
    }

    #[tokio::test]
    async fn update_plan_emits_plan_update_event_and_ack_message() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        let mut budget = ValidationBudget::default();
        let call = ToolCall {
            id: "plan-1".into(),
            name: "update_plan".into(),
            arguments: json!({
                "explanation": "kickoff",
                "plan": [
                    {"step": "scout", "status": "in_progress"},
                    {"step": "ship", "status": "pending"}
                ]
            }),
        };
        s.run_one_tool_exec_only(&call, &mut budget).await.unwrap();

        assert!(
            s.events
                .iter()
                .any(|e| e.kind == "plan_update" && e.detail.contains("scout")),
            "expected plan_update event, got {:?}",
            s.events
        );
        let tool_msg = s
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Tool)
            .expect("tool message");
        assert_eq!(tool_msg.content, "Plan updated");
        assert_eq!(tool_msg.name.as_deref(), Some("update_plan"));
    }

    // --- Verified Task Completion: integration tests --------------------

    /// Governance/HITL gating is orthogonal to completion verification —
    /// these tests disable it so a tool call executes directly and the
    /// evaluator's evidence-based decision is what's under test.
    fn no_gov_cfg(dir: &std::path::Path) -> LoopConfig {
        LoopConfig {
            enable_governance: false,
            ..base_cfg(dir)
        }
    }

    fn script(responses: Vec<ModelResponse>) -> Arc<MockModelClient> {
        Arc::new(MockModelClient::script(responses))
    }

    fn text_only(text: &str) -> ModelResponse {
        ModelResponse {
            text: text.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    fn tool_call_response(calls: Vec<ToolCall>) -> ModelResponse {
        ModelResponse {
            text: "".into(),
            tool_calls: calls,
            usage: None,
            thinking: None,
        }
    }

    async fn git(dir: &std::path::Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    async fn init_repo(dir: &std::path::Path) {
        git(dir, &["init", "-q"]).await;
        git(dir, &["config", "user.email", "forge@example.com"]).await;
        git(dir, &["config", "user.name", "Forge Test"]).await;
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]).await;
        git(dir, &["commit", "-q", "-m", "init"]).await;
    }

    // Model claims a write succeeded ("Done") but the tool never actually
    // performed one — a turn with a file-edit expectation but no matching
    // verified evidence must fail, never trust the narration.
    #[tokio::test]
    async fn model_claims_success_without_a_verified_edit_fails() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "apply_patch".into(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch"
                }),
            }]),
            text_only("Done — file removed."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("delete missing.txt").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert!(s
            .messages
            .iter()
            .any(|m| m.content.starts_with(TURN_FAILED_MARKER)));
    }

    #[tokio::test]
    async fn write_file_success_completes() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "new.txt", "content": "hello\n"}),
            }]),
            text_only("Created new.txt."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("create new.txt").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::EditVerified
        );
    }

    #[tokio::test]
    async fn bash_nonzero_exit_fails_with_exit_code_in_message() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "exit 7"}),
            }]),
            text_only("Ran the command."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        let failure = s
            .messages
            .iter()
            .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .unwrap();
        assert!(
            failure.content.contains("exited with code 7"),
            "{}",
            failure.content
        );
        let tool_message = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .expect("the bash tool result should be recorded");
        assert_eq!(
            tool_message.outcome,
            ExecutionOutcome::Failed { exit_code: Some(7) }
        );
    }

    #[tokio::test]
    async fn bash_exit_zero_completes_and_reports_success() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "exit 0"}),
            }]),
            text_only("Ran the command."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        let tool_message = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .expect("the bash tool result should be recorded");
        assert_eq!(tool_message.outcome, ExecutionOutcome::Success);
    }

    #[tokio::test]
    async fn bash_exit_127_reports_spawn_failed_command_not_found() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "definitely_not_a_real_command_xyz; exit 127"}),
            }]),
            text_only("Ran the command."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("run it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        let tool_message = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .expect("the bash tool result should be recorded");
        assert_eq!(
            tool_message.outcome,
            ExecutionOutcome::SpawnFailed {
                reason: "command not found".into()
            }
        );
    }

    #[tokio::test]
    async fn hitl_denial_message_carries_denied_outcome() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("push").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Waiting);

        s.resolve_hitl_with_feedback(HitlDecision::Deny, "test", None)
            .await
            .unwrap();

        let tool_message = s
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Tool)
            .expect("a tool result should record the denial");
        assert!(matches!(
            tool_message.outcome,
            ExecutionOutcome::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn acl_denial_message_carries_denied_outcome() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "SENTINEL-CONTENT").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "secret.txt"}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            },
        ]));
        let mut s = AgentSession::create(base_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        let mut acl = AclPolicy::allow_all();
        acl.deny("read_file".into());
        s.set_governance(Governance::default().with_acl(acl));
        s.run_user_message("read it").await.unwrap();

        let tool_message = s
            .messages
            .iter()
            .find(|m| m.role == MessageRole::Tool)
            .expect("a tool result should record the denial");
        assert!(matches!(
            tool_message.outcome,
            ExecutionOutcome::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn write_then_failing_validation_in_same_turn_never_completes() {
        // `classify_turn` picks exactly one `TaskExpectation` category per
        // turn by precedence (git > file-edit > tool-execution > search >
        // read-only). A turn that both writes a file (succeeds) and runs a
        // failing validation command classifies as `FileEdit` only, so
        // without the cross-category evidence gate in `apply_model_response`
        // the failing bash evidence would never be consulted and this turn
        // would incorrectly read `Completed`.
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![
                ToolCall {
                    id: "1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path": "ok.txt", "content": "fine\n"}),
                },
                ToolCall {
                    id: "2".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "exit 1"}),
                },
            ]),
            text_only("Wrote the file and ran the tests."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("write and validate").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::PartialFailure
        );
        // The write still happened on disk — the gate fails the turn without
        // pretending the edit didn't occur.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
            "fine\n"
        );
    }

    #[tokio::test]
    async fn search_zero_matches_completes() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "ffgrep".into(),
                arguments: json!({"pattern": "definitely_not_present_anywhere"}),
            }]),
            text_only("No matches found."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("search for it").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().evidence_summary.detail,
            "Search completed with 0 matches."
        );
    }

    #[tokio::test]
    async fn two_edits_one_fails_is_partial_failure_not_completed() {
        let dir = tempdir().unwrap();
        let model = script(vec![
            tool_call_response(vec![
                ToolCall {
                    id: "1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path": "ok.txt", "content": "fine\n"}),
                },
                ToolCall {
                    id: "2".into(),
                    name: "apply_patch".into(),
                    arguments: json!({
                        "patch": "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch"
                    }),
                },
            ]),
            text_only("Updated both files."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("update both").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::PartialFailure
        );
        // The successful half of the turn still happened on disk — the
        // evaluator fails the turn without pretending the edit didn't occur.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
            "fine\n"
        );
    }

    #[tokio::test]
    async fn read_only_turn_completes_without_tool_calls() {
        let dir = tempdir().unwrap();
        let model = script(vec![text_only("Forge is a Rust workspace.")]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("what is this repo?").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::NoChangesRequired
        );
    }

    // Regression test for the false-completion bug found in the 2026-08-01
    // usability audit: a small/local model that doesn't reliably use the
    // structured tool-calling wire format instead dumps a JSON-ish blob
    // naming a real tool as plain assistant text. `last.tool_calls` is empty
    // (the model never actually invoked anything), so before this fix it fell
    // through to `TaskExpectation::ReadOnly`, which completes on any
    // non-empty text — reporting success while `greeter.py` was never
    // touched. It must now fail instead.
    #[tokio::test]
    async fn dangling_tool_call_text_does_not_complete() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("greeter.py"),
            "def greet(name):\n    return f\"Hello, {name}!\"\n",
        )
        .unwrap();
        let model = script(vec![text_only(
            "```json\n{\"write_file\", {\"path\": \"greeter.py\", \"content\": \"class Greeter:\\n\\tdef greet(self, name):\\n\\t\\treturn f'Hi there, {name}!'\"}}\n```",
        )]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("change the greeting in greeter.py")
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        // This path bypasses the evidence-based evaluator entirely (same as
        // the sibling `no_final_answer` branch just above it in
        // `apply_model_response`), so `last_completion` stays `None` — the
        // real signal is the terminal lifecycle plus the journalled event
        // category, checked below.
        assert!(s.events.iter().any(|e| e.kind == "turn_failed"
            && e.detail
                .starts_with(CompletionReason::DanglingToolCallText.as_category())));
        // The file must be provably untouched — no silent partial write.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("greeter.py")).unwrap(),
            "def greet(name):\n    return f\"Hello, {name}!\"\n"
        );
        let failure = s
            .messages
            .iter()
            .find(|m| m.content.starts_with(TURN_FAILED_MARKER))
            .unwrap();
        assert!(
            failure.content.contains("didn't format the call correctly"),
            "{}",
            failure.content
        );
    }

    // A legitimate answer that merely *mentions* a tool by name in prose
    // (no call-shaped quote+punctuation adjacency) must still complete
    // normally — the detection heuristic must not be trigger-happy.
    #[tokio::test]
    async fn prose_mentioning_a_tool_name_still_completes() {
        let dir = tempdir().unwrap();
        let model = script(vec![text_only(
            "You can ask me to use \"write_file\" to create that for you.",
        )]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("how do I create a file?").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::NoChangesRequired
        );
    }

    #[tokio::test]
    async fn git_add_with_no_changes_fails_effect_not_observed() {
        let dir = tempdir().unwrap();
        init_repo(dir.path()).await;
        let model = script(vec![
            tool_call_response(vec![ToolCall {
                id: "1".into(),
                name: "git".into(),
                arguments: json!({"subcommand": "add", "args": ["a.txt"]}),
            }]),
            text_only("Staged a.txt."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("stage a.txt").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::GitEffectNotObserved
        );
    }

    #[tokio::test]
    async fn git_add_then_commit_completes_with_verified_effect() {
        let dir = tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let model = script(vec![
            tool_call_response(vec![
                ToolCall {
                    id: "1".into(),
                    name: "git".into(),
                    arguments: json!({"subcommand": "add", "args": ["a.txt"]}),
                },
                ToolCall {
                    id: "2".into(),
                    name: "git".into(),
                    arguments: json!({"subcommand": "commit", "args": ["-m", "update a.txt"]}),
                },
            ]),
            text_only("Committed the change."),
        ]);
        let mut s = AgentSession::create(no_gov_cfg(dir.path()), model, ToolRegistry::new())
            .await
            .unwrap();
        s.run_user_message("commit the change").await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Completed);
        assert_eq!(
            s.last_completion.as_ref().unwrap().reason,
            CompletionReason::GitEffectVerified
        );
    }

    // A failed turn later receiving narration claiming success must never
    // flip to Completed — terminal states are not overwritten by later
    // model text, even via a direct (non-`run_agent_turns`) re-entry.
    #[tokio::test]
    async fn failed_turn_is_not_overwritten_by_later_success_narration() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.append_user_message("run the tests").await.unwrap();
        s.finalize_turn_failure("cargo test exited with code 101.", "tool_exited_nonzero")
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);

        let outcome = s
            .apply_model_response(text_only("Actually, all tests passed now!"))
            .await
            .unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Failed);
        assert!(matches!(outcome, ApplyOutcome::Done(_)));
    }

    #[tokio::test]
    async fn cancellation_yields_interrupted_and_never_completes() {
        let dir = tempdir().unwrap();
        let mut s = idle_session(dir.path()).await;
        s.append_user_message("do something").await.unwrap();
        s.mark_cancelled().await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);

        // A later model step must not resurrect the turn into Completed.
        let outcome = s.apply_model_response(text_only("Done!")).await.unwrap();
        assert_eq!(s.active_task.lifecycle, TaskLifecycle::Cancelled);
        assert!(matches!(outcome, ApplyOutcome::Done(_)));
    }
}
