//! A frontend-independent driver for an [`AgentSession`].
//!
//! The session itself is not `Sync` and every mutation is `&mut self`, so it
//! cannot simply be shared between a UI and a background worker. The runner
//! resolves that the usual way: one task owns the session, and everyone else
//! talks to it over channels — [`SessionCommand`] in, [`SessionEvent`] out.
//!
//! `forge-core` already knows how to drive a turn (`run_agent_turns`); what it
//! does not decide is what should happen when a tool needs human approval and
//! there is no human attached. That is [`ApprovalPolicy`], and it is the one
//! genuinely new decision in this module.

use forge_core::{AgentSession, LoopError};
use forge_types::{HitlDecision, HitlPayload, ModelStreamEvent, Usage};
use tokio::sync::mpsc;

/// What to do when a tool call requires approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalPolicy {
    /// Emit [`SessionEvent::AwaitingApproval`] and wait for a
    /// [`SessionCommand::Resolve`]. The safe default: nothing runs until
    /// somebody says so.
    #[default]
    Ask,
    /// Deny every request without asking. The agent still sees the denial as
    /// a tool result and may adapt, so this is "run, but touch nothing that
    /// needs permission" rather than a hard stop.
    DenyAll,
    /// Approve every request without asking.
    ///
    /// This executes model-authored tool calls — shell commands included —
    /// with no human in the loop. It exists for sandboxes and CI, and callers
    /// should make opting into it deliberate.
    ApproveAll,
}

/// A request into the session task.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Send a user message and drive the agent until it finishes, fails, or
    /// needs approval.
    Prompt(String),
    /// Answer an outstanding [`SessionEvent::AwaitingApproval`], then keep
    /// driving the same turn.
    Resolve(HitlDecision),
    /// Stop after the current turn and close the event stream.
    Shutdown,
}

/// Something that happened inside the session.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A chunk of assistant text.
    TextDelta(String),
    /// A chunk of reasoning text, for models that expose it.
    ThinkingDelta(String),
    /// A tool call the model finished requesting.
    ToolCall { id: String, name: String },
    /// A tool needs approval and the policy is [`ApprovalPolicy::Ask`].
    /// The runner is now idle until a [`SessionCommand::Resolve`] arrives.
    AwaitingApproval(Box<HitlPayload>),
    /// The turn finished.
    TurnComplete {
        text: String,
        usage: Option<Box<Usage>>,
    },
    /// The turn ended badly. The session stays usable.
    Error(String),
}

/// The caller's end of a running session task.
pub struct SessionHandle {
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
}

impl SessionHandle {
    /// Queue a command. Fails only once the session task is gone.
    pub async fn send(&self, command: SessionCommand) -> Result<(), RunnerGone> {
        self.commands.send(command).await.map_err(|_| RunnerGone)
    }

    /// Await the next event, or `None` once the session task has stopped.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
    }
}

/// The session task ended before the command could be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerGone;

impl std::fmt::Display for RunnerGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session runner has stopped")
    }
}

impl std::error::Error for RunnerGone {}

/// A turn that needs approval, gets it, needs approval again, and so on
/// should still terminate. `forge-core` already stops a denial spiral via
/// its consecutive-denial limit; this is the backstop for the approving
/// case, where nothing else bounds the round count.
const MAX_APPROVAL_ROUNDS: usize = 64;

/// Take ownership of `session` on a background task and return a handle to it.
pub fn spawn(session: AgentSession, policy: ApprovalPolicy) -> SessionHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (event_tx, event_rx) = mpsc::channel(256);

    tokio::spawn(async move {
        Runner {
            session,
            policy,
            events: event_tx,
        }
        .serve(cmd_rx)
        .await;
    });

    SessionHandle {
        commands: cmd_tx,
        events: event_rx,
    }
}

struct Runner {
    session: AgentSession,
    policy: ApprovalPolicy,
    events: mpsc::Sender<SessionEvent>,
}

impl Runner {
    async fn serve(mut self, mut commands: mpsc::Receiver<SessionCommand>) {
        while let Some(command) = commands.recv().await {
            match command {
                SessionCommand::Shutdown => break,
                SessionCommand::Prompt(text) => {
                    if let Err(e) = self.session.append_user_message(&text).await {
                        self.emit(SessionEvent::Error(e.to_string())).await;
                        continue;
                    }
                    self.drive().await;
                }
                SessionCommand::Resolve(decision) => {
                    match self.session.resolve_hitl(decision, "headless").await {
                        Ok(()) => self.drive().await,
                        Err(e) => self.emit(SessionEvent::Error(e.to_string())).await,
                    }
                }
            }
        }
    }

    /// Run turns until the agent is done, fails, or parks on an approval the
    /// policy will not answer.
    async fn drive(&mut self) {
        for _ in 0..MAX_APPROVAL_ROUNDS {
            let response = match self.run_streaming_turns().await {
                Ok(response) => response,
                Err(e) => {
                    self.emit(SessionEvent::Error(e.to_string())).await;
                    return;
                }
            };

            let Some(pending) = self.session.pending_hitl().cloned() else {
                self.emit(SessionEvent::TurnComplete {
                    text: response.text,
                    usage: response.usage.map(Box::new),
                })
                .await;
                return;
            };

            // `resolve_hitl` executes or denies the call but does not itself
            // continue the agent loop, so each branch falls through to another
            // pass rather than returning.
            let decision = match self.policy {
                ApprovalPolicy::Ask => {
                    self.emit(SessionEvent::AwaitingApproval(Box::new(pending)))
                        .await;
                    return;
                }
                ApprovalPolicy::DenyAll => HitlDecision::Deny,
                ApprovalPolicy::ApproveAll => HitlDecision::Approve,
            };
            if let Err(e) = self.session.resolve_hitl(decision, "headless").await {
                self.emit(SessionEvent::Error(e.to_string())).await;
                return;
            }
        }

        self.emit(SessionEvent::Error(format!(
            "stopped after {MAX_APPROVAL_ROUNDS} approval rounds in a single turn"
        )))
        .await;
    }

    /// Drive one `run_agent_turns` call, forwarding token deltas as events.
    ///
    /// The model streams over a `std::sync::mpsc` channel, so the forwarding
    /// side blocks; it gets a blocking task of its own rather than stalling
    /// the runtime. The sender is dropped when `run_agent_turns` returns,
    /// which is what ends the pump.
    async fn run_streaming_turns(&mut self) -> Result<forge_types::ModelResponse, LoopError> {
        let (stream_tx, stream_rx) = std::sync::mpsc::channel::<ModelStreamEvent>();
        let events = self.events.clone();
        let pump = tokio::task::spawn_blocking(move || {
            while let Ok(event) = stream_rx.recv() {
                let mapped = match event {
                    ModelStreamEvent::TextDelta { text } => SessionEvent::TextDelta(text),
                    ModelStreamEvent::ThinkingDelta { text } => SessionEvent::ThinkingDelta(text),
                    ModelStreamEvent::ToolCallEnd { call } => SessionEvent::ToolCall {
                        id: call.id,
                        name: call.name,
                    },
                    ModelStreamEvent::Error { message } => SessionEvent::Error(message),
                    _ => continue,
                };
                if events.blocking_send(mapped).is_err() {
                    break;
                }
            }
        });

        let result = self.session.run_agent_turns(Some(stream_tx)).await;
        let _ = pump.await;
        result
    }

    async fn emit(&self, event: SessionEvent) {
        let _ = self.events.send(event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::{ModelResponse, ToolCall};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::{tempdir, TempDir};

    fn text(body: &str) -> ModelResponse {
        ModelResponse {
            text: body.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    /// `git push` is classified as an exec side effect, so default governance
    /// parks it on approval — the cheapest way to reach a HITL pause.
    fn wants_git_push() -> ModelResponse {
        ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"command": "git push origin main"}),
            }],
            usage: None,
            thinking: None,
        }
    }

    async fn runner_with(
        script: Vec<ModelResponse>,
        policy: ApprovalPolicy,
    ) -> (SessionHandle, TempDir) {
        let dir = tempdir().unwrap();
        let cfg = LoopConfig {
            max_turns: 5,
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        let session = AgentSession::create(
            cfg,
            Arc::new(MockModelClient::script(script)),
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        // The tempdir is returned so the journal outlives the runner.
        (spawn(session, policy), dir)
    }

    /// Collect events until one matches `stop`, so a test never blocks
    /// forever on an event that is not coming.
    async fn drain_until(
        handle: &mut SessionHandle,
        stop: impl Fn(&SessionEvent) -> bool,
    ) -> Vec<SessionEvent> {
        let mut seen = Vec::new();
        let collected = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(event) = handle.next_event().await {
                let done = stop(&event);
                seen.push(event);
                if done {
                    return;
                }
            }
        })
        .await;
        assert!(collected.is_ok(), "timed out; events so far: {seen:?}");
        seen
    }

    #[tokio::test]
    async fn a_prompt_streams_text_and_then_completes() {
        let (mut handle, _dir) = runner_with(vec![text("hello there")], ApprovalPolicy::Ask).await;
        handle
            .send(SessionCommand::Prompt("hi".into()))
            .await
            .unwrap();

        let events = drain_until(&mut handle, |e| {
            matches!(e, SessionEvent::TurnComplete { .. })
        })
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::TextDelta(t) if t == "hello there")),
            "expected the streamed delta to be forwarded: {events:?}"
        );
        let Some(SessionEvent::TurnComplete { text, .. }) = events.last() else {
            panic!("expected TurnComplete last, got {events:?}");
        };
        assert_eq!(text, "hello there");
    }

    /// The whole point of `Ask`: nothing runs, and the runner stays idle
    /// until the caller answers.
    #[tokio::test]
    async fn ask_policy_parks_on_approval_then_resumes_when_resolved() {
        let (mut handle, _dir) = runner_with(
            vec![wants_git_push(), text("gave up on pushing")],
            ApprovalPolicy::Ask,
        )
        .await;
        handle
            .send(SessionCommand::Prompt("push".into()))
            .await
            .unwrap();

        let events = drain_until(&mut handle, |e| {
            matches!(e, SessionEvent::AwaitingApproval(_))
        })
        .await;
        let Some(SessionEvent::AwaitingApproval(payload)) = events.last() else {
            panic!("expected AwaitingApproval, got {events:?}");
        };
        assert_eq!(payload.tool, "bash");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnComplete { .. })),
            "the turn must not complete while approval is outstanding"
        );

        handle
            .send(SessionCommand::Resolve(HitlDecision::Deny))
            .await
            .unwrap();
        let after = drain_until(&mut handle, |e| {
            matches!(e, SessionEvent::TurnComplete { .. })
        })
        .await;
        assert!(
            matches!(after.last(), Some(SessionEvent::TurnComplete { .. })),
            "resolving should resume the same turn: {after:?}"
        );
    }

    #[tokio::test]
    async fn deny_all_policy_never_asks() {
        let (mut handle, _dir) = runner_with(
            vec![wants_git_push(), text("understood, not pushing")],
            ApprovalPolicy::DenyAll,
        )
        .await;
        handle
            .send(SessionCommand::Prompt("push".into()))
            .await
            .unwrap();

        let events = drain_until(&mut handle, |e| {
            matches!(e, SessionEvent::TurnComplete { .. })
        })
        .await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::AwaitingApproval(_))),
            "DenyAll must resolve without emitting an approval request: {events:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_closes_the_event_stream() {
        let (mut handle, _dir) = runner_with(vec![text("unused")], ApprovalPolicy::Ask).await;
        handle.send(SessionCommand::Shutdown).await.unwrap();
        assert!(
            handle.next_event().await.is_none(),
            "the event stream should close once the runner stops"
        );
    }

    /// A command sent after shutdown fails rather than hanging.
    #[tokio::test]
    async fn commands_after_shutdown_report_the_runner_is_gone() {
        let (mut handle, _dir) = runner_with(vec![text("unused")], ApprovalPolicy::Ask).await;
        handle.send(SessionCommand::Shutdown).await.unwrap();
        while handle.next_event().await.is_some() {}
        assert_eq!(
            handle
                .send(SessionCommand::Prompt("anyone there".into()))
                .await,
            Err(RunnerGone)
        );
    }
}
