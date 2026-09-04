use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forge_types::SessionId;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentActivity {
    pub action: String,
    pub agent_id: SessionId,
    pub parent_id: Option<SessionId>,
    pub status: AgentStatus,
    pub detail: Option<String>,
}

impl From<AgentCoordinatorConfig> for forge_config::AgentConfig {
    fn from(config: AgentCoordinatorConfig) -> Self {
        Self {
            max_live_agents: config.max_live_agents,
            max_depth: config.max_depth,
            min_wait_ms: config.min_wait.as_millis() as u64,
            default_wait_ms: config.default_wait.as_millis() as u64,
            max_wait_ms: config.max_wait.as_millis() as u64,
        }
    }
}

impl AgentStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl From<&forge_config::AgentConfig> for AgentCoordinatorConfig {
    fn from(config: &forge_config::AgentConfig) -> Self {
        Self {
            max_live_agents: config.max_live_agents,
            max_depth: config.max_depth,
            min_wait: Duration::from_millis(config.min_wait_ms),
            default_wait: Duration::from_millis(config.default_wait_ms),
            max_wait: Duration::from_millis(config.max_wait_ms),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub id: SessionId,
    pub parent_id: Option<SessionId>,
    pub task_name: String,
    pub status: AgentStatus,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentCoordinatorConfig {
    pub max_live_agents: usize,
    pub max_depth: usize,
    pub min_wait: Duration,
    pub default_wait: Duration,
    pub max_wait: Duration,
}

impl Default for AgentCoordinatorConfig {
    fn default() -> Self {
        Self {
            max_live_agents: 4,
            max_depth: 2,
            min_wait: Duration::from_millis(100),
            default_wait: Duration::from_secs(10),
            max_wait: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentCoordinatorError {
    #[error("agent `{0}` was not found")]
    NotFound(SessionId),
    #[error("agent `{target}` is not a descendant of `{requester}`")]
    NotDescendant {
        requester: SessionId,
        target: SessionId,
    },
    #[error("cannot interrupt the root agent")]
    RootInterrupt,
    #[error("cannot interrupt the requesting agent")]
    SelfInterrupt,
    #[error("maximum live agent limit ({0}) reached")]
    LiveLimit(usize),
    #[error("maximum agent nesting depth ({0}) reached")]
    DepthLimit(usize),
    #[error("agent `{0}` is currently running or waiting")]
    Busy(SessionId),
    #[error("agent `{0}` has no actor")]
    NoActor(SessionId),
    #[error("agent command channel is closed")]
    ChannelClosed,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentWaitResult {
    pub timed_out: bool,
    pub revision: u64,
    pub agents: Vec<AgentSnapshot>,
}

pub(crate) enum AgentCommand {
    Wake { cancel: CancellationToken },
}

struct AgentRecord {
    snapshot: AgentSnapshot,
    depth: usize,
    cancel: CancellationToken,
    mailbox: VecDeque<String>,
    actor: Option<mpsc::UnboundedSender<AgentCommand>>,
}

struct State {
    root_id: SessionId,
    revision: u64,
    records: HashMap<SessionId, AgentRecord>,
}

#[derive(Clone)]
pub struct AgentCoordinator {
    state: Arc<Mutex<State>>,
    revision_tx: watch::Sender<u64>,
    config: AgentCoordinatorConfig,
}

impl AgentCoordinator {
    pub fn new(root_id: SessionId) -> Self {
        Self::with_config(root_id, AgentCoordinatorConfig::default())
    }

    pub fn with_config(root_id: SessionId, config: AgentCoordinatorConfig) -> Self {
        let root_cancel = CancellationToken::new();
        let mut records = HashMap::new();
        records.insert(
            root_id,
            AgentRecord {
                snapshot: AgentSnapshot {
                    id: root_id,
                    parent_id: None,
                    task_name: "root".into(),
                    status: AgentStatus::Running,
                    summary: None,
                },
                depth: 0,
                cancel: root_cancel,
                mailbox: VecDeque::new(),
                actor: None,
            },
        );
        Self {
            state: Arc::new(Mutex::new(State {
                root_id,
                revision: 0,
                records,
            })),
            revision_tx: watch::channel(0).0,
            config,
        }
    }

    pub fn config(&self) -> AgentCoordinatorConfig {
        self.config
    }

    pub fn revision(&self) -> u64 {
        self.state.lock().unwrap().revision
    }

    pub fn root_id(&self) -> SessionId {
        self.state.lock().unwrap().root_id
    }

    pub fn contains(&self, id: SessionId) -> bool {
        self.state.lock().unwrap().records.contains_key(&id)
    }

    pub fn restore_child(
        &self,
        parent_id: SessionId,
        child_id: SessionId,
        task_name: String,
        status: AgentStatus,
        summary: Option<String>,
    ) -> Result<(), AgentCoordinatorError> {
        let cancel = CancellationToken::new();
        self.register_child_with_cancel(parent_id, child_id, task_name, cancel)?;
        self.update(child_id, status, summary)
    }

    pub fn register_child(
        &self,
        parent_id: SessionId,
        child_id: SessionId,
        task_name: String,
    ) -> Result<CancellationToken, AgentCoordinatorError> {
        let cancel = CancellationToken::new();
        self.register_child_with_cancel(parent_id, child_id, task_name, cancel.clone())?;
        Ok(cancel)
    }

    pub fn register_child_with_cancel(
        &self,
        parent_id: SessionId,
        child_id: SessionId,
        task_name: String,
        cancel: CancellationToken,
    ) -> Result<(), AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        let parent = state
            .records
            .get(&parent_id)
            .ok_or(AgentCoordinatorError::NotFound(parent_id))?;
        if !is_descendant_or_self(&state, state.root_id, parent_id) {
            return Err(AgentCoordinatorError::NotFound(parent_id));
        }
        let depth = parent.depth + 1;
        if depth > self.config.max_depth {
            return Err(AgentCoordinatorError::DepthLimit(self.config.max_depth));
        }
        let live = state
            .records
            .values()
            .filter(|record| !record.snapshot.status.is_terminal())
            .count()
            .saturating_sub(1);
        if live >= self.config.max_live_agents {
            return Err(AgentCoordinatorError::LiveLimit(
                self.config.max_live_agents,
            ));
        }
        state.records.insert(
            child_id,
            AgentRecord {
                snapshot: AgentSnapshot {
                    id: child_id,
                    parent_id: Some(parent_id),
                    task_name,
                    status: AgentStatus::Running,
                    summary: None,
                },
                depth,
                cancel,
                mailbox: VecDeque::new(),
                actor: None,
            },
        );
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(())
    }

    pub(crate) fn actor_channel(
        &self,
        id: SessionId,
    ) -> Result<
        (
            mpsc::UnboundedSender<AgentCommand>,
            mpsc::UnboundedReceiver<AgentCommand>,
        ),
        AgentCoordinatorError,
    > {
        let (tx, rx) = mpsc::unbounded_channel();
        self.attach_actor(id, tx.clone())?;
        Ok((tx, rx))
    }

    pub(crate) fn attach_actor(
        &self,
        id: SessionId,
        actor: mpsc::UnboundedSender<AgentCommand>,
    ) -> Result<(), AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&id)
            .ok_or(AgentCoordinatorError::NotFound(id))?;
        record.actor = Some(actor);
        Ok(())
    }

    pub fn update(
        &self,
        id: SessionId,
        status: AgentStatus,
        summary: Option<String>,
    ) -> Result<(), AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&id)
            .ok_or(AgentCoordinatorError::NotFound(id))?;
        record.snapshot.status = status;
        if summary.is_some() {
            record.snapshot.summary = summary;
        }
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(())
    }

    pub fn descendants(
        &self,
        requester: SessionId,
        prefix: Option<&str>,
    ) -> Result<Vec<AgentSnapshot>, AgentCoordinatorError> {
        let state = self.state.lock().unwrap();
        if !state.records.contains_key(&requester) {
            return Err(AgentCoordinatorError::NotFound(requester));
        }
        let mut agents: Vec<_> = state
            .records
            .values()
            .filter(|record| record.snapshot.id != requester)
            .filter(|record| is_descendant_or_self(&state, requester, record.snapshot.id))
            .filter(|record| {
                prefix.is_none_or(|prefix| record.snapshot.id.to_string().starts_with(prefix))
            })
            .map(|record| record.snapshot.clone())
            .collect();
        agents.sort_by_key(|agent| agent.id);
        Ok(agents)
    }

    pub fn descendant(
        &self,
        requester: SessionId,
        target: SessionId,
    ) -> Result<AgentSnapshot, AgentCoordinatorError> {
        self.descendants(requester, None)?
            .into_iter()
            .find(|agent| agent.id == target)
            .ok_or(AgentCoordinatorError::NotFound(target))
    }

    pub fn send_message(
        &self,
        requester: SessionId,
        target: SessionId,
        message: String,
    ) -> Result<(), AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        ensure_target_descendant(&state, requester, target)?;
        let record = state
            .records
            .get_mut(&target)
            .ok_or(AgentCoordinatorError::NotFound(target))?;
        record.mailbox.push_back(message);
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(())
    }

    pub fn followup(
        &self,
        requester: SessionId,
        target: SessionId,
        message: String,
    ) -> Result<CancellationToken, AgentCoordinatorError> {
        self.followup_with_cancel(requester, target, message, CancellationToken::new())
    }

    pub fn followup_with_cancel(
        &self,
        requester: SessionId,
        target: SessionId,
        message: String,
        cancel: CancellationToken,
    ) -> Result<CancellationToken, AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        ensure_target_descendant(&state, requester, target)?;
        let record = state
            .records
            .get_mut(&target)
            .ok_or(AgentCoordinatorError::NotFound(target))?;
        if matches!(
            record.snapshot.status,
            AgentStatus::Running | AgentStatus::Waiting
        ) {
            return Err(AgentCoordinatorError::Busy(target));
        }
        let actor = record
            .actor
            .clone()
            .ok_or(AgentCoordinatorError::NoActor(target))?;
        record.mailbox.push_back(message);
        record.cancel = cancel.clone();
        actor
            .send(AgentCommand::Wake {
                cancel: cancel.clone(),
            })
            .map_err(|_| AgentCoordinatorError::ChannelClosed)?;
        record.snapshot.status = AgentStatus::Running;
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(cancel)
    }

    pub fn take_mailbox(&self, id: SessionId) -> Result<Vec<String>, AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&id)
            .ok_or(AgentCoordinatorError::NotFound(id))?;
        Ok(record.mailbox.drain(..).collect())
    }

    pub fn restore_mailbox(
        &self,
        id: SessionId,
        messages: impl IntoIterator<Item = String>,
    ) -> Result<(), AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        let record = state
            .records
            .get_mut(&id)
            .ok_or(AgentCoordinatorError::NotFound(id))?;
        record.mailbox.extend(messages);
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(())
    }

    pub fn interrupt(
        &self,
        requester: SessionId,
        target: SessionId,
    ) -> Result<bool, AgentCoordinatorError> {
        let mut state = self.state.lock().unwrap();
        ensure_descendant(&state, requester, target)?;
        let record = state
            .records
            .get(&target)
            .ok_or(AgentCoordinatorError::NotFound(target))?;
        if record.snapshot.status.is_terminal() {
            return Ok(false);
        }
        record.cancel.cancel();
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.publish_revision();
        Ok(true)
    }

    pub fn cancellation_token(
        &self,
        id: SessionId,
    ) -> Result<CancellationToken, AgentCoordinatorError> {
        self.state
            .lock()
            .unwrap()
            .records
            .get(&id)
            .map(|record| record.cancel.clone())
            .ok_or(AgentCoordinatorError::NotFound(id))
    }

    pub async fn wait_for_change(
        &self,
        requester: SessionId,
        since_revision: u64,
        requested: Option<Duration>,
    ) -> Result<AgentWaitResult, AgentCoordinatorError> {
        let duration = requested
            .unwrap_or(self.config.default_wait)
            .clamp(self.config.min_wait, self.config.max_wait);
        self.descendants(requester, None)?;
        let mut revisions = self.revision_tx.subscribe();
        let changed = self.revision() > since_revision || *revisions.borrow() > since_revision;
        let timed_out = if changed {
            false
        } else {
            timeout(duration, revisions.changed()).await.is_err()
        };
        Ok(AgentWaitResult {
            timed_out,
            revision: self.revision(),
            agents: self.descendants(requester, None)?,
        })
    }

    fn publish_revision(&self) {
        let _ = self.revision_tx.send(self.revision());
    }
}

fn ensure_descendant(
    state: &State,
    requester: SessionId,
    target: SessionId,
) -> Result<(), AgentCoordinatorError> {
    if !state.records.contains_key(&target) {
        return Err(AgentCoordinatorError::NotFound(target));
    }
    if target == state.root_id {
        return Err(AgentCoordinatorError::RootInterrupt);
    }
    if requester == target {
        return Err(AgentCoordinatorError::SelfInterrupt);
    }
    if !is_descendant_or_self(state, requester, target) {
        return Err(AgentCoordinatorError::NotDescendant { requester, target });
    }
    Ok(())
}

fn ensure_target_descendant(
    state: &State,
    requester: SessionId,
    target: SessionId,
) -> Result<(), AgentCoordinatorError> {
    if !state.records.contains_key(&target) {
        return Err(AgentCoordinatorError::NotFound(target));
    }
    if !is_descendant_or_self(state, requester, target) || requester == target {
        return Err(AgentCoordinatorError::NotDescendant { requester, target });
    }
    Ok(())
}

fn is_descendant_or_self(state: &State, ancestor: SessionId, candidate: SessionId) -> bool {
    let mut current = Some(candidate);
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = state
            .records
            .get(&id)
            .and_then(|record| record.snapshot.parent_id);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uuid::Uuid;

    fn ids() -> (SessionId, SessionId, SessionId) {
        (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
    }

    #[test]
    fn limits_and_descendant_visibility_are_enforced() {
        let (root, child, grandchild) = ids();
        let coordinator = AgentCoordinator::with_config(
            root,
            AgentCoordinatorConfig {
                max_live_agents: 2,
                max_depth: 1,
                ..Default::default()
            },
        );
        coordinator
            .register_child(root, child, "child".into())
            .unwrap();
        assert_eq!(coordinator.descendants(root, None).unwrap().len(), 1);
        assert_eq!(
            coordinator.register_child(child, grandchild, "grandchild".into()),
            Err(AgentCoordinatorError::DepthLimit(1))
        );
    }

    #[test]
    fn mailbox_is_not_woken_until_followup() {
        let (root, child, _) = ids();
        let coordinator = AgentCoordinator::new(root);
        coordinator
            .register_child(root, child, "child".into())
            .unwrap();
        coordinator
            .send_message(root, child, "later".into())
            .unwrap();
        assert_eq!(coordinator.take_mailbox(child).unwrap(), vec!["later"]);
    }

    #[test]
    fn interrupt_rejects_self_and_root_and_tolerates_terminal_target() {
        let (root, child, _) = ids();
        let coordinator = AgentCoordinator::new(root);
        coordinator
            .register_child(root, child, "child".into())
            .unwrap();
        assert_eq!(
            coordinator.interrupt(root, root),
            Err(AgentCoordinatorError::RootInterrupt)
        );
        assert_eq!(
            coordinator.interrupt(child, child),
            Err(AgentCoordinatorError::SelfInterrupt)
        );
        coordinator
            .update(child, AgentStatus::Completed, Some("done".into()))
            .unwrap();
        assert_eq!(coordinator.interrupt(root, child).unwrap(), false);
    }

    #[tokio::test]
    async fn wait_reports_timeout_and_change() {
        let (root, child, _) = ids();
        let coordinator = AgentCoordinator::with_config(
            root,
            AgentCoordinatorConfig {
                min_wait: Duration::from_millis(1),
                default_wait: Duration::from_millis(1),
                max_wait: Duration::from_millis(2),
                ..Default::default()
            },
        );
        let revision = coordinator.revision();
        let result = coordinator
            .wait_for_change(root, revision, Some(Duration::from_millis(1)))
            .await
            .unwrap();
        assert!(result.timed_out);
        coordinator
            .register_child(root, child, "child".into())
            .unwrap();
        let result = coordinator
            .wait_for_change(root, revision, Some(Duration::from_millis(1)))
            .await
            .unwrap();
        assert!(!result.timed_out);
    }
}
