//! Registry of background tasks — shell jobs and subagents — that run
//! concurrently with the single foreground `ActiveTaskState`. Deliberately a
//! separate store rather than folded into `ActiveTaskState`: the foreground
//! task is "what the user is actively talking to," background tasks are
//! "what's running alongside it," and conflating the two invariants (only
//! one foreground task; any number of background tasks) would weaken both.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use forge_types::{BackgroundTaskId, HitlPayload, SessionId, TaskId};
use tokio_util::sync::CancellationToken;

use crate::{AgentSession, LoopError};

/// What kind of work a background task represents.
#[derive(Debug, Clone)]
pub enum BackgroundTaskKind {
    /// A non-agentic shell command (compile/test/index/etc).
    Shell { command: String },
    /// An independent nested agent loop with its own `AgentSession`.
    Subagent { role: String, prompt: String },
}

impl BackgroundTaskKind {
    /// Informational tag used in journal payloads — not parsed back.
    pub fn tag(&self) -> &'static str {
        match self {
            BackgroundTaskKind::Shell { .. } => "shell",
            BackgroundTaskKind::Subagent { .. } => "subagent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundTaskStatus {
    Queued,
    Running,
    /// Only reachable by a `Subagent` task whose child session has entered
    /// `TaskLifecycle::Waiting` on a HITL request. Not terminal — the
    /// subagent's spawned task is still alive, blocked on
    /// `AgentSession::resolve_subagent_hitl` supplying a decision.
    WaitingForApproval {
        payload: HitlPayload,
    },
    Succeeded {
        summary: String,
    },
    Failed {
        error: String,
    },
    Cancelled,
}

impl BackgroundTaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BackgroundTaskStatus::Succeeded { .. }
                | BackgroundTaskStatus::Failed { .. }
                | BackgroundTaskStatus::Cancelled
        )
    }

    /// Informational tag used in journal payloads.
    pub fn tag(&self) -> &'static str {
        match self {
            BackgroundTaskStatus::Queued => "queued",
            BackgroundTaskStatus::Running => "running",
            BackgroundTaskStatus::WaitingForApproval { .. } => "waiting_for_approval",
            BackgroundTaskStatus::Succeeded { .. } => "succeeded",
            BackgroundTaskStatus::Failed { .. } => "failed",
            BackgroundTaskStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackgroundTaskHandle {
    pub id: BackgroundTaskId,
    /// The foreground `TaskId` that was active when this background task
    /// was spawned — informational lineage, not a lifecycle dependency.
    pub parent_task_id: TaskId,
    pub kind: BackgroundTaskKind,
    pub label: String,
    pub status: BackgroundTaskStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancel: CancellationToken,
    /// `Some` for a `Subagent` task — its own independent session/journal.
    pub child_session_id: Option<SessionId>,
    /// Live snapshot of the subagent's most recent assistant message —
    /// `None` for shell tasks. Shared (not polled through a channel) so a
    /// cheap lock+clone at render time is enough; `drive_subagent` updates
    /// it at each natural checkpoint (start, and after every HITL resume).
    pub latest_message: Arc<Mutex<Option<String>>>,
    /// `Some` for a `Subagent` task once its worktree exists — surfaced so
    /// a finished subagent's work can be found and reviewed manually (no
    /// automated merge-back — see `forge_storage::worktree`'s module docs).
    pub worktree_path: Option<PathBuf>,
    pub worktree_branch: Option<String>,
}

/// Map of in-flight and recently-finished background tasks for one session.
/// Not itself durable — callers are responsible for journaling lifecycle
/// transitions (`Journal::append_background_task_started/finished`) before
/// or alongside mutating this registry, matching the record-before-side-
/// effect discipline used elsewhere (see `TaskQueue`).
#[derive(Debug, Default)]
pub struct BackgroundTaskRegistry {
    tasks: HashMap<BackgroundTaskId, BackgroundTaskHandle>,
    next_id: u64,
}

impl BackgroundTaskRegistry {
    const MAX_TERMINAL_TASKS: usize = 64;

    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            next_id: 1,
        }
    }

    /// Allocate a new task slot in `Queued` status. The caller journals
    /// `BackgroundTaskStarted` (and, for a subagent, `SubagentSpawned`)
    /// before or alongside this call, then transitions the returned id to
    /// `Running` via `mark_running` once the underlying work actually
    /// starts.
    pub fn spawn_slot(
        &mut self,
        kind: BackgroundTaskKind,
        label: impl Into<String>,
        parent_task_id: TaskId,
        cancel: CancellationToken,
        child_session_id: Option<SessionId>,
    ) -> BackgroundTaskId {
        let id = BackgroundTaskId(self.next_id);
        self.next_id += 1;
        self.tasks.insert(
            id,
            BackgroundTaskHandle {
                id,
                parent_task_id,
                kind,
                label: label.into(),
                status: BackgroundTaskStatus::Queued,
                started_at: Utc::now(),
                finished_at: None,
                cancel,
                child_session_id,
                latest_message: Arc::new(Mutex::new(None)),
                worktree_path: None,
                worktree_branch: None,
            },
        );
        id
    }

    /// Like `spawn_slot`, but reuses a specific id instead of minting a
    /// fresh one — for reconciling a task that already has journal history
    /// under that id (an auto-resumed subagent). Bumps `next_id` past `id`
    /// so a later `spawn_slot` call can never collide with it. Unlike
    /// `spawn_slot`'s fresh ids (which reset every process and only ever
    /// need to be *distinct within this process*, per `lifecycle.rs`'s doc
    /// comment on why they aren't persisted), reusing the journaled id here
    /// matters: it's what lets `finish_background_task`'s eventual
    /// `append_background_task_finished` close the SAME pair
    /// `BackgroundTaskStarted` opened under, rather than leaving that
    /// original id looking orphaned again on every subsequent restart.
    pub fn resume_slot(
        &mut self,
        id: BackgroundTaskId,
        kind: BackgroundTaskKind,
        label: impl Into<String>,
        parent_task_id: TaskId,
        cancel: CancellationToken,
        child_session_id: Option<SessionId>,
    ) -> BackgroundTaskId {
        self.next_id = self.next_id.max(id.0 + 1);
        self.tasks.insert(
            id,
            BackgroundTaskHandle {
                id,
                parent_task_id,
                kind,
                label: label.into(),
                status: BackgroundTaskStatus::Queued,
                started_at: Utc::now(),
                finished_at: None,
                cancel,
                child_session_id,
                latest_message: Arc::new(Mutex::new(None)),
                worktree_path: None,
                worktree_branch: None,
            },
        );
        id
    }

    /// Record where a subagent's worktree lives, for display (Tasks tab)
    /// and manual post-hoc review — no automated merge-back.
    pub fn set_worktree(&mut self, id: BackgroundTaskId, path: PathBuf, branch: String) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.worktree_path = Some(path);
            task.worktree_branch = Some(branch);
        }
    }

    /// Clone out the shared "latest message" cell for a task, so a spawned
    /// subagent-driving task can write live snapshots into it independent
    /// of the registry (which it doesn't otherwise have access to — it owns
    /// the child `AgentSession`, not `&mut self`).
    pub fn latest_message_cell(&self, id: BackgroundTaskId) -> Option<Arc<Mutex<Option<String>>>> {
        self.tasks.get(&id).map(|t| t.latest_message.clone())
    }

    pub fn mark_running(&mut self, id: BackgroundTaskId) {
        if let Some(task) = self.tasks.get_mut(&id) {
            if !task.status.is_terminal() {
                task.status = BackgroundTaskStatus::Running;
            }
        }
    }

    pub fn set_status(&mut self, id: BackgroundTaskId, status: BackgroundTaskStatus) {
        let terminal = status.is_terminal();
        if let Some(task) = self.tasks.get_mut(&id) {
            if terminal {
                task.finished_at = Some(Utc::now());
            }
            task.status = status;
        }
        if terminal {
            self.prune_terminal_tasks();
        }
    }

    fn prune_terminal_tasks(&mut self) {
        let mut terminal: Vec<_> = self
            .tasks
            .values()
            .filter(|task| task.status.is_terminal())
            .map(|task| (task.id, task.finished_at))
            .collect();
        if terminal.len() <= Self::MAX_TERMINAL_TASKS {
            return;
        }
        let remove_count = terminal.len() - Self::MAX_TERMINAL_TASKS;
        terminal.sort_by_key(|(_, finished_at)| *finished_at);
        for (id, _) in terminal.into_iter().take(remove_count) {
            self.tasks.remove(&id);
        }
    }

    pub fn get(&self, id: BackgroundTaskId) -> Option<&BackgroundTaskHandle> {
        self.tasks.get(&id)
    }

    pub fn list(&self) -> impl Iterator<Item = &BackgroundTaskHandle> {
        self.tasks.values()
    }

    /// Number of tracked background tasks without walking the registry.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether no background tasks are currently tracked.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// All non-terminal tasks spawned under a given foreground task —
    /// used to propagate cancellation when that foreground task ends.
    pub fn children_of(
        &self,
        parent_task_id: TaskId,
    ) -> impl Iterator<Item = &BackgroundTaskHandle> {
        self.tasks
            .values()
            .filter(move |t| t.parent_task_id == parent_task_id && !t.status.is_terminal())
    }

    /// Request cancellation of a task. Returns `false` if the id is unknown
    /// or the task is already terminal (cancelling a finished task is a
    /// no-op, not an error).
    pub fn cancel(&mut self, id: BackgroundTaskId) -> bool {
        match self.tasks.get(&id) {
            Some(task) if !task.status.is_terminal() => {
                task.cancel.cancel();
                true
            }
            _ => false,
        }
    }
}

/// Result of a finished background job, kind-agnostic at the polling layer
/// so future kinds (subagents, in Milestone 2) can reuse `poll_background_tasks`
/// without changing its shape — only `finish_background_task`'s match arms grow.
#[derive(Debug)]
pub enum BackgroundTaskOutcome {
    Shell {
        output: String,
        is_error: bool,
        exit_code: Option<i32>,
    },
    /// User-requested cancellation won the race against the job finishing —
    /// distinct from `Shell { is_error: true, .. }` so a cancelled task
    /// reports `BackgroundTaskStatus::Cancelled`, not `Failed`.
    Cancelled,
    /// A subagent's nested agent loop finished, failed, or was cancelled —
    /// see `crate::subagent`. Unlike `Shell`/`Cancelled`, this variant
    /// already carries the final `BackgroundTaskStatus` (derived from the
    /// child's own `TaskLifecycle`), so `finish_background_task` doesn't
    /// re-derive it from an `is_error` flag.
    Subagent(crate::subagent::SubagentOutcome),
    /// **Not terminal.** A subagent entered `TaskLifecycle::Waiting` on a
    /// HITL request. Sent on the same channel as the terminal outcome
    /// because a subagent may hit several of these before finishing —
    /// `poll_background_tasks` updates the live status and keeps polling
    /// the same receiver rather than treating this as the end of the task.
    WaitingForApproval(HitlPayload),
}

async fn run_shell_job(
    command: String,
    workspace_root: std::path::PathBuf,
    egress: Option<std::sync::Arc<forge_tools::sandbox::EgressGrant>>,
    session_tmp: Option<std::sync::Arc<forge_tools::SessionTempDir>>,
) -> BackgroundTaskOutcome {
    // Backgrounded work gets the same network the foreground has. It was
    // calling the grantless variant, so a background `cargo build` was confined
    // *and* offline while the identical foreground command worked — a
    // difference with no reason behind it, and one that would have surfaced as
    // a mysteriously failing build rather than as a permission decision.
    match forge_tools::run_shell_command_with_egress_and_temp(
        &command,
        &workspace_root,
        egress.as_deref(),
        session_tmp.as_deref().map(|temp| temp.path()),
    )
    .await
    {
        Ok(out) => BackgroundTaskOutcome::Shell {
            output: out.content,
            is_error: out.is_error,
            exit_code: out.exit_code,
        },
        Err(e) => BackgroundTaskOutcome::Shell {
            output: format!("failed to start background command: {e}"),
            is_error: true,
            exit_code: None,
        },
    }
}

impl AgentSession {
    /// Reconcile background tasks left in flight by a crash/restart. A shell
    /// job's process cannot survive a process restart (no PID
    /// resurrection), so every orphaned (`finished: false`) shell task is
    /// marked `Cancelled` and re-registered in the session task store purely for
    /// display — nothing is re-spawned. A subagent orphan is different: its
    /// state lives entirely in its own journal (not an OS process), so it
    /// can genuinely be auto-resumed — see `resume_subagent_task`.
    /// `subagent_workspaces` (from `Journal::replay`'s
    /// `ReplayState::subagent_workspaces`) is how the worktree path for
    /// each subagent orphan is found.
    pub(crate) async fn reconcile_orphaned_background_tasks(
        &mut self,
        orphaned: &[forge_durable::RestoredBackgroundTask],
        subagent_workspaces: &HashMap<u64, std::path::PathBuf>,
    ) -> Result<(), LoopError> {
        for task in orphaned.iter().filter(|t| !t.finished) {
            if task.kind == "subagent" {
                if let Some(workspace) = subagent_workspaces.get(&task.id.0) {
                    self.resume_subagent_task(task, workspace.clone()).await?;
                    continue;
                }
                // No recorded workspace — shouldn't happen for a subagent
                // that reached `append_subagent_spawned` (the worktree must
                // already have existed at that point), but fall through to
                // the conservative mark-cancelled path below rather than
                // guessing a path or silently dropping the task.
            }
            let kind = if task.kind == "subagent" {
                BackgroundTaskKind::Subagent {
                    role: task.label.clone(),
                    prompt: String::new(),
                }
            } else {
                BackgroundTaskKind::Shell {
                    command: String::new(),
                }
            };
            // The in-memory registry allocates its own ids per process (see
            // `lifecycle.rs`'s doc comment on why task/attempt ids aren't
            // persisted across restarts either) — `task.id` is the journaled
            // id, `new_id` is this process's display id for the same task.
            let new_id = self.tasks.background.spawn_slot(
                kind,
                task.label.clone(),
                self.active_task.task_id,
                CancellationToken::new(),
                task.child_session_id,
            );
            self.tasks
                .background
                .set_status(new_id, BackgroundTaskStatus::Cancelled);
            let summary = format!(
                "Background task '{}' was interrupted by a restart and was not resumed.",
                task.label
            );
            self.journal
                .append_background_task_finished(
                    self.session_id,
                    task.id,
                    BackgroundTaskStatus::Cancelled.tag(),
                    &summary,
                )
                .await?;
        }
        Ok(())
    }

    /// Start `command` running in the background, not blocking the current
    /// turn. The result surfaces later via `poll_background_tasks`, which
    /// enqueues an observation for the model's next turn (see
    /// `finish_background_task`) rather than interrupting this one.
    pub async fn spawn_background_shell(
        &mut self,
        command: String,
        label: String,
    ) -> Result<BackgroundTaskId, LoopError> {
        let cancel = CancellationToken::new();
        let id = self.tasks.background.spawn_slot(
            BackgroundTaskKind::Shell {
                command: command.clone(),
            },
            label.clone(),
            self.active_task.task_id,
            cancel.clone(),
            None,
        );
        self.journal
            .append_background_task_started(self.session_id, id, "shell", &label, None)
            .await?;
        self.tasks.background.mark_running(id);

        let (tx, rx) = std::sync::mpsc::channel();
        let workspace_root = self.tool_ctx.workspace_root.clone();
        let egress = self.tool_ctx.egress.clone();
        let session_tmp = self.tool_ctx.session_tmp.clone();
        tokio::spawn(async move {
            let outcome = tokio::select! {
                outcome = run_shell_job(command, workspace_root, egress, session_tmp) => outcome,
                _ = cancel.cancelled() => BackgroundTaskOutcome::Cancelled,
            };
            let _ = tx.send(outcome);
        });
        self.tasks.receivers.insert(id, std::sync::Mutex::new(rx));
        Ok(id)
    }

    /// Non-blocking; call once per render tick (mirrors `forge-tui`'s
    /// `GitStatusCache::poll`). Drains every finished-or-waiting background
    /// task channel: a `WaitingForApproval` update is applied in place (the
    /// task stays registered, still polled next tick), while any other
    /// outcome finalizes the task and removes its receiver.
    pub async fn poll_background_tasks(&mut self) -> Result<(), LoopError> {
        let mut live_updates: Vec<(BackgroundTaskId, HitlPayload)> = Vec::new();
        let mut finished: Vec<(BackgroundTaskId, Option<BackgroundTaskOutcome>)> = Vec::new();
        for (id, rx) in self.tasks.receivers.iter() {
            let rx = rx.lock().unwrap();
            loop {
                match rx.try_recv() {
                    Ok(BackgroundTaskOutcome::WaitingForApproval(payload)) => {
                        live_updates.push((*id, payload));
                    }
                    Ok(outcome) => {
                        finished.push((*id, Some(outcome)));
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        finished.push((*id, None));
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        for (id, payload) in live_updates {
            self.tasks
                .background
                .set_status(id, BackgroundTaskStatus::WaitingForApproval { payload });
        }
        for (id, outcome) in finished {
            self.tasks.receivers.remove(&id);
            self.tasks.subagent_hitl_senders.remove(&id);
            self.finish_background_task(id, outcome).await?;
        }
        Ok(())
    }

    /// Route an approve/deny decision to the specific subagent waiting on
    /// it. Returns `false` if `id` isn't a subagent currently waiting (e.g.
    /// it already finished, or was never a subagent) — a stale UI selection
    /// must not silently no-op without the caller knowing.
    pub fn resolve_subagent_hitl(
        &mut self,
        id: BackgroundTaskId,
        decision: forge_types::HitlDecision,
    ) -> bool {
        match self.tasks.subagent_hitl_senders.get(&id) {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Entry point for `run_one_tool`'s `background_run` interception:
    /// starts the job and immediately returns a tool result announcing it
    /// started — never waits for the job itself to finish.
    pub(crate) async fn dispatch_background_run(
        &mut self,
        call: &forge_types::ToolCall,
    ) -> Result<Option<forge_types::ModelResponse>, LoopError> {
        #[derive(serde::Deserialize)]
        struct Args {
            command: String,
            #[serde(default)]
            label: Option<String>,
        }

        let output = match serde_json::from_value::<Args>(call.arguments.clone()) {
            Ok(args) => {
                let label = args
                    .label
                    .unwrap_or_else(|| crate::truncate(&args.command, 40));
                let id = self
                    .spawn_background_shell(args.command, label.clone())
                    .await?;
                forge_types::ToolOutput::success(format!(
                    "Started background task #{} ('{label}'). You'll see the result once it finishes.",
                    id.0
                ))
            }
            Err(e) => forge_types::ToolOutput::failed_exit(
                format!("invalid background_run arguments: {e}"),
                None,
            ),
        };

        self.journal
            .append_tool_result(self.session_id, call, &output)
            .await?;
        self.remember_tool_result(call, &output);
        self.messages.push(forge_types::Message {
            outcome: output.effective_outcome(),
            role: forge_types::MessageRole::Tool,
            content: output.content.clone(),
            tool_call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        });
        Ok(None)
    }

    /// Finalize a background task: journal `BackgroundTaskFinished`, update
    /// the registry, and — per the "next natural turn boundary" delivery
    /// decision — enqueue the result as a future-task observation via the
    /// existing `TaskQueue`, exactly like a queued user message.
    async fn finish_background_task(
        &mut self,
        id: BackgroundTaskId,
        outcome: Option<BackgroundTaskOutcome>,
    ) -> Result<(), LoopError> {
        let label = self
            .tasks
            .background
            .get(id)
            .map(|t| t.label.clone())
            .unwrap_or_default();

        let (status, summary) = match outcome {
            Some(BackgroundTaskOutcome::Shell {
                output,
                is_error,
                exit_code,
            }) => {
                let summary = if is_error {
                    format!("Background task '{label}' failed (exit {exit_code:?}):\n{output}")
                } else {
                    format!("Background task '{label}' finished:\n{output}")
                };
                let status = if is_error {
                    BackgroundTaskStatus::Failed {
                        error: summary.clone(),
                    }
                } else {
                    BackgroundTaskStatus::Succeeded {
                        summary: summary.clone(),
                    }
                };
                (status, summary)
            }
            Some(BackgroundTaskOutcome::Cancelled) => (
                BackgroundTaskStatus::Cancelled,
                format!("Background task '{label}' was cancelled"),
            ),
            Some(BackgroundTaskOutcome::Subagent(outcome)) => {
                let summary = format!("Subagent '{label}' finished: {}", outcome.summary);
                self.journal
                    .append_subagent_finished(
                        self.session_id,
                        id,
                        outcome.child_session_id,
                        outcome.status.tag(),
                        &outcome.summary,
                    )
                    .await?;
                (outcome.status, summary)
            }
            // `poll_background_tasks` intercepts this variant itself (as a
            // live, non-terminal status update) and never forwards it here —
            // reaching this arm would mean that dispatch broke.
            Some(BackgroundTaskOutcome::WaitingForApproval(_)) => {
                unreachable!(
                    "WaitingForApproval must be handled in poll_background_tasks, not finish_background_task"
                )
            }
            // Sender dropped without sending — the spawned task panicked or
            // was torn down; treat conservatively as cancelled rather than
            // silently leaving the task's status stuck at `Running` forever.
            None => (
                BackgroundTaskStatus::Cancelled,
                format!("Background task '{label}' was interrupted"),
            ),
        };

        self.journal
            .append_background_task_finished(self.session_id, id, status.tag(), &summary)
            .await?;
        self.tasks.background.set_status(id, status);
        self.enqueue_task(&summary).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tid(n: u64) -> TaskId {
        TaskId(n)
    }

    mod session_tests {
        use std::sync::Arc;
        use std::time::Duration;

        use forge_model::MockModelClient;
        use forge_tools::ToolRegistry;
        use forge_types::{ModelResponse, ToolCall};
        use tempfile::tempdir;

        use crate::{AgentSession, LoopConfig};

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

        async fn session(dir: &std::path::Path) -> AgentSession {
            let model = Arc::new(MockModelClient::script(vec![ModelResponse {
                text: "ok".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            }]));
            AgentSession::create(cfg(dir), model, ToolRegistry::new())
                .await
                .unwrap()
        }

        /// Polls until the task reaches a terminal status or the timeout
        /// elapses — a single `poll_background_tasks` call would race the
        /// spawned job, since it runs on its own tokio task.
        async fn wait_terminal(
            session: &mut AgentSession,
            id: forge_types::BackgroundTaskId,
        ) -> super::BackgroundTaskStatus {
            for _ in 0..200 {
                session.poll_background_tasks().await.unwrap();
                if let Some(task) = session.tasks.background.get(id) {
                    if task.status.is_terminal() {
                        return task.status.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("background task {id:?} did not finish within timeout");
        }

        #[tokio::test]
        async fn successful_shell_job_finishes_succeeded_and_enqueues_observation() {
            let dir = tempdir().unwrap();
            let mut s = session(dir.path()).await;
            let id = s
                .spawn_background_shell("echo hello-from-bg".into(), "echo".into())
                .await
                .unwrap();

            let status = wait_terminal(&mut s, id).await;
            match status {
                super::BackgroundTaskStatus::Succeeded { summary } => {
                    assert!(summary.contains("hello-from-bg"));
                }
                other => panic!("expected Succeeded, got {other:?}"),
            }
            // Result delivery: next-turn-boundary via the existing queue.
            assert_eq!(s.queue().len(), 1);
        }

        #[tokio::test]
        async fn failing_shell_job_finishes_failed() {
            let dir = tempdir().unwrap();
            let mut s = session(dir.path()).await;
            let id = s
                .spawn_background_shell("exit 3".into(), "exit3".into())
                .await
                .unwrap();

            let status = wait_terminal(&mut s, id).await;
            assert!(matches!(status, super::BackgroundTaskStatus::Failed { .. }));
        }

        #[tokio::test]
        async fn cancelling_a_running_shell_job_finishes_cancelled() {
            let dir = tempdir().unwrap();
            let mut s = session(dir.path()).await;
            let id = s
                .spawn_background_shell("sleep 5".into(), "sleep".into())
                .await
                .unwrap();
            assert!(s.cancel_background_task(id));

            let status = wait_terminal(&mut s, id).await;
            assert_eq!(status, super::BackgroundTaskStatus::Cancelled);
        }

        #[tokio::test]
        async fn dispatch_background_run_returns_immediately_and_registers_task() {
            let dir = tempdir().unwrap();
            let mut s = session(dir.path()).await;
            let call = ToolCall {
                id: "call-1".into(),
                name: "background_run".into(),
                arguments: serde_json::json!({ "command": "echo hi", "label": "greet" }),
            };
            let response = s.dispatch_background_run(&call).await.unwrap();
            assert!(response.is_none());
            assert_eq!(s.background().list().count(), 1);
            let task = s.background().list().next().unwrap();
            assert_eq!(task.label, "greet");

            let last = s.messages.last().unwrap();
            assert_eq!(last.role, forge_types::MessageRole::Tool);
            assert!(last.content.contains("Started background task"));
        }

        #[tokio::test]
        async fn resuming_marks_orphaned_shell_task_cancelled_without_respawning() {
            let dir = tempdir().unwrap();
            let cfg = cfg(dir.path());
            let model = Arc::new(MockModelClient::script(vec![ModelResponse {
                text: "ok".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            }]));
            // First process: start a background task via the tool path, but
            // crash (drop the session) before it finishes / before resume
            // would have a chance to poll it — the journal only has
            // `BackgroundTaskStarted`, never a `BackgroundTaskFinished`.
            let session_id = {
                let mut s = AgentSession::create(cfg.clone(), model.clone(), ToolRegistry::new())
                    .await
                    .unwrap();
                let call = ToolCall {
                    id: "call-1".into(),
                    name: "background_run".into(),
                    arguments: serde_json::json!({ "command": "sleep 5", "label": "long-job" }),
                };
                s.dispatch_background_run(&call).await.unwrap();
                s.session_id
            };

            // Second process: resume. The orphaned task must show up as
            // Cancelled, not silently vanish or stay stuck "Running" forever.
            let resumed = AgentSession::resume(cfg, model, ToolRegistry::new(), session_id)
                .await
                .unwrap();
            assert_eq!(resumed.background().list().count(), 1);
            let task = resumed.background().list().next().unwrap();
            assert_eq!(task.label, "long-job");
            assert_eq!(task.status, super::BackgroundTaskStatus::Cancelled);
        }

        #[tokio::test]
        async fn dispatch_background_run_rejects_malformed_arguments() {
            let dir = tempdir().unwrap();
            let mut s = session(dir.path()).await;
            let call = ToolCall {
                id: "call-1".into(),
                name: "background_run".into(),
                arguments: serde_json::json!({ "not_command": "oops" }),
            };
            s.dispatch_background_run(&call).await.unwrap();
            assert_eq!(s.background().list().count(), 0);
            let last = s.messages.last().unwrap();
            assert!(last.content.contains("invalid background_run arguments"));
        }
    }

    #[test]
    fn spawn_slot_assigns_stable_increasing_ids() {
        let mut reg = BackgroundTaskRegistry::new();
        let a = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "cargo check".into(),
            },
            "cargo check",
            tid(1),
            CancellationToken::new(),
            None,
        );
        let b = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "cargo test".into(),
            },
            "cargo test",
            tid(1),
            CancellationToken::new(),
            None,
        );
        assert_eq!(a, BackgroundTaskId(1));
        assert_eq!(b, BackgroundTaskId(2));
        assert_eq!(reg.list().count(), 2);
    }

    #[test]
    fn new_slot_starts_queued_then_running() {
        let mut reg = BackgroundTaskRegistry::new();
        let id = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "x".into(),
            },
            "x",
            tid(1),
            CancellationToken::new(),
            None,
        );
        assert_eq!(reg.get(id).unwrap().status, BackgroundTaskStatus::Queued);
        reg.mark_running(id);
        assert_eq!(reg.get(id).unwrap().status, BackgroundTaskStatus::Running);
    }

    #[test]
    fn set_status_to_terminal_records_finished_at() {
        let mut reg = BackgroundTaskRegistry::new();
        let id = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "x".into(),
            },
            "x",
            tid(1),
            CancellationToken::new(),
            None,
        );
        assert!(reg.get(id).unwrap().finished_at.is_none());
        reg.set_status(
            id,
            BackgroundTaskStatus::Succeeded {
                summary: "ok".into(),
            },
        );
        let task = reg.get(id).unwrap();
        assert!(task.status.is_terminal());
        assert!(task.finished_at.is_some());
    }

    #[test]
    fn terminal_history_is_bounded() {
        let mut reg = BackgroundTaskRegistry::new();
        for _ in 0..(BackgroundTaskRegistry::MAX_TERMINAL_TASKS + 10) {
            let id = reg.spawn_slot(
                BackgroundTaskKind::Shell {
                    command: "x".into(),
                },
                "x",
                tid(1),
                CancellationToken::new(),
                None,
            );
            reg.set_status(id, BackgroundTaskStatus::Cancelled);
        }
        assert_eq!(
            reg.list().count(),
            BackgroundTaskRegistry::MAX_TERMINAL_TASKS
        );
    }

    #[test]
    fn cancel_flips_token_only_for_non_terminal_tasks() {
        let mut reg = BackgroundTaskRegistry::new();
        let token = CancellationToken::new();
        let id = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "x".into(),
            },
            "x",
            tid(1),
            token.clone(),
            None,
        );
        assert!(reg.cancel(id));
        assert!(token.is_cancelled());

        reg.set_status(id, BackgroundTaskStatus::Cancelled);
        let token2 = CancellationToken::new();
        // Re-cancelling an already-terminal task is a no-op, not an error.
        assert!(!reg.cancel(id));
        let _ = token2; // unrelated token, just asserting cancel() returns false
    }

    #[test]
    fn cancel_unknown_id_returns_false() {
        let mut reg = BackgroundTaskRegistry::new();
        assert!(!reg.cancel(BackgroundTaskId(999)));
    }

    #[test]
    fn children_of_filters_by_parent_and_excludes_terminal() {
        let mut reg = BackgroundTaskRegistry::new();
        let a = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "x".into(),
            },
            "x",
            tid(1),
            CancellationToken::new(),
            None,
        );
        let _b = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "y".into(),
            },
            "y",
            tid(2),
            CancellationToken::new(),
            None,
        );
        let c = reg.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "z".into(),
            },
            "z",
            tid(1),
            CancellationToken::new(),
            None,
        );

        let children: Vec<_> = reg.children_of(tid(1)).map(|t| t.id).collect();
        assert_eq!(children.len(), 2);
        assert!(children.contains(&a));
        assert!(children.contains(&c));

        reg.set_status(a, BackgroundTaskStatus::Cancelled);
        let children: Vec<_> = reg.children_of(tid(1)).map(|t| t.id).collect();
        assert_eq!(children, vec![c]);
    }

    #[test]
    fn subagent_slot_carries_child_session_id() {
        let mut reg = BackgroundTaskRegistry::new();
        let child_sid = uuid::Uuid::new_v4();
        let id = reg.spawn_slot(
            BackgroundTaskKind::Subagent {
                role: "test-fixer".into(),
                prompt: "fix the failing tests".into(),
            },
            "test-fixer",
            tid(1),
            CancellationToken::new(),
            Some(child_sid),
        );
        assert_eq!(reg.get(id).unwrap().child_session_id, Some(child_sid));
    }
}
