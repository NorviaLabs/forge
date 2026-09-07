//! Concurrent repository session ownership behind a command/event API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use forge_config::Config;
use forge_core::{AgentSession, BackgroundTaskId, LoopError, ToolCall};
use forge_model::{client_from_config, ModelClient};
use forge_storage::{RepositoryRuntimeStorage, RuntimeDataKind, RuntimeStorage};
use forge_types::{
    AskUserQuestionResult, HitlDecision, ModelStreamEvent, SessionId, TaskLifecycle,
};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    connect_credentials, open_session_with_model, resolve_journal_dir, NewRepositorySession,
    RepositoryControl, RepositoryLease, RepositorySession, RepositorySessionError,
    SessionDetailsSnapshot, SessionLifecycle, SessionSnapshot, SessionTarget, SupervisorTurnState,
    TranscriptSnapshot, WorktreeOwnership,
};

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const ATTACH_SESSION_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct SessionRuntimeSnapshot {
    pub task: RepositorySession,
    pub session: SessionSnapshot,
    pub transcript: TranscriptSnapshot,
    pub details: Option<SessionDetailsSnapshot>,
    pub queued_prompts: Vec<(i64, String)>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SupervisorEvent {
    Roster(Vec<SessionRuntimeSnapshot>),
    /// Boxed: a snapshot is an order of magnitude larger than every other
    /// variant, and this event is broadcast on every turn transition.
    SessionUpdated(Box<SessionRuntimeSnapshot>),
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
    CreateSession {
        label: String,
        first_prompt: Option<String>,
    },
    AttachWorktree {
        workspace: PathBuf,
        label: String,
        branch: String,
    },
    ArchiveSession {
        session_id: SessionId,
    },
    RenameSession {
        session_id: SessionId,
        label: String,
    },
    PinSession {
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
        feedback: Option<String>,
    },
    ResolveQuestion {
        session_id: SessionId,
        answers: Option<AskUserQuestionResult>,
        actor: String,
    },
    SelectSession {
        session_id: Option<SessionId>,
    },
    SetModel {
        session_id: SessionId,
        model_id: String,
        route_id: String,
        reasoning_effort: Option<String>,
    },
    SetThinking {
        session_id: SessionId,
        enabled: bool,
    },
    SetCapabilities {
        session_id: SessionId,
        image_input_supported: bool,
        context_window: Option<(usize, Option<usize>)>,
    },
    CancelQueuedPrompt {
        session_id: SessionId,
        one_based: usize,
    },
    PollSession {
        session_id: SessionId,
    },
    CompactContext {
        session_id: SessionId,
    },
    CancelBackgroundTask {
        session_id: SessionId,
        task_id: BackgroundTaskId,
    },
    ResolveBackgroundApproval {
        session_id: SessionId,
        task_id: BackgroundTaskId,
        decision: HitlDecision,
    },
    GrantEgressHost {
        session_id: SessionId,
        pattern: String,
    },
    AllowSessionPattern {
        session_id: SessionId,
        call: ToolCall,
    },
    ClearSessionApprovals {
        session_id: SessionId,
    },
    ApplyProviderEnv {
        pairs: Vec<(String, String)>,
    },
    ClearProviderEnv,
    Refresh,
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepositorySupervisorError {
    #[error(transparent)]
    Control(#[from] RepositorySessionError),
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

    /// Queue a command from synchronous UI code without blocking the terminal
    /// owner. Failures in command execution are still published as supervisor
    /// events; this only reports whether the command entered the actor queue.
    pub fn try_command(
        &self,
        command: SupervisorCommand,
    ) -> Result<(), RepositorySupervisorError> {
        let (reply, _response) = oneshot::channel();
        self.commands
            .try_send(CommandEnvelope { command, reply })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => RepositorySupervisorError::Closed,
                mpsc::error::TrySendError::Full(_) => {
                    RepositorySupervisorError::Command("supervisor command queue is full".into())
                }
            })
    }
}

struct SessionActor {
    session: Mutex<AgentSession>,
    snapshot: RwLock<SessionRuntimeSnapshot>,
    driving: AtomicBool,
    running_cancel: StdMutex<Option<CancellationToken>>,
}

impl SessionActor {
    fn new(task: RepositorySession, session: AgentSession) -> Self {
        let snapshot = SessionRuntimeSnapshot {
            task,
            session: SessionSnapshot::capture(&session),
            transcript: TranscriptSnapshot::capture(&session),
            details: Some(SessionDetailsSnapshot::capture(&session)),
            queued_prompts: Vec::new(),
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
    actors: RwLock<HashMap<SessionId, Arc<SessionActor>>>,
    unavailable_tasks: RwLock<Vec<RepositorySession>>,
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
        RepositorySupervisor::open_from_bootstrap(self, cfg, primary_session_id, None).await
    }

    /// Adopt the already-open primary session into the supervisor actor set,
    /// then discover and open its sibling sessions. This is the migration path
    /// for the TUI: repository ownership is still acquired before opening the
    /// primary, but once opened the primary is no longer special to the
    /// supervisor.
    pub async fn open_with_primary(
        self,
        cfg: &Config,
        primary: AgentSession,
    ) -> Result<(RepositorySupervisor, SupervisorHandle), RepositorySupervisorError> {
        let primary_session_id = primary.session_id;
        RepositorySupervisor::open_from_bootstrap(self, cfg, primary_session_id, Some(primary))
            .await
    }
}

impl RepositorySupervisor {
    pub async fn open_siblings(
        cfg: &Config,
        primary_session_id: SessionId,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let bootstrap = RepositoryBootstrap::acquire(cfg).await?;
        Self::open_from_bootstrap(bootstrap, cfg, primary_session_id, None).await
    }

    async fn open_from_bootstrap(
        bootstrap: RepositoryBootstrap,
        cfg: &Config,
        primary_session_id: SessionId,
        primary: Option<AgentSession>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let RepositoryBootstrap {
            lease,
            control,
            main_worktree,
        } = bootstrap;
        let worktrees = forge_storage::list_worktree_records(&main_worktree)?;
        control.reconcile_worktrees(&worktrees).await?;

        if control.session(primary_session_id).await.is_err() {
            let workspace = cfg.workspace_root().to_path_buf();
            for stale in control.sessions().await?.into_iter().filter(|task| {
                task.ownership == WorktreeOwnership::Primary
                    && same_path(&task.workspace, &workspace)
            }) {
                match stale.lifecycle {
                    SessionLifecycle::Active => {
                        control.mark_unavailable(stale.session_id).await?;
                        control.forget_unavailable_primary(stale.session_id).await?;
                    }
                    SessionLifecycle::Unavailable => {
                        control.forget_unavailable_primary(stale.session_id).await?
                    }
                    SessionLifecycle::Archived | SessionLifecycle::Removed => {}
                }
            }
            let branch = worktrees
                .iter()
                .find(|worktree| same_path(&worktree.path, &workspace))
                .and_then(|worktree| worktree.branch.clone())
                .ok_or(forge_storage::WorktreeError::DetachedHead)?;
            control
                .register_session(
                    NewRepositorySession {
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

        let unavailable_tasks = control
            .sessions()
            .await?
            .into_iter()
            .filter(|task| task.lifecycle == SessionLifecycle::Unavailable)
            .collect();

        let model: Arc<dyn ModelClient> = Arc::from(client_from_config(cfg)?);
        model.apply_provider_env(&connect_credentials());

        let mut sessions = Vec::new();
        if let Some(primary) = primary {
            let primary_record = control.session(primary_session_id).await?;
            sessions.push((primary_record, primary));
        }

        let mut session_tasks = tokio::task::JoinSet::new();
        for task in control.sessions().await?.into_iter().filter(|task| {
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
        while let Some(result) = session_tasks.join_next().await {
            sessions.push(
                result.map_err(|error| {
                    RepositorySupervisorError::Assembly(anyhow::anyhow!(error))
                })??,
            );
        }
        Self::spawn_with_unavailable_tasks(
            control,
            lease,
            sessions,
            unavailable_tasks,
            DEFAULT_MAX_CONCURRENCY,
            cfg.clone(),
            model,
            None,
        )
        .await
    }

    pub async fn open(cfg: &Config) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        Self::open_with_primary(cfg, None).await
    }

    pub async fn open_with_primary(
        cfg: &Config,
        mut primary: Option<AgentSession>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let storage = RepositoryRuntimeStorage::new(cfg.workspace_root())?;
        let control_dir = storage.path_for(RuntimeDataKind::Control)?;
        let lease = RepositoryLease::acquire(&control_dir, cfg.workspace_root())?;
        let control = Arc::new(RepositoryControl::open(&control_dir).await?);
        let worktrees = forge_storage::list_worktree_records(storage.main_worktree())?;
        control.reconcile_worktrees(&worktrees).await?;
        let unavailable_tasks = control
            .sessions()
            .await?
            .into_iter()
            .filter(|task| task.lifecycle == SessionLifecycle::Unavailable)
            .collect();

        let model: Arc<dyn ModelClient> = Arc::from(client_from_config(cfg)?);
        model.apply_provider_env(&connect_credentials());
        let mut tasks = control.sessions().await?;
        if tasks.is_empty() {
            let opened = match primary.take() {
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
                .register_session(
                    NewRepositorySession {
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
            tasks = control.sessions().await?;
            let task = tasks
                .iter()
                .find(|task| task.session_id == opened.session.session_id)
                .cloned()
                .ok_or(RepositorySessionError::NotFound(opened.session.session_id))?;
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

        let primary_session_id = primary.as_ref().map(|session| session.session_id);
        let mut sessions = Vec::new();
        if let Some(primary) = primary.take() {
            let record = tasks
                .iter()
                .find(|session| session.session_id == primary.session_id)
                .cloned()
                .ok_or(RepositorySessionError::NotFound(primary.session_id))?;
            sessions.push((record, primary));
        }
        for task in tasks.into_iter().filter(|task| {
            task.lifecycle == SessionLifecycle::Active
                && Some(task.session_id) != primary_session_id
        }) {
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
        Self::spawn_with_unavailable_tasks(
            control,
            lease,
            sessions,
            unavailable_tasks,
            DEFAULT_MAX_CONCURRENCY,
            cfg.clone(),
            model,
            None,
        )
        .await
    }

    pub async fn spawn(
        control: Arc<RepositoryControl>,
        lease: RepositoryLease,
        sessions: Vec<(RepositorySession, AgentSession)>,
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
        sessions: Vec<(RepositorySession, AgentSession)>,
        max_concurrency: usize,
        cfg: Config,
        model: Arc<dyn ModelClient>,
        trust_store: Option<PathBuf>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        Self::spawn_with_unavailable_tasks(
            control,
            lease,
            sessions,
            Vec::new(),
            max_concurrency,
            cfg,
            model,
            trust_store,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_with_unavailable_tasks(
        control: Arc<RepositoryControl>,
        lease: RepositoryLease,
        sessions: Vec<(RepositorySession, AgentSession)>,
        unavailable_tasks: Vec<RepositorySession>,
        max_concurrency: usize,
        cfg: Config,
        model: Arc<dyn ModelClient>,
        trust_store: Option<PathBuf>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let (events, _) = broadcast::channel(512);
        let actors = sessions
            .into_iter()
            .map(|(task, session)| (task.session_id, Arc::new(SessionActor::new(task, session))))
            .collect();
        let state = Arc::new(SupervisorState {
            cfg,
            model,
            control,
            actors: RwLock::new(actors),
            unavailable_tasks: RwLock::new(unavailable_tasks),
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

    pub async fn snapshots(&self) -> Vec<SessionRuntimeSnapshot> {
        snapshots(&self.state).await
    }

    pub async fn snapshot(&self, session_id: SessionId) -> Option<SessionRuntimeSnapshot> {
        let actor = self.state.actors.read().await.get(&session_id).cloned()?;
        let snapshot = actor.snapshot.read().await.clone();
        Some(snapshot)
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
        SupervisorCommand::CreateSession {
            label,
            first_prompt,
        } => {
            // An empty label is legal: the task starts unnamed and is named
            // from its first prompt. A prompt supplied here (the form path)
            // is used directly; the TUI renames an unnamed task when the
            // operator types its first message instead.
            let label = if label.trim().is_empty() {
                first_prompt
                    .as_deref()
                    .map(forge_storage::label_from_prompt)
                    .unwrap_or_default()
            } else {
                label
            };
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
            let worktree = forge_storage::create_session_worktree(
                &state.cfg.resolved_workspace,
                &base_dir,
                pending.operation_id,
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
            let task = RepositorySession {
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
                .register_session(
                    NewRepositorySession {
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
            let actor = Arc::new(SessionActor::new(task.clone(), opened.session));
            state.actors.write().await.insert(task.session_id, actor);
            if first_prompt.is_some() {
                // A parked prompt runs as soon as the operator confirms
                // trust, so the creation pauses on the trust modal. The
                // prompt stays parked on the pending operation until
                // `FinalizeCreation`; queueing it here would be rejected by
                // the `awaiting_trust` guard, and bypassing that guard
                // would run a model turn in a worktree the operator has not
                // confirmed.
                let _ = state.events.send(SupervisorEvent::TrustRequired {
                    operation_id: pending.operation_id,
                    label: task.label,
                    workspace: task.workspace,
                });
            } else {
                // Prompt-less creation is one keypress and nothing can run
                // until the operator types in the task, so no modal: record
                // the trust grant now and finalize immediately. A failure to
                // persist rolls the creation back rather than leaving a task
                // that looks trusted but is not recorded.
                let completed = state
                    .control
                    .complete_creation(pending.operation_id)
                    .await?;
                if let Some(session_id) = completed.session_id {
                    let workspace = state.control.session(session_id).await?.workspace;
                    if let Err(error) = state.grant_trust(&workspace) {
                        let _ = rollback_creation(&state, pending.operation_id).await;
                        let _ = state
                            .events
                            .send(SupervisorEvent::Roster(snapshots(&state).await));
                        return Err(error);
                    }
                }
            }
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
            let opened = tokio::time::timeout(
                ATTACH_SESSION_INIT_TIMEOUT,
                open_session_with_model(&task_cfg, SessionTarget::New, state.model.clone()),
            )
            .await
            .map_err(|_| {
                RepositorySupervisorError::Command(format!(
                    "timed out initializing attached worktree `{}`",
                    workspace.display()
                ))
            })??;
            let task = state
                .control
                .attach_worktree(
                    NewRepositorySession {
                        session_id: opened.session.session_id,
                        label,
                        workspace,
                        branch,
                        ownership: WorktreeOwnership::Attached,
                        slot: None,
                        model_id: opened.session.active_model.clone(),
                        route_id: opened.session.active_route_id.clone(),
                        reasoning_effort: None,
                    },
                    None,
                )
                .await?;
            state.actors.write().await.insert(
                task.session_id,
                Arc::new(SessionActor::new(task.clone(), opened.session)),
            );
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::ArchiveSession { session_id } => {
            let task = state.control.session(session_id).await?;
            if task.ownership == WorktreeOwnership::Primary {
                return Err(RepositorySupervisorError::Command(
                    "the primary session cannot be archived".into(),
                ));
            }
            state.control.archive_session(session_id).await?;
            state.actors.write().await.remove(&session_id);
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::RenameSession { session_id, label } => {
            state.control.rename_session(session_id, &label).await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::PinSession {
            session_id,
            slot,
            swap,
        } => {
            state.control.pin_session(session_id, slot, swap).await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::RemoveManagedWorktree { session_id } => {
            let task = state.control.session(session_id).await?;
            if task.ownership != WorktreeOwnership::Managed {
                return Err(RepositorySupervisorError::Command(
                    "only Forge-managed worktrees can be removed".into(),
                ));
            }
            let actors = state.actors.read().await;
            if actors.contains_key(&session_id) {
                return Err(RepositorySupervisorError::Command(
                    "archive the session before removing its worktree".into(),
                ));
            }
            drop(actors);
            forge_storage::remove_clean_worktree(&state.cfg.resolved_workspace, &task.workspace)?;
            state.control.remove_session(session_id).await?;
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::FinalizeCreation { operation_id } => {
            finalize_creation(&state, operation_id).await?;
        }
        SupervisorCommand::CancelCreation { operation_id } => {
            rollback_creation(&state, operation_id).await?;
        }
        SupervisorCommand::TrustWorkspace { workspace } => {
            state.grant_trust(&workspace)?;
        }
        SupervisorCommand::SubmitPrompt { session_id, text } => {
            submit_prompt(&state, session_id, text).await?;
        }
        SupervisorCommand::ContinueTurn { session_id } => {
            continue_turn(&state, session_id).await?;
        }
        SupervisorCommand::StopTurn { session_id } => {
            let actor = actor(&state, session_id).await?;
            if !actor.request_cancel() {
                state.control.mark_turn_stopped(session_id).await?;
                refresh_actor_from_state(&state, &actor).await?;
            }
        }
        SupervisorCommand::ResolveApproval {
            session_id,
            decision,
            actor: approval_actor,
            feedback,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.resolve_hitl_with_feedback(decision, approval_actor, feedback).await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ResolveQuestion {
            session_id,
            answers,
            actor: question_actor,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.resolve_question(answers, question_actor).await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::SelectSession { session_id } => {
            state.control.select_session(session_id).await?;
            let _ = state.events.send(SupervisorEvent::Selected(session_id));
        }
        SupervisorCommand::SetModel {
            session_id,
            model_id,
            route_id,
            reasoning_effort,
        } => {
            let task_actor = actor(&state, session_id).await?;
            {
                let mut session = task_actor.session.lock().await;
                session.set_active_model(model_id.clone());
                session.set_active_route_id(route_id.clone());
                session.set_reasoning_effort(reasoning_effort.clone());
                refresh_actor(&state, &task_actor, &session).await?;
            }
            state
                .control
                .set_model(session_id, &model_id, &route_id, reasoning_effort.as_deref())
                .await?;
        }
        SupervisorCommand::SetThinking {
            session_id,
            enabled,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.set_thinking_enabled(enabled);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::SetCapabilities {
            session_id,
            image_input_supported,
            context_window,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.set_image_input_supported(image_input_supported);
            if let Some((capacity, output)) = context_window {
                session.set_context_window(capacity, output);
            }
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::CancelQueuedPrompt {
            session_id,
            one_based,
        } => {
            state
                .control
                .cancel_queued_prompt_at(session_id, one_based)
                .await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::PollSession { session_id } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.poll_background_tasks().await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::CompactContext { session_id } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            let pending =
                session.begin_context_compaction(forge_core::CompactionTrigger::Manual);
            let completed = pending.execute().await;
            session.finish_context_compaction(completed).await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::CancelBackgroundTask {
            session_id,
            task_id,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.cancel_background_task(task_id);
            session.poll_background_tasks().await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ResolveBackgroundApproval {
            session_id,
            task_id,
            decision,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.resolve_subagent_hitl(task_id, decision);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::GrantEgressHost {
            session_id,
            pattern,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.grant_egress_host(&pattern);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::AllowSessionPattern { session_id, call } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.allow_session_pattern(&call);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ClearSessionApprovals { session_id } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = task_actor.session.lock().await;
            session.clear_session_approvals();
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ApplyProviderEnv { pairs } => {
            for (key, value) in pairs {
                state.model.set_provider_env(&key, &value);
            }
        }
        SupervisorCommand::ClearProviderEnv => {
            state.model.clear_provider_env();
        }
        SupervisorCommand::Refresh => {
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::Shutdown => {}
    }
    Ok(())
}

async fn actor(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<Arc<SessionActor>, RepositorySupervisorError> {
    state
        .actors
        .read()
        .await
        .get(&session_id)
        .cloned()
        .ok_or(RepositorySupervisorError::NoActor(session_id))
}

async fn snapshots(state: &Arc<SupervisorState>) -> Vec<SessionRuntimeSnapshot> {
    let actors: Vec<Arc<SessionActor>> = state.actors.read().await.values().cloned().collect();
    let mut snapshots = Vec::with_capacity(actors.len() + state.unavailable_tasks.read().await.len());
    for actor in actors {
        snapshots.push(actor.snapshot.read().await.clone());
    }
    for task in state.unavailable_tasks.read().await.iter().cloned() {
        snapshots.push(SessionRuntimeSnapshot {
            session: SessionSnapshot::unavailable(task.session_id, task.workspace.clone()),
            transcript: TranscriptSnapshot::default(),
            details: None,
            queued_prompts: Vec::new(),
            task,
        });
    }
    snapshots.sort_by_key(|snapshot| {
        (
            snapshot.task.slot.unwrap_or(u8::MAX),
            snapshot.task.created_at,
        )
    });
    snapshots
}

async fn publish_actor(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(state, session_id).await?;
    let session = task_actor.session.lock().await;
    refresh_actor(state, &task_actor, &session).await
}

async fn refresh_actor_from_state(
    state: &Arc<SupervisorState>,
    task_actor: &Arc<SessionActor>,
) -> Result<(), RepositorySupervisorError> {
    let session = task_actor.session.lock().await;
    refresh_actor(state, task_actor, &session).await
}

async fn refresh_actor(
    state: &Arc<SupervisorState>,
    task_actor: &Arc<SessionActor>,
    session: &AgentSession,
) -> Result<(), RepositorySupervisorError> {
    let session_id = session.session_id;
    let task = state.control.session(session_id).await?;
    let queued_prompts = state.control.queued_prompts(session_id).await?;
    let mut snapshot = task_actor.snapshot.write().await;
    snapshot.task = task;
    snapshot.session = SessionSnapshot::capture(session);
    snapshot.transcript = TranscriptSnapshot::capture(session);
    snapshot.details = Some(SessionDetailsSnapshot::capture(session));
    snapshot.queued_prompts = queued_prompts
        .into_iter()
        .map(|item| (item.id, item.text))
        .collect();
    let _ = state
        .events
        .send(SupervisorEvent::SessionUpdated(Box::new(snapshot.clone())));
    Ok(())
}

async fn submit_prompt(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
    text: String,
) -> Result<(), RepositorySupervisorError> {
    state.control.enqueue_prompt(session_id, &text).await?;
    start_or_queue(state, session_id).await
}

async fn continue_turn(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task = state.control.session(session_id).await?;
    if matches!(
        task.turn_state,
        SupervisorTurnState::WaitingApproval | SupervisorTurnState::WaitingQuestion
    ) {
        return Ok(());
    }
    start_or_queue(state, session_id).await
}

async fn start_or_queue(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(state, session_id).await?;
    if task_actor
        .driving
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    let state = state.clone();
    tokio::spawn(async move {
        let _ = drive_session(state.clone(), task_actor.clone(), session_id).await;
        task_actor.driving.store(false, Ordering::Release);
    });
    Ok(())
}

async fn drive_session(
    state: Arc<SupervisorState>,
    task_actor: Arc<SessionActor>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let _permit = state.permits.acquire().await.map_err(|_| {
        RepositorySupervisorError::Command("repository concurrency limiter closed".into())
    })?;
    loop {
        let task = state.control.session(session_id).await?;
        if task.lifecycle != SessionLifecycle::Active {
            return Ok(());
        }
        if matches!(
            task.turn_state,
            SupervisorTurnState::WaitingApproval | SupervisorTurnState::WaitingQuestion
        ) {
            refresh_actor_from_state(&state, &task_actor).await?;
            return Ok(());
        }
        let Some(prompt) = state.control.claim_next_prompt(session_id).await? else {
            if task.turn_state == SupervisorTurnState::Running {
                state.control.mark_turn_stopped(session_id).await?;
            }
            refresh_actor_from_state(&state, &task_actor).await?;
            return Ok(());
        };
        {
            let session = task_actor.session.lock().await;
            if session.pending_hitl().is_some() || session.pending_question().is_some() {
                refresh_actor(&state, &task_actor, &session).await?;
                return Ok(());
            }
        }
        state.control.mark_turn_running(session_id).await?;
        refresh_actor_from_state(&state, &task_actor).await?;

        let cancel = CancellationToken::new();
        {
            let mut running = task_actor
                .running_cancel
                .lock()
                .expect("turn cancel lock poisoned");
            *running = Some(cancel.clone());
        }
        let stream = state.events.clone();
        let result = {
            let mut session = task_actor.session.lock().await;
            session
                .run_turn_streaming_with_cancel(&prompt.text, cancel, |event| {
                    let _ = stream.send(SupervisorEvent::Stream {
                        session_id,
                        event: event.clone(),
                    });
                })
                .await
        };
        {
            let mut running = task_actor
                .running_cancel
                .lock()
                .expect("turn cancel lock poisoned");
            *running = None;
        }
        match result {
            Ok(_) => {
                state.control.complete_prompt(prompt.id).await?;
                state.control.mark_turn_stopped(session_id).await?;
                refresh_actor_from_state(&state, &task_actor).await?;
            }
            Err(error) if error.is_hitl_pending() => {
                state.control.requeue_prompt(prompt.id).await?;
                state.control.mark_waiting_approval(session_id).await?;
                refresh_actor_from_state(&state, &task_actor).await?;
                let _ = state.events.send(SupervisorEvent::Attention {
                    session_id,
                    state: SupervisorTurnState::WaitingApproval,
                    message: "approval required".into(),
                });
                return Ok(());
            }
            Err(error) if error.is_question_pending() => {
                state.control.requeue_prompt(prompt.id).await?;
                state.control.mark_waiting_question(session_id).await?;
                refresh_actor_from_state(&state, &task_actor).await?;
                let _ = state.events.send(SupervisorEvent::Attention {
                    session_id,
                    state: SupervisorTurnState::WaitingQuestion,
                    message: "question requires an answer".into(),
                });
                return Ok(());
            }
            Err(error) if error.is_cancelled() => {
                state.control.requeue_prompt(prompt.id).await?;
                state.control.mark_turn_stopped(session_id).await?;
                refresh_actor_from_state(&state, &task_actor).await?;
                return Ok(());
            }
            Err(error) => {
                state.control.complete_prompt(prompt.id).await?;
                state.control.mark_turn_stopped(session_id).await?;
                refresh_actor_from_state(&state, &task_actor).await?;
                let _ = state.events.send(SupervisorEvent::Error {
                    session_id: Some(session_id),
                    message: error.to_string(),
                });
            }
        }
    }
}

async fn finalize_creation(
    state: &Arc<SupervisorState>,
    operation_id: u64,
) -> Result<(), RepositorySupervisorError> {
    let pending = state.control.pending_creation(operation_id).await?;
    let workspace = pending.workspace.clone().ok_or_else(|| {
        RepositorySupervisorError::Command("pending task has no worktree".into())
    })?;
    state.grant_trust(&workspace)?;
    let completed = state.control.complete_creation(operation_id).await?;
    if let Some(session_id) = completed.session_id {
        if let Some(prompt) = completed.first_prompt {
            submit_prompt(state, session_id, prompt).await?;
        }
    }
    let _ = state
        .events
        .send(SupervisorEvent::Roster(snapshots(state).await));
    Ok(())
}

async fn rollback_creation(
    state: &Arc<SupervisorState>,
    operation_id: u64,
) -> Result<(), RepositorySupervisorError> {
    let pending = state.control.pending_creation(operation_id).await?;
    state
        .control
        .cancel_creation(operation_id, "operator rejected worktree trust")
        .await?;
    if let Some(session_id) = pending.session_id {
        state.actors.write().await.remove(&session_id);
    }
    if let Some(workspace) = pending.workspace {
        if workspace.is_dir() {
            forge_storage::remove_clean_worktree(&state.cfg.resolved_workspace, &workspace)?;
        }
    }
    let _ = state
        .events
        .send(SupervisorEvent::Roster(snapshots(state).await));
    Ok(())
}

fn validate_attach_target(
    state: &Arc<SupervisorState>,
    workspace: &std::path::Path,
    branch: &str,
) -> Result<PathBuf, RepositorySupervisorError> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| RepositorySupervisorError::Command(error.to_string()))?;
    let records = forge_storage::list_worktree_records(&state.cfg.resolved_workspace)?;
    let record = records
        .iter()
        .find(|record| same_path(&record.path, &workspace))
        .ok_or_else(|| {
            RepositorySupervisorError::Command(format!(
                "{} is not a worktree in this repository",
                workspace.display()
            ))
        })?;
    let actual_branch = record.branch.as_deref().ok_or_else(|| {
        RepositorySupervisorError::Command("detached worktrees cannot be attached".into())
    })?;
    if actual_branch != branch {
        return Err(RepositorySupervisorError::Command(format!(
            "worktree branch is `{actual_branch}`, not `{branch}`"
        )));
    }
    let main = forge_storage::RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace)?
        .main_worktree()
        .to_path_buf();
    if same_path(&workspace, &main) {
        return Err(RepositorySupervisorError::Command(
            "the main worktree is already the primary task".into(),
        ));
    }
    Ok(workspace)
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.canonicalize().ok() == b.canonicalize().ok()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use forge_model::{ModelError, ModelRequest, ModelResponse, ModelStream};
    use forge_types::ModelStreamEvent;
    use futures::stream;
    use tempfile::TempDir;

    use super::*;

    struct MockModel {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ModelClient for MockModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                content: "done".into(),
                ..Default::default()
            })
        }

        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(stream::iter(vec![
                Ok(ModelStreamEvent::TextDelta {
                    text: "done".into(),
                }),
                Ok(ModelStreamEvent::Done {
                    input_tokens: 1,
                    output_tokens: 1,
                    cached_input_tokens: 0,
                }),
            ])))
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    fn setup_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "--initial-branch=main"]);
        git(dir.path(), &["config", "user.email", "forge@example.com"]);
        git(dir.path(), &["config", "user.name", "Forge Test"]);
        std::fs::write(dir.path().join("README.md"), "test\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        dir
    }

    fn test_config(dir: &TempDir) -> Config {
        let mut cfg = Config::default();
        cfg.resolved_workspace = dir.path().canonicalize().unwrap();
        cfg.workspace_root = Some(cfg.resolved_workspace.display().to_string());
        cfg
    }

    async fn seeded_supervisor(
        dir: &TempDir,
        model: Arc<dyn ModelClient>,
    ) -> (RepositorySupervisor, SupervisorHandle, SessionId) {
        let cfg = test_config(dir);
        let storage = RepositoryRuntimeStorage::new(&cfg.resolved_workspace).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let lease = RepositoryLease::acquire(&control_dir, &cfg.resolved_workspace).unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let mut session =
            forge_core::AgentSession::new_for_test(cfg.resolved_workspace.clone(), model.clone())
                .await
                .unwrap();
        session.set_active_model("mock/model".into());
        let branch = "main".to_string();
        let task = control
            .register_session(
                NewRepositorySession {
                    session_id: session.session_id,
                    label: "primary".into(),
                    workspace: cfg.resolved_workspace.clone(),
                    branch,
                    ownership: WorktreeOwnership::Primary,
                    slot: Some(1),
                    model_id: "mock/model".into(),
                    route_id: "native".into(),
                    reasoning_effort: None,
                },
                None,
            )
            .await
            .unwrap();
        let id = session.session_id;
        let (supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![(task, session)],
            4,
            cfg,
            model,
        )
        .await
        .unwrap();
        (supervisor, handle, id)
    }

    async fn wait_for<F>(mut events: broadcast::Receiver<SupervisorEvent>, predicate: F)
    where
        F: Fn(&SupervisorEvent) -> bool,
    {
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                _ = &mut timeout => panic!("timed out waiting for supervisor state"),
                Ok(event) = events.recv() => {
                    if predicate(&event) {
                        return;
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn actors_process_submissions_and_broadcast_stream_and_attention() {
        let dir = setup_repo();
        let model = Arc::new(MockModel {
            calls: AtomicUsize::new(0),
        });
        let (_supervisor, handle, session_id) = seeded_supervisor(&dir, model.clone()).await;
        let events = handle.subscribe();
        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id,
                text: "hello".into(),
            })
            .await
            .unwrap();
        wait_for(events, |event| {
            matches!(
                event,
                SupervisorEvent::Stream {
                    session_id: id,
                    event: ModelStreamEvent::TextDelta { .. },
                } if *id == session_id
            )
        })
        .await;
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn primary_session_can_be_adopted_into_the_actor_roster() {
        let dir = setup_repo();
        let cfg = test_config(&dir);
        let model: Arc<dyn ModelClient> = Arc::new(MockModel {
            calls: AtomicUsize::new(0),
        });
        let storage = RepositoryRuntimeStorage::new(&cfg.resolved_workspace).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let bootstrap = RepositoryBootstrap::acquire(&cfg).await.unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let mut primary = AgentSession::new_for_test(cfg.resolved_workspace.clone(), model)
            .await
            .unwrap();
        primary.set_active_model("mock/model".into());
        let primary_id = primary.session_id;
        control
            .register_session(
                NewRepositorySession {
                    session_id: primary_id,
                    label: "main".into(),
                    workspace: cfg.resolved_workspace.clone(),
                    branch: "main".into(),
                    ownership: WorktreeOwnership::Primary,
                    slot: Some(1),
                    model_id: "mock/model".into(),
                    route_id: "native".into(),
                    reasoning_effort: None,
                },
                None,
            )
            .await
            .unwrap();
        drop(control);

        let (supervisor, _handle) = bootstrap.open_with_primary(&cfg, primary).await.unwrap();
        let snapshot = supervisor.snapshot(primary_id).await.unwrap();
        assert_eq!(snapshot.task.session_id, primary_id);
        assert_eq!(snapshot.task.ownership, WorktreeOwnership::Primary);
    }
}
