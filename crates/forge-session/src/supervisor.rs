//! Concurrent repository session ownership behind a command/event API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use forge_config::Config;
use forge_core::{AgentSession, LoopError};
use forge_model::{client_from_config, ModelClient};
use forge_storage::{RepositoryRuntimeStorage, RuntimeDataKind, RuntimeStorage};
use forge_types::{
    AskUserQuestionResult, HitlDecision, ModelStreamEvent, SessionId, TaskLifecycle,
};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    connect_credentials, open_session_with_model, resolve_journal_dir, NewRepositoryTask,
    RepositoryControl, RepositoryLease, RepositoryTask, RepositoryTaskError, SessionLifecycle,
    SessionSnapshot, SessionTarget, SupervisorTurnState, TranscriptSnapshot, WorktreeOwnership,
};

const DEFAULT_MAX_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct TaskRuntimeSnapshot {
    pub task: RepositoryTask,
    pub session: SessionSnapshot,
    pub transcript: TranscriptSnapshot,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SupervisorEvent {
    Roster(Vec<TaskRuntimeSnapshot>),
    /// Boxed: a snapshot is an order of magnitude larger than every other
    /// variant, and this event is broadcast on every turn transition.
    TaskUpdated(Box<TaskRuntimeSnapshot>),
    Stream {
        session_id: SessionId,
        event: ModelStreamEvent,
    },
    Attention {
        session_id: SessionId,
        state: SupervisorTurnState,
        message: String,
    },
    Selected(Option<SessionId>),
    Error {
        session_id: Option<SessionId>,
        message: String,
    },
    TrustRequired {
        operation_id: u64,
        label: String,
        workspace: PathBuf,
    },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SupervisorCommand {
    CreateTask {
        label: String,
        first_prompt: Option<String>,
    },
    AttachWorktree {
        workspace: PathBuf,
        label: String,
        branch: String,
    },
    ArchiveTask {
        session_id: SessionId,
    },
    RenameTask {
        session_id: SessionId,
        label: String,
    },
    PinTask {
        session_id: SessionId,
        slot: Option<u8>,
        swap: bool,
    },
    RemoveManagedWorktree {
        session_id: SessionId,
    },
    FinalizeCreation {
        operation_id: u64,
    },
    CancelCreation {
        operation_id: u64,
    },
    /// Record operator-confirmed trust for a workspace Forge is about to run
    /// in. Sent by the attach flow after its confirmation step.
    TrustWorkspace {
        workspace: PathBuf,
    },
    SubmitPrompt {
        session_id: SessionId,
        text: String,
    },
    ContinueTurn {
        session_id: SessionId,
    },
    StopTurn {
        session_id: SessionId,
    },
    ResolveApproval {
        session_id: SessionId,
        decision: HitlDecision,
        actor: String,
    },
    ResolveQuestion {
        session_id: SessionId,
        answers: Option<AskUserQuestionResult>,
        actor: String,
    },
    SelectTask {
        session_id: Option<SessionId>,
    },
    SetModel {
        session_id: SessionId,
        model_id: String,
        route_id: String,
        reasoning_effort: Option<String>,
    },
    Refresh,
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepositorySupervisorError {
    #[error(transparent)]
    Control(#[from] RepositoryTaskError),
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("session assembly failed: {0}")]
    Assembly(#[from] anyhow::Error),
    #[error("model setup failed: {0}")]
    Model(#[from] forge_model::ModelError),
    #[error("storage setup failed: {0}")]
    Storage(#[from] forge_storage::StorageError),
    #[error("git worktree lookup failed: {0}")]
    Worktree(#[from] forge_storage::WorktreeError),
    #[error("task `{0}` has no live session actor")]
    NoActor(SessionId),
    #[error("supervisor command channel closed")]
    Closed,
    #[error("supervisor command failed: {0}")]
    Command(String),
}

struct CommandEnvelope {
    command: SupervisorCommand,
    reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct SupervisorHandle {
    commands: mpsc::Sender<CommandEnvelope>,
    events: broadcast::Sender<SupervisorEvent>,
}

impl SupervisorHandle {
    pub async fn command(
        &self,
        command: SupervisorCommand,
    ) -> Result<(), RepositorySupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(CommandEnvelope { command, reply })
            .await
            .map_err(|_| RepositorySupervisorError::Closed)?;
        response
            .await
            .map_err(|_| RepositorySupervisorError::Closed)?
            .map_err(RepositorySupervisorError::Command)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.events.subscribe()
    }
}

struct TaskActor {
    session: Mutex<AgentSession>,
    snapshot: RwLock<TaskRuntimeSnapshot>,
    driving: AtomicBool,
    running_cancel: StdMutex<Option<CancellationToken>>,
}

impl TaskActor {
    fn new(task: RepositoryTask, session: AgentSession) -> Self {
        let snapshot = TaskRuntimeSnapshot {
            task,
            session: SessionSnapshot::capture(&session),
            transcript: TranscriptSnapshot::capture(&session),
        };
        Self {
            session: Mutex::new(session),
            snapshot: RwLock::new(snapshot),
            driving: AtomicBool::new(false),
            running_cancel: StdMutex::new(None),
        }
    }

    fn request_cancel(&self) -> bool {
        let guard = self
            .running_cancel
            .lock()
            .expect("turn cancel lock poisoned");
        if let Some(token) = guard.as_ref() {
            token.cancel();
            true
        } else {
            false
        }
    }
}

struct SupervisorState {
    cfg: Config,
    model: Arc<dyn ModelClient>,
    control: Arc<RepositoryControl>,
    actors: RwLock<HashMap<SessionId, Arc<TaskActor>>>,
    permits: Arc<Semaphore>,
    events: broadcast::Sender<SupervisorEvent>,
    lease: RepositoryLease,
    /// Where operator-granted trust is recorded. `None` means the real
    /// user-global store; tests point it at a temporary file so granting
    /// trust for a fixture worktree never touches the developer's own.
    trust_store: Option<PathBuf>,
}

impl SupervisorState {
    fn grant_trust(&self, workspace: &std::path::Path) -> Result<(), RepositorySupervisorError> {
        let result = match self.trust_store.as_deref() {
            Some(store) => forge_config::grant_trust_at(store, workspace),
            None => forge_config::grant_trust(workspace),
        };
        result.map(|_| ()).map_err(|error| {
            RepositorySupervisorError::Command(format!(
                "could not record trust for {}: {error}",
                workspace.display()
            ))
        })
    }
}

pub struct RepositorySupervisor {
    state: Arc<SupervisorState>,
}

/// Repository ownership taken *before* any session is created.
///
/// The exclusive lease is what makes "one Forge per repository group" true, so
/// it has to be held before a competing process can write session state. This
/// type lets the CLI acquire ownership first, open its primary session second,
/// and only then hand both to the supervisor — see
/// [`RepositoryBootstrap::open_siblings`].
pub struct RepositoryBootstrap {
    lease: RepositoryLease,
    control: Arc<RepositoryControl>,
    main_worktree: PathBuf,
}

impl RepositoryBootstrap {
    /// Acquire the repository-group lease and open the control database.
    /// Fails before any session exists if another Forge already owns it.
    pub async fn acquire(cfg: &Config) -> Result<Self, RepositorySupervisorError> {
        let storage = RepositoryRuntimeStorage::new(cfg.workspace_root())?;
        let control_dir = storage.path_for(RuntimeDataKind::Control)?;
        let lease = RepositoryLease::acquire(&control_dir, cfg.workspace_root())?;
        let control = Arc::new(RepositoryControl::open(&control_dir).await?);
        Ok(Self {
            lease,
            control,
            main_worktree: storage.main_worktree().to_path_buf(),
        })
    }

    pub fn lease_owner(&self) -> &crate::LeaseOwner {
        self.lease.owner()
    }

    /// Roll back managed creations an earlier process left unresolved. A task
    /// stuck in `awaiting_trust` can never accept a prompt, so it is cancelled
    /// and its worktree removed when clean. Returns one notice per rollback.
    pub async fn recover_interrupted_creations(
        &self,
    ) -> Result<Vec<String>, RepositorySupervisorError> {
        let mut notices = Vec::new();
        for stale in self.control.stale_creations().await? {
            self.control
                .cancel_creation(stale.operation_id, "interrupted before trust was granted")
                .await?;
            let removed = match stale.workspace.as_ref() {
                Some(workspace) if workspace.is_dir() => {
                    forge_storage::remove_clean_worktree(&self.main_worktree, workspace).is_ok()
                }
                _ => true,
            };
            notices.push(if removed {
                format!("rolled back interrupted task `{}`", stale.label)
            } else {
                format!(
                    "interrupted task `{}` left a worktree with uncommitted work at {}",
                    stale.label,
                    stale
                        .workspace
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                )
            });
        }
        Ok(notices)
    }

    /// Adopt an already-open primary session and start the supervisor over the
    /// remaining sibling worktrees.
    pub async fn open_siblings(
        self,
        cfg: &Config,
        primary_session_id: SessionId,
    ) -> Result<(RepositorySupervisor, SupervisorHandle), RepositorySupervisorError> {
        RepositorySupervisor::open_siblings_with(self, cfg, primary_session_id).await
    }
}

impl RepositorySupervisor {
    pub async fn open_siblings(
        cfg: &Config,
        primary_session_id: SessionId,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let bootstrap = RepositoryBootstrap::acquire(cfg).await?;
        Self::open_siblings_with(bootstrap, cfg, primary_session_id).await
    }

    async fn open_siblings_with(
        bootstrap: RepositoryBootstrap,
        cfg: &Config,
        primary_session_id: SessionId,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let RepositoryBootstrap {
            lease,
            control,
            main_worktree,
        } = bootstrap;
        let worktrees = forge_storage::list_worktree_records(&main_worktree)?;
        control.reconcile_worktrees(&worktrees).await?;

        if control.task(primary_session_id).await.is_err() {
            let workspace = cfg.workspace_root().to_path_buf();
            let branch = worktrees
                .iter()
                .find(|worktree| same_path(&worktree.path, &workspace))
                .and_then(|worktree| worktree.branch.clone())
                .ok_or(forge_storage::WorktreeError::DetachedHead)?;
            control
                .register_task(
                    NewRepositoryTask {
                        session_id: primary_session_id,
                        label: branch
                            .rsplit('/')
                            .next()
                            .filter(|label| !label.is_empty())
                            .unwrap_or("primary")
                            .to_string(),
                        workspace,
                        branch,
                        ownership: WorktreeOwnership::Primary,
                        slot: Some(1),
                        model_id: cfg.model.model.clone(),
                        route_id: "native".into(),
                        reasoning_effort: None,
                    },
                    None,
                )
                .await?;
        }

        let model: Arc<dyn ModelClient> = Arc::from(client_from_config(cfg)?);
        model.apply_provider_env(&connect_credentials());
        let mut session_tasks = tokio::task::JoinSet::new();
        for task in control.tasks().await?.into_iter().filter(|task| {
            task.session_id != primary_session_id && task.lifecycle == SessionLifecycle::Active
        }) {
            let mut task_cfg = cfg.clone();
            task_cfg.resolved_workspace = task.workspace.clone();
            task_cfg.workspace_root = Some(task.workspace.display().to_string());
            let (journal_dir, _) = resolve_journal_dir(&task_cfg);
            task_cfg.journal.path = journal_dir.display().to_string();
            let model = model.clone();
            session_tasks.spawn(async move {
                open_session_with_model(&task_cfg, SessionTarget::Resume(task.session_id), model)
                    .await
                    .map(|opened| (task, opened.session))
            });
        }
        let mut sessions = Vec::new();
        while let Some(result) = session_tasks.join_next().await {
            sessions.push(
                result.map_err(|error| {
                    RepositorySupervisorError::Assembly(anyhow::anyhow!(error))
                })??,
            );
        }
        Self::spawn(
            control,
            lease,
            sessions,
            DEFAULT_MAX_CONCURRENCY,
            cfg.clone(),
            model,
        )
        .await
    }

    pub async fn open(cfg: &Config) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        Self::open_with_primary(cfg, None).await
    }

    pub async fn open_with_primary(
        cfg: &Config,
        primary: Option<AgentSession>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let storage = RepositoryRuntimeStorage::new(cfg.workspace_root())?;
        let control_dir = storage.path_for(RuntimeDataKind::Control)?;
        let lease = RepositoryLease::acquire(&control_dir, cfg.workspace_root())?;
        let control = Arc::new(RepositoryControl::open(&control_dir).await?);
        let worktrees = forge_storage::list_worktree_records(storage.main_worktree())?;
        control.reconcile_worktrees(&worktrees).await?;

        let model: Arc<dyn ModelClient> = Arc::from(client_from_config(cfg)?);
        model.apply_provider_env(&connect_credentials());
        let mut tasks = control.tasks().await?;
        if tasks.is_empty() {
            let opened = match primary {
                Some(session) => crate::OpenedSession {
                    session,
                    notices: Vec::new(),
                },
                None => open_session_with_model(cfg, SessionTarget::New, model.clone()).await?,
            };
            let workspace = cfg.workspace_root().to_path_buf();
            let branch = worktrees
                .iter()
                .find(|worktree| same_path(&worktree.path, &workspace))
                .and_then(|worktree| worktree.branch.clone())
                .ok_or(forge_storage::WorktreeError::DetachedHead)?;
            let label = branch
                .rsplit('/')
                .next()
                .filter(|label| !label.is_empty())
                .unwrap_or("primary")
                .to_string();
            control
                .register_task(
                    NewRepositoryTask {
                        session_id: opened.session.session_id,
                        label,
                        workspace,
                        branch,
                        ownership: WorktreeOwnership::Primary,
                        slot: Some(1),
                        model_id: opened.session.active_model.clone(),
                        route_id: opened.session.active_route_id.clone(),
                        reasoning_effort: None,
                    },
                    None,
                )
                .await?;
            tasks = control.tasks().await?;
            let task = tasks
                .iter()
                .find(|task| task.session_id == opened.session.session_id)
                .cloned()
                .ok_or(RepositoryTaskError::NotFound(opened.session.session_id))?;
            return Self::spawn(
                control,
                lease,
                vec![(task, opened.session)],
                DEFAULT_MAX_CONCURRENCY,
                cfg.clone(),
                model.clone(),
            )
            .await;
        }

        let mut sessions = Vec::new();
        for task in tasks
            .into_iter()
            .filter(|task| task.lifecycle == SessionLifecycle::Active)
        {
            let mut task_cfg = cfg.clone();
            task_cfg.resolved_workspace = task.workspace.clone();
            task_cfg.workspace_root = Some(task.workspace.display().to_string());
            let (journal_dir, _) = resolve_journal_dir(&task_cfg);
            task_cfg.journal.path = journal_dir.display().to_string();
            let opened = open_session_with_model(
                &task_cfg,
                SessionTarget::Resume(task.session_id),
                model.clone(),
            )
            .await?;
            sessions.push((task, opened.session));
        }
        Self::spawn(
            control,
            lease,
            sessions,
            DEFAULT_MAX_CONCURRENCY,
            cfg.clone(),
            model,
        )
        .await
    }

    pub async fn spawn(
        control: Arc<RepositoryControl>,
        lease: RepositoryLease,
        sessions: Vec<(RepositoryTask, AgentSession)>,
        max_concurrency: usize,
        cfg: Config,
        model: Arc<dyn ModelClient>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        Self::spawn_with_trust_store(control, lease, sessions, max_concurrency, cfg, model, None)
            .await
    }

    /// `spawn`, with the trust store redirected. Only a test has a reason to
    /// pass anything but `None` — see [`SupervisorState::trust_store`].
    pub async fn spawn_with_trust_store(
        control: Arc<RepositoryControl>,
        lease: RepositoryLease,
        sessions: Vec<(RepositoryTask, AgentSession)>,
        max_concurrency: usize,
        cfg: Config,
        model: Arc<dyn ModelClient>,
        trust_store: Option<PathBuf>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let (events, _) = broadcast::channel(512);
        let actors = sessions
            .into_iter()
            .map(|(task, session)| (task.session_id, Arc::new(TaskActor::new(task, session))))
            .collect();
        let state = Arc::new(SupervisorState {
            cfg,
            model,
            control,
            actors: RwLock::new(actors),
            permits: Arc::new(Semaphore::new(max_concurrency.max(1))),
            events: events.clone(),
            lease,
            trust_store,
        });
        let (commands, receiver) = mpsc::channel(128);
        let handle = SupervisorHandle { commands, events };
        let supervisor = Self {
            state: state.clone(),
        };
        tokio::spawn(run_commands(state, receiver));
        supervisor.publish_roster().await;
        Ok((supervisor, handle))
    }

    pub async fn snapshots(&self) -> Vec<TaskRuntimeSnapshot> {
        snapshots(&self.state).await
    }

    pub fn lease_owner(&self) -> &crate::LeaseOwner {
        self.state.lease.owner()
    }

    async fn publish_roster(&self) {
        let _ = self
            .state
            .events
            .send(SupervisorEvent::Roster(self.snapshots().await));
    }
}

async fn run_commands(state: Arc<SupervisorState>, mut receiver: mpsc::Receiver<CommandEnvelope>) {
    while let Some(envelope) = receiver.recv().await {
        let shutdown = matches!(envelope.command, SupervisorCommand::Shutdown);
        let result = execute_command(state.clone(), envelope.command)
            .await
            .map_err(|error| error.to_string());
        let _ = envelope.reply.send(result);
        if shutdown {
            break;
        }
    }
}

async fn execute_command(
    state: Arc<SupervisorState>,
    command: SupervisorCommand,
) -> Result<(), RepositorySupervisorError> {
    match command {
        SupervisorCommand::CreateTask {
            label,
            first_prompt,
        } => {
            let storage = RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace)?;
            let base_dir = storage.path_for(RuntimeDataKind::Worktree)?;
            let pending = state
                .control
                .begin_managed_creation(
                    &label,
                    &state.cfg.resolved_workspace,
                    first_prompt.as_deref(),
                )
                .await?;
            // Branch from the *initiating* worktree's committed HEAD, not the
            // main worktree's — launching Forge from a linked worktree must
            // fork the work that worktree is actually on.
            let worktree = forge_storage::create_task_worktree(
                &state.cfg.resolved_workspace,
                &base_dir,
                pending.operation_id,
                &label,
            )?;
            state
                .control
                .mark_worktree_created(pending.operation_id, &worktree.path, &worktree.branch)
                .await?;
            let mut task_cfg = state.cfg.clone();
            task_cfg.resolved_workspace = worktree.path.clone();
            task_cfg.workspace_root = Some(worktree.path.display().to_string());
            let (journal_dir, _) = resolve_journal_dir(&task_cfg);
            task_cfg.journal.path = journal_dir.display().to_string();
            let opened =
                open_session_with_model(&task_cfg, SessionTarget::New, state.model.clone()).await?;
            let task = RepositoryTask {
                session_id: opened.session.session_id,
                label,
                workspace: worktree.path,
                branch: worktree.branch,
                ownership: WorktreeOwnership::Managed,
                lifecycle: SessionLifecycle::Active,
                turn_state: SupervisorTurnState::Idle,
                slot: None,
                model_id: opened.session.active_model.clone(),
                route_id: opened.session.active_route_id.clone(),
                reasoning_effort: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                archived_at: None,
            };
            state
                .control
                .register_task(
                    NewRepositoryTask {
                        session_id: task.session_id,
                        label: task.label.clone(),
                        workspace: task.workspace.clone(),
                        branch: task.branch.clone(),
                        ownership: task.ownership,
                        slot: task.slot,
                        model_id: task.model_id.clone(),
                        route_id: task.route_id.clone(),
                        reasoning_effort: task.reasoning_effort.clone(),
                    },
                    Some(pending.operation_id),
                )
                .await?;
            let actor = Arc::new(TaskActor::new(task.clone(), opened.session));
            state.actors.write().await.insert(task.session_id, actor);
            // The first prompt stays parked on the pending operation until
            // `FinalizeCreation`; queueing it here would be rejected by the
            // `awaiting_trust` guard, and bypassing that guard would run a
            // model turn in a worktree the operator has not confirmed.
            let _ = state.events.send(SupervisorEvent::TrustRequired {
                operation_id: pending.operation_id,
                label: task.label,
                workspace: task.workspace,
            });
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::AttachWorktree {
            workspace,
            label,
            branch,
        } => {
            let workspace = validate_attach_target(&state, &workspace, &branch)?;
            let mut task_cfg = state.cfg.clone();
            task_cfg.resolved_workspace = workspace.clone();
            task_cfg.workspace_root = Some(workspace.display().to_string());
            let (journal_dir, _) = resolve_journal_dir(&task_cfg);
            task_cfg.journal.path = journal_dir.display().to_string();
            let opened =
                open_session_with_model(&task_cfg, SessionTarget::New, state.model.clone()).await?;
            let task = NewRepositoryTask {
                session_id: opened.session.session_id,
                label,
                workspace,
                branch,
                ownership: WorktreeOwnership::Attached,
                slot: None,
                model_id: opened.session.active_model.clone(),
                route_id: opened.session.active_route_id.clone(),
                reasoning_effort: None,
            };
            state.control.register_task(task.clone(), None).await?;
            let actor_task = state.control.task(task.session_id).await?;
            state.actors.write().await.insert(
                task.session_id,
                Arc::new(TaskActor::new(actor_task, opened.session)),
            );
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::ArchiveTask { session_id } => {
            state.control.archive(session_id).await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::RenameTask { session_id, label } => {
            state.control.rename(session_id, &label).await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::PinTask {
            session_id,
            slot,
            swap,
        } => {
            state.control.assign_slot(session_id, slot, swap).await?;
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::RemoveManagedWorktree { session_id } => {
            let task = state.control.task(session_id).await?;
            if task.ownership != WorktreeOwnership::Managed
                || task.lifecycle != SessionLifecycle::Archived
            {
                return Err(RepositorySupervisorError::Command(
                    "only archived managed tasks can remove a worktree".into(),
                ));
            }
            let storage = RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace)?;
            forge_storage::remove_clean_worktree(storage.main_worktree(), &task.workspace)?;
        }
        SupervisorCommand::FinalizeCreation { operation_id } => {
            let completed = state.control.complete_creation(operation_id).await?;
            // Trust is what the operator actually confirmed, so persist it
            // before the task can run. A managed worktree normally inherits
            // trust from the repository root, but the grant must be recorded
            // explicitly so the task keeps working if that changes.
            if let Some(session_id) = completed.session_id {
                let workspace = state.control.task(session_id).await?.workspace;
                if let Err(error) = state.grant_trust(&workspace) {
                    // Persisting failed: roll the creation back rather than
                    // leave a task that looks trusted but is not recorded.
                    let _ = rollback_creation(&state, operation_id).await;
                    let _ = state
                        .events
                        .send(SupervisorEvent::Roster(snapshots(&state).await));
                    return Err(error);
                }
                if let Some(text) = completed.first_prompt {
                    state.control.enqueue_prompt(session_id, &text).await?;
                    state
                        .control
                        .set_turn_state(session_id, SupervisorTurnState::Queued)
                        .await?;
                    publish_actor(&state, session_id).await?;
                    start_prompt_driver(state.clone(), session_id).await?;
                }
            }
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::CancelCreation { operation_id } => {
            rollback_creation(&state, operation_id).await?;
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::TrustWorkspace { workspace } => {
            state.grant_trust(&workspace)?;
        }
        SupervisorCommand::SubmitPrompt { session_id, text } => {
            state.control.enqueue_prompt(session_id, &text).await?;
            state
                .control
                .set_turn_state(session_id, SupervisorTurnState::Queued)
                .await?;
            publish_actor(&state, session_id).await?;
            start_prompt_driver(state, session_id).await?;
        }
        SupervisorCommand::ContinueTurn { session_id } => {
            start_continue_driver(state, session_id).await?;
        }
        SupervisorCommand::StopTurn { session_id } => {
            let actor = actor(&state, session_id).await?;
            if !actor.request_cancel() {
                let mut session = actor.session.lock().await;
                session.mark_cancelled().await?;
                state
                    .control
                    .set_turn_state(session_id, SupervisorTurnState::Cancelled)
                    .await?;
                refresh_actor(&state, &actor, &session).await?;
            }
        }
        SupervisorCommand::ResolveApproval {
            session_id,
            decision,
            actor: decision_actor,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.resolve_hitl(decision, &decision_actor).await?;
            refresh_actor(&state, &task_actor, &session).await?;
            drop(session);
            start_continue_driver(state, session_id).await?;
        }
        SupervisorCommand::ResolveQuestion {
            session_id,
            answers,
            actor: answer_actor,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.resolve_question(answers, &answer_actor).await?;
            refresh_actor(&state, &task_actor, &session).await?;
            drop(session);
            start_continue_driver(state, session_id).await?;
        }
        SupervisorCommand::SelectTask { session_id } => {
            state.control.set_selected(session_id).await?;
            let _ = state.events.send(SupervisorEvent::Selected(session_id));
        }
        SupervisorCommand::SetModel {
            session_id,
            model_id,
            route_id,
            reasoning_effort,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.set_active_model(model_id);
            session.set_active_route_id(route_id);
            session.set_reasoning_effort(reasoning_effort);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::Refresh => {
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::Shutdown => {
            let actors: Vec<_> = state.actors.read().await.values().cloned().collect();
            for task_actor in actors {
                task_actor.request_cancel();
            }
        }
    }
    Ok(())
}

/// Check an attach target against Git's own worktree list before Forge binds
/// a session to it. Everything here is a settled product rule: attached
/// worktrees must belong to *this* repository, the repository's main worktree
/// is not attachable as a second task, and the branch recorded on the task is
/// the one Git actually has checked out (bindings are immutable, so a wrong
/// branch here means a permanently unavailable task).
fn validate_attach_target(
    state: &SupervisorState,
    workspace: &std::path::Path,
    branch: &str,
) -> Result<PathBuf, RepositorySupervisorError> {
    let storage = RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace)?;
    let main = storage.main_worktree().to_path_buf();
    let canonical = workspace.canonicalize().map_err(|error| {
        RepositorySupervisorError::Command(format!("{}: {error}", workspace.display()))
    })?;
    if same_path(&canonical, &main) {
        return Err(RepositorySupervisorError::Command(
            "the repository's main worktree is already the primary task".into(),
        ));
    }
    let records = forge_storage::list_worktree_records(&main)?;
    let Some(record) = records
        .iter()
        .find(|record| same_path(&record.path, &canonical))
    else {
        return Err(RepositorySupervisorError::Command(format!(
            "{} is not a worktree of this repository",
            canonical.display()
        )));
    };
    match record.branch.as_deref() {
        Some(actual) if actual == branch => Ok(record.path.clone()),
        Some(actual) => Err(RepositorySupervisorError::Command(format!(
            "{} has `{actual}` checked out, not `{branch}`",
            canonical.display()
        ))),
        None => Err(RepositorySupervisorError::Worktree(
            forge_storage::WorktreeError::DetachedHead,
        )),
    }
}

/// Undo a managed creation: drop the provisional task row and actor, then
/// remove the worktree if Git will let go of it cleanly. A worktree holding
/// uncommitted work is left in place — losing work is worse than an orphan.
async fn rollback_creation(
    state: &Arc<SupervisorState>,
    operation_id: u64,
) -> Result<(), RepositorySupervisorError> {
    let (workspace, session_id) = state
        .control
        .cancel_creation(operation_id, "cancelled by operator")
        .await?;
    if let Some(session_id) = session_id {
        state.actors.write().await.remove(&session_id);
    }
    if let Some(workspace) = workspace {
        let storage = RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace)?;
        if workspace.is_dir() {
            let _ = forge_storage::remove_clean_worktree(storage.main_worktree(), &workspace);
        }
    }
    Ok(())
}

async fn start_prompt_driver(
    state: Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(&state, session_id).await?;
    if task_actor.driving.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    tokio::spawn(async move {
        if let Err(error) = drive_prompts(state.clone(), task_actor.clone()).await {
            let _ = state.events.send(SupervisorEvent::Error {
                session_id: Some(session_id),
                message: error.to_string(),
            });
        }
        task_actor.driving.store(false, Ordering::Release);
    });
    Ok(())
}

async fn start_continue_driver(
    state: Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(&state, session_id).await?;
    if task_actor.driving.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    tokio::spawn(async move {
        if let Err(error) = run_one(state.clone(), task_actor.clone(), None).await {
            let _ = state.events.send(SupervisorEvent::Error {
                session_id: Some(session_id),
                message: error.to_string(),
            });
        }
        task_actor.driving.store(false, Ordering::Release);
        let _ = start_prompt_driver(state, session_id).await;
    });
    Ok(())
}

async fn drive_prompts(
    state: Arc<SupervisorState>,
    task_actor: Arc<TaskActor>,
) -> Result<(), RepositorySupervisorError> {
    let session_id = task_actor.snapshot.read().await.task.session_id;
    loop {
        let waiting = task_actor.snapshot.read().await.session.lifecycle == TaskLifecycle::Waiting;
        if waiting {
            break;
        }
        let Some(prompt) = state.control.claim_next_prompt(session_id).await? else {
            break;
        };
        let should_continue = run_one(state.clone(), task_actor.clone(), Some(prompt)).await?;
        if !should_continue {
            break;
        }
    }
    Ok(())
}

async fn run_one(
    state: Arc<SupervisorState>,
    task_actor: Arc<TaskActor>,
    prompt: Option<(u64, String)>,
) -> Result<bool, RepositorySupervisorError> {
    let session_id = task_actor.snapshot.read().await.task.session_id;
    let permit = state
        .permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RepositorySupervisorError::Closed)?;
    state
        .control
        .set_turn_state(session_id, SupervisorTurnState::Running)
        .await?;
    publish_actor(&state, session_id).await?;

    let mut session = task_actor.session.lock().await;
    let cancel = session.begin_turn_cancellation_scope();
    *task_actor
        .running_cancel
        .lock()
        .expect("turn cancel lock poisoned") = Some(cancel);
    let (stream_sender, stream_receiver) = std::sync::mpsc::channel();
    let events = state.events.clone();
    let stream_forwarder = tokio::task::spawn_blocking(move || {
        while let Ok(event) = stream_receiver.recv() {
            let _ = events.send(SupervisorEvent::Stream { session_id, event });
        }
    });
    let result = match prompt.as_ref() {
        Some((_, text)) => {
            session.append_user_message(text).await?;
            session.run_agent_turns(Some(stream_sender)).await
        }
        None => session.run_agent_turns(Some(stream_sender)).await,
    };
    drop(permit);
    *task_actor
        .running_cancel
        .lock()
        .expect("turn cancel lock poisoned") = None;
    stream_forwarder
        .await
        .map_err(|error| RepositorySupervisorError::Command(error.to_string()))?;

    let turn_state = match &result {
        Ok(_) if session.active_task.lifecycle == TaskLifecycle::Waiting => {
            SupervisorTurnState::Waiting
        }
        Ok(_) => SupervisorTurnState::Completed,
        Err(LoopError::Cancelled) => {
            session.mark_cancelled().await?;
            SupervisorTurnState::Cancelled
        }
        Err(error) => {
            session.mark_model_call_failed(&error.to_string()).await?;
            SupervisorTurnState::Failed
        }
    };
    if let Some((queue_id, _)) = prompt {
        let queue_status = match turn_state {
            SupervisorTurnState::Completed => "completed",
            SupervisorTurnState::Waiting => "waiting",
            SupervisorTurnState::Cancelled => "cancelled",
            SupervisorTurnState::Failed => "failed",
            _ => "completed",
        };
        state.control.finish_prompt(queue_id, queue_status).await?;
    }
    state.control.set_turn_state(session_id, turn_state).await?;
    refresh_actor(&state, &task_actor, &session).await?;
    let message = match turn_state {
        SupervisorTurnState::Waiting => "Task needs input",
        SupervisorTurnState::Completed => "Task completed",
        SupervisorTurnState::Failed => "Task failed",
        SupervisorTurnState::Cancelled => "Task stopped",
        _ => "Task updated",
    };
    if matches!(
        turn_state,
        SupervisorTurnState::Waiting
            | SupervisorTurnState::Completed
            | SupervisorTurnState::Failed
            | SupervisorTurnState::Cancelled
    ) {
        let _ = state.events.send(SupervisorEvent::Attention {
            session_id,
            state: turn_state,
            message: message.into(),
        });
    }
    Ok(!matches!(
        turn_state,
        SupervisorTurnState::Waiting | SupervisorTurnState::Cancelled
    ))
}

async fn actor(
    state: &SupervisorState,
    session_id: SessionId,
) -> Result<Arc<TaskActor>, RepositorySupervisorError> {
    state
        .actors
        .read()
        .await
        .get(&session_id)
        .cloned()
        .ok_or(RepositorySupervisorError::NoActor(session_id))
}

async fn refresh_actor(
    state: &SupervisorState,
    task_actor: &TaskActor,
    session: &AgentSession,
) -> Result<(), RepositorySupervisorError> {
    let task = state.control.task(session.session_id).await?;
    let snapshot = TaskRuntimeSnapshot {
        task,
        session: SessionSnapshot::capture(session),
        transcript: TranscriptSnapshot::capture(session),
    };
    *task_actor.snapshot.write().await = snapshot.clone();
    let _ = state
        .events
        .send(SupervisorEvent::TaskUpdated(Box::new(snapshot)));
    Ok(())
}

async fn publish_actor(
    state: &SupervisorState,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(state, session_id).await?;
    let task = state.control.task(session_id).await?;
    let mut snapshot = task_actor.snapshot.write().await;
    snapshot.task = task;
    let _ = state
        .events
        .send(SupervisorEvent::TaskUpdated(Box::new(snapshot.clone())));
    Ok(())
}

async fn snapshots(state: &SupervisorState) -> Vec<TaskRuntimeSnapshot> {
    let actors: Vec<_> = state.actors.read().await.values().cloned().collect();
    let mut snapshots = Vec::with_capacity(actors.len());
    for task_actor in actors {
        snapshots.push(task_actor.snapshot.read().await.clone());
    }
    snapshots.sort_by_key(|snapshot| {
        (
            snapshot.task.lifecycle != SessionLifecycle::Active,
            snapshot.task.slot.unwrap_or(u8::MAX),
            snapshot.task.created_at,
        )
    });
    snapshots
}

fn same_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use forge_config::Config;
    use forge_model::MockModelClient;
    use forge_types::ModelResponse;
    use tempfile::TempDir;

    fn text_response(text: &str) -> ModelResponse {
        ModelResponse {
            text: text.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    async fn scripted_session(cfg: &Config, text: &str) -> AgentSession {
        let model: Arc<dyn ModelClient> =
            Arc::new(MockModelClient::script(vec![text_response(text)]));
        open_session_with_model(cfg, SessionTarget::New, model)
            .await
            .unwrap()
            .session
    }

    fn task_for(session_id: SessionId, label: &str, workspace: &std::path::Path) -> RepositoryTask {
        RepositoryTask {
            session_id,
            label: label.into(),
            workspace: workspace.to_path_buf(),
            branch: format!("forge/{label}"),
            ownership: WorktreeOwnership::Managed,
            lifecycle: SessionLifecycle::Active,
            turn_state: SupervisorTurnState::Idle,
            slot: None,
            model_id: "mock".into(),
            route_id: "native".into(),
            reasoning_effort: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            archived_at: None,
        }
    }

    async fn wait_for_task_state(
        handle: &SupervisorHandle,
        session_id: SessionId,
        predicate: impl Fn(&TaskRuntimeSnapshot) -> bool,
    ) -> TaskRuntimeSnapshot {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut events = handle.subscribe();
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SupervisorEvent::TaskUpdated(snapshot))) => {
                    if snapshot.task.session_id == session_id && predicate(&snapshot) {
                        return *snapshot;
                    }
                }
                Ok(Ok(SupervisorEvent::Roster(roster))) => {
                    if let Some(snapshot) = roster.into_iter().find(|snapshot| {
                        snapshot.task.session_id == session_id && predicate(snapshot)
                    }) {
                        return snapshot;
                    }
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {}
            }
        }
        panic!("timed out waiting for supervisor state");
    }

    #[tokio::test]
    async fn actors_process_submissions_and_broadcast_stream_and_attention() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let first_workspace = temp.path().join("first");
        let second_workspace = temp.path().join("second");
        std::fs::create_dir_all(&first_workspace).unwrap();
        std::fs::create_dir_all(&second_workspace).unwrap();

        let mut first_cfg = Config {
            resolved_workspace: first_workspace.clone(),
            workspace_root: Some(first_workspace.display().to_string()),
            ..Default::default()
        };
        first_cfg.journal.path = temp.path().join("journals").display().to_string();
        let first_session = scripted_session(&first_cfg, "first answer").await;
        let first_id = first_session.session_id;
        let first_task = task_for(first_id, "first", &first_workspace);

        let mut second_cfg = first_cfg.clone();
        second_cfg.resolved_workspace = second_workspace.clone();
        second_cfg.workspace_root = Some(second_workspace.display().to_string());
        let second_session = scripted_session(&second_cfg, "second answer").await;
        let second_id = second_session.session_id;
        let second_task = task_for(second_id, "second", &second_workspace);

        for task in [&first_task, &second_task] {
            control
                .register_task(
                    NewRepositoryTask {
                        session_id: task.session_id,
                        label: task.label.clone(),
                        workspace: task.workspace.clone(),
                        branch: task.branch.clone(),
                        ownership: task.ownership,
                        slot: task.slot,
                        model_id: task.model_id.clone(),
                        route_id: task.route_id.clone(),
                        reasoning_effort: task.reasoning_effort.clone(),
                    },
                    None,
                )
                .await
                .unwrap();
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(vec![text_response(
            "shared answer",
        )]));
        let (_supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![(first_task, first_session), (second_task, second_session)],
            1,
            first_cfg,
            model,
        )
        .await
        .unwrap();

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: first_id,
                text: "run first".into(),
            })
            .await
            .unwrap();
        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: second_id,
                text: "run second".into(),
            })
            .await
            .unwrap();

        let first_done = wait_for_task_state(&handle, first_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        let second_done = wait_for_task_state(&handle, second_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;

        assert!(first_done
            .transcript
            .messages()
            .iter()
            .any(|message| message.content.contains("first answer")));
        assert!(second_done
            .transcript
            .messages()
            .iter()
            .any(|message| message.content.contains("second answer")));

        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    /// A supervisor rooted in a real repository, with trust redirected at a
    /// temporary store so granting it never touches the developer's own.
    async fn git_backed_supervisor(
        workspace: &std::path::Path,
        trust_store: &std::path::Path,
        journal: &std::path::Path,
    ) -> (Config, Arc<RepositoryControl>, SupervisorHandle) {
        let storage = RepositoryRuntimeStorage::new(workspace).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let lease = RepositoryLease::acquire(&control_dir, workspace).unwrap();

        let mut cfg = Config {
            resolved_workspace: workspace.to_path_buf(),
            workspace_root: Some(workspace.display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = journal.display().to_string();
        let model: Arc<dyn ModelClient> =
            Arc::new(MockModelClient::script(vec![text_response("done")]));
        let (_supervisor, handle) = RepositorySupervisor::spawn_with_trust_store(
            control.clone(),
            lease,
            Vec::new(),
            2,
            cfg.clone(),
            model,
            Some(trust_store.to_path_buf()),
        )
        .await
        .unwrap();
        (cfg, control, handle)
    }

    #[tokio::test]
    async fn a_first_prompt_runs_only_after_trust_finalizes_the_creation() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let mut events = handle.subscribe();
        handle
            .command(SupervisorCommand::CreateTask {
                label: "parser".into(),
                first_prompt: Some("rewrite the lexer".into()),
            })
            .await
            .unwrap();

        let operation_id = loop {
            match events.recv().await.unwrap() {
                SupervisorEvent::TrustRequired { operation_id, .. } => break operation_id,
                _ => continue,
            }
        };

        // Awaiting trust: the task exists but nothing is queued against it.
        let task = control
            .tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "parser")
            .expect("provisional task row");
        assert!(control
            .queued_prompts(task.session_id)
            .await
            .unwrap()
            .is_empty());

        handle
            .command(SupervisorCommand::FinalizeCreation { operation_id })
            .await
            .unwrap();

        wait_for_task_state(&handle, task.session_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        assert!(forge_config::is_trusted_at(&trust_store, &task.workspace));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_trust_removes_the_worktree_and_the_task_row() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let mut events = handle.subscribe();
        handle
            .command(SupervisorCommand::CreateTask {
                label: "parser".into(),
                first_prompt: Some("rewrite the lexer".into()),
            })
            .await
            .unwrap();
        let operation_id = loop {
            match events.recv().await.unwrap() {
                SupervisorEvent::TrustRequired { operation_id, .. } => break operation_id,
                _ => continue,
            }
        };
        let workspace = control
            .tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "parser")
            .expect("provisional task row")
            .workspace;

        handle
            .command(SupervisorCommand::CancelCreation { operation_id })
            .await
            .unwrap();

        assert!(!workspace.exists(), "cancelled worktree should be removed");
        assert!(control
            .tasks()
            .await
            .unwrap()
            .iter()
            .all(|task| task.label != "parser"));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn attach_refuses_the_main_worktree_a_foreign_path_and_a_wrong_branch() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, _control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let base = TempDir::new().unwrap();
        let linked =
            forge_storage::create_task_worktree(repo.path(), base.path(), 1, "linked").unwrap();

        let main_worktree = handle
            .command(SupervisorCommand::AttachWorktree {
                workspace: repo.path().to_path_buf(),
                label: "main".into(),
                branch: "main".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(
            main_worktree.contains("main worktree"),
            "unexpected error: {main_worktree}"
        );

        let foreign = handle
            .command(SupervisorCommand::AttachWorktree {
                workspace: scratch.path().to_path_buf(),
                label: "elsewhere".into(),
                branch: "whatever".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(
            foreign.contains("not a worktree of this repository"),
            "unexpected error: {foreign}"
        );

        let drifted = handle
            .command(SupervisorCommand::AttachWorktree {
                workspace: linked.path.clone(),
                label: "linked".into(),
                branch: "forge/not-the-branch".into(),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(
            drifted.contains(&linked.branch),
            "unexpected error: {drifted}"
        );

        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }
}
