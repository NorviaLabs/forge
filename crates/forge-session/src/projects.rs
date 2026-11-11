//! Several projects in one process: the [`ProjectService`] actor.
//!
//! It owns the registry and one [`RepositorySupervisor`] per open project.
//! Each supervisor is unchanged from single-project use: its own config,
//! trust, owner lock, storage and run slots. The service adds only what
//! spans projects: opening and closing, the registry, and one
//! [`WorkspaceState`] that merges every open project's [`ProjectState`].
//!
//! `WorkspaceState` is updated by one small task per open project, outside
//! the command loop, so a slow open or close never delays attention updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_config::{Config, ConfigOverrides};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    AttentionItem, LaunchError, LaunchPlan, LaunchTarget, LeaseOwner, Project, ProjectId,
    ProjectRegistry, ProjectState, RegistryError, RepositorySession, RepositorySessionError,
    RepositorySupervisor, RepositorySupervisorError, SessionRuntimeSnapshot, SupervisorHandle,
    SupervisorTurnState,
};
use forge_types::SessionId;

/// How long a project's sessions get to stop before the supervisor is
/// dropped anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// One registered project and, when open, its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project: Project,
    pub open: bool,
    /// Default (no sessions) while the project is closed.
    pub state: ProjectState,
    /// The project's most recent error or failed turn. A frontend shows the
    /// ones from projects that aren't on screen, since it doesn't receive
    /// their events.
    pub last_error: Option<ProjectNotice>,
}

/// An error a project reported. `seq` increases with each one, so a reader
/// can tell a new error from one it already showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNotice {
    pub seq: u64,
    pub session_id: Option<SessionId>,
    pub message: String,
}

/// Every registered project, in registry order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceState {
    /// Increases by one on every change.
    pub version: u64,
    pub projects: Vec<ProjectSummary>,
}

impl WorkspaceState {
    /// Whether any open project has a session queued, running, or waiting on
    /// you: quitting now would stop work, so the window asks first.
    pub fn has_busy_sessions(&self) -> bool {
        self.projects
            .iter()
            .any(|summary| !busy_sessions(&summary.state).is_empty())
    }
}

/// Attention items in `next` that were not in `previous`, keyed by
/// `(project, session, call_id)`. Drives the badge and the sound, so the same
/// request never rings twice.
pub fn new_attention(
    previous: &WorkspaceState,
    next: &WorkspaceState,
) -> Vec<(ProjectId, AttentionItem)> {
    let seen = |project: ProjectId, item: &AttentionItem| {
        previous
            .projects
            .iter()
            .filter(|summary| summary.project.id == project)
            .flat_map(|summary| &summary.state.attention)
            .any(|old| {
                old.session_id == item.session_id
                    && old.kind == item.kind
                    && old.call_id == item.call_id
            })
    };
    next.projects
        .iter()
        .flat_map(|summary| {
            summary
                .state
                .attention
                .iter()
                .map(move |item| (summary.project.id, item))
        })
        .filter(|(project, item)| !seen(*project, item))
        .map(|(project, item)| (project, item.clone()))
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("the project folder {0} no longer exists")]
    RootMissing(PathBuf),
    #[error("{0} is not trusted yet")]
    NeedsTrust(PathBuf),
    #[error("{}", already_owned(.owner))]
    AlreadyOwned { owner: Option<LeaseOwner> },
    #[error("cannot load config: {0}")]
    Config(String),
    #[error("cannot open the project: {0}")]
    Launch(String),
    #[error("cannot record trust: {0}")]
    Trust(String),
    #[error("the project isn't open")]
    NotOpen,
    #[error("close the project first")]
    Open,
    /// Sessions that are queued, running, or waiting on you.
    #[error("{} session(s) are still busy", .sessions.len())]
    Busy {
        sessions: Vec<(SessionId, SupervisorTurnState)>,
    },
    #[error(transparent)]
    Supervisor(#[from] RepositorySupervisorError),
    #[error("the project service has stopped")]
    Stopped,
}

fn already_owned(owner: &Option<LeaseOwner>) -> String {
    match owner {
        Some(owner) => format!(
            "another Forge (pid {}, started {}) has {} open",
            owner.pid,
            owner.started_at.to_rfc3339(),
            owner.workspace.display()
        ),
        None => "another Forge has this project open".into(),
    }
}

/// An open project's supervisor. `None` once the project has closed; a
/// reader holding an old [`OpenProjectRef`] then gets
/// [`TargetError::ShuttingDown`].
type SharedSupervisor = Arc<tokio::sync::RwLock<Option<RepositorySupervisor>>>;

/// Why a session, or the project that owns it, could not be reached.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// No open project knows the session, live or closed.
    #[error("that session no longer exists")]
    UnknownSession(SessionId),
    /// The project closed while the caller still held it.
    #[error("Forge is shutting down")]
    ShuttingDown,
    #[error(transparent)]
    Supervisor(#[from] RepositorySupervisorError),
    #[error(transparent)]
    Project(#[from] ProjectError),
}

/// What a frontend needs to work with one open project: commands and events
/// through `handle`, reads through the methods here.
#[derive(Clone)]
pub struct OpenProjectRef {
    pub project: Project,
    pub handle: SupervisorHandle,
    supervisor: SharedSupervisor,
}

impl OpenProjectRef {
    /// Every live session.
    pub async fn snapshots(&self) -> Result<Vec<SessionRuntimeSnapshot>, TargetError> {
        let guard = self.supervisor.read().await;
        let supervisor = guard.as_ref().ok_or(TargetError::ShuttingDown)?;
        Ok(supervisor.snapshots().await)
    }

    /// One live session; `None` for a session this process has closed.
    pub async fn snapshot(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionRuntimeSnapshot>, TargetError> {
        let guard = self.supervisor.read().await;
        let supervisor = guard.as_ref().ok_or(TargetError::ShuttingDown)?;
        Ok(supervisor.snapshot(session_id).await)
    }

    /// Durable session rows, including sessions this process has closed.
    pub async fn session_records(&self) -> Result<Vec<RepositorySession>, TargetError> {
        let guard = self.supervisor.read().await;
        let supervisor = guard.as_ref().ok_or(TargetError::ShuttingDown)?;
        Ok(supervisor.session_records().await?)
    }

    /// The session the operator last selected in this project.
    pub async fn selected_session_id(&self) -> Result<Option<SessionId>, TargetError> {
        let guard = self.supervisor.read().await;
        let supervisor = guard.as_ref().ok_or(TargetError::ShuttingDown)?;
        Ok(supervisor.selected_session_id().await?)
    }

    /// Where this project's session journals live.
    pub async fn journal_dir(&self) -> Result<PathBuf, TargetError> {
        let guard = self.supervisor.read().await;
        let supervisor = guard.as_ref().ok_or(TargetError::ShuttingDown)?;
        Ok(supervisor.journal_dir())
    }

    /// Whether the session is live in this project.
    fn lists(&self, session_id: SessionId) -> bool {
        self.handle
            .project_state()
            .borrow()
            .sessions
            .iter()
            .any(|session| session.session_id == session_id)
    }
}

/// Loads a project's config from its root.
pub type ConfigLoader = Arc<dyn Fn(&Path) -> Result<Config, String> + Send + Sync>;

pub struct ProjectServiceOptions {
    pub registry: ProjectRegistry,
    /// `None` is the user's trust store; tests use a temporary file.
    pub trust_store: Option<PathBuf>,
    pub load_config: ConfigLoader,
}

impl ProjectServiceOptions {
    /// The user's registry and trust store, and each project's own
    /// `forge.toml` (with the user and environment layers on top).
    pub fn user() -> Result<Self, RegistryError> {
        Ok(Self {
            registry: ProjectRegistry::user()?,
            trust_store: None,
            load_config: Arc::new(|root| {
                Config::load(ConfigOverrides {
                    workspace: Some(root.to_path_buf()),
                    project_root: Some(root.to_path_buf()),
                    ..Default::default()
                })
                .map_err(|error| error.to_string())
            }),
        })
    }
}

type Reply<T> = oneshot::Sender<Result<T, ProjectError>>;

enum Request {
    Add(PathBuf, Reply<Project>),
    ResolveLaunch(PathBuf, Reply<crate::LaunchResolution>),
    Open(ProjectId, Reply<Vec<String>>),
    Trust(ProjectId, Reply<()>),
    Relocate(ProjectId, PathBuf, Reply<Project>),
    Close(ProjectId, bool, Reply<Vec<String>>),
    Remove(ProjectId, Reply<Project>),
    Project(ProjectId, Reply<OpenProjectRef>),
    OpenProjects(Reply<Vec<OpenProjectRef>>),
    Shutdown(Reply<Vec<String>>),
}

/// The way to reach the service. Cloneable; the service stops (closing every
/// project) when the last handle is dropped or on [`shutdown`](Self::shutdown).
#[derive(Clone)]
pub struct ProjectServiceHandle {
    requests: mpsc::Sender<Request>,
    workspace: watch::Receiver<WorkspaceState>,
}

impl ProjectServiceHandle {
    /// Every registered project and the state of the open ones. A slow
    /// reader never misses an update; it sees the latest value.
    pub fn workspace(&self) -> watch::Receiver<WorkspaceState> {
        self.workspace.clone()
    }

    /// Register the project containing `path`. See [`ProjectRegistry::add`].
    pub async fn add(&self, path: PathBuf) -> Result<Project, ProjectError> {
        self.ask(|reply| Request::Add(path, reply)).await
    }

    /// The project a launch from `cwd` should open; see
    /// [`ProjectRegistry::resolve_launch`]. A Git repository is registered.
    pub async fn resolve_launch(
        &self,
        cwd: PathBuf,
    ) -> Result<crate::LaunchResolution, ProjectError> {
        self.ask(|reply| Request::ResolveLaunch(cwd, reply)).await
    }

    /// Open a registered project: load its config, check trust, take its
    /// owner lock and start its sessions. Writes nothing into an untrusted
    /// project. Opening an open project does nothing. Returns startup
    /// notices.
    pub async fn open(&self, id: ProjectId) -> Result<Vec<String>, ProjectError> {
        self.ask(|reply| Request::Open(id, reply)).await
    }

    /// Record trust for a project's root. Trusting a trusted project is fine.
    pub async fn trust(&self, id: ProjectId) -> Result<(), ProjectError> {
        self.ask(|reply| Request::Trust(id, reply)).await
    }

    /// Point a closed project at its moved folder.
    pub async fn relocate(&self, id: ProjectId, path: PathBuf) -> Result<Project, ProjectError> {
        self.ask(|reply| Request::Relocate(id, path, reply)).await
    }

    /// Close an open project. Refuses with [`ProjectError::Busy`] while a
    /// session is queued, running, or waiting on you, unless `stop_running`.
    /// Queued prompts are kept and wait for you when the project reopens.
    pub async fn close(
        &self,
        id: ProjectId,
        stop_running: bool,
    ) -> Result<Vec<String>, ProjectError> {
        self.ask(|reply| Request::Close(id, stop_running, reply))
            .await
    }

    /// Unregister a closed project. Never touches its folder or sessions.
    pub async fn remove(&self, id: ProjectId) -> Result<Project, ProjectError> {
        self.ask(|reply| Request::Remove(id, reply)).await
    }

    /// An open project's supervisor, for its sessions and events.
    pub async fn project(&self, id: ProjectId) -> Result<OpenProjectRef, ProjectError> {
        self.ask(|reply| Request::Project(id, reply)).await
    }

    /// Every open project, in registry order.
    pub async fn open_projects(&self) -> Result<Vec<OpenProjectRef>, ProjectError> {
        self.ask(Request::OpenProjects).await
    }

    /// Session targeting: the open project that owns `session_id`, whether
    /// the session is live or closed. A session no open project knows is
    /// [`TargetError::UnknownSession`]; there is no fallback project.
    pub async fn project_of(&self, session_id: SessionId) -> Result<OpenProjectRef, TargetError> {
        let open = self.open_projects().await?;
        if let Some(project) = open.iter().find(|project| project.lists(session_id)) {
            return Ok(project.clone());
        }
        // Not live anywhere: a closed session, or one created so recently
        // that its project has not published it yet. The durable rows know.
        for project in open {
            match project.session_records().await {
                Ok(records) => {
                    if records.iter().any(|record| record.session_id == session_id) {
                        return Ok(project);
                    }
                }
                // A project closing under us owns nothing reachable.
                Err(TargetError::ShuttingDown) => {}
                Err(error) => return Err(error),
            }
        }
        Err(TargetError::UnknownSession(session_id))
    }

    /// The supervisor handle of an open project.
    pub async fn supervisor(&self, id: ProjectId) -> Result<SupervisorHandle, ProjectError> {
        Ok(self.project(id).await?.handle)
    }

    /// Stop every project's agents and close them all, in parallel.
    pub async fn shutdown(&self) -> Result<Vec<String>, ProjectError> {
        self.ask(Request::Shutdown).await
    }

    async fn ask<T>(&self, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, ProjectError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_| ProjectError::Stopped)?;
        response.await.map_err(|_| ProjectError::Stopped)?
    }
}

struct OpenProject {
    project: Project,
    supervisor: SharedSupervisor,
    handle: SupervisorHandle,
    forwarder: tokio::task::JoinHandle<()>,
}

impl OpenProject {
    fn reference(&self) -> OpenProjectRef {
        OpenProjectRef {
            project: self.project.clone(),
            handle: self.handle.clone(),
            supervisor: self.supervisor.clone(),
        }
    }
}

pub struct ProjectService {
    options: ProjectServiceOptions,
    open: HashMap<ProjectId, OpenProject>,
    workspace: Arc<watch::Sender<WorkspaceState>>,
}

impl ProjectService {
    /// Start the service. Reads the registry once so the first
    /// [`WorkspaceState`] lists every project.
    pub fn spawn(options: ProjectServiceOptions) -> Result<ProjectServiceHandle, RegistryError> {
        let projects = options.registry.list()?;
        let (workspace, workspace_rx) = watch::channel(WorkspaceState {
            version: 1,
            projects: projects
                .into_iter()
                .map(|project| ProjectSummary {
                    project,
                    open: false,
                    state: ProjectState::default(),
                    last_error: None,
                })
                .collect(),
        });
        let (requests, receiver) = mpsc::channel(64);
        let service = Self {
            options,
            open: HashMap::new(),
            workspace: Arc::new(workspace),
        };
        tokio::spawn(service.run(receiver));
        Ok(ProjectServiceHandle {
            requests,
            workspace: workspace_rx,
        })
    }

    async fn run(mut self, mut receiver: mpsc::Receiver<Request>) {
        while let Some(request) = receiver.recv().await {
            match request {
                Request::Add(path, reply) => {
                    let _ = reply.send(self.add(&path));
                }
                Request::ResolveLaunch(cwd, reply) => {
                    let resolved = self.options.registry.resolve_launch(&cwd);
                    let published = self.publish_registry();
                    let _ = reply.send(
                        resolved
                            .map_err(ProjectError::from)
                            .and_then(|resolved| published.map(|()| resolved).map_err(Into::into)),
                    );
                }
                Request::Open(id, reply) => {
                    let _ = reply.send(self.open_project(id).await);
                }
                Request::Trust(id, reply) => {
                    let _ = reply.send(self.trust(id));
                }
                Request::Relocate(id, path, reply) => {
                    let _ = reply.send(self.relocate(id, &path));
                }
                Request::Close(id, stop_running, reply) => {
                    let _ = reply.send(self.close(id, stop_running).await);
                }
                Request::Remove(id, reply) => {
                    let _ = reply.send(self.remove(id));
                }
                Request::Project(id, reply) => {
                    let _ = reply.send(
                        self.open
                            .get(&id)
                            .map(OpenProject::reference)
                            .ok_or(ProjectError::NotOpen),
                    );
                }
                Request::OpenProjects(reply) => {
                    let order = self.options.registry.list().unwrap_or_default();
                    let _ = reply.send(Ok(order
                        .iter()
                        .filter_map(|project| self.open.get(&project.id))
                        .map(OpenProject::reference)
                        .collect()));
                }
                Request::Shutdown(reply) => {
                    let _ = reply.send(Ok(self.close_all().await));
                    return;
                }
            }
        }
        // Every handle is gone: nobody can close the projects, so stop them.
        self.close_all().await;
    }

    fn add(&mut self, path: &Path) -> Result<Project, ProjectError> {
        let project = self.options.registry.add(path)?;
        self.publish_registry()?;
        Ok(project)
    }

    fn trust(&self, id: ProjectId) -> Result<(), ProjectError> {
        let project = self.options.registry.get(id)?;
        let result = match self.options.trust_store.as_deref() {
            Some(store) => forge_config::grant_trust_at(store, &project.root),
            None => forge_config::grant_trust(&project.root),
        };
        result
            .map(|_| ())
            .map_err(|error| ProjectError::Trust(error.to_string()))
    }

    fn relocate(&mut self, id: ProjectId, path: &Path) -> Result<Project, ProjectError> {
        if self.open.contains_key(&id) {
            return Err(ProjectError::Open);
        }
        let project = self.options.registry.relocate(id, path)?;
        self.publish_registry()?;
        Ok(project)
    }

    fn remove(&mut self, id: ProjectId) -> Result<Project, ProjectError> {
        if self.open.contains_key(&id) {
            return Err(ProjectError::Open);
        }
        let project = self.options.registry.remove(id)?;
        self.publish_registry()?;
        Ok(project)
    }

    async fn open_project(&mut self, id: ProjectId) -> Result<Vec<String>, ProjectError> {
        if self.open.contains_key(&id) {
            return Ok(Vec::new());
        }
        let project = self.options.registry.get(id)?;
        if !project.root.is_dir() {
            return Err(ProjectError::RootMissing(project.root));
        }
        // Loading config writes nothing, and the project layer is
        // untrusted either way, so it is safe before the trust check.
        let cfg = (self.options.load_config)(&project.root).map_err(ProjectError::Config)?;
        let plan = LaunchPlan::prepare_with_trust_store(&cfg, self.options.trust_store.clone())
            .await
            .map_err(launch_error)?;
        if plan.needs_trust() {
            return Err(ProjectError::NeedsTrust(project.root));
        }
        let launched = plan
            .launch(LaunchTarget::Continue)
            .await
            .map_err(launch_error)?;
        let (supervisor, handle) = launched
            .bootstrap
            .open_with_primary(&cfg, launched.opened.session)
            .await?;
        let forwarder = tokio::spawn(forward_project_state(
            id,
            handle.project_state(),
            handle.subscribe(),
            self.workspace.clone(),
        ));
        self.open.insert(
            id,
            OpenProject {
                project,
                supervisor: Arc::new(tokio::sync::RwLock::new(Some(supervisor))),
                handle,
                forwarder,
            },
        );
        let name = &self.open[&id].project.name;
        Ok(launched
            .notices
            .into_iter()
            .map(|notice| format!("{name}: {notice}"))
            .collect())
    }

    async fn close(
        &mut self,
        id: ProjectId,
        stop_running: bool,
    ) -> Result<Vec<String>, ProjectError> {
        let open = self.open.get(&id).ok_or(ProjectError::NotOpen)?;
        let sessions = busy_sessions(&open.handle.project_state().borrow());
        if !sessions.is_empty() && !stop_running {
            return Err(ProjectError::Busy { sessions });
        }
        let open = self.open.remove(&id).ok_or(ProjectError::NotOpen)?;
        let notices = stop_project(open).await;
        self.workspace.send_modify(|workspace| {
            if let Some(summary) = workspace.projects.iter_mut().find(|s| s.project.id == id) {
                summary.open = false;
                summary.state = ProjectState::default();
            }
            workspace.version += 1;
        });
        Ok(notices)
    }

    async fn close_all(&mut self) -> Vec<String> {
        let mut stopping = tokio::task::JoinSet::new();
        for (_, open) in self.open.drain() {
            stopping.spawn(stop_project(open));
        }
        let mut notices = Vec::new();
        while let Some(result) = stopping.join_next().await {
            notices.extend(result.unwrap_or_default());
        }
        self.workspace.send_modify(|workspace| {
            for summary in &mut workspace.projects {
                summary.open = false;
                summary.state = ProjectState::default();
            }
            workspace.version += 1;
        });
        notices
    }

    /// Re-read the registry into the workspace, keeping open projects' state.
    fn publish_registry(&self) -> Result<(), RegistryError> {
        let projects = self.options.registry.list()?;
        self.workspace.send_modify(|workspace| {
            let previous = std::mem::take(&mut workspace.projects);
            workspace.projects = projects
                .into_iter()
                .map(|project| {
                    let kept = previous
                        .iter()
                        .find(|summary| summary.project.id == project.id);
                    ProjectSummary {
                        open: self.open.contains_key(&project.id),
                        state: kept
                            .map(|summary| summary.state.clone())
                            .unwrap_or_default(),
                        last_error: kept.and_then(|summary| summary.last_error.clone()),
                        project,
                    }
                })
                .collect();
            workspace.version += 1;
        });
        Ok(())
    }
}

/// Sessions that would lose work if the project closed now.
fn busy_sessions(state: &ProjectState) -> Vec<(SessionId, SupervisorTurnState)> {
    state
        .sessions
        .iter()
        .filter(|session| {
            matches!(
                session.turn_state,
                SupervisorTurnState::Queued
                    | SupervisorTurnState::Running
                    | SupervisorTurnState::Waiting
            )
        })
        .map(|session| (session.session_id, session.turn_state))
        .collect()
}

/// Stop a project's sessions and release its owner lock. Bounded: past
/// [`CLOSE_TIMEOUT`] the supervisor is dropped, which releases the lock.
async fn stop_project(open: OpenProject) -> Vec<String> {
    open.forwarder.abort();
    let name = open.project.name.clone();
    // Taking it waits for readers to finish, and every later reader sees the
    // project closed.
    let Some(supervisor) = open.supervisor.write().await.take() else {
        return Vec::new();
    };
    match tokio::time::timeout(CLOSE_TIMEOUT, supervisor.shutdown()).await {
        Ok(Ok(())) => Vec::new(),
        Ok(Err(error)) => vec![format!("{name}: a session did not stop cleanly: {error}")],
        Err(_) => vec![format!(
            "{name}: sessions did not stop within {} seconds; they were interrupted",
            CLOSE_TIMEOUT.as_secs()
        )],
    }
}

/// Copy one project's state into the workspace whenever it changes, and
/// record its errors and failed turns as `last_error`.
async fn forward_project_state(
    id: ProjectId,
    mut state: watch::Receiver<ProjectState>,
    mut events: tokio::sync::broadcast::Receiver<crate::SupervisorEvent>,
    workspace: Arc<watch::Sender<WorkspaceState>>,
) {
    use tokio::sync::broadcast::error::RecvError;
    let update = |change: &dyn Fn(&mut ProjectSummary)| {
        workspace.send_modify(|workspace| {
            if let Some(summary) = workspace.projects.iter_mut().find(|s| s.project.id == id) {
                change(summary);
            }
            workspace.version += 1;
        });
    };
    let mut seq = 0;
    let mut state_open = true;
    let mut events_open = true;
    let current = state.borrow_and_update().clone();
    update(&|summary| {
        summary.open = true;
        summary.state = current.clone();
    });
    while state_open || events_open {
        tokio::select! {
            changed = state.changed(), if state_open => {
                if changed.is_err() {
                    state_open = false;
                    continue;
                }
                let current = state.borrow_and_update().clone();
                update(&|summary| summary.state = current.clone());
            }
            event = events.recv(), if events_open => {
                let notice = match event {
                    Ok(crate::SupervisorEvent::Error { session_id, message }) => {
                        Some((session_id, message))
                    }
                    Ok(crate::SupervisorEvent::Attention {
                        session_id,
                        state: SupervisorTurnState::Failed,
                        message,
                    }) => Some((Some(session_id), message)),
                    Ok(_) | Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => {
                        events_open = false;
                        None
                    }
                };
                if let Some((session_id, message)) = notice {
                    seq += 1;
                    let notice = ProjectNotice { seq, session_id, message };
                    update(&|summary| summary.last_error = Some(notice.clone()));
                }
            }
        }
    }
}

fn launch_error(error: LaunchError) -> ProjectError {
    match error {
        LaunchError::Untrusted { workspace } => ProjectError::NeedsTrust(workspace),
        LaunchError::Repository(RepositorySupervisorError::Control(
            RepositorySessionError::AlreadyOwned { owner, .. },
        )) => ProjectError::AlreadyOwned { owner },
        error => ProjectError::Launch(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttentionKind, SessionLifecycle, SessionSummary, SupervisorCommand};
    use tempfile::TempDir;

    struct Fixture {
        _config: TempDir,
        work: TempDir,
        trust_store: PathBuf,
        handle: ProjectServiceHandle,
    }

    /// A service whose projects run the mock model, with a private registry
    /// and trust store.
    fn fixture() -> Fixture {
        let config = TempDir::new().unwrap();
        let trust_store = config.path().join("trust.toml");
        let options = ProjectServiceOptions {
            registry: ProjectRegistry::at(config.path().join("projects.toml")),
            trust_store: Some(trust_store.clone()),
            load_config: Arc::new(|root| {
                let mut cfg = Config {
                    resolved_workspace: root.to_path_buf(),
                    workspace_root: Some(root.display().to_string()),
                    ..Default::default()
                };
                cfg.model.mock = true;
                Ok(cfg)
            }),
        };
        Fixture {
            _config: config,
            work: TempDir::new().unwrap(),
            trust_store,
            handle: ProjectService::spawn(options).unwrap(),
        }
    }

    fn folder(fixture: &Fixture, name: &str) -> PathBuf {
        let path = fixture.work.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn summary(state: &WorkspaceState, id: ProjectId) -> ProjectSummary {
        state
            .projects
            .iter()
            .find(|summary| summary.project.id == id)
            .unwrap()
            .clone()
    }

    #[tokio::test]
    async fn two_projects_open_side_by_side_and_close_releases_the_lock() {
        let f = fixture();
        let mut ids = Vec::new();
        for name in ["api", "notes"] {
            let root = folder(&f, name);
            forge_config::grant_trust_at(&f.trust_store, &root).unwrap();
            let project = f.handle.add(root).await.unwrap();
            f.handle.open(project.id).await.unwrap();
            assert!(f.handle.open(project.id).await.unwrap().is_empty(), "no-op");
            ids.push(project.id);
        }
        let mut workspace = f.handle.workspace();
        let state = workspace
            .wait_for(|state| {
                state.projects.len() == 2
                    && state
                        .projects
                        .iter()
                        .all(|summary| summary.open && summary.state.sessions.len() == 1)
            })
            .await
            .unwrap()
            .clone();
        assert_eq!(state.projects[0].project.name, "api");

        let open = f.handle.open_projects().await.unwrap();
        assert_eq!(open.iter().map(|p| p.project.id).collect::<Vec<_>>(), ids);
        let held = open[0].clone();
        assert_eq!(held.snapshots().await.unwrap().len(), 1);

        f.handle.close(ids[0], false).await.unwrap();
        assert!(
            matches!(held.snapshots().await, Err(TargetError::ShuttingDown)),
            "readers see the project closed"
        );
        let after = f.handle.workspace().borrow().clone();
        assert!(!summary(&after, ids[0]).open);
        assert!(summary(&after, ids[0]).state.sessions.is_empty());
        assert!(summary(&after, ids[1]).open);
        assert!(matches!(
            f.handle.supervisor(ids[0]).await,
            Err(ProjectError::NotOpen)
        ));
        // The lock went with it: the project opens again.
        f.handle.open(ids[0]).await.unwrap();
        f.handle.shutdown().await.unwrap();
    }

    /// A frontend only receives events from the project on screen, so a
    /// failure elsewhere must show up in the workspace, numbered.
    #[tokio::test]
    async fn a_project_error_reaches_the_workspace_with_a_sequence_number() {
        let f = fixture();
        let root = folder(&f, "worker");
        forge_config::grant_trust_at(&f.trust_store, &root).unwrap();
        let project = f.handle.add(root).await.unwrap();
        f.handle.open(project.id).await.unwrap();
        let open = f.handle.project(project.id).await.unwrap();
        let session_id = SessionId::new_v4();
        {
            let guard = open.supervisor.read().await;
            let supervisor = guard.as_ref().unwrap();
            supervisor.emit_for_test(crate::SupervisorEvent::Error {
                session_id: None,
                message: "provider unreachable".into(),
            });
            supervisor.emit_for_test(crate::SupervisorEvent::Attention {
                session_id,
                state: SupervisorTurnState::Failed,
                message: "turn failed".into(),
            });
            // Not an error: a turn waiting on you is attention, not failure.
            supervisor.emit_for_test(crate::SupervisorEvent::Attention {
                session_id,
                state: SupervisorTurnState::Waiting,
                message: "waiting".into(),
            });
        }
        let mut workspace = f.handle.workspace();
        let state = tokio::time::timeout(
            Duration::from_secs(2),
            workspace.wait_for(|state| {
                summary(state, project.id)
                    .last_error
                    .is_some_and(|notice| notice.seq == 2)
            }),
        )
        .await
        .expect("both errors arrived")
        .unwrap()
        .clone();
        let notice = summary(&state, project.id).last_error.unwrap();
        assert_eq!(notice.message, "turn failed");
        assert_eq!(notice.session_id, Some(session_id));
        f.handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_untrusted_project_opens_only_after_trust_and_writes_nothing_before() {
        let f = fixture();
        let root = folder(&f, "fresh");
        let project = f.handle.add(root.clone()).await.unwrap();
        assert!(matches!(
            f.handle.open(project.id).await,
            Err(ProjectError::NeedsTrust(path)) if path == root
        ));
        assert!(!root.join(".forge").exists());
        f.handle.trust(project.id).await.unwrap();
        f.handle.trust(project.id).await.unwrap();
        f.handle.open(project.id).await.unwrap();
        assert!(root.join(".forge/local/control").is_dir());
        f.handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_project_owned_by_another_process_names_its_owner() {
        let f = fixture();
        let root = folder(&f, "taken");
        forge_config::grant_trust_at(&f.trust_store, &root).unwrap();
        let project = f.handle.add(root.clone()).await.unwrap();
        let control = crate::ProjectTopology::detect(&root)
            .unwrap()
            .storage()
            .unwrap()
            .path_for(forge_storage::RuntimeDataKind::Control)
            .unwrap();
        let _held = crate::RepositoryLease::acquire(&control, &root).unwrap();
        match f.handle.open(project.id).await {
            Err(ProjectError::AlreadyOwned { owner: Some(owner) }) => {
                assert_eq!(owner.pid, std::process::id());
            }
            other => panic!("expected AlreadyOwned, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_waits_for_close_and_a_missing_folder_is_reported() {
        let f = fixture();
        let root = folder(&f, "gone");
        forge_config::grant_trust_at(&f.trust_store, &root).unwrap();
        let project = f.handle.add(root.clone()).await.unwrap();
        f.handle.open(project.id).await.unwrap();
        assert!(matches!(
            f.handle.remove(project.id).await,
            Err(ProjectError::Open)
        ));
        f.handle.close(project.id, false).await.unwrap();

        std::fs::remove_dir_all(&root).unwrap();
        assert!(matches!(
            f.handle.open(project.id).await,
            Err(ProjectError::RootMissing(_))
        ));
        f.handle.remove(project.id).await.unwrap();
        assert!(f.handle.workspace().borrow().projects.is_empty());
    }

    /// Two trusted Git projects, open, each with its first session live.
    async fn two_open_projects(f: &Fixture) -> Vec<(ProjectId, SessionId)> {
        let mut opened = Vec::new();
        for name in ["api", "notes"] {
            let root = folder(f, name);
            forge_test_support::init_repo_with_commit(&root);
            forge_config::grant_trust_at(&f.trust_store, &root).unwrap();
            let project = f.handle.add(root).await.unwrap();
            f.handle.open(project.id).await.unwrap();
            let reference = f.handle.project(project.id).await.unwrap();
            let session = reference.snapshots().await.unwrap()[0].task.session_id;
            let mut state = reference.handle.project_state();
            state
                .wait_for(|state| state.sessions.len() == 1)
                .await
                .unwrap();
            opened.push((project.id, session));
        }
        opened
    }

    #[tokio::test]
    async fn a_live_session_resolves_to_the_project_that_owns_it() {
        let f = fixture();
        let opened = two_open_projects(&f).await;
        for (project, session) in &opened {
            let owner = f.handle.project_of(*session).await.unwrap();
            assert_eq!(owner.project.id, *project);
            assert!(owner.snapshot(*session).await.unwrap().is_some());
        }
        // The other project does not have it, live or closed.
        let other = f.handle.project(opened[1].0).await.unwrap();
        assert!(other.snapshot(opened[0].1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_session_no_open_project_knows_is_unknown_not_a_fallback() {
        let f = fixture();
        let opened = two_open_projects(&f).await;
        let stranger = SessionId::from_u128(0x5eed);
        assert!(matches!(
            f.handle.project_of(stranger).await,
            Err(TargetError::UnknownSession(id)) if id == stranger
        ));

        // A closed project's sessions are unreachable, and a reference held
        // from before the close says so instead of answering.
        let (project, session) = opened[0];
        let held = f.handle.project(project).await.unwrap();
        f.handle.close(project, false).await.unwrap();
        assert!(matches!(
            f.handle.project_of(session).await,
            Err(TargetError::UnknownSession(_))
        ));
        assert!(matches!(
            held.snapshot(session).await,
            Err(TargetError::ShuttingDown)
        ));
        assert!(matches!(
            held.session_records().await,
            Err(TargetError::ShuttingDown)
        ));
        assert!(matches!(
            held.selected_session_id().await,
            Err(TargetError::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn a_closed_session_resolves_through_its_durable_record() {
        let f = fixture();
        let opened = two_open_projects(&f).await;
        let (project, first) = opened[1];
        let reference = f.handle.project(project).await.unwrap();
        reference
            .handle
            .command(SupervisorCommand::CreateSession {
                label: "second".into(),
                first_prompt: None,
                github_issue_number: None,
            })
            .await
            .unwrap();
        let mut state = reference.handle.project_state();
        let second = tokio::time::timeout(
            Duration::from_secs(20),
            state.wait_for(|state| state.sessions.len() == 2),
        )
        .await
        .expect("the second session appears")
        .unwrap()
        .sessions
        .iter()
        .map(|session| session.session_id)
        .find(|id| *id != first)
        .unwrap();
        reference
            .handle
            .command(SupervisorCommand::CloseSession { session_id: second })
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(20),
            state.wait_for(|state| state.sessions.len() == 1),
        )
        .await
        .expect("the session closes")
        .unwrap();

        // No project lists it live any more; its durable row still names
        // the owner, which is not the first project asked.
        assert!(reference.snapshot(second).await.unwrap().is_none());
        let owner = f.handle.project_of(second).await.unwrap();
        assert_eq!(owner.project.id, project);
    }

    fn session(turn_state: SupervisorTurnState) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new_v4(),
            label: "s".into(),
            lifecycle: SessionLifecycle::Active,
            turn_state,
        }
    }

    #[test]
    fn queued_running_and_waiting_sessions_are_busy() {
        let state = ProjectState {
            sessions: [
                SupervisorTurnState::Idle,
                SupervisorTurnState::Queued,
                SupervisorTurnState::Running,
                SupervisorTurnState::Waiting,
                SupervisorTurnState::Completed,
                SupervisorTurnState::Interrupted,
            ]
            .into_iter()
            .map(session)
            .collect(),
            ..Default::default()
        };
        let busy: Vec<_> = busy_sessions(&state).into_iter().map(|(_, s)| s).collect();
        assert_eq!(
            busy,
            vec![
                SupervisorTurnState::Queued,
                SupervisorTurnState::Running,
                SupervisorTurnState::Waiting
            ]
        );
    }

    #[test]
    fn a_busy_session_in_any_project_makes_the_workspace_busy() {
        let summary = |turn_state| ProjectSummary {
            project: Project {
                id: ProjectId::new(),
                root: "/p".into(),
                name: "p".into(),
                added_at: chrono::Utc::now(),
            },
            open: true,
            state: ProjectState {
                sessions: vec![session(turn_state)],
                ..Default::default()
            },
            last_error: None,
        };
        let idle = WorkspaceState {
            version: 1,
            projects: vec![
                summary(SupervisorTurnState::Idle),
                summary(SupervisorTurnState::Completed),
            ],
        };
        assert!(!idle.has_busy_sessions());
        // The busy one is not the first project: a background project counts.
        let mut background = idle.clone();
        background
            .projects
            .push(summary(SupervisorTurnState::Waiting));
        assert!(background.has_busy_sessions());
    }

    #[test]
    fn only_requests_not_seen_before_are_new() {
        let project = |id: ProjectId, calls: &[&str]| ProjectSummary {
            project: Project {
                id,
                root: "/p".into(),
                name: "p".into(),
                added_at: chrono::Utc::now(),
            },
            open: true,
            state: ProjectState {
                attention: calls
                    .iter()
                    .map(|call| AttentionItem {
                        session_id: SessionId::nil(),
                        kind: AttentionKind::Approval,
                        call_id: (*call).into(),
                        since: chrono::Utc::now(),
                    })
                    .collect(),
                ..Default::default()
            },
            last_error: None,
        };
        let (a, b) = (ProjectId::new(), ProjectId::new());
        let previous = WorkspaceState {
            version: 1,
            projects: vec![project(a, &["one"]), project(b, &[])],
        };
        // The same call id in another project is a different request.
        let next = WorkspaceState {
            version: 2,
            projects: vec![project(a, &["one", "two"]), project(b, &["one"])],
        };
        let fresh: Vec<_> = new_attention(&previous, &next)
            .into_iter()
            .map(|(id, item)| (id, item.call_id))
            .collect();
        assert_eq!(fresh, vec![(a, "two".to_string()), (b, "one".to_string())]);
        assert!(new_attention(&next, &next).is_empty());
    }
}
