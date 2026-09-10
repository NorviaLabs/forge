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
    AskUserQuestionResult, BackgroundTaskId, HitlDecision, ModelStreamEvent, SessionId,
    TaskLifecycle, ToolCall,
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
    pub queued_prompts: Vec<(u64, String)>,
    /// Prompts whose dispatch crossed the claim boundary but never reached a
    /// durable terminal result. These require explicit operator review and
    /// are never fed back into the runnable queue automatically.
    pub interrupted_prompts: Vec<(u64, String)>,
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
        session_id: SessionId,
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
    SubmitPromptWithAttachments {
        session_id: SessionId,
        text: String,
        attachments: Vec<forge_types::ImageRef>,
    },
    ContinueTurn {
        session_id: SessionId,
    },
    ForkSession {
        session_id: SessionId,
    },
    ResumeSession {
        current_session_id: SessionId,
        session_id: SessionId,
    },
    /// Stop managing one session without archiving its durable journal or
    /// worktree. The session can be resumed by a later supervisor.
    CloseSession {
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
    #[error("Session `{0}` has no live actor")]
    NoActor(SessionId),
    #[error("supervisor command channel closed")]
    Closed,
    #[error("session actor is busy running a turn")]
    Contention,
    #[error("session `{0}` is retiring its workspace resources")]
    Retiring(SessionId),
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
    pub fn try_command(&self, command: SupervisorCommand) -> Result<(), RepositorySupervisorError> {
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
    retiring: AtomicBool,
    running_cancel: StdMutex<Option<CancellationToken>>,
    /// The session's own model client, so provider-env updates reach a busy
    /// actor without waiting on its session lock.
    model: Arc<dyn ModelClient>,
}

impl SessionActor {
    fn new(
        task: RepositorySession,
        session: AgentSession,
        queued_prompts: Vec<(u64, String)>,
        interrupted_prompts: Vec<(u64, String)>,
    ) -> Self {
        let model = session.model_client();
        let snapshot = SessionRuntimeSnapshot {
            task,
            session: SessionSnapshot::capture(&session),
            transcript: TranscriptSnapshot::capture(&session),
            details: Some(SessionDetailsSnapshot::capture(&session)),
            queued_prompts,
            interrupted_prompts,
        };
        Self {
            session: Mutex::new(session),
            snapshot: RwLock::new(snapshot),
            driving: AtomicBool::new(false),
            retiring: AtomicBool::new(false),
            running_cancel: StdMutex::new(None),
            model,
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

    fn begin_retirement(&self) -> bool {
        self.retiring
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish_retirement(&self) {
        self.retiring.store(false, Ordering::Release);
    }
}

struct SupervisorState {
    cfg: Config,
    model: Arc<dyn ModelClient>,
    control: Arc<RepositoryControl>,
    /// Forge-managed worktree/ref administration is shared Git authority.
    /// The repository lease excludes other Forge processes; this gate keeps
    /// detached create/attach/remove operations in this process from racing
    /// over the common worktree metadata. Explicit user `git` tools remain
    /// shared operator authority under the cooperative isolation contract.
    git_mutation: Arc<Mutex<()>>,
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

    /// The model selection a newly created session should inherit: the live
    /// selection of the selected session, else the primary worktree's session,
    /// else any open session, else the process config. The creating session is
    /// whichever one the operator is on, which the supervisor tracks via
    /// `selected()`; falling back to the primary keeps inheritance stable when
    /// the operator has not switched sessions yet.
    async fn inherited_model(&self) -> (String, String, Option<String>) {
        let candidates: Vec<SessionId> = self.actors.read().await.keys().cloned().collect();
        if candidates.is_empty() {
            return (self.cfg.model.model.clone(), "native".into(), None);
        }
        let selected = self.control.selected().await.ok().flatten();
        let primary = self
            .control
            .sessions()
            .await
            .map(|tasks| {
                tasks
                    .iter()
                    .find(|task| task.ownership == WorktreeOwnership::Primary)
                    .map(|task| task.session_id)
            })
            .ok()
            .flatten();
        let Some(id) = selected
            .filter(|id| candidates.contains(id))
            .or_else(|| primary.filter(|id| candidates.contains(id)))
            .or_else(|| candidates.first().copied())
        else {
            return (self.cfg.model.model.clone(), "native".into(), None);
        };
        let actor = self.actors.read().await.get(&id).cloned();
        let Some(actor) = actor else {
            return (self.cfg.model.model.clone(), "native".into(), None);
        };
        // Lock-free: the snapshot carries the same model selection the
        // session holds, so creating a session never waits out another
        // session's running turn.
        let snapshot = actor.snapshot.read().await.clone();
        match snapshot.details.as_ref() {
            Some(details) => (
                details.active_model.clone(),
                details.active_route_id.clone(),
                details.reasoning_effort.clone(),
            ),
            None => (self.cfg.model.model.clone(), "native".into(), None),
        }
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
/// [`RepositoryBootstrap::open_with_primary`].
pub struct RepositoryBootstrap {
    lease: RepositoryLease,
    control: Arc<RepositoryControl>,
    main_worktree: PathBuf,
    git_mutation: Arc<Mutex<()>>,
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
            git_mutation: Arc::new(Mutex::new(())),
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
                    let _git_mutation = self.git_mutation.lock().await;
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

    /// Adopt the already-open primary session into the supervisor actor set,
    /// then discover and open the other repository Sessions. Repository
    /// ownership is acquired before opening the primary, and the session is
    /// moved here rather than reopened from its journal.
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
            git_mutation,
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
                    .map(|mut opened| {
                        restore_task_model(&mut opened.session, &task);
                        (task, opened.session)
                    })
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
            git_mutation,
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
            let mut session = opened.session;
            restore_task_model(&mut session, &task);
            sessions.push((task, session));
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
            Arc::new(Mutex::new(())),
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
            Arc::new(Mutex::new(())),
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
        git_mutation: Arc<Mutex<()>>,
    ) -> Result<(Self, SupervisorHandle), RepositorySupervisorError> {
        let (events, _) = broadcast::channel(512);
        let recovered = control.reconcile_running_prompt_claims().await?;
        let mut actors = HashMap::with_capacity(sessions.len());
        for (mut task, session) in sessions {
            // `reconcile_running_prompt_claims` also repairs the durable task
            // row. Refresh the copy handed to the actor before its first
            // roster snapshot so a restart cannot show it as running.
            if recovered.iter().any(|(id, _, _)| *id == task.session_id) {
                task.turn_state = SupervisorTurnState::Interrupted;
            }
            let interrupted = control.interrupted_prompts(task.session_id).await?;
            let queued = control.queued_prompts(task.session_id).await?;
            actors.insert(
                task.session_id,
                Arc::new(SessionActor::new(task, session, queued, interrupted)),
            );
        }
        let state = Arc::new(SupervisorState {
            cfg,
            model,
            control,
            git_mutation,
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
        for (session_id, _, _) in recovered {
            let _ = supervisor.state.events.send(SupervisorEvent::Attention {
                session_id,
                state: SupervisorTurnState::Interrupted,
                message: "A prompt was interrupted; review it before retrying".into(),
            });
        }
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
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(200));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut deferred = std::collections::VecDeque::new();
    let mut operations = tokio::task::JoinSet::new();
    loop {
        while operations.try_join_next().is_some() {}
        // New arrivals win over deferred contention retries, so a command
        // waiting on a busy actor never head-of-line-blocks unrelated
        // sessions (or a Shutdown) behind it.
        let envelope = match receiver.try_recv() {
            Ok(envelope) => envelope,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => match deferred.pop_front() {
                Some(envelope) => envelope,
                None => tokio::select! {
                    envelope = receiver.recv() => match envelope {
                        Some(envelope) => envelope,
                        None => break,
                    },
                    _ = poll.tick() => {
                        poll_background_tasks(&state).await;
                        continue;
                    }
                },
            },
        };
        let shutdown = matches!(envelope.command, SupervisorCommand::Shutdown);
        if runs_outside_command_loop(&envelope.command) {
            let command = envelope.command.clone();
            let reply = envelope.reply;
            let session_id = command_session_id(&command);
            let operation_state = state.clone();
            operations.spawn(async move {
                let result = execute_detached_command(operation_state.clone(), command).await;
                let result = result.map_err(|error| error.to_string());
                if let Err(message) = &result {
                    let _ = operation_state.events.send(SupervisorEvent::Error {
                        session_id,
                        message: message.clone(),
                    });
                }
                let _ = reply.send(result);
            });
            continue;
        }
        // Cloned: a contended command is requeued whole, so the loop keeps
        // ownership of the envelope.
        let command = execute_command(state.clone(), envelope.command.clone());
        tokio::pin!(command);
        // Keep stop requests reachable while an earlier mutation waits for a
        // running actor. Other commands retain their FIFO order.
        let result: Result<(), RepositorySupervisorError> = loop {
            tokio::select! {
                result = &mut command => break result,
                next = receiver.recv(), if !shutdown => {
                    if let Some(next) = next {
                        if let SupervisorCommand::StopTurn { session_id } = next.command {
                            let cancelled = actor(&state, session_id).await
                                .is_ok_and(|actor| actor.request_cancel());
                            if cancelled {
                                let _ = next.reply.send(Ok(()));
                            } else {
                                deferred.push_back(next);
                            }
                        } else {
                            if matches!(next.command, SupervisorCommand::Shutdown) {
                                for actor in state.actors.read().await.values() {
                                    actor.request_cancel();
                                }
                            }
                            deferred.push_back(next);
                        }
                    }
                }
            }
        };
        if matches!(result, Err(RepositorySupervisorError::Contention)) {
            // The target actor is mid-turn; requeue behind newer work and
            // retry when it frees. Sleep briefly so a long turn doesn't spin
            // the loop, but wake immediately on new arrivals.
            deferred.push_back(envelope);
            tokio::select! {
                biased;
                next = receiver.recv() => {
                    match next {
                        Some(next) => {
                            if matches!(next.command, SupervisorCommand::Shutdown) {
                                for actor in state.actors.read().await.values() {
                                    actor.request_cancel();
                                }
                                deferred.push_front(next);
                            } else {
                                deferred.push_back(next);
                            }
                        }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            }
            continue;
        }
        let result = result.map_err(|error| error.to_string());
        if let Err(message) = &result {
            let _ = state.events.send(SupervisorEvent::Error {
                session_id: None,
                message: message.clone(),
            });
        }
        let _ = envelope.reply.send(result);
        if shutdown {
            // Long-running operations own their reply channels and are tracked
            // here so shutdown does not leave a compaction, MCP startup, or
            // worktree mutation detached after the command loop exits.
            while operations.join_next().await.is_some() {}
            break;
        }
    }
}

/// Commands whose work can wait on a provider, MCP process, or Git operation
/// must not occupy the shared command dispatcher. Their operation task still
/// uses the actor mutex, so commands for the same session retain ordering;
/// unrelated sessions continue to receive commands while the operation waits.
fn runs_outside_command_loop(command: &SupervisorCommand) -> bool {
    matches!(
        command,
        SupervisorCommand::CreateSession { .. }
            | SupervisorCommand::AttachWorktree { .. }
            | SupervisorCommand::RemoveManagedWorktree { .. }
            | SupervisorCommand::CompactContext { .. }
            | SupervisorCommand::CloseSession { .. }
    )
}

fn command_session_id(command: &SupervisorCommand) -> Option<SessionId> {
    match command {
        SupervisorCommand::RemoveManagedWorktree { session_id }
        | SupervisorCommand::CompactContext { session_id }
        | SupervisorCommand::CloseSession { session_id } => Some(*session_id),
        _ => None,
    }
}

async fn execute_detached_command(
    state: Arc<SupervisorState>,
    command: SupervisorCommand,
) -> Result<(), RepositorySupervisorError> {
    loop {
        match execute_command(state.clone(), command.clone()).await {
            Err(RepositorySupervisorError::Contention) => {
                // A turn or another operation owns this actor. Retrying in
                // the operation task keeps the shared command loop available
                // and preserves the existing FIFO contention behavior.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            result => return result,
        }
    }
}

/// Lock a session actor without stalling the shared command loop. The turn
/// driver holds this mutex for the whole turn, so a blocking wait here would
/// head-of-line-block every unrelated session behind it. Callers get
/// [`RepositorySupervisorError::Contention`] instead, and `run_commands`
/// requeues the command until the actor frees.
fn try_session(
    task_actor: &SessionActor,
) -> Result<tokio::sync::MutexGuard<'_, AgentSession>, RepositorySupervisorError> {
    if task_actor.retiring.load(Ordering::Acquire) {
        return Err(RepositorySupervisorError::Command(
            "session actor is retiring its workspace resources".into(),
        ));
    }
    task_actor
        .session
        .try_lock()
        .map_err(|_| RepositorySupervisorError::Contention)
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
            // Worktree allocation and repository-local runtime setup both
            // update shared Git administration (the common exclude file is
            // one example). Keep the whole creation binding under the same
            // process-wide gate, including session assembly's journal setup,
            // so another managed attach/remove cannot observe half-complete
            // repository state.
            let _git_mutation = state.git_mutation.lock().await;
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
            let worktree =
                forge_storage::create_session_worktree(&state.cfg.resolved_workspace, &base_dir)?;
            state
                .control
                .mark_worktree_created(pending.operation_id, &worktree.path, &worktree.branch)
                .await?;
            let mut task_cfg = state.cfg.clone();
            task_cfg.resolved_workspace = worktree.path.clone();
            task_cfg.workspace_root = Some(worktree.path.display().to_string());
            let (journal_dir, _) = resolve_journal_dir(&task_cfg);
            task_cfg.journal.path = journal_dir.display().to_string();
            let mut opened =
                open_session_with_model(&task_cfg, SessionTarget::New, state.model.clone()).await?;
            // A fresh session must start on the model the operator is already
            // using, not the process config default: config usually carries no
            // model and the selection lives on the sessions. Inherit it from
            // the selected/primary session before the task row is written.
            let (model_id, route_id, reasoning_effort) = state.inherited_model().await;
            if !model_id.is_empty() {
                opened.session.set_active_model(model_id);
                opened.session.set_active_route_id(route_id);
                opened.session.set_reasoning_effort(reasoning_effort);
            }
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
                reasoning_effort: opened.session.reasoning_effort().map(str::to_string),
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
            let actor = Arc::new(SessionActor::new(
                task.clone(),
                opened.session,
                Vec::new(),
                Vec::new(),
            ));
            state.actors.write().await.insert(task.session_id, actor);
            drop(_git_mutation);
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
                    session_id: task.session_id,
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
            // Hold the Git administration gate from the initial ownership
            // check through session storage setup and the final durable bind.
            // The second validation remains deliberate: it checks the
            // immutable path/branch binding immediately before registration.
            let _git_mutation = state.git_mutation.lock().await;
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
                    "attaching `{}` timed out after {} seconds",
                    workspace.display(),
                    ATTACH_SESSION_INIT_TIMEOUT.as_secs()
                ))
            })??;
            let mut task = NewRepositorySession {
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
            // Recheck the immutable worktree binding immediately before the
            // durable attach. The first check selected the workspace for
            // session assembly; this one closes the gap where a concurrent
            // Forge-managed removal could otherwise detach it underneath us.
            task.workspace = validate_attach_target(&state, &task.workspace, &task.branch)?;
            state.control.register_session(task.clone(), None).await?;
            let actor_task = state.control.session(task.session_id).await?;
            state.actors.write().await.insert(
                task.session_id,
                Arc::new(SessionActor::new(
                    actor_task,
                    opened.session,
                    Vec::new(),
                    Vec::new(),
                )),
            );
            drop(_git_mutation);
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
        }
        SupervisorCommand::ArchiveSession { session_id } => {
            let task_actor = retire_actor_resources(&state, session_id, false).await?;
            if let Some(task_actor) = &task_actor {
                let session = task_actor.session.lock().await;
                if let Err(error) = refresh_actor(&state, task_actor, &session).await {
                    task_actor.finish_retirement();
                    return Err(error);
                }
            }
            let result = state.control.archive(session_id).await;
            if let Some(task_actor) = task_actor {
                task_actor.finish_retirement();
            }
            result?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::RenameSession { session_id, label } => {
            state.control.rename(session_id, &label).await?;
            publish_actor(&state, session_id).await?;
        }
        SupervisorCommand::PinSession {
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
            let task = state.control.session(session_id).await?;
            if task.lifecycle == SessionLifecycle::Removed {
                // A process may have been restarted after the durable state
                // was written but before the in-memory actor map was rebuilt.
                // Removed is already the terminal Git state, but a leftover
                // actor still needs the normal resource drain before it is
                // discarded.
                let task_actor = wait_for_actor_retirement(&state, session_id, true).await?;
                state.actors.write().await.remove(&session_id);
                drop(task_actor);
                let _ = state
                    .events
                    .send(SupervisorEvent::Roster(snapshots(&state).await));
                if state.control.selected().await.ok() == Some(Some(session_id)) {
                    let fallback = state.actors.read().await.keys().next().copied();
                    if let Err(error) = state.control.set_selected(fallback).await {
                        let _ = state.events.send(SupervisorEvent::Error {
                            session_id: Some(session_id),
                            message: format!("removed Session could not update selection: {error}"),
                        });
                    } else {
                        let _ = state.events.send(SupervisorEvent::Selected(fallback));
                    }
                }
                return Ok(());
            }
            if task.ownership != WorktreeOwnership::Managed
                || task.lifecycle != SessionLifecycle::Archived
            {
                return Err(RepositorySupervisorError::Command(
                    "only archived managed Sessions can remove a worktree".into(),
                ));
            }

            let task_actor = retire_actor_resources(&state, session_id, false).await?;

            let storage = match RepositoryRuntimeStorage::new(&state.cfg.resolved_workspace) {
                Ok(storage) => storage,
                Err(error) => {
                    if let Some(task_actor) = &task_actor {
                        task_actor.finish_retirement();
                    }
                    return Err(error.into());
                }
            };
            let removal = {
                let _git_mutation = state.git_mutation.lock().await;
                let worktrees = match forge_storage::list_worktree_records(storage.main_worktree())
                {
                    Ok(worktrees) => worktrees,
                    Err(error) => {
                        if let Some(task_actor) = &task_actor {
                            task_actor.finish_retirement();
                        }
                        return Err(error.into());
                    }
                };
                if worktrees
                    .iter()
                    .any(|worktree| same_path(&worktree.path, &task.workspace))
                {
                    forge_storage::remove_clean_worktree_if_branch(
                        storage.main_worktree(),
                        &task.workspace,
                        &task.branch,
                    )
                } else if task.workspace.exists() {
                    Err(forge_storage::WorktreeError::RemoveFailed(format!(
                        "{} is no longer a registered worktree; refusing to remove it",
                        task.workspace.display()
                    )))
                } else {
                    Ok(())
                }
            };
            if let Err(error) = removal {
                if let Some(task_actor) = &task_actor {
                    task_actor.finish_retirement();
                }
                return Err(error.into());
            }
            if let Err(error) = state.control.mark_worktree_removed(session_id).await {
                if let Some(task_actor) = &task_actor {
                    task_actor.finish_retirement();
                }
                return Err(error.into());
            }
            state.actors.write().await.remove(&session_id);
            drop(task_actor);
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
            if state.control.selected().await.ok() == Some(Some(session_id)) {
                let fallback = state.actors.read().await.keys().next().copied();
                if let Err(error) = state.control.set_selected(fallback).await {
                    let _ = state.events.send(SupervisorEvent::Error {
                        session_id: Some(session_id),
                        message: format!(
                            "worktree removed but selection could not be updated: {error}"
                        ),
                    });
                } else {
                    let _ = state.events.send(SupervisorEvent::Selected(fallback));
                }
            }
        }
        SupervisorCommand::FinalizeCreation { operation_id } => {
            let completed = state.control.complete_creation(operation_id).await?;
            // Trust is what the operator actually confirmed, so persist it
            // before the task can run. A managed worktree normally inherits
            // trust from the repository root, but the grant must be recorded
            // explicitly so the task keeps working if that changes.
            if let Some(session_id) = completed.session_id {
                let workspace = state.control.session(session_id).await?.workspace;
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
            if state.control.session(session_id).await?.label.is_empty() {
                state
                    .control
                    .rename(session_id, &forge_storage::label_from_prompt(&text))
                    .await?;
            }
            state.control.enqueue_prompt(session_id, &text).await?;
            state
                .control
                .set_turn_state(session_id, SupervisorTurnState::Queued)
                .await?;
            publish_actor(&state, session_id).await?;
            start_prompt_driver(state, session_id).await?;
        }
        SupervisorCommand::SubmitPromptWithAttachments {
            session_id,
            text,
            attachments,
        } => {
            if state.control.session(session_id).await?.label.is_empty() {
                state
                    .control
                    .rename(session_id, &forge_storage::label_from_prompt(&text))
                    .await?;
            }
            let attachments = serde_json::to_string(&attachments)
                .map_err(|error| RepositorySupervisorError::Command(error.to_string()))?;
            state
                .control
                .enqueue_prompt_with_attachments(session_id, &text, &attachments)
                .await?;
            publish_actor(&state, session_id).await?;
            start_prompt_driver(state, session_id).await?;
        }
        SupervisorCommand::ContinueTurn { session_id } => {
            start_continue_driver(state, session_id).await?;
        }
        SupervisorCommand::ForkSession { session_id } => {
            replace_conversation(&state, session_id, None).await?;
        }
        SupervisorCommand::ResumeSession {
            current_session_id,
            session_id,
        } => {
            if state.actors.read().await.contains_key(&session_id) {
                state.control.set_selected(Some(session_id)).await?;
                let _ = state
                    .events
                    .send(SupervisorEvent::Selected(Some(session_id)));
            } else {
                replace_conversation(&state, current_session_id, Some(session_id)).await?;
            }
        }
        SupervisorCommand::CloseSession { session_id } => {
            close_session(&state, session_id).await?;
        }
        SupervisorCommand::StopTurn { session_id } => {
            let actor = actor(&state, session_id).await?;
            if !actor.request_cancel() {
                let mut session = try_session(&actor)?;
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
            feedback,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
            session
                .resolve_hitl_with_feedback(decision, &decision_actor, feedback.as_deref())
                .await?;
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
            let mut session = try_session(&task_actor)?;
            session.resolve_question(answers, &answer_actor).await?;
            refresh_actor(&state, &task_actor, &session).await?;
            drop(session);
            start_continue_driver(state, session_id).await?;
        }
        SupervisorCommand::SelectSession { session_id } => {
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
            let mut session = try_session(&task_actor)?;
            session.set_active_model(model_id);
            session.set_active_route_id(route_id);
            session.set_reasoning_effort(reasoning_effort);
            // Persist the selection on the task row so a restart restores it
            // instead of dropping back to a blank model.
            state
                .control
                .set_session_model(
                    session_id,
                    &session.active_model,
                    &session.active_route_id,
                    session.reasoning_effort(),
                )
                .await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::SetThinking {
            session_id,
            enabled,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
            session.set_thinking_enabled(enabled);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::SetCapabilities {
            session_id,
            image_input_supported,
            context_window,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
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
            // Skipping a busy actor is safe: the 200ms ticker retries it and
            // the turn-end refresh publishes the final state. Blocking here
            // would stall every unrelated session behind this poll.
            let lock = task_actor.session.try_lock();
            if let Ok(mut session) = lock {
                session.poll_background_tasks().await?;
                refresh_actor(&state, &task_actor, &session).await?;
            }
        }
        SupervisorCommand::CompactContext { session_id } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
            let pending = session.begin_context_compaction(forge_core::CompactionTrigger::Manual);
            let completed = pending.execute().await;
            session.finish_context_compaction(completed).await?;
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::CancelBackgroundTask {
            session_id,
            task_id,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
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
            let mut session = try_session(&task_actor)?;
            session.resolve_subagent_hitl(task_id, decision);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::GrantEgressHost {
            session_id,
            pattern,
        } => {
            let task_actor = actor(&state, session_id).await?;
            let session = try_session(&task_actor)?;
            session.grant_egress_host(&pattern);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::AllowSessionPattern { session_id, call } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
            session.allow_suggested_pattern_for_session(&call);
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ClearSessionApprovals { session_id } => {
            let task_actor = actor(&state, session_id).await?;
            let mut session = try_session(&task_actor)?;
            session.clear_session_pattern_allows();
            refresh_actor(&state, &task_actor, &session).await?;
        }
        SupervisorCommand::ApplyProviderEnv { pairs } => {
            state.model.apply_provider_env(&pairs);
            for actor in state.actors.read().await.values() {
                actor.model.apply_provider_env(&pairs);
            }
        }
        SupervisorCommand::ClearProviderEnv => {
            state.model.clear_provider_env();
            for actor in state.actors.read().await.values() {
                actor.model.clear_provider_env();
            }
        }
        SupervisorCommand::Refresh => {
            poll_background_tasks(&state).await;
            let _ = state
                .events
                .send(SupervisorEvent::Roster(snapshots(&state).await));
            let selected = state.control.selected().await?;
            let _ = state.events.send(SupervisorEvent::Selected(selected));
        }
        SupervisorCommand::Shutdown => {
            let session_ids: Vec<_> = state.actors.read().await.keys().copied().collect();
            for session_id in session_ids {
                let Some(task_actor) = wait_for_actor_retirement(&state, session_id, true).await?
                else {
                    continue;
                };
                let refresh = {
                    let session = task_actor.session.lock().await;
                    refresh_actor(&state, &task_actor, &session).await
                };
                task_actor.finish_retirement();
                refresh?;
            }
        }
    }
    Ok(())
}

async fn poll_background_tasks(state: &SupervisorState) {
    let actors: Vec<_> = state.actors.read().await.values().cloned().collect();
    for task_actor in actors {
        if let Ok(mut session) = task_actor.session.try_lock() {
            if session
                .background()
                .list()
                .any(|task| !task.status.is_terminal())
            {
                let _ = session.poll_background_tasks().await;
                let _ = refresh_actor(state, &task_actor, &session).await;
            }
        }
    }
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
            "the repository's main worktree is already the primary Session".into(),
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
            let _git_mutation = state.git_mutation.lock().await;
            let _ = forge_storage::remove_clean_worktree(storage.main_worktree(), &workspace);
        }
    }
    Ok(())
}

async fn replace_conversation(
    state: &Arc<SupervisorState>,
    previous: SessionId,
    resume: Option<SessionId>,
) -> Result<(), RepositorySupervisorError> {
    let old_actor = actor(state, previous).await?;
    if old_actor.driving.load(Ordering::Acquire) {
        return Err(RepositorySupervisorError::Command(
            "stop the turn before replacing its conversation".into(),
        ));
    }
    let session = match old_actor.session.try_lock() {
        Ok(session) => session,
        Err(_) if old_actor.driving.load(Ordering::Acquire) => {
            return Err(RepositorySupervisorError::Command(
                "stop the turn before replacing its conversation".into(),
            ));
        }
        Err(_) => return Err(RepositorySupervisorError::Contention),
    };
    if session.pending_hitl().is_some()
        || session.pending_question().is_some()
        || session
            .background()
            .list()
            .any(|task| !task.status.is_terminal())
    {
        return Err(RepositorySupervisorError::Command(
            "resolve pending interactions and background jobs before replacing the conversation"
                .into(),
        ));
    }
    let replacement = if let Some(session_id) = resume {
        if let Ok(record) = state.control.session(session_id).await {
            if !same_path(&record.workspace, session.workspace_root()) {
                return Err(RepositorySupervisorError::Command(
                    "resume this Session from its own worktree".into(),
                ));
            }
        }
        let mut cfg = state.cfg.clone();
        cfg.resolved_workspace = session.workspace_root().to_path_buf();
        cfg.workspace_root = Some(cfg.resolved_workspace.display().to_string());
        cfg.journal.path = session.journal_dir().display().to_string();
        open_session_with_model(
            &cfg,
            SessionTarget::Resume(session_id),
            session.model_client(),
        )
        .await?
        .session
    } else {
        session.fork().await?
    };
    let next = replacement.session_id;
    state.control.replace_active_session(previous, next).await?;
    let record = state.control.session(next).await?;
    drop(session);
    let mut actors = state.actors.write().await;
    actors.remove(&previous);
    actors.insert(
        next,
        Arc::new(SessionActor::new(
            record,
            replacement,
            Vec::new(),
            Vec::new(),
        )),
    );
    drop(actors);
    let mut archived = state.unavailable_tasks.write().await;
    archived.retain(|task| task.session_id != next && task.session_id != previous);
    archived.push(state.control.session(previous).await?);
    drop(archived);
    let _ = state
        .events
        .send(SupervisorEvent::Roster(snapshots(state).await));
    let _ = state.events.send(SupervisorEvent::Selected(Some(next)));
    Ok(())
}

/// Stop managing one session while leaving its durable task, journal, queue,
/// and worktree available for a later resume. This is deliberately different
/// from archiving: closing a session is a process/UI boundary, not a durable
/// lifecycle change.
async fn close_session(
    state: &Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let Some(task_actor) = wait_for_actor_retirement(state, session_id, true).await? else {
        return Err(RepositorySupervisorError::NoActor(session_id));
    };

    let next_session_id = snapshots(state)
        .await
        .into_iter()
        .find(|snapshot| {
            snapshot.task.session_id != session_id
                && snapshot.task.lifecycle == SessionLifecycle::Active
        })
        .map(|snapshot| snapshot.task.session_id);
    if let Err(error) = state.control.set_selected(next_session_id).await {
        task_actor.finish_retirement();
        return Err(error.into());
    }
    state.actors.write().await.remove(&session_id);
    task_actor.finish_retirement();

    let _ = state
        .events
        .send(SupervisorEvent::Roster(snapshots(state).await));
    let _ = state
        .events
        .send(SupervisorEvent::Selected(next_session_id));
    Ok(())
}

async fn start_prompt_driver(
    state: Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(&state, session_id).await?;
    if task_actor.retiring.load(Ordering::Acquire) {
        return Err(RepositorySupervisorError::Retiring(session_id));
    }
    if task_actor.driving.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    tokio::spawn(async move {
        loop {
            if let Err(error) = drive_prompts(state.clone(), task_actor.clone()).await {
                let _ = state.events.send(SupervisorEvent::Error {
                    session_id: Some(session_id),
                    message: error.to_string(),
                });
                task_actor.driving.store(false, Ordering::Release);
                break;
            }
            task_actor.driving.store(false, Ordering::Release);
            let lifecycle = task_actor.snapshot.read().await.session.lifecycle;
            let queued = state
                .control
                .queued_prompts(session_id)
                .await
                .unwrap_or_default();
            if matches!(lifecycle, TaskLifecycle::Waiting | TaskLifecycle::Cancelled)
                || queued.is_empty()
                || task_actor.driving.swap(true, Ordering::AcqRel)
            {
                break;
            }
        }
    });
    Ok(())
}

async fn start_continue_driver(
    state: Arc<SupervisorState>,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(&state, session_id).await?;
    if task_actor.retiring.load(Ordering::Acquire) {
        return Err(RepositorySupervisorError::Retiring(session_id));
    }
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
    task_actor: Arc<SessionActor>,
) -> Result<(), RepositorySupervisorError> {
    let session_id = task_actor.snapshot.read().await.task.session_id;
    loop {
        if task_actor.retiring.load(Ordering::Acquire) {
            break;
        }
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
    task_actor: Arc<SessionActor>,
    prompt: Option<(u64, String)>,
) -> Result<bool, RepositorySupervisorError> {
    let session_id = task_actor.snapshot.read().await.task.session_id;
    let queue_id = prompt.as_ref().map(|(queue_id, _)| *queue_id);
    let result = run_one_inner(state.clone(), task_actor, prompt).await;
    if result.is_err() {
        if let Some(queue_id) = queue_id {
            if state.control.interrupt_prompt_claim(queue_id).await? {
                let _ = publish_actor(&state, session_id).await;
                let _ = state.events.send(SupervisorEvent::Attention {
                    session_id,
                    state: SupervisorTurnState::Interrupted,
                    message: "A prompt was interrupted; review it before retrying".into(),
                });
            }
        }
    }
    result
}

async fn run_one_inner(
    state: Arc<SupervisorState>,
    task_actor: Arc<SessionActor>,
    prompt: Option<(u64, String)>,
) -> Result<bool, RepositorySupervisorError> {
    let session_id = task_actor.snapshot.read().await.task.session_id;
    let prompt_attachments = match prompt.as_ref() {
        Some((queue_id, _)) => Some(
            serde_json::from_str(&state.control.prompt_attachments(*queue_id).await?)
                .map_err(|error| RepositorySupervisorError::Command(error.to_string()))?,
        ),
        None => None,
    };
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
        Some((_, text)) => match session
            .append_user_message_with_attachments(text, prompt_attachments.unwrap_or_default())
            .await
        {
            Ok(()) => session.run_agent_turns_in_scope(Some(stream_sender)).await,
            Err(error) => Err(error),
        },
        None => session.run_agent_turns_in_scope(Some(stream_sender)).await,
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
        SupervisorTurnState::Waiting => "Session needs input",
        SupervisorTurnState::Completed => "Session completed",
        SupervisorTurnState::Failed => "Session failed",
        SupervisorTurnState::Cancelled => "Session stopped",
        _ => "Session updated",
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
) -> Result<Arc<SessionActor>, RepositorySupervisorError> {
    state
        .actors
        .read()
        .await
        .get(&session_id)
        .cloned()
        .ok_or(RepositorySupervisorError::NoActor(session_id))
}

/// Close the runtime handles owned by an actor before Git is allowed to
/// remove its workspace. The retirement flag closes the race where a queued
/// prompt or continuation tries to start while the actor is being drained.
/// The actor remains in the map until the caller has completed its durable or
/// filesystem mutation, so a failed cleanup can be retried safely.
async fn retire_actor_resources(
    state: &SupervisorState,
    session_id: SessionId,
    allow_pending_interaction: bool,
) -> Result<Option<Arc<SessionActor>>, RepositorySupervisorError> {
    let Some(task_actor) = state.actors.read().await.get(&session_id).cloned() else {
        return Ok(None);
    };
    if !task_actor.begin_retirement() {
        return Err(RepositorySupervisorError::Retiring(session_id));
    }
    task_actor.request_cancel();

    let retirement = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while task_actor.driving.load(Ordering::Acquire) {
                task_actor.request_cancel();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            RepositorySupervisorError::Command(
                "session turn did not stop before workspace retirement".into(),
            )
        })?;

        let mut session =
            tokio::time::timeout(std::time::Duration::from_secs(5), task_actor.session.lock())
                .await
                .map_err(|_| {
                    RepositorySupervisorError::Command(
                        "session lock did not become available before workspace retirement".into(),
                    )
                })?;
        if !allow_pending_interaction
            && (session.pending_hitl().is_some() || session.pending_question().is_some())
        {
            return Err(RepositorySupervisorError::Command(
                "resolve the pending interaction before retiring this workspace".into(),
            ));
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(6),
            session.retire_resources(),
        )
        .await
        .map_err(|_| {
            RepositorySupervisorError::Command(
                "session resources did not stop before workspace retirement".into(),
            )
        })?
        .map_err(|error| {
            RepositorySupervisorError::Command(format!(
                "session resources did not stop before workspace retirement: {error}"
            ))
        })?;
        Ok::<(), RepositorySupervisorError>(())
    }
    .await;
    if let Err(error) = retirement {
        task_actor.finish_retirement();
        return Err(error);
    }
    Ok(Some(task_actor))
}

async fn wait_for_actor_retirement(
    state: &SupervisorState,
    session_id: SessionId,
    allow_pending_interaction: bool,
) -> Result<Option<Arc<SessionActor>>, RepositorySupervisorError> {
    loop {
        match retire_actor_resources(state, session_id, allow_pending_interaction).await {
            Err(RepositorySupervisorError::Retiring(_)) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
}

async fn refresh_actor(
    state: &SupervisorState,
    task_actor: &SessionActor,
    session: &AgentSession,
) -> Result<(), RepositorySupervisorError> {
    let task = state.control.session(session.session_id).await?;
    let snapshot = SessionRuntimeSnapshot {
        task,
        session: SessionSnapshot::capture(session),
        transcript: TranscriptSnapshot::capture(session),
        details: Some(SessionDetailsSnapshot::capture(session)),
        queued_prompts: state.control.queued_prompts(session.session_id).await?,
        interrupted_prompts: state
            .control
            .interrupted_prompts(session.session_id)
            .await?,
    };
    *task_actor.snapshot.write().await = snapshot.clone();
    let _ = state
        .events
        .send(SupervisorEvent::SessionUpdated(Box::new(snapshot)));
    Ok(())
}

async fn publish_actor(
    state: &SupervisorState,
    session_id: SessionId,
) -> Result<(), RepositorySupervisorError> {
    let task_actor = actor(state, session_id).await?;
    let task = state.control.session(session_id).await?;
    let mut snapshot = task_actor.snapshot.write().await;
    snapshot.task = task;
    snapshot.queued_prompts = state.control.queued_prompts(session_id).await?;
    snapshot.interrupted_prompts = state.control.interrupted_prompts(session_id).await?;
    let _ = state
        .events
        .send(SupervisorEvent::SessionUpdated(Box::new(snapshot.clone())));
    Ok(())
}

async fn snapshots(state: &SupervisorState) -> Vec<SessionRuntimeSnapshot> {
    let actors: Vec<_> = state.actors.read().await.values().cloned().collect();
    let unavailable_tasks = state.unavailable_tasks.read().await.clone();
    let mut snapshots = Vec::with_capacity(actors.len() + unavailable_tasks.len());
    for task_actor in actors {
        snapshots.push(task_actor.snapshot.read().await.clone());
    }
    for task in unavailable_tasks {
        snapshots.push(SessionRuntimeSnapshot {
            session: SessionSnapshot {
                session_id: task.session_id,
                lifecycle: TaskLifecycle::Interrupted,
                workspace_root: task.workspace.clone(),
                ..SessionSnapshot::default()
            },
            transcript: TranscriptSnapshot::default(),
            details: None,
            queued_prompts: Vec::new(),
            interrupted_prompts: state
                .control
                .interrupted_prompts(task.session_id)
                .await
                .unwrap_or_default(),
            task,
        });
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

/// Reapply the model/route/effort recorded on the task row after a restart
/// reopens the session, so the operator's selection survives instead of
/// falling back to an empty config default. A blank stored model means the
/// row predates model persistence; leave whatever `cfg` produced in place.
fn restore_task_model(session: &mut AgentSession, task: &RepositorySession) {
    if task.model_id.is_empty() {
        return;
    }
    session.set_active_model(task.model_id.clone());
    if !task.route_id.is_empty() {
        session.set_active_route_id(task.route_id.clone());
    }
    session.set_reasoning_effort(task.reasoning_effort.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use forge_config::Config;
    use forge_core::LoopConfig;
    use forge_model::{MockModelClient, ModelError, ModelRequest};
    use forge_tools::{Tool, ToolContext, ToolError, ToolRegistry};
    use forge_types::{AskUserQuestionAnswerItem, ModelResponse, ToolOutput};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Simulates a command the OS sandbox blocks while confined and that still
    /// fails once HITL approval escalates it to an unconfined run — the shape
    /// of a real `bash(git …)` call the sandbox refuses and that genuinely
    /// cannot succeed. Its failure must be ordinary tool feedback, not a turn
    /// failure.
    struct EscalatingSandboxDeniedTool;

    impl Tool for EscalatingSandboxDeniedTool {
        fn name(&self) -> &str {
            "escalating_sandbox_denied"
        }
        fn description(&self) -> &str {
            "Simulate a command blocked by the sandbox that also fails unconfined"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({"type": "object", "additionalProperties": false})
        }
        fn side_effect_class(&self) -> forge_types::SideEffectClass {
            forge_types::SideEffectClass::Exec
        }
        fn call<'life0, 'life1, 'async_trait>(
            &'life0 self,
            ctx: &'life1 ToolContext,
            _args: serde_json::Value,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ToolOutput, ToolError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                if ctx.unconfined_shell {
                    return Ok(ToolOutput::failed_exit(
                        "git failed even outside the sandbox",
                        Some(1),
                    ));
                }
                Err(ToolError::SandboxDenied {
                    content: "Operation not permitted\nblocked by the sandbox".into(),
                    reason:
                        "blocked by the sandbox: filesystem access is confined to the workspace"
                            .into(),
                    denied_host: None,
                })
            })
        }
    }

    struct CredentialTrackingModel {
        clear_count: Arc<AtomicUsize>,
    }

    impl ModelClient for CredentialTrackingModel {
        fn complete<'life0, 'async_trait>(
            &'life0 self,
            _request: ModelRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ModelResponse, ModelError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Ok(text_response("ok")) })
        }

        fn clear_provider_env(&self) {
            self.clear_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    async fn escalation_session(cfg: &Config, responses: Vec<ModelResponse>) -> AgentSession {
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(responses));
        let loop_cfg = LoopConfig {
            workspace: cfg.workspace_root().to_path_buf(),
            journal_dir: cfg.journal.path.clone().into(),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(EscalatingSandboxDeniedTool));
        AgentSession::create(loop_cfg, model, tools).await.unwrap()
    }

    fn text_response(text: &str) -> ModelResponse {
        ModelResponse {
            text: text.into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }
    }

    struct SlowModel;

    impl ModelClient for SlowModel {
        fn complete<'life0, 'async_trait>(
            &'life0 self,
            _request: ModelRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ModelResponse, ModelError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                Ok(text_response("should be cancelled"))
            })
        }
    }

    async fn scripted_session(cfg: &Config, text: &str) -> AgentSession {
        scripted_session_with(cfg, vec![text_response(text)]).await
    }

    async fn scripted_session_with(cfg: &Config, responses: Vec<ModelResponse>) -> AgentSession {
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(responses));
        open_session_with_model(cfg, SessionTarget::New, model)
            .await
            .unwrap()
            .session
    }

    async fn tracking_session(cfg: &Config, model: Arc<dyn ModelClient>) -> AgentSession {
        let loop_cfg = LoopConfig {
            workspace: cfg.workspace_root().to_path_buf(),
            journal_dir: cfg.journal.path.clone().into(),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        };
        AgentSession::create(loop_cfg, model, ToolRegistry::new())
            .await
            .unwrap()
    }

    fn approval_then_question_script(label: &str) -> Vec<ModelResponse> {
        vec![
            ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: format!("{label}-approval"),
                    name: "git".into(),
                    arguments: json!({"subcommand": "reset", "args": ["--hard", "HEAD"]}),
                }],
                usage: None,
                thinking: None,
            },
            ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: format!("{label}-question"),
                    name: "ask_user_question".into(),
                    arguments: json!({
                        "questions": [{
                            "id": "choice",
                            "header": "Choice",
                            "question": "Continue?",
                            "options": [
                                {"label": "Yes (Recommended)", "description": "Continue."},
                                {"label": "No", "description": "Stop."}
                            ]
                        }]
                    }),
                }],
                usage: None,
                thinking: None,
            },
            text_response(&format!("{label} interaction complete")),
            ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: format!("{label}-background"),
                    name: "background_run".into(),
                    arguments: json!({
                        "command": "sleep 30",
                        "label": format!("{label} background")
                    }),
                }],
                usage: None,
                thinking: None,
            },
            text_response(&format!("{label} background started")),
        ]
    }

    fn task_for(
        session_id: SessionId,
        label: &str,
        workspace: &std::path::Path,
    ) -> RepositorySession {
        RepositorySession {
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
        predicate: impl Fn(&SessionRuntimeSnapshot) -> bool,
    ) -> SessionRuntimeSnapshot {
        // Generous headroom: the turn itself is fast, but the workspace tests
        // run concurrently on shared CI runners, and a starved worker could
        // otherwise blow a tight deadline while the state it polls for is
        // still coming. A genuine hang still trips this.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut events = handle.subscribe();
        while std::time::Instant::now() < deadline {
            handle.command(SupervisorCommand::Refresh).await.unwrap();
            match tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SupervisorEvent::SessionUpdated(snapshot))) => {
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
    async fn startup_reconciles_running_prompt_claims_without_replaying_them() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut cfg = Config {
            resolved_workspace: workspace.clone(),
            workspace_root: Some(workspace.display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        let session = scripted_session(&cfg, "unused").await;
        let session_id = session.session_id;
        let task = task_for(session_id, "recovered", &workspace);
        control
            .register_session(
                NewRepositorySession {
                    session_id,
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
        let interrupted_id = control
            .enqueue_prompt(session_id, "may already have run")
            .await
            .unwrap();
        let queued_id = control
            .enqueue_prompt(session_id, "run after review")
            .await
            .unwrap();
        assert_eq!(
            control.claim_next_prompt(session_id).await.unwrap(),
            Some((interrupted_id, "may already have run".into()))
        );

        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let (supervisor, handle) = RepositorySupervisor::spawn(
            control.clone(),
            lease,
            vec![(task, session)],
            1,
            cfg,
            model,
        )
        .await
        .unwrap();

        let snapshot = supervisor.snapshot(session_id).await.unwrap();
        assert_eq!(snapshot.task.turn_state, SupervisorTurnState::Interrupted);
        assert_eq!(
            snapshot.interrupted_prompts,
            vec![(interrupted_id, "may already have run".into())]
        );
        assert_eq!(
            snapshot.queued_prompts,
            vec![(queued_id, "run after review".into())]
        );
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn clearing_provider_env_reaches_every_session_actor() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let clear_count = Arc::new(AtomicUsize::new(0));
        let mut sessions = Vec::new();
        for (index, name) in ["first", "second"].into_iter().enumerate() {
            let workspace = temp.path().join(name);
            std::fs::create_dir_all(&workspace).unwrap();
            let mut cfg = Config {
                resolved_workspace: workspace.clone(),
                workspace_root: Some(workspace.display().to_string()),
                ..Default::default()
            };
            cfg.journal.path = temp
                .path()
                .join(format!("journals-{index}"))
                .display()
                .to_string();
            let model = Arc::new(CredentialTrackingModel {
                clear_count: Arc::clone(&clear_count),
            });
            let session = tracking_session(&cfg, model).await;
            let task = task_for(session.session_id, name, &workspace);
            control
                .register_session(
                    NewRepositorySession {
                        session_id: task.session_id,
                        label: task.label.clone(),
                        workspace: task.workspace.clone(),
                        branch: task.branch.clone(),
                        ownership: task.ownership,
                        slot: Some((index + 1) as u8),
                        model_id: task.model_id.clone(),
                        route_id: task.route_id.clone(),
                        reasoning_effort: task.reasoning_effort.clone(),
                    },
                    None,
                )
                .await
                .unwrap();
            sessions.push((task, session));
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };
        let model: Arc<dyn ModelClient> = Arc::new(CredentialTrackingModel {
            clear_count: Arc::clone(&clear_count),
        });
        let (_supervisor, handle) =
            RepositorySupervisor::spawn(control, lease, sessions, 2, cfg, model)
                .await
                .unwrap();

        handle
            .command(SupervisorCommand::ClearProviderEnv)
            .await
            .unwrap();
        assert_eq!(clear_count.load(Ordering::SeqCst), 3);
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
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
        let mut first_task = task_for(first_id, "first", &first_workspace);
        first_task.ownership = WorktreeOwnership::Primary;

        let mut second_cfg = first_cfg.clone();
        second_cfg.resolved_workspace = second_workspace.clone();
        second_cfg.workspace_root = Some(second_workspace.display().to_string());
        let second_session = scripted_session(&second_cfg, "second answer").await;
        let second_id = second_session.session_id;
        let second_task = task_for(second_id, "second", &second_workspace);

        for task in [&first_task, &second_task] {
            control
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
            control.clone(),
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
        // Queue a follow-up for both while their first turn may still be
        // driving. Primary and managed Sessions use the exact same command
        // and actor queue.
        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: first_id,
                text: "run first again".into(),
            })
            .await
            .unwrap();
        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: second_id,
                text: "run second again".into(),
            })
            .await
            .unwrap();

        let first_done = wait_for_task_state(&handle, first_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
                && snapshot
                    .transcript
                    .messages()
                    .iter()
                    .filter(|message| message.role == forge_types::MessageRole::User)
                    .count()
                    == 2
        })
        .await;
        let second_done = wait_for_task_state(&handle, second_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
                && snapshot
                    .transcript
                    .messages()
                    .iter()
                    .filter(|message| message.role == forge_types::MessageRole::User)
                    .count()
                    == 2
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

        for session_id in [first_id, second_id] {
            handle
                .command(SupervisorCommand::SetModel {
                    session_id,
                    model_id: "mock-v2".into(),
                    route_id: "native-v2".into(),
                    reasoning_effort: Some("high".into()),
                })
                .await
                .unwrap();
            let changed = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot
                    .details
                    .as_ref()
                    .is_some_and(|details| details.active_model == "mock-v2")
            })
            .await;
            let details = changed.details.as_ref().unwrap();
            assert_eq!(details.active_route_id, "native-v2");
            assert_eq!(details.reasoning_effort.as_deref(), Some("high"));
            // The selection must be persisted on the task row, not just held
            // in the actor: a restart reopens from the row and would lose it.
            let persisted = control.session(session_id).await.unwrap();
            assert_eq!(persisted.model_id, "mock-v2");
            assert_eq!(persisted.route_id, "native-v2");
            assert_eq!(persisted.reasoning_effort.as_deref(), Some("high"));
        }

        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn open_with_primary_adopts_exactly_one_primary_actor() {
        let temp = TempDir::new().unwrap();
        for args in [
            &["init", "-q", "--initial-branch=main"][..],
            &["config", "user.email", "forge@example.com"][..],
            &["config", "user.name", "Forge Test"][..],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(temp.path().join("tracked.txt"), "one\n").unwrap();
        for args in [
            &["add", "tracked.txt"][..],
            &["commit", "-q", "-m", "init"][..],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }

        let mut cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        let bootstrap = RepositoryBootstrap::acquire(&cfg).await.unwrap();
        let primary = scripted_session(&cfg, "one answer").await;
        let primary_id = primary.session_id;
        bootstrap
            .control
            .register_session(
                NewRepositorySession {
                    session_id: primary_id,
                    label: "main".into(),
                    workspace: temp.path().to_path_buf(),
                    branch: "main".into(),
                    ownership: WorktreeOwnership::Primary,
                    slot: Some(1),
                    model_id: "mock".into(),
                    route_id: "native".into(),
                    reasoning_effort: None,
                },
                None,
            )
            .await
            .unwrap();

        let (supervisor, handle) = bootstrap.open_with_primary(&cfg, primary).await.unwrap();
        let snapshots = supervisor.snapshots().await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].task.session_id, primary_id);
        assert_eq!(snapshots[0].task.ownership, WorktreeOwnership::Primary);

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: primary_id,
                text: "run once".into(),
            })
            .await
            .unwrap();
        let done = wait_for_task_state(&handle, primary_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        assert_eq!(
            done.transcript
                .messages()
                .iter()
                .filter(|message| message.role == forge_types::MessageRole::User)
                .count(),
            1
        );
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn primary_and_managed_actors_share_hitl_question_and_continue_commands() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg = Config {
            resolved_workspace: temp.path().join("primary"),
            workspace_root: Some(temp.path().join("primary").display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg.resolved_workspace).unwrap();
        let managed_workspace = temp.path().join("managed");
        std::fs::create_dir_all(&managed_workspace).unwrap();

        let primary_session =
            scripted_session_with(&cfg, approval_then_question_script("primary")).await;
        let primary_id = primary_session.session_id;
        let mut primary_task = task_for(primary_id, "primary", &cfg.resolved_workspace);
        primary_task.ownership = WorktreeOwnership::Primary;

        let mut managed_cfg = cfg.clone();
        managed_cfg.resolved_workspace = managed_workspace.clone();
        managed_cfg.workspace_root = Some(managed_workspace.display().to_string());
        let managed_session =
            scripted_session_with(&managed_cfg, approval_then_question_script("managed")).await;
        let managed_id = managed_session.session_id;
        let managed_task = task_for(managed_id, "managed", &managed_workspace);

        for task in [&primary_task, &managed_task] {
            control
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
                    None,
                )
                .await
                .unwrap();
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let (_supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![
                (primary_task, primary_session),
                (managed_task, managed_session),
            ],
            2,
            cfg,
            model,
        )
        .await
        .unwrap();

        for session_id in [primary_id, managed_id] {
            handle
                .command(SupervisorCommand::SubmitPrompt {
                    session_id,
                    text: "exercise interaction routing".into(),
                })
                .await
                .unwrap();
        }
        for session_id in [primary_id, managed_id] {
            let waiting = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.session.pending_hitl.is_some()
            })
            .await;
            assert_eq!(waiting.task.turn_state, SupervisorTurnState::Waiting);
            handle
                .command(SupervisorCommand::ResolveApproval {
                    session_id,
                    decision: HitlDecision::Deny,
                    actor: "test".into(),
                    feedback: Some("use a safer approach".into()),
                })
                .await
                .unwrap();
        }
        for session_id in [primary_id, managed_id] {
            let waiting = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.session.pending_question.is_some()
            })
            .await;
            assert_eq!(waiting.task.turn_state, SupervisorTurnState::Waiting);
            handle
                .command(SupervisorCommand::ResolveQuestion {
                    session_id,
                    answers: Some(AskUserQuestionResult {
                        answers: vec![AskUserQuestionAnswerItem {
                            id: "choice".into(),
                            selected: vec!["Yes (Recommended)".into()],
                            custom: None,
                        }],
                    }),
                    actor: "test".into(),
                })
                .await
                .unwrap();
        }
        for (session_id, label) in [(primary_id, "primary"), (managed_id, "managed")] {
            let done = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.task.turn_state == SupervisorTurnState::Completed
            })
            .await;
            assert!(done.transcript.messages().iter().any(|message| message
                .content
                .contains(&format!("{label} interaction complete"))));
        }

        for session_id in [primary_id, managed_id] {
            handle
                .command(SupervisorCommand::SubmitPrompt {
                    session_id,
                    text: "start a background job".into(),
                })
                .await
                .unwrap();
        }
        let mut background_ids = Vec::new();
        for session_id in [primary_id, managed_id] {
            let started = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.task.turn_state == SupervisorTurnState::Completed
                    && snapshot
                        .details
                        .as_ref()
                        .is_some_and(|details| !details.background.is_empty())
            })
            .await;
            let task_id = started.details.as_ref().unwrap().background[0].id;
            background_ids.push((session_id, task_id));
        }
        for (session_id, task_id) in background_ids {
            handle
                .command(SupervisorCommand::CancelBackgroundTask {
                    session_id,
                    task_id,
                })
                .await
                .unwrap();
            let settled = wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.details.as_ref().is_some_and(|details| {
                    details
                        .background
                        .iter()
                        .find(|task| task.id == task_id)
                        .is_some_and(|task| task.status.is_terminal())
                })
            })
            .await;
            let status = &settled
                .details
                .as_ref()
                .unwrap()
                .background
                .iter()
                .find(|task| task.id == task_id)
                .unwrap()
                .status;
            assert!(
                matches!(status, forge_core::BackgroundTaskStatus::Cancelled)
                    || matches!(
                        status,
                        forge_core::BackgroundTaskStatus::Failed { error }
                            if error.contains("sandbox unavailable")
                    ),
                "unexpected background terminal state: {status:?}"
            );
        }
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn approved_sandbox_escalation_resumes_the_turn_instead_of_failing() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg = Config {
            resolved_workspace: temp.path().join("primary"),
            workspace_root: Some(temp.path().join("primary").display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg.resolved_workspace).unwrap();

        let session = escalation_session(
            &cfg,
            vec![
                ModelResponse {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "escalate-1".into(),
                        name: "escalating_sandbox_denied".into(),
                        arguments: json!({}),
                    }],
                    usage: None,
                    thinking: None,
                },
                ModelResponse {
                    text: "finished after approved escalation".into(),
                    tool_calls: vec![],
                    usage: None,
                    thinking: None,
                },
            ],
        )
        .await;
        let session_id = session.session_id;
        let task = task_for(session_id, "escalate", &cfg.resolved_workspace);
        control
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
                None,
            )
            .await
            .unwrap();
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let (_supervisor, handle) =
            RepositorySupervisor::spawn(control, lease, vec![(task, session)], 1, cfg, model)
                .await
                .unwrap();

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id,
                text: "exercise escalation".into(),
            })
            .await
            .unwrap();
        let waiting = wait_for_task_state(&handle, session_id, |snapshot| {
            snapshot.session.pending_hitl.is_some()
        })
        .await;
        assert_eq!(waiting.task.turn_state, SupervisorTurnState::Waiting);
        let payload = waiting.session.pending_hitl.as_ref().unwrap();
        assert!(
            payload.sandbox_escalation,
            "the sandbox denial must be flagged as an escalation"
        );

        handle
            .command(SupervisorCommand::ResolveApproval {
                session_id,
                decision: HitlDecision::Approve,
                actor: "test".into(),
                feedback: None,
            })
            .await
            .unwrap();

        let done = wait_for_task_state(&handle, session_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
                || snapshot.task.turn_state == SupervisorTurnState::Failed
        })
        .await;
        assert_eq!(
            done.task.turn_state,
            SupervisorTurnState::Completed,
            "approved escalation must resume the turn, not fail it: {:?}",
            done.task.turn_state
        );
        assert!(done.transcript.messages().iter().any(|message| message
            .content
            .contains("finished after approved escalation")));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn primary_and_managed_running_turns_share_stop_command() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg = Config {
            resolved_workspace: temp.path().join("primary"),
            workspace_root: Some(temp.path().join("primary").display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg.resolved_workspace).unwrap();
        let managed_workspace = temp.path().join("managed");
        std::fs::create_dir_all(&managed_workspace).unwrap();

        let primary_session =
            open_session_with_model(&cfg, SessionTarget::New, Arc::new(SlowModel))
                .await
                .unwrap()
                .session;
        let primary_id = primary_session.session_id;
        let mut primary_task = task_for(primary_id, "primary", &cfg.resolved_workspace);
        primary_task.ownership = WorktreeOwnership::Primary;

        let mut managed_cfg = cfg.clone();
        managed_cfg.resolved_workspace = managed_workspace.clone();
        managed_cfg.workspace_root = Some(managed_workspace.display().to_string());
        let managed_session =
            open_session_with_model(&managed_cfg, SessionTarget::New, Arc::new(SlowModel))
                .await
                .unwrap()
                .session;
        let managed_id = managed_session.session_id;
        let managed_task = task_for(managed_id, "managed", &managed_workspace);

        for task in [&primary_task, &managed_task] {
            control
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
                    None,
                )
                .await
                .unwrap();
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let (supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![
                (primary_task, primary_session),
                (managed_task, managed_session),
            ],
            2,
            cfg,
            model,
        )
        .await
        .unwrap();

        for session_id in [primary_id, managed_id] {
            handle
                .command(SupervisorCommand::SubmitPrompt {
                    session_id,
                    text: "wait until cancelled".into(),
                })
                .await
                .unwrap();
        }
        for session_id in [primary_id, managed_id] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let snapshot = supervisor.snapshot(session_id).await.unwrap();
                if snapshot.task.turn_state == SupervisorTurnState::Running {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "turn never started");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            handle
                .command(SupervisorCommand::StopTurn { session_id })
                .await
                .unwrap();
        }
        for session_id in [primary_id, managed_id] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let cancelled = loop {
                let snapshot = supervisor.snapshot(session_id).await.unwrap();
                if snapshot.task.turn_state == SupervisorTurnState::Cancelled {
                    break snapshot;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "turn did not cancel; last state: {:?}",
                    snapshot.task.turn_state
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            };
            assert_eq!(cancelled.session.lifecycle, TaskLifecycle::Cancelled);
        }
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    struct GateModel {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl ModelClient for GateModel {
        async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            self.entered.notify_one();
            self.release.notified().await;
            let text = request
                .messages
                .last()
                .filter(|message| message.content.starts_with("[forge:compaction]"))
                .map(|_| {
                    r#"<forge_checkpoint version="1">
<objective>Keep the session isolated.</objective>
<user_constraints>Preserve the session boundary.</user_constraints>
<current_work>Testing supervisor scheduling.</current_work>
<next_action>Release the compaction gate.</next_action>
</forge_checkpoint>"#
                })
                .unwrap_or("a done");
            Ok(text_response(text))
        }
    }

    /// Foreground sessions must stay responsive while another session's turn
    /// holds its actor lock. The supervisor command loop is shared, so a
    /// handler that blocks on a busy actor stalls unrelated sessions: typed
    /// input queues behind it, then fires as a burst when the turn ends.
    #[tokio::test]
    async fn unrelated_session_commands_progress_while_another_turn_holds_its_actor() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg_a = Config {
            resolved_workspace: temp.path().join("a"),
            workspace_root: Some(temp.path().join("a").display().to_string()),
            ..Default::default()
        };
        cfg_a.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg_a.resolved_workspace).unwrap();
        let mut cfg_b = cfg_a.clone();
        cfg_b.resolved_workspace = temp.path().join("b");
        cfg_b.workspace_root = Some(temp.path().join("b").display().to_string());
        std::fs::create_dir_all(&cfg_b.resolved_workspace).unwrap();

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let gate: Arc<dyn ModelClient> = Arc::new(GateModel {
            entered: entered.clone(),
            release: release.clone(),
        });
        let session_a = open_session_with_model(&cfg_a, SessionTarget::New, gate)
            .await
            .unwrap()
            .session;
        let id_a = session_a.session_id;
        let mut task_a = task_for(id_a, "a", &cfg_a.resolved_workspace);
        task_a.ownership = WorktreeOwnership::Primary;
        let session_b = scripted_session_with(&cfg_b, vec![text_response("b done")]).await;
        let id_b = session_b.session_id;
        let task_b = task_for(id_b, "b", &cfg_b.resolved_workspace);
        for task in [&task_a, &task_b] {
            control
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
                    None,
                )
                .await
                .unwrap();
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(vec![]));
        let (supervisor, handle) = RepositorySupervisor::spawn(
            control.clone(),
            lease,
            vec![(task_a, session_a), (task_b, session_b)],
            2,
            cfg_a,
            model,
        )
        .await
        .unwrap();

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id: id_a,
                text: "hold A open".into(),
            })
            .await
            .unwrap();
        // A's driver is inside the gated model call, holding A's actor lock.
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("A turn never started");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if supervisor.snapshot(id_a).await.unwrap().task.turn_state
                == SupervisorTurnState::Running
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "A never reached Running"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Occupy the shared command loop with a poll against the busy actor,
        // then prove an unrelated session still moves: selection (the F3
        // analog), prompt submit (the typed-input analog), and completion.
        let poll_a = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .command(SupervisorCommand::PollSession { session_id: id_a })
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let start = std::time::Instant::now();
        let foreground = async {
            let selected = std::time::Instant::now();
            handle
                .command(SupervisorCommand::SelectSession {
                    session_id: Some(id_b),
                })
                .await
                .unwrap();
            let select_latency = selected.elapsed();
            let submitted = std::time::Instant::now();
            handle
                .command(SupervisorCommand::SubmitPrompt {
                    session_id: id_b,
                    text: "b hello".into(),
                })
                .await
                .unwrap();
            let submit_latency = submitted.elapsed();
            wait_for_task_state(&handle, id_b, |snapshot| {
                snapshot.task.turn_state == SupervisorTurnState::Completed
            })
            .await;
            (select_latency, submit_latency, start.elapsed())
        };
        // Bound the wait: before the fix this times out (the shared command
        // loop sits in PollSession(A)'s lock wait, so B's select/submit never
        // run). Release A on timeout so the harness can still drain.
        let Ok((select_latency, submit_latency, progress_latency)) =
            tokio::time::timeout(std::time::Duration::from_secs(8), foreground).await
        else {
            release.notify_waiters();
            let _ = poll_a.await;
            panic!("foreground session B stalled while A held its actor: bug reproduced");
        };
        // B finished while A was still gated: nothing queued up to burst later.
        assert_eq!(
            supervisor.snapshot(id_a).await.unwrap().task.turn_state,
            SupervisorTurnState::Running,
            "B must complete while A is still gated"
        );
        eprintln!(
            "fg-stall latencies: select={select_latency:?} submit={submit_latency:?} progress={progress_latency:?}"
        );
        assert!(
            select_latency < std::time::Duration::from_secs(2),
            "selecting B stalled behind A: {select_latency:?}"
        );
        assert!(
            submit_latency < std::time::Duration::from_secs(2),
            "submitting to B stalled behind A: {submit_latency:?}"
        );
        assert!(
            progress_latency < std::time::Duration::from_secs(3),
            "B did not progress while A was gated: {progress_latency:?}"
        );

        release.notify_waiters();
        poll_a.await.unwrap().unwrap();
        let done_a = wait_for_task_state(&handle, id_a, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        let done_b = wait_for_task_state(&handle, id_b, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        for (snapshot, own_prompt, own_answer) in [
            (&done_a, "hold A open", "a done"),
            (&done_b, "b hello", "b done"),
        ] {
            let users: Vec<_> = snapshot
                .transcript
                .messages()
                .iter()
                .filter(|message| message.role == forge_types::MessageRole::User)
                .collect();
            assert_eq!(
                users.len(),
                1,
                "session must hold only its own prompt, no burst from the other session"
            );
            assert!(users[0].content.contains(own_prompt));
            assert!(
                snapshot
                    .transcript
                    .messages()
                    .iter()
                    .any(|message| message.content.contains(own_answer)),
                "missing answer `{own_answer}`"
            );
        }

        // A large queue must not slow the lock-free foreground path either:
        // buffer 500 prompts on B (the keystroke-burst shape), then time a
        // roster refresh, which clones every snapshot.
        for index in 0..500 {
            control
                .enqueue_prompt(id_b, &format!("queued filler {index}"))
                .await
                .unwrap();
        }
        let refreshed = std::time::Instant::now();
        handle.command(SupervisorCommand::Refresh).await.unwrap();
        let refresh_latency = refreshed.elapsed();
        eprintln!("fg-stall large-queue refresh: {refresh_latency:?}");
        assert!(
            refresh_latency < std::time::Duration::from_secs(2),
            "refresh stalled with a large queue: {refresh_latency:?}"
        );
        // And nothing burst: B is still Completed with its queue intact.
        assert_eq!(
            supervisor.snapshot(id_b).await.unwrap().task.turn_state,
            SupervisorTurnState::Completed
        );
        assert_eq!(control.queued_prompts(id_b).await.unwrap().len(), 500);

        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn blocked_compaction_does_not_block_an_unrelated_session() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg_a = Config {
            resolved_workspace: temp.path().join("a"),
            workspace_root: Some(temp.path().join("a").display().to_string()),
            ..Default::default()
        };
        cfg_a.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg_a.resolved_workspace).unwrap();
        let mut cfg_b = cfg_a.clone();
        cfg_b.resolved_workspace = temp.path().join("b");
        cfg_b.workspace_root = Some(temp.path().join("b").display().to_string());
        std::fs::create_dir_all(&cfg_b.resolved_workspace).unwrap();

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let gate: Arc<dyn ModelClient> = Arc::new(GateModel {
            entered: entered.clone(),
            release: release.clone(),
        });
        let mut session_a = open_session_with_model(&cfg_a, SessionTarget::New, gate)
            .await
            .unwrap()
            .session;
        session_a.append_user_message("seed context").await.unwrap();
        let id_a = session_a.session_id;
        let task_a = task_for(id_a, "a", &cfg_a.resolved_workspace);
        let session_b = scripted_session_with(&cfg_b, vec![text_response("b done")]).await;
        let id_b = session_b.session_id;
        let task_b = task_for(id_b, "b", &cfg_b.resolved_workspace);
        for task in [&task_a, &task_b] {
            control
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
                    None,
                )
                .await
                .unwrap();
        }
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(vec![]));
        let (supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![(task_a, session_a), (task_b, session_b)],
            2,
            cfg_a,
            model,
        )
        .await
        .unwrap();

        let compact = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .command(SupervisorCommand::CompactContext { session_id: id_a })
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("compaction never reached the model");

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle.command(SupervisorCommand::SelectSession {
                session_id: Some(id_b),
            }),
        )
        .await
        .expect("selection of B stalled behind A compaction")
        .unwrap();

        release.notify_waiters();
        let compact_result = compact.await.unwrap();
        assert!(
            compact_result.is_err(),
            "the deliberately minimal fixture should reject its checkpoint"
        );
        assert_eq!(
            supervisor.snapshot(id_a).await.unwrap().task.turn_state,
            SupervisorTurnState::Idle
        );
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    /// A follow-up prompt must drive a new model request even after a turn
    /// that ended with a queue hand-off.
    ///
    /// Regression: when the session's future-task queue is non-empty, a turn
    /// that finishes at a tool boundary yields to the queue
    /// (`ApplyOutcome::YieldToQueue`) — but the supervised driver returned
    /// that outcome without transitioning the lifecycle (the unsupervised
    /// TUI path calls `yield_current_turn_for_queue` itself; the coordinator
    /// did not). The lifecycle stayed `Working`, so the next prompt journaled
    /// its `user_message`, then `start_fresh_attempt` rejected the new task
    /// (illegal from `Working`) and the driver's `?` dropped the error
    /// without marking the queue item or turn state — the follow-up wedged
    /// `running` forever, never reaching the model.
    #[tokio::test]
    async fn follow_up_after_a_queue_yielding_turn_reaches_the_model() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg = Config {
            resolved_workspace: temp.path().join("primary"),
            workspace_root: Some(temp.path().join("primary").display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg.resolved_workspace).unwrap();
        std::fs::write(
            cfg.resolved_workspace.join("readme.txt"),
            "queued future-task instruction\n",
        )
        .unwrap();

        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(vec![
            // First turn: issue a tool call, then finish at the tool boundary.
            ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "read-1".into(),
                    name: "read_file".into(),
                    arguments: json!({ "path": "readme.txt" }),
                }],
                usage: None,
                thinking: None,
            },
            // Follow-up turn: a plain final answer.
            text_response("follow-up answer"),
        ]));
        let mut opened = open_session_with_model(&cfg, SessionTarget::New, model.clone())
            .await
            .unwrap();
        // A finished background task leaves its summary in the future-task
        // queue (background.rs enqueue_task). A non-empty queue makes a turn
        // that ends at a tool boundary hand off to the queue instead of
        // completing normally.
        opened
            .session
            .enqueue_task("Background task 'noop' finished")
            .await
            .unwrap();
        let session_id = opened.session.session_id;
        let task = task_for(session_id, "followup", &cfg.resolved_workspace);
        control
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
                None,
            )
            .await
            .unwrap();
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let (_supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![(task, opened.session)],
            1,
            cfg,
            model,
        )
        .await
        .unwrap();

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id,
                text: "first".into(),
            })
            .await
            .unwrap();
        let first = wait_for_task_state(&handle, session_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        assert_eq!(
            first
                .transcript
                .messages()
                .iter()
                .filter(|m| m.role == forge_types::MessageRole::User)
                .count(),
            1
        );

        // Follow-up. The turn must reach the model and complete — bounded, so
        // a stall fails the test instead of hanging the suite.
        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id,
                text: "follow-up".into(),
            })
            .await
            .unwrap();
        let follow_up = async {
            wait_for_task_state(&handle, session_id, |snapshot| {
                snapshot.task.turn_state == SupervisorTurnState::Completed
                    && snapshot
                        .transcript
                        .messages()
                        .iter()
                        .filter(|m| m.role == forge_types::MessageRole::User)
                        .count()
                        == 2
            })
            .await
        };
        let follow_up = tokio::time::timeout(std::time::Duration::from_secs(8), follow_up)
            .await
            .expect("follow-up turn stalled after a queue-yielding turn: bug reproduced");
        assert!(follow_up
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("follow-up answer")));

        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn continuing_a_failed_turn_recovers_the_supervised_session() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let mut cfg = Config {
            resolved_workspace: temp.path().join("primary"),
            workspace_root: Some(temp.path().join("primary").display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        std::fs::create_dir_all(&cfg.resolved_workspace).unwrap();

        // First model call fails (a transient provider fault); the continue
        // afterwards succeeds.
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::stream_error_then(
            ModelError::Transport("transient provider fault".into()),
            vec![text_response("recovered after failure")],
        ));
        let opened = open_session_with_model(&cfg, SessionTarget::New, model.clone())
            .await
            .unwrap();
        let session_id = opened.session.session_id;
        let task = task_for(session_id, "failed", &cfg.resolved_workspace);
        control
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
                None,
            )
            .await
            .unwrap();
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let (_supervisor, handle) = RepositorySupervisor::spawn(
            control,
            lease,
            vec![(task, opened.session)],
            1,
            cfg,
            model,
        )
        .await
        .unwrap();

        handle
            .command(SupervisorCommand::SubmitPrompt {
                session_id,
                text: "do the thing".into(),
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let failed = loop {
            let snapshot = _supervisor.snapshot(session_id).await.unwrap();
            if snapshot.task.turn_state == SupervisorTurnState::Failed {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "turn never failed; last state {:?} lifecycle {:?}",
                snapshot.task.turn_state,
                snapshot.session.lifecycle
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(failed.session.lifecycle, TaskLifecycle::Failed);

        // Continue after the failure must start a fresh attempt and recover
        // to Completed instead of staying wedged in Failed.
        handle
            .command(SupervisorCommand::ContinueTurn { session_id })
            .await
            .unwrap();
        let recovered = wait_for_task_state(&handle, session_id, |snapshot| {
            snapshot.task.turn_state == SupervisorTurnState::Completed
        })
        .await;
        assert_eq!(recovered.session.lifecycle, TaskLifecycle::Completed);
        assert!(recovered
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("recovered after failure")));
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
    async fn managed_creation_waits_for_the_shared_git_mutation_gate() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let storage = RepositoryRuntimeStorage::new(repo.path()).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let lease = RepositoryLease::acquire(&control_dir, repo.path()).unwrap();
        let mut cfg = Config {
            resolved_workspace: repo.path().to_path_buf(),
            workspace_root: Some(repo.path().display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = scratch.path().join("journals").display().to_string();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let (supervisor, handle) = RepositorySupervisor::spawn_with_trust_store(
            control.clone(),
            lease,
            Vec::new(),
            2,
            cfg,
            model,
            Some(trust_store.to_path_buf()),
        )
        .await
        .unwrap();

        let git_gate = supervisor.state.git_mutation.clone();
        let gate = git_gate.lock().await;
        let creation = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .command(SupervisorCommand::CreateSession {
                        label: "gated".into(),
                        first_prompt: None,
                    })
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !creation.is_finished(),
            "managed creation bypassed the repository Git mutation gate"
        );
        drop(gate);
        creation.await.unwrap().unwrap();

        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "gated")
            .expect("gated task row");
        assert!(task.workspace.exists());
        assert!(forge_config::is_trusted_at(&trust_store, &task.workspace));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
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
            .command(SupervisorCommand::CreateSession {
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
            .sessions()
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
    async fn a_prompt_less_task_skips_the_trust_modal_and_is_ready_immediately() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let mut events = handle.subscribe();
        handle
            .command(SupervisorCommand::CreateSession {
                label: String::new(),
                first_prompt: None,
            })
            .await
            .unwrap();

        // One-key creation must not park on a modal: no TrustRequired ever
        // fires, the task is registered, and its worktree is trusted before
        // the operator can type anything in it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SupervisorEvent::TrustRequired { .. })) => {
                    panic!("prompt-less creation must not park on trust")
                }
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }
        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label.is_empty())
            .expect("unnamed task row");
        assert!(task.workspace.exists());
        assert!(forge_config::is_trusted_at(&trust_store, &task.workspace));
        // The generated identity is a UUID4 shared by the path and branch.
        let branch_id = task
            .branch
            .strip_prefix("forge/session-")
            .expect("session branch prefix");
        assert_eq!(branch_id.parse::<SessionId>().unwrap().get_version_num(), 4);
        assert!(
            task.workspace
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("session-")),
            "path: {}",
            task.workspace.display()
        );
        assert_eq!(
            task.workspace.file_name().unwrap().to_string_lossy(),
            format!("session-{branch_id}")
        );
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn an_unnamed_task_with_a_prompt_takes_its_label_from_the_prompt() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let mut events = handle.subscribe();
        handle
            .command(SupervisorCommand::CreateSession {
                label: String::new(),
                first_prompt: Some("rewrite the lexer".into()),
            })
            .await
            .unwrap();

        // A prompt still parks on trust (the form path), but the empty
        // label is derived from the prompt before the worktree is created.
        let operation_id = loop {
            match events.recv().await.unwrap() {
                SupervisorEvent::TrustRequired { operation_id, .. } => break operation_id,
                _ => continue,
            }
        };
        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "rewrite-the-lexer")
            .expect("prompt-derived label");
        let branch_id = task
            .branch
            .strip_prefix("forge/session-")
            .expect("session branch prefix");
        assert_eq!(branch_id.parse::<SessionId>().unwrap().get_version_num(), 4);

        handle
            .command(SupervisorCommand::FinalizeCreation { operation_id })
            .await
            .unwrap();
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
            .command(SupervisorCommand::CreateSession {
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
            .sessions()
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
            .sessions()
            .await
            .unwrap()
            .iter()
            .all(|task| task.label != "parser"));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn archived_cleanup_removes_only_the_checkout_and_retires_the_task_actor() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        let mut events = handle.subscribe();
        handle
            .command(SupervisorCommand::CreateSession {
                label: "cleanup".into(),
                first_prompt: None,
            })
            .await
            .unwrap();
        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "cleanup")
            .expect("created task");
        assert!(task.workspace.exists());

        handle
            .command(SupervisorCommand::ArchiveSession {
                session_id: task.session_id,
            })
            .await
            .unwrap();
        handle
            .command(SupervisorCommand::RemoveManagedWorktree {
                session_id: task.session_id,
            })
            .await
            .unwrap();

        assert!(!task.workspace.exists());
        assert_eq!(
            control.session(task.session_id).await.unwrap().lifecycle,
            SessionLifecycle::Removed
        );
        loop {
            match events.recv().await.unwrap() {
                SupervisorEvent::Roster(roster)
                    if roster
                        .iter()
                        .all(|snapshot| snapshot.task.session_id != task.session_id) =>
                {
                    break;
                }
                _ => continue,
            }
        }
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn archived_cleanup_drains_background_work_before_removing_the_checkout() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let worktree = forge_storage::create_session_worktree(repo.path(), scratch.path()).unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let control_dir = RepositoryRuntimeStorage::new(repo.path())
            .unwrap()
            .path_for(RuntimeDataKind::Control)
            .unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let lease = RepositoryLease::acquire(&control_dir, repo.path()).unwrap();
        let journal = scratch.path().join("journals");
        let mut session_cfg = Config {
            resolved_workspace: worktree.path.clone(),
            workspace_root: Some(worktree.path.display().to_string()),
            ..Default::default()
        };
        session_cfg.journal.path = journal.display().to_string();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(vec![]));
        let mut opened = open_session_with_model(&session_cfg, SessionTarget::New, model.clone())
            .await
            .unwrap();
        let session_id = opened.session.session_id;
        opened
            .session
            .spawn_background_shell("sleep 30".into(), "retire me".into())
            .await
            .unwrap();
        let mut task = task_for(session_id, "retire", &worktree.path);
        task.branch = worktree.branch.clone();
        control
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
                None,
            )
            .await
            .unwrap();
        let mut supervisor_cfg = Config {
            resolved_workspace: repo.path().to_path_buf(),
            workspace_root: Some(repo.path().display().to_string()),
            ..Default::default()
        };
        supervisor_cfg.journal.path = journal.display().to_string();
        let (supervisor, handle) = RepositorySupervisor::spawn_with_trust_store(
            control.clone(),
            lease,
            vec![(task.clone(), opened.session)],
            2,
            supervisor_cfg,
            model,
            Some(trust_store),
        )
        .await
        .unwrap();

        let started = supervisor.snapshot(session_id).await.unwrap();
        assert!(started.details.as_ref().is_some_and(|details| {
            details
                .background
                .iter()
                .any(|task| !task.status.is_terminal())
        }));
        handle
            .command(SupervisorCommand::ArchiveSession { session_id })
            .await
            .unwrap();
        handle
            .command(SupervisorCommand::RemoveManagedWorktree { session_id })
            .await
            .unwrap();

        assert!(!task.workspace.exists());
        assert_eq!(
            control.session(session_id).await.unwrap().lifecycle,
            SessionLifecycle::Removed
        );
        assert!(supervisor.snapshot(session_id).await.is_none());
        // Cleanup is idempotent after the durable Removed transition.
        handle
            .command(SupervisorCommand::RemoveManagedWorktree { session_id })
            .await
            .unwrap();
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_drains_background_work_before_releasing_session_actors() {
        let temp = TempDir::new().unwrap();
        let control = Arc::new(RepositoryControl::open(temp.path()).await.unwrap());
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut cfg = Config {
            resolved_workspace: workspace.clone(),
            workspace_root: Some(workspace.display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = temp.path().join("journals").display().to_string();
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient::script(Vec::new()));
        let mut session = open_session_with_model(&cfg, SessionTarget::New, model.clone())
            .await
            .unwrap()
            .session;
        let session_id = session.session_id;
        let background_id = session
            .spawn_background_shell("sleep 30".into(), "shutdown me".into())
            .await
            .unwrap();
        let task = task_for(session_id, "shutdown", &workspace);
        control
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
                None,
            )
            .await
            .unwrap();
        let lease = RepositoryLease::acquire(temp.path(), temp.path()).unwrap();
        let (supervisor, handle) =
            RepositorySupervisor::spawn(control, lease, vec![(task, session)], 1, cfg, model)
                .await
                .unwrap();

        handle.command(SupervisorCommand::Shutdown).await.unwrap();

        let snapshot = supervisor.snapshot(session_id).await.unwrap();
        assert!(snapshot.details.as_ref().is_some_and(|details| {
            details
                .background
                .iter()
                .find(|task| task.id == background_id)
                .is_some_and(|task| task.status.is_terminal())
        }));
    }

    #[tokio::test]
    async fn dirty_archived_cleanup_preserves_the_worktree_and_task_binding() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let (_cfg, control, handle) =
            git_backed_supervisor(repo.path(), &trust_store, &scratch.path().join("journals"))
                .await;

        handle
            .command(SupervisorCommand::CreateSession {
                label: "dirty-cleanup".into(),
                first_prompt: None,
            })
            .await
            .unwrap();
        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "dirty-cleanup")
            .expect("created task");
        std::fs::write(task.workspace.join("uncommitted.txt"), "keep me").unwrap();

        handle
            .command(SupervisorCommand::ArchiveSession {
                session_id: task.session_id,
            })
            .await
            .unwrap();
        let error = handle
            .command(SupervisorCommand::RemoveManagedWorktree {
                session_id: task.session_id,
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("dirty"), "unexpected cleanup error: {error}");
        assert!(task.workspace.exists());
        assert_eq!(
            control.session(task.session_id).await.unwrap().lifecycle,
            SessionLifecycle::Archived
        );
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
        let linked = forge_storage::create_session_worktree(repo.path(), base.path()).unwrap();

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

    #[tokio::test]
    async fn a_created_session_inherits_the_selected_sessions_model() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let trust_store = scratch.path().join("trust.toml");
        let storage = RepositoryRuntimeStorage::new(repo.path()).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());
        let lease = RepositoryLease::acquire(&control_dir, repo.path()).unwrap();

        let mut cfg = Config {
            resolved_workspace: repo.path().to_path_buf(),
            workspace_root: Some(repo.path().display().to_string()),
            model: forge_config::ModelConfig {
                provider: forge_config::ModelProviderKind::Mock,
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.journal.path = scratch.path().join("journals").display().to_string();
        let primary = scripted_session(&cfg, "primary answer").await;
        let primary_id = primary.session_id;
        let mut primary_task = task_for(primary_id, "main", repo.path());
        primary_task.ownership = WorktreeOwnership::Primary;
        primary_task.slot = Some(1);
        control
            .register_session(
                NewRepositorySession {
                    session_id: primary_task.session_id,
                    label: primary_task.label.clone(),
                    workspace: primary_task.workspace.clone(),
                    branch: primary_task.branch.clone(),
                    ownership: primary_task.ownership,
                    slot: primary_task.slot,
                    model_id: primary_task.model_id.clone(),
                    route_id: primary_task.route_id.clone(),
                    reasoning_effort: primary_task.reasoning_effort.clone(),
                },
                None,
            )
            .await
            .unwrap();

        let model: Arc<dyn ModelClient> =
            Arc::new(MockModelClient::script(vec![text_response("done")]));
        let (_supervisor, handle) = RepositorySupervisor::spawn_with_trust_store(
            control.clone(),
            lease,
            vec![(primary_task, primary)],
            2,
            cfg,
            model,
            Some(trust_store.to_path_buf()),
        )
        .await
        .unwrap();

        handle
            .command(SupervisorCommand::SetModel {
                session_id: primary_id,
                model_id: "mock-v2".into(),
                route_id: "native-v2".into(),
                reasoning_effort: Some("high".into()),
            })
            .await
            .unwrap();
        handle
            .command(SupervisorCommand::CreateSession {
                label: "inherits".into(),
                first_prompt: None,
            })
            .await
            .unwrap();

        let task = control
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.label == "inherits")
            .expect("created task row");
        assert_eq!(task.model_id, "mock-v2");
        assert_eq!(task.route_id, "native-v2");
        assert_eq!(task.reasoning_effort.as_deref(), Some("high"));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn restoring_a_task_reapplies_its_stored_model() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config {
            resolved_workspace: dir.path().to_path_buf(),
            workspace_root: Some(dir.path().display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = dir.path().join("journal").display().to_string();
        let mut session = scripted_session(&cfg, "resumed answer").await;
        assert!(session.active_model.is_empty());

        let task = task_for(session.session_id, "resumed", dir.path());
        let mut stored = task.clone();
        stored.model_id = "mock-v2".into();
        stored.route_id = "native-v2".into();
        stored.reasoning_effort = Some("high".into());
        restore_task_model(&mut session, &stored);
        assert_eq!(session.active_model, "mock-v2");
        assert_eq!(session.active_route_id, "native-v2");
        assert_eq!(session.reasoning_effort(), Some("high"));

        // A row without a stored model leaves the in-memory selection alone.
        let mut blank = task.clone();
        blank.model_id = String::new();
        blank.route_id = "native-v3".into();
        blank.reasoning_effort = Some("low".into());
        restore_task_model(&mut session, &blank);
        assert_eq!(session.active_model, "mock-v2");
    }

    #[tokio::test]
    async fn a_restart_restores_the_stored_model_of_an_open_session() {
        let repo = TempDir::new().unwrap();
        forge_test_support::init_repo_with_commit(repo.path());
        let scratch = TempDir::new().unwrap();
        let base = scratch.path().join("worktrees");
        std::fs::create_dir_all(&base).unwrap();
        let linked = forge_storage::create_session_worktree(repo.path(), &base).unwrap();
        let journal = scratch.path().join("journals");

        let storage = RepositoryRuntimeStorage::new(repo.path()).unwrap();
        let control_dir = storage.path_for(RuntimeDataKind::Control).unwrap();
        let control = Arc::new(RepositoryControl::open(&control_dir).await.unwrap());

        let mut cfg = Config {
            resolved_workspace: repo.path().to_path_buf(),
            workspace_root: Some(repo.path().display().to_string()),
            model: forge_config::ModelConfig {
                provider: forge_config::ModelProviderKind::Mock,
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.journal.path = journal.display().to_string();
        let primary = scripted_session(&cfg, "primary answer").await;
        let primary_id = primary.session_id;
        let mut primary_task = task_for(primary_id, "main", repo.path());
        primary_task.ownership = WorktreeOwnership::Primary;
        primary_task.slot = Some(1);
        primary_task.branch = "main".into();
        control
            .register_session(
                NewRepositorySession {
                    session_id: primary_task.session_id,
                    label: primary_task.label.clone(),
                    workspace: primary_task.workspace.clone(),
                    branch: primary_task.branch.clone(),
                    ownership: primary_task.ownership,
                    slot: primary_task.slot,
                    model_id: primary_task.model_id.clone(),
                    route_id: primary_task.route_id.clone(),
                    reasoning_effort: primary_task.reasoning_effort.clone(),
                },
                None,
            )
            .await
            .unwrap();

        // A managed session whose row records a model selection, as SetModel
        // now persists it. Its journal lives in the shared dir so resume finds it.
        let mut managed_cfg = Config {
            resolved_workspace: linked.path.clone(),
            workspace_root: Some(linked.path.display().to_string()),
            model: forge_config::ModelConfig {
                provider: forge_config::ModelProviderKind::Mock,
                ..Default::default()
            },
            ..Default::default()
        };
        managed_cfg.journal.path = journal.display().to_string();
        let managed = scripted_session(&managed_cfg, "managed answer").await;
        let managed_id = managed.session_id;
        control
            .register_session(
                NewRepositorySession {
                    session_id: managed_id,
                    label: "managed".into(),
                    workspace: linked.path.clone(),
                    branch: linked.branch.clone(),
                    ownership: WorktreeOwnership::Managed,
                    slot: None,
                    model_id: "mock-v2".into(),
                    route_id: "native-v2".into(),
                    reasoning_effort: Some("high".into()),
                },
                None,
            )
            .await
            .unwrap();

        let bootstrap = RepositoryBootstrap::acquire(&cfg).await.unwrap();
        let (supervisor, handle) = bootstrap.open_with_primary(&cfg, primary).await.unwrap();
        let reopened = supervisor
            .snapshots()
            .await
            .into_iter()
            .find(|snapshot| snapshot.task.session_id == managed_id)
            .expect("managed session reopened on restart");
        let details = reopened.details.as_ref().unwrap();
        assert_eq!(details.active_model, "mock-v2");
        assert_eq!(details.active_route_id, "native-v2");
        assert_eq!(details.reasoning_effort.as_deref(), Some("high"));
        handle.command(SupervisorCommand::Shutdown).await.unwrap();
    }
}
