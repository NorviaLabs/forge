//! The Claude Agent wire: Claude models through the user's own Claude Code.
//!
//! Each Forge session gets one long-running `claude` process. Claude Code's
//! built-in tools are off; Forge's tools are offered through an in-process
//! MCP server, so every tool call runs through Forge's own permission and
//! execution path. When Claude calls them, the step ends and the response
//! carries the calls; Claude Code waits while Forge runs them, and the
//! session's next request releases the results. Forge's transcript stays
//! the source of truth: whatever Claude has not seen (another model's
//! replies, an edited history) is sent as a `<conversation-checkpoint>`.
//!
//! Side calls (compaction, goal checks) run in a throwaway process so the
//! session's Claude conversation is untouched.

mod convert;
mod process;
mod stream;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use forge_types::{ImageRef, Message, MessageRole, ModelResponse};
use serde_json::Value;
use tokio::sync::{Mutex, OwnedMutexGuard};

use self::convert::ToolNames;
use self::process::{ClaudeProcess, Launch, SessionMode};
use self::stream::{Step, StepEnd};
use super::{NativeModelClient, Target};
use crate::{ModelError, ModelRequest, StreamEventTx};

/// A process with no turn waiting on Forge stops after this long.
const IDLE: Duration = Duration::from_secs(5 * 60);
/// A process waiting on Forge's tools (subagents, permission prompts,
/// questions) stops after this long.
const PARKED_IDLE: Duration = Duration::from_secs(60 * 60);

type SpawnFuture = Pin<Box<dyn Future<Output = Result<ClaudeProcess, ModelError>> + Send>>;
/// Starts a process; tests substitute a fake `claude`.
type Spawner = Arc<dyn Fn(Launch, SessionMode) -> SpawnFuture + Send + Sync>;

/// The Claude Agent sessions one model client runs.
#[derive(Default)]
pub(crate) struct ClaudeAgentSessions {
    entries: StdMutex<HashMap<String, Arc<Mutex<Entry>>>>,
    spawner: Option<Spawner>,
}

impl ClaudeAgentSessions {
    async fn spawn(
        &self,
        launch: &Launch,
        session: &SessionMode,
    ) -> Result<ClaudeProcess, ModelError> {
        match &self.spawner {
            Some(spawner) => spawner(launch.clone(), session.clone()).await,
            None => ClaudeProcess::spawn(launch, session).await,
        }
    }

    fn entry(&self, session_id: &str) -> Arc<Mutex<Entry>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(entries.entry(session_id.to_string()).or_default())
    }
}

/// What Claude has seen of a Forge session.
#[derive(Default)]
struct Entry {
    process: Option<ClaudeProcess>,
    /// The saved Claude conversation a restarted process resumes.
    claude_session: Option<String>,
    seen: Option<Seen>,
    /// Tool calls Claude is waiting on.
    waiting: Vec<String>,
    /// Bumped by every request; an idle timer only fires if it is unchanged.
    generation: u64,
}

/// The prefix of Forge's conversation Claude has.
#[derive(Clone, Copy)]
struct Seen {
    len: usize,
    hash: [u8; 32],
    /// What follows the prefix in Forge's transcript.
    reply: Reply,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reply {
    /// Forge stored Claude's complete reply right after the prefix.
    Complete,
    /// The turn was interrupted; Forge may have stored a partial reply.
    Partial,
}

impl Seen {
    /// Where Claude's knowledge of `conversation` ends, or `None` when
    /// Forge's history no longer starts with what Claude saw.
    fn end_in(&self, conversation: &[Message]) -> Option<usize> {
        if conversation.len() < self.len
            || convert::fingerprint(&conversation[..self.len]) != self.hash
        {
            return None;
        }
        let next = conversation.get(self.len);
        let skip_reply = match self.reply {
            Reply::Complete => next.is_some_and(|message| message.role == MessageRole::Assistant),
            Reply::Partial => next.is_some_and(|message| {
                message.role == MessageRole::Assistant && message.tool_calls.is_empty()
            }),
        };
        if self.reply == Reply::Complete && !skip_reply {
            return None;
        }
        Some(self.len + usize::from(skip_reply))
    }
}

impl Entry {
    async fn stop_process(&mut self) {
        self.waiting.clear();
        if let Some(process) = self.process.take() {
            process.stop().await;
        }
    }

    /// Forget the Claude conversation; the next turn starts a new one.
    async fn reset(&mut self) {
        self.stop_process().await;
        self.claude_session = None;
        self.seen = None;
    }
}

/// What the next step does.
enum Plan {
    /// Release the results Claude's parked turn waits for.
    Continue(Vec<(String, Value)>),
    /// Start a Claude turn with this user message.
    Turn(Vec<Value>),
}

pub(super) async fn complete(
    client: &NativeModelClient,
    req: ModelRequest,
    target: Target,
    tx: Option<StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    let load = |image: &ImageRef| {
        client
            .load_image_ref(&req.workspace_root, image)
            .ok()
            .map(|loaded| (loaded.mime.clone(), loaded.base64().to_string()))
    };
    let launch = Launch {
        cwd: req.workspace_root.clone(),
        model: target.bare_model.clone(),
        effort: effort(&req),
        system: convert::system_prompt(&req.messages),
        tools: if req.side_call {
            Vec::new()
        } else {
            convert::tool_definitions(&req.tools)
        },
    };
    let names = if req.side_call {
        ToolNames::default()
    } else {
        ToolNames::new(&req.tools)
    };
    match req.session_id.as_deref() {
        Some(session_id) if !req.side_call => {
            let entry = client.claude_sessions.entry(session_id);
            let guard = Arc::clone(&entry).lock_owned().await;
            let mut turn = Turn {
                entry,
                guard: Some(guard),
                armed: false,
                conversation_len: 0,
                hash: [0; 32],
            };
            turn.run(
                &client.claude_sessions,
                &req,
                &launch,
                &names,
                &load,
                tx.as_ref(),
            )
            .await
        }
        _ => {
            side_call(
                &client.claude_sessions,
                &req,
                &launch,
                &names,
                &load,
                tx.as_ref(),
            )
            .await
        }
    }
}

/// Forge's effort as a Claude Code `--effort` level.
fn effort(req: &ModelRequest) -> Option<String> {
    use forge_connect::effort::EffortLevel;
    let level = EffortLevel::parse(req.reasoning_effort.as_deref()?)?;
    Some(
        match level {
            EffortLevel::Minimal | EffortLevel::Low => "low",
            EffortLevel::Medium => "medium",
            EffortLevel::High => "high",
            EffortLevel::XHigh => "xhigh",
            EffortLevel::Max => "max",
        }
        .to_string(),
    )
}

/// One step of a session, holding the session's lock. If the request is
/// dropped mid-turn (the user stopped it), `Drop` interrupts Claude.
struct Turn {
    entry: Arc<Mutex<Entry>>,
    guard: Option<OwnedMutexGuard<Entry>>,
    /// A Claude turn was started or continued and has not finished.
    armed: bool,
    conversation_len: usize,
    hash: [u8; 32],
}

impl Turn {
    fn state(&mut self) -> &mut Entry {
        self.guard.as_mut().expect("held until drop")
    }

    async fn run(
        &mut self,
        sessions: &ClaudeAgentSessions,
        req: &ModelRequest,
        launch: &Launch,
        names: &ToolNames,
        load: convert::ImageLoader<'_>,
        tx: Option<&StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let conversation = convert::conversation(&req.messages);
        self.conversation_len = conversation.len();
        self.hash = convert::fingerprint(conversation);
        self.state().generation += 1;

        let plan = self.plan(conversation, load).await;
        let plan = self
            .prepare(sessions, launch, plan, conversation, load)
            .await?;

        let state = self.state();
        let process = state.process.as_mut().expect("prepared");
        let continuing = matches!(plan, Plan::Continue(_));
        match plan {
            Plan::Continue(results) => {
                for (id, result) in results {
                    process.shared.tools.resolve(&id, result);
                }
            }
            Plan::Turn(content) => {
                if process.running {
                    process.interrupt().await;
                }
                process.send_user(content).await?;
            }
        }
        state.waiting.clear();
        self.armed = true;

        let state = self.state();
        let process = state.process.as_mut().expect("prepared");
        let result = read_step(process, names, tx, !launch.tools.is_empty() && !continuing).await;
        self.armed = false;
        let (len, hash) = (self.conversation_len, self.hash);
        let state = self.state();
        match result {
            Ok((response, end)) => {
                state.waiting = match end {
                    StepEnd::Tools => response
                        .tool_calls
                        .iter()
                        .map(|call| call.id.clone())
                        .collect(),
                    StepEnd::Done => Vec::new(),
                };
                state.seen = Some(Seen {
                    len,
                    hash,
                    reply: Reply::Complete,
                });
                self.arm_idle();
                Ok(response)
            }
            Err(error) => {
                match state.process.as_mut() {
                    Some(process) if !process.ended() => process.interrupt().await,
                    _ => {
                        state.stop_process().await;
                    }
                }
                state.waiting.clear();
                state.seen = Some(Seen {
                    len,
                    hash,
                    reply: Reply::Partial,
                });
                self.arm_idle();
                Err(error)
            }
        }
    }

    /// Decide how to bring Claude up to date with `conversation`.
    async fn plan(&mut self, conversation: &[Message], load: convert::ImageLoader<'_>) -> Plan {
        let state = self.state();
        let Some(end) = state.seen.and_then(|seen| seen.end_in(conversation)) else {
            // History Claude has no matching copy of (first turn, edit,
            // undo, compaction): a new Claude conversation with all of it.
            state.reset().await;
            return Plan::Turn(convert::catch_up(conversation, load));
        };
        let parked = !state.waiting.is_empty()
            && state
                .process
                .as_ref()
                .is_some_and(|process| process.running && !process.ended());
        if parked {
            if let Some(results) = parked_results(&state.waiting, &conversation[end..], load) {
                if results.after == conversation.len() - end && !results.cancelled {
                    return Plan::Continue(results.values);
                }
            }
            // The results are missing, the user stopped a tool, or a new
            // message arrived: end Claude's turn and start a new one.
            if let Some(process) = state.process.as_mut() {
                process.interrupt().await;
            }
        }
        if end == conversation.len() {
            // Nothing new (a retry of a failed step): start over.
            state.reset().await;
            return Plan::Turn(convert::catch_up(conversation, load));
        }
        Plan::Turn(convert::catch_up(&conversation[end..], load))
    }

    /// Make sure a compatible process runs.
    async fn prepare(
        &mut self,
        sessions: &ClaudeAgentSessions,
        launch: &Launch,
        plan: Plan,
        conversation: &[Message],
        load: convert::ImageLoader<'_>,
    ) -> Result<Plan, ModelError> {
        let state = self.state();
        let mut plan = plan;
        let stale = state
            .process
            .as_ref()
            .is_some_and(|process| process.ended() || process.launch_key != launch.key());
        if stale {
            state.stop_process().await;
            if matches!(plan, Plan::Continue(_)) {
                // A new process cannot continue the old one's turn; resume
                // the conversation and send what Claude has not seen.
                let end = state
                    .seen
                    .and_then(|seen| seen.end_in(conversation))
                    .unwrap_or(0);
                plan = Plan::Turn(convert::catch_up(&conversation[end..], load));
            }
        }
        if state.process.is_none() {
            let session = match &state.claude_session {
                Some(id) => SessionMode::Resume(id.clone()),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    state.claude_session = Some(id.clone());
                    SessionMode::New(id)
                }
            };
            match sessions.spawn(launch, &session).await {
                Ok(process) => state.process = Some(process),
                Err(_) if matches!(session, SessionMode::Resume(_)) => {
                    // The saved conversation is gone: start a new one with
                    // the whole history.
                    state.reset().await;
                    let id = uuid::Uuid::new_v4().to_string();
                    state.claude_session = Some(id.clone());
                    state.process = Some(sessions.spawn(launch, &SessionMode::New(id)).await?);
                    plan = Plan::Turn(convert::catch_up(conversation, load));
                }
                Err(error) => {
                    state.claude_session = None;
                    return Err(error);
                }
            }
        }
        let process = state.process.as_mut().expect("started");
        if process.model != launch.model {
            process.set_model(&launch.model).await?;
        }
        Ok(plan)
    }

    /// Stop the process once the session has been idle long enough.
    fn arm_idle(&mut self) {
        let state = self.state();
        let generation = state.generation;
        let delay = if state.waiting.is_empty() {
            IDLE
        } else {
            PARKED_IDLE
        };
        let entry = Arc::clone(&self.entry);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(delay).await;
                let mut state = entry.lock().await;
                if state.generation == generation {
                    state.stop_process().await;
                }
            });
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The request was dropped mid-turn: stop Claude's turn and keep
        // what it saw, since its transcript has the input and any partial
        // reply.
        let (len, hash) = (self.conversation_len, self.hash);
        let state = self.state();
        if let Some(process) = state.process.as_mut() {
            process.interrupt_detached();
        }
        state.waiting.clear();
        state.seen = Some(Seen {
            len,
            hash,
            reply: Reply::Partial,
        });
        self.arm_idle();
    }
}

/// The parked calls' results, from the tool messages right after Claude's
/// knowledge ends.
struct ParkedResults {
    values: Vec<(String, Value)>,
    /// Messages consumed after `end`.
    after: usize,
    cancelled: bool,
}

fn parked_results(
    waiting: &[String],
    unseen: &[Message],
    load: convert::ImageLoader<'_>,
) -> Option<ParkedResults> {
    let tools: Vec<&Message> = unseen
        .iter()
        .take_while(|message| message.role == MessageRole::Tool)
        .collect();
    let mut values = Vec::new();
    let mut cancelled = false;
    for id in waiting {
        let message = tools
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some(id.as_str()))?;
        cancelled |= convert::was_cancelled(message);
        values.push((id.clone(), convert::tool_result(message, load)));
    }
    Some(ParkedResults {
        values,
        after: tools.len(),
        cancelled,
    })
}

/// Read until the step ends, skipping what interrupted turns left behind.
async fn read_step(
    process: &mut ClaudeProcess,
    names: &ToolNames,
    tx: Option<&StreamEventTx>,
    expect_tools: bool,
) -> Result<(ModelResponse, StepEnd), ModelError> {
    let mut step = Step::new(names, tx, expect_tools);
    loop {
        let Some(message) = process.next_event().await else {
            process.running = false;
            return Err(process.exit_error());
        };
        let is_result = message.get("type").and_then(Value::as_str) == Some("result");
        if process.stale_results > 0 {
            if is_result {
                process.stale_results -= 1;
            }
            continue;
        }
        if is_result {
            process.running = false;
        }
        if let Some(end) = step.consume(&message)? {
            return Ok((step.finish(), end));
        }
    }
}

/// A one-off call in a throwaway process with no tools.
async fn side_call(
    sessions: &ClaudeAgentSessions,
    req: &ModelRequest,
    launch: &Launch,
    names: &ToolNames,
    load: convert::ImageLoader<'_>,
    tx: Option<&StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    let mut process = sessions.spawn(launch, &SessionMode::Ephemeral).await?;
    // Every message after the leading system prompt, including a trailing
    // system instruction, is the request.
    let content = convert::catch_up(convert::conversation(&req.messages), load);
    let result = match process.send_user(content).await {
        Ok(()) => loop {
            match read_step(&mut process, names, tx, false).await {
                Ok((response, StepEnd::Done)) => break Ok(response),
                // No tools are offered; keep reading to the result.
                Ok((_, StepEnd::Tools)) => continue,
                Err(error) => break Err(error),
            }
        },
        Err(error) => Err(error),
    };
    process.stop().await;
    result
}

#[cfg(test)]
mod tests;
