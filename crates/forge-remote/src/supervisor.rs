//! The real backend: Forge's open projects, through the desktop's
//! `ProjectService`.
//!
//! It follows projects as they open and close, turns each project's
//! supervisor events into [`BackendEvent`]s for the sessions someone follows,
//! and sends prompts, decisions and stops through the same supervisor
//! commands the desktop uses. Which decisions a device may make comes from
//! [`forge_session::approvals`], the source the desktop's card reads too.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use forge_session::approvals::{approval_options, escape_invisible};
use forge_session::{
    AttentionKind, OpenProjectRef, ProjectId, ProjectServiceHandle, SessionLifecycle,
    SessionRuntimeSnapshot, SupervisorCommand, SupervisorEvent, TargetError, WaitingOn,
};
use forge_types::{
    ApprovalKind, GrantDecision, HitlPayload, Message, MessageRole, ModelStreamEvent, SessionId,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::backend::{BackendError, BackendEvent, Interest, RemoteBackend};
use crate::protocol::{
    MessagePage, NoticeKind, RemoteApproval, RemoteDecision, RemoteMessage, SessionListItem,
    SessionState,
};

/// Tool output longer than this is cut, as on the desktop.
pub const TOOL_TEXT_LIMIT: usize = 2000;

/// The newest messages a `session` event carries.
pub const MESSAGE_WINDOW: usize = 30;

pub struct SupervisorBackend {
    projects: ProjectServiceHandle,
}

impl SupervisorBackend {
    pub fn new(projects: ProjectServiceHandle) -> Arc<Self> {
        Arc::new(Self { projects })
    }

    /// The open project that owns a live `session_id`, and its snapshot.
    async fn live(
        &self,
        session_id: SessionId,
    ) -> Result<(OpenProjectRef, SessionRuntimeSnapshot), BackendError> {
        let project = self
            .projects
            .project_of(session_id)
            .await
            .map_err(target_error)?;
        let snapshot = project
            .snapshot(session_id)
            .await
            .map_err(target_error)?
            .filter(|snapshot| snapshot.task.lifecycle == SessionLifecycle::Active)
            .ok_or(BackendError::UnknownSession)?;
        Ok((project, snapshot))
    }

    fn send(project: &OpenProjectRef, command: SupervisorCommand) -> Result<(), BackendError> {
        // Fire and forget, as the desktop does: the outcome arrives as
        // session events.
        project
            .handle
            .submit(command)
            .map(drop)
            .map_err(|error| BackendError::Unavailable(error.to_string()))
    }
}

fn target_error(error: TargetError) -> BackendError {
    match error {
        TargetError::UnknownSession(_) => BackendError::UnknownSession,
        other => BackendError::Unavailable(other.to_string()),
    }
}

#[async_trait]
impl RemoteBackend for SupervisorBackend {
    async fn sessions(&self) -> Result<Vec<SessionListItem>, BackendError> {
        let projects = self
            .projects
            .open_projects()
            .await
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let mut sessions = Vec::new();
        for project in projects {
            let state = project.handle.project_state().borrow().clone();
            let name = escape_invisible(&project.project.name);
            for session in state
                .sessions
                .iter()
                .filter(|session| session.lifecycle == SessionLifecycle::Active)
            {
                sessions.push(session_list_item(&name, session, &state.attention));
            }
        }
        Ok(sessions)
    }

    async fn session_state(&self, session_id: SessionId) -> Result<SessionState, BackendError> {
        let (project, snapshot) = self.live(session_id).await?;
        Ok(session_state(&project.project.name, &snapshot))
    }

    async fn messages(
        &self,
        session_id: SessionId,
        before: Option<usize>,
        limit: usize,
    ) -> Result<MessagePage, BackendError> {
        let (_, snapshot) = self.live(session_id).await?;
        Ok(message_page(snapshot.transcript.messages(), before, limit))
    }

    fn subscribe(&self, interest: Interest) -> mpsc::Receiver<BackendEvent> {
        let (events, receiver) = mpsc::channel(1024);
        tokio::spawn(follow_projects(self.projects.clone(), interest, events));
        receiver
    }

    async fn submit_prompt(&self, session_id: SessionId, text: String) -> Result<(), BackendError> {
        let (project, _) = self.live(session_id).await?;
        Self::send(
            &project,
            SupervisorCommand::SubmitPrompt { session_id, text },
        )
    }

    async fn decide(
        &self,
        session_id: SessionId,
        call_id: &str,
        decision: RemoteDecision,
        note: Option<String>,
        actor: &str,
    ) -> Result<(), BackendError> {
        let (project, snapshot) = self.live(session_id).await?;
        let payload = snapshot
            .session
            .pending_hitl
            .filter(|payload| payload.call_id == call_id)
            .ok_or(BackendError::NotPending)?;
        // Checked against the live request: a device can never make a
        // decision the request does not offer it.
        if !remote_options(&payload).contains(&decision) {
            return Err(BackendError::NotAllowed(
                "this decision can only be made on the desktop".into(),
            ));
        }
        let decision = match decision {
            RemoteDecision::AllowOnce => GrantDecision::AllowOnce,
            RemoteDecision::Deny => GrantDecision::Deny,
            RemoteDecision::DenyWithNote => {
                let note = note.unwrap_or_default();
                if note.trim().is_empty() {
                    return Err(BackendError::Invalid("a note is required".into()));
                }
                GrantDecision::DenyWithNote(note)
            }
        };
        Self::send(
            &project,
            SupervisorCommand::DecideApproval {
                session_id,
                // The session refuses it if another request took its place,
                // including the escalation that shares this call's id.
                expected_call_id: Some(call_id.to_string()),
                expected_kind: Some(payload.kind()),
                decision,
                actor: actor.to_string(),
            },
        )
    }

    async fn stop(&self, session_id: SessionId) -> Result<(), BackendError> {
        let (project, _) = self.live(session_id).await?;
        Self::send(&project, SupervisorCommand::StopTurn { session_id })
    }
}

/// A project's active sessions, shared between its forwarder and the task
/// that reports them gone when the project closes.
type ProjectSessions = Arc<Mutex<HashSet<SessionId>>>;

/// The projects open in `state`. Only a change here needs the project
/// service asked again; the state changes far more often (every turn).
pub fn open_project_ids(
    state: &forge_session::WorkspaceState,
) -> std::collections::BTreeSet<ProjectId> {
    state
        .projects
        .iter()
        .filter(|summary| summary.open)
        .map(|summary| summary.project.id)
        .collect()
}

/// Forward every open project's events, following projects as they open
/// and close, until the hub stops listening.
async fn follow_projects(
    projects: ProjectServiceHandle,
    interest: Interest,
    events: mpsc::Sender<BackendEvent>,
) {
    let mut workspace = projects.workspace();
    let mut forwarders: HashMap<ProjectId, (JoinHandle<()>, ProjectSessions)> = HashMap::new();
    // The open projects last synced; `None` before the first sync.
    let mut synced: Option<std::collections::BTreeSet<ProjectId>> = None;
    loop {
        let wanted = open_project_ids(&workspace.borrow_and_update());
        if synced.as_ref() != Some(&wanted) {
            let open = projects.open_projects().await.unwrap_or_default();
            let open_ids: HashSet<ProjectId> = open.iter().map(|p| p.project.id).collect();
            // Projects that closed: their followed sessions are gone.
            let closed: Vec<ProjectId> = forwarders
                .keys()
                .filter(|id| !open_ids.contains(id))
                .copied()
                .collect();
            for id in closed {
                if let Some((task, sessions)) = forwarders.remove(&id) {
                    task.abort();
                    let sessions: Vec<SessionId> = sessions
                        .lock()
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default();
                    for session_id in sessions {
                        if interest.contains(session_id)
                            && events.send(BackendEvent::Gone(session_id)).await.is_err()
                        {
                            return;
                        }
                    }
                }
            }
            for project in open {
                if forwarders.contains_key(&project.project.id) {
                    continue;
                }
                let sessions = Arc::new(Mutex::new(HashSet::new()));
                let task = tokio::spawn(forward_project(
                    project.clone(),
                    interest.clone(),
                    events.clone(),
                    sessions.clone(),
                ));
                forwarders.insert(project.project.id, (task, sessions));
            }
            synced = Some(wanted);
        }
        tokio::select! {
            changed = workspace.changed() => if changed.is_err() { break },
            () = events.closed() => break,
        }
    }
    for (task, _) in forwarders.into_values() {
        task.abort();
    }
}

/// One project's supervisor events, as [`BackendEvent`]s for the sessions
/// someone follows. `sessions` tracks the project's active sessions so a
/// closing project can report them gone.
async fn forward_project(
    project: OpenProjectRef,
    interest: Interest,
    events: mpsc::Sender<BackendEvent>,
    sessions: Arc<Mutex<HashSet<SessionId>>>,
) {
    // Only the receiver is kept: holding the project would keep its event
    // channel open after it closes.
    let mut receiver = project.handle.subscribe();
    let name = project.project.name.clone();
    if let Ok(snapshots) = project.snapshots().await {
        if let Ok(mut set) = sessions.lock() {
            set.extend(
                snapshots
                    .iter()
                    .filter(|s| s.task.lifecycle == SessionLifecycle::Active)
                    .map(|s| s.task.session_id),
            );
        }
    }
    drop(project);
    let mine = |id: SessionId| sessions.lock().map(|s| s.contains(&id)).unwrap_or(false);
    loop {
        let event = match receiver.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let lagged: Vec<SessionId> = sessions
                    .lock()
                    .map(|s| {
                        s.iter()
                            .copied()
                            .filter(|id| interest.contains(*id))
                            .collect()
                    })
                    .unwrap_or_default();
                if events
                    .send(BackendEvent::Lagged { sessions: lagged })
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                let gone: Vec<SessionId> = sessions
                    .lock()
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                for session_id in gone {
                    if interest.contains(session_id) {
                        let _ = events.send(BackendEvent::Gone(session_id)).await;
                    }
                }
                return;
            }
        };
        let out = match event {
            SupervisorEvent::SessionUpdated(snapshot) => {
                let session_id = snapshot.task.session_id;
                let active = snapshot.task.lifecycle == SessionLifecycle::Active;
                if let Ok(mut set) = sessions.lock() {
                    if active {
                        set.insert(session_id);
                    } else {
                        set.remove(&session_id);
                    }
                }
                if !interest.contains(session_id) {
                    continue;
                }
                if active {
                    BackendEvent::Updated(Box::new(session_state(&name, &snapshot)))
                } else {
                    BackendEvent::Gone(session_id)
                }
            }
            SupervisorEvent::Roster(snapshots) => {
                let present: HashSet<SessionId> = snapshots
                    .iter()
                    .filter(|s| s.task.lifecycle == SessionLifecycle::Active)
                    .map(|s| s.task.session_id)
                    .collect();
                let left: Vec<SessionId> = match sessions.lock() {
                    Ok(mut set) => {
                        let left = set.difference(&present).copied().collect();
                        *set = present;
                        left
                    }
                    Err(_) => Vec::new(),
                };
                for session_id in left {
                    if interest.contains(session_id)
                        && events.send(BackendEvent::Gone(session_id)).await.is_err()
                    {
                        return;
                    }
                }
                continue;
            }
            SupervisorEvent::Stream {
                session_id,
                event: ModelStreamEvent::TextDelta { text },
            } if interest.contains(session_id) => BackendEvent::Text {
                session_id,
                delta: text,
            },
            SupervisorEvent::Attention {
                session_id,
                message,
                ..
            } if interest.contains(session_id) && mine(session_id) => BackendEvent::Notice {
                session_id,
                kind: NoticeKind::Attention,
                message: escape_invisible(&message),
            },
            SupervisorEvent::Error {
                session_id: Some(session_id),
                message,
            } if interest.contains(session_id) => BackendEvent::Notice {
                session_id,
                kind: NoticeKind::Error,
                message: escape_invisible(&message),
            },
            _ => continue,
        };
        if events.send(out).await.is_err() {
            return;
        }
    }
}

/// One row of the session list.
pub fn session_list_item(
    project: &str,
    session: &forge_session::SessionSummary,
    attention: &[forge_session::AttentionItem],
) -> SessionListItem {
    SessionListItem {
        session_id: session.session_id,
        project: project.to_string(),
        label: escape_invisible(&session.label),
        turn_state: session.turn_state.label().to_string(),
        needs_approval: attention.iter().any(|item| {
            item.session_id == session.session_id && item.kind == AttentionKind::Approval
        }),
        runs_on: runs_on(&session.target),
    }
}

/// `host:folder` for a session on an SSH host, as the device shows it.
fn runs_on(target: &forge_types::ExecutionTarget) -> Option<String> {
    match target {
        forge_types::ExecutionTarget::Local => None,
        forge_types::ExecutionTarget::Ssh { host, path, .. } => {
            Some(escape_invisible(&format!("{host}:{path}")))
        }
    }
}

/// The decisions a device may make for `payload`: those the request offers
/// that are not privileged and are one of the three remote decisions.
pub fn remote_options(payload: &HitlPayload) -> Vec<RemoteDecision> {
    approval_options(payload)
        .iter()
        .filter(|option| !option.privileged)
        .filter_map(|option| match option.decision.as_str() {
            "allow_once" => Some(RemoteDecision::AllowOnce),
            "deny" => Some(RemoteDecision::Deny),
            "deny_with_note" => Some(RemoteDecision::DenyWithNote),
            _ => None,
        })
        .collect()
}

/// A pending approval as the device shows it.
pub fn remote_approval(payload: &HitlPayload) -> RemoteApproval {
    let arguments = serde_json::to_string_pretty(&payload.args_redacted)
        .unwrap_or_else(|_| payload.args_redacted.to_string());
    let remote_options = remote_options(payload);
    RemoteApproval {
        call_id: payload.call_id.clone(),
        tool: escape_invisible(&payload.tool),
        arguments: escape_invisible(&arguments),
        reason: escape_invisible(&payload.reason),
        kind: match payload.kind() {
            ApprovalKind::NetworkHost => "network_host",
            ApprovalKind::SandboxEscalation => "sandbox_escalation",
            ApprovalKind::ScreenControl => "screen_control",
            ApprovalKind::FormSubmit => "form_submit",
            ApprovalKind::Governance => "governance",
        }
        .to_string(),
        desktop_required: approval_options(payload).len() > remote_options.len(),
        remote_options,
        runs_on: payload.target.as_deref().map(escape_invisible),
    }
}

/// One transcript message for the device, or `None` for messages the
/// desktop does not show either (system prompts, empty tool-call replies).
pub fn remote_message(message: &Message) -> Option<RemoteMessage> {
    let role = match message.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        _ => return None,
    };
    let content = match message.role {
        MessageRole::Assistant => message
            .content
            .strip_prefix(forge_core::TURN_FAILED_MARKER)
            .unwrap_or(&message.content),
        _ => &message.content,
    };
    if message.role == MessageRole::Assistant
        && content.trim().is_empty()
        && message.attachments.is_empty()
    {
        return None;
    }
    let text = if message.role == MessageRole::Tool && content.chars().count() > TOOL_TEXT_LIMIT {
        content.chars().take(TOOL_TEXT_LIMIT).collect::<String>() + "\n…"
    } else {
        content.to_string()
    };
    Some(RemoteMessage {
        role: role.to_string(),
        name: message.name.as_deref().map(escape_invisible),
        text,
        attachments: message.attachments.len(),
    })
}

/// Up to `limit` visible messages before index `before`, converted.
///
/// Only the page is converted: counting what is visible allocates nothing,
/// so the cost does not grow with the transcript's text.
pub fn message_page(messages: &[Message], before: Option<usize>, limit: usize) -> MessagePage {
    let visible: Vec<&Message> = messages.iter().filter(|m| is_visible(m)).collect();
    let (message_offset, chosen) = crate::protocol::page(&visible, before, limit);
    MessagePage {
        messages: chosen.into_iter().filter_map(remote_message).collect(),
        message_offset,
        message_total: visible.len(),
    }
}

/// Whether [`remote_message`] shows `message`, without building it.
fn is_visible(message: &Message) -> bool {
    match message.role {
        MessageRole::User | MessageRole::Tool => true,
        MessageRole::Assistant => {
            !message.attachments.is_empty()
                || !message
                    .content
                    .strip_prefix(forge_core::TURN_FAILED_MARKER)
                    .unwrap_or(&message.content)
                    .trim()
                    .is_empty()
        }
        _ => false,
    }
}

/// A live session as the device shows it.
pub fn session_state(project: &str, snapshot: &SessionRuntimeSnapshot) -> SessionState {
    let window = message_page(snapshot.transcript.messages(), None, MESSAGE_WINDOW);
    SessionState {
        session_id: snapshot.task.session_id,
        project: escape_invisible(project),
        label: escape_invisible(&snapshot.task.label),
        turn_state: snapshot.task.turn_state.label().to_string(),
        messages: window.messages,
        message_offset: window.message_offset,
        message_total: window.message_total,
        pending_approval: snapshot.session.pending_hitl.as_ref().map(remote_approval),
        queue: snapshot
            .queued_prompts
            .iter()
            .map(|(_, text)| text.clone())
            .collect(),
        waiting_on: snapshot
            .session
            .waiting_on
            .as_ref()
            .map(|waiting| match waiting {
                WaitingOn::Reply => "a reply".to_string(),
                WaitingOn::Configuration(key) => {
                    format!("the setting {}", escape_invisible(key))
                }
                WaitingOn::ExternalAction(action) => escape_invisible(action),
                WaitingOn::Other => "something outside Forge".to_string(),
            }),
        runs_on: runs_on(&snapshot.task.target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_session::{
        RepositorySession, SessionLifecycle, SessionSnapshot, SupervisorTurnState,
        TranscriptSnapshot, WaitingOn, WorktreeOwnership,
    };
    use forge_types::MessageRole;

    fn payload(sandbox_escalation: bool, denied_host: Option<&str>) -> HitlPayload {
        HitlPayload {
            call_id: "call-1".into(),
            tool: "git".into(),
            args_redacted: serde_json::json!({"subcommand": "reset", "args": ["--hard"]}),
            reason: "destructive git".into(),
            failure: None,
            sandbox_escalation,
            denied_host: denied_host.map(str::to_string),
            denied_path: None,
            target: None,
            consent: None,
        }
    }

    fn project(open: bool) -> forge_session::ProjectSummary {
        forge_session::ProjectSummary {
            project: forge_session::Project {
                id: ProjectId::new(),
                root: std::path::PathBuf::from("/work/p"),
                name: "p".into(),
                added_at: chrono::Utc::now(),
            },
            open,
            state: Default::default(),
            last_error: None,
        }
    }

    #[test]
    fn only_a_change_in_which_projects_are_open_is_worth_a_lookup() {
        let (a, b, closed) = (project(true), project(true), project(false));
        let state = forge_session::WorkspaceState {
            version: 1,
            projects: vec![a.clone(), b.clone(), closed],
        };
        let ids = open_project_ids(&state);
        assert_eq!(ids, [a.project.id, b.project.id].into_iter().collect());
        let mut busier = state.clone();
        busier.version = 2;
        busier.projects[0].state.running = 3;
        assert_eq!(
            open_project_ids(&busier),
            ids,
            "a session changing state changes nothing here"
        );
        let mut one_closed = state;
        one_closed.projects[1].open = false;
        assert_ne!(open_project_ids(&one_closed), ids);
    }

    #[test]
    fn a_governance_request_can_be_allowed_once_or_denied_remotely() {
        assert_eq!(
            remote_options(&payload(false, None)),
            [
                RemoteDecision::AllowOnce,
                RemoteDecision::Deny,
                RemoteDecision::DenyWithNote
            ]
        );
    }

    #[test]
    fn leaving_the_sandbox_or_opening_a_host_can_only_be_denied_remotely() {
        for privileged in [payload(true, None), payload(false, Some("example.com"))] {
            assert_eq!(
                remote_options(&privileged),
                [RemoteDecision::Deny, RemoteDecision::DenyWithNote],
                "{privileged:?}"
            );
        }
    }

    fn on_devbox() -> forge_types::ExecutionTarget {
        forge_types::ExecutionTarget::Ssh {
            host: "devbox".into(),
            path: "/srv/app".into(),
            ask: true,
        }
    }

    /// A command on an SSH host runs outside the sandbox, with that host's
    /// network and credentials: the device is told where, as the desktop
    /// card does, and can only run it once or refuse it.
    #[test]
    fn an_approval_names_the_host_the_command_runs_on() {
        let mut remote = payload(false, None);
        remote.tool = "bash".into();
        remote.target = Some("dev\u{202E}box".into());
        let approval = remote_approval(&remote);
        assert_eq!(approval.runs_on.as_deref(), Some("dev⟨U+202E⟩box"));
        assert_eq!(
            approval.remote_options,
            [
                RemoteDecision::AllowOnce,
                RemoteDecision::Deny,
                RemoteDecision::DenyWithNote
            ]
        );
        assert!(
            approval.desktop_required,
            "trusting every command stays on the desktop"
        );
        let local = remote_approval(&payload(false, None));
        assert_eq!(local.runs_on, None);
        assert!(serde_json::to_value(&local)
            .unwrap()
            .get("runs_on")
            .is_none());
    }

    #[test]
    fn a_session_on_a_host_says_where_it_runs() {
        let mut remote = snapshot(SessionId::new_v4(), Vec::new(), None);
        remote.task.target = on_devbox();
        assert_eq!(
            session_state("p", &remote).runs_on.as_deref(),
            Some("devbox:/srv/app")
        );
        let local = snapshot(SessionId::new_v4(), Vec::new(), None);
        assert_eq!(session_state("p", &local).runs_on, None);

        let summary = forge_session::SessionSummary {
            session_id: remote.task.session_id,
            label: "on the box".into(),
            lifecycle: SessionLifecycle::Active,
            turn_state: SupervisorTurnState::Idle,
            target: on_devbox(),
        };
        let item = session_list_item("p", &summary, &[]);
        assert_eq!(item.runs_on.as_deref(), Some("devbox:/srv/app"));
        let local_item = session_list_item(
            "p",
            &forge_session::SessionSummary {
                target: forge_types::ExecutionTarget::Local,
                ..summary
            },
            &[],
        );
        assert_eq!(local_item.runs_on, None);
    }

    #[test]
    fn an_approval_says_what_only_the_desktop_can_do() {
        let governance = remote_approval(&payload(false, None));
        assert_eq!(governance.call_id, "call-1");
        assert_eq!(governance.kind, "governance");
        assert!(
            governance.desktop_required,
            "patterns and approve-all are desktop only"
        );
        let escalation = remote_approval(&payload(true, None));
        assert_eq!(escalation.kind, "sandbox_escalation");
        assert!(escalation.desktop_required);
        let host = remote_approval(&payload(false, Some("example.com")));
        assert_eq!(host.kind, "network_host");
    }

    #[test]
    fn approval_text_shows_hidden_characters() {
        let mut disguised = payload(false, None);
        disguised.args_redacted = serde_json::json!({"command": "echo safe\u{202E}fr- mr"});
        disguised.reason = "why\u{200B}".into();
        let approval = remote_approval(&disguised);
        assert!(
            approval.arguments.contains("⟨U+202E⟩"),
            "{}",
            approval.arguments
        );
        assert_eq!(approval.reason, "why⟨U+200B⟩");
        assert_eq!(approval.tool, "git");
    }

    #[test]
    fn messages_are_trimmed_like_the_desktop_transcript() {
        assert_eq!(
            remote_message(&Message::new(MessageRole::System, "prompt")),
            None
        );
        let user = remote_message(&Message::new(MessageRole::User, "hello")).unwrap();
        assert_eq!((user.role.as_str(), user.text.as_str()), ("user", "hello"));

        let failed = Message::new(
            MessageRole::Assistant,
            format!("{}the provider refused", forge_core::TURN_FAILED_MARKER),
        );
        assert_eq!(
            remote_message(&failed).unwrap().text,
            "the provider refused"
        );
        assert_eq!(
            remote_message(&Message::new(MessageRole::Assistant, "  ")),
            None,
            "a reply that is only tool calls has nothing to show"
        );

        let mut tool = Message::new(MessageRole::Tool, "x".repeat(TOOL_TEXT_LIMIT + 500));
        tool.name = Some("read_file".into());
        let tool = remote_message(&tool).unwrap();
        assert_eq!(tool.name.as_deref(), Some("read_file"));
        assert!(
            tool.text.chars().count() <= TOOL_TEXT_LIMIT + 2,
            "tool output is cut"
        );

        let image = forge_types::ImageRef {
            path: "/Users/someone/secret/shot.png".into(),
            mime: "image/png".into(),
            byte_len: 10,
            width: None,
            height: None,
            detail: "high".into(),
        };
        let pasted = Message::new(MessageRole::User, "look").with_attachments(vec![image]);
        let pasted = remote_message(&pasted).unwrap();
        assert_eq!(pasted.attachments, 1);
        assert!(
            !serde_json::to_string(&pasted).unwrap().contains("secret"),
            "attachment paths never leave the computer"
        );
    }

    fn snapshot(
        id: SessionId,
        messages: Vec<Message>,
        pending: Option<HitlPayload>,
    ) -> SessionRuntimeSnapshot {
        SessionRuntimeSnapshot {
            task: RepositorySession {
                session_id: id,
                label: "Fix\u{202E}test".into(),
                workspace: std::path::PathBuf::from("/work/repo"),
                branch: "main".into(),
                base_sha: None,
                ownership: WorktreeOwnership::Primary,
                lifecycle: SessionLifecycle::Active,
                turn_state: SupervisorTurnState::Waiting,
                slot: None,
                model_id: String::new(),
                profile_id: String::new(),
                reasoning_effort: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                archived_at: None,
                github_issue_number: None,
                github_pr_url: None,
                github_retry_attempts: 0,
                github_tests_passed: false,
                target: forge_types::ExecutionTarget::Local,
            },
            session: SessionSnapshot {
                pending_hitl: pending,
                waiting_on: Some(WaitingOn::Configuration("OPENAI_API_KEY".into())),
                ..SessionSnapshot::default()
            },
            transcript: TranscriptSnapshot::from_messages(messages, 1),
            details: None,
            queued_prompts: vec![(1, "next prompt".into())],
            interrupted_prompts: Vec::new(),
            host_questions: Vec::new(),
            computer_use: Default::default(),
        }
    }

    fn long_transcript(visible: usize) -> Vec<Message> {
        let mut messages = vec![Message::new(MessageRole::System, "system prompt")];
        for i in 0..visible {
            let role = if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            messages.push(Message::new(role, format!("message {i}")));
            if i % 10 == 0 {
                // A reply that is only tool calls: never shown, never counted.
                messages.push(Message::new(MessageRole::Assistant, ""));
            }
        }
        messages
    }

    #[test]
    fn a_session_state_carries_only_the_latest_messages() {
        let id = SessionId::new_v4();
        let state = session_state("p", &snapshot(id, long_transcript(100), None));
        assert_eq!(state.message_total, 100, "only visible messages count");
        assert_eq!(state.message_offset, 70);
        assert_eq!(state.messages.len(), MESSAGE_WINDOW);
        assert_eq!(state.messages[0].text, "message 70");
        assert_eq!(state.messages.last().unwrap().text, "message 99");

        let short = session_state("p", &snapshot(id, long_transcript(5), None));
        assert_eq!((short.message_offset, short.message_total), (0, 5));
        assert_eq!(short.messages.len(), 5);
    }

    #[test]
    fn older_messages_page_backwards_in_the_same_numbering() {
        let transcript = long_transcript(100);
        let page = message_page(&transcript, Some(70), 30);
        assert_eq!((page.message_offset, page.message_total), (40, 100));
        assert_eq!(page.messages.len(), 30);
        assert_eq!(page.messages[0].text, "message 40");
        assert_eq!(page.messages.last().unwrap().text, "message 69");
        let first = message_page(&transcript, Some(1), 30);
        assert_eq!(first.messages[0].text, "message 0");
        assert_eq!(first.message_offset, 0);
    }

    #[test]
    fn a_session_state_carries_the_transcript_approval_and_queue() {
        let id = SessionId::new_v4();
        let state = session_state(
            "billing-api",
            &snapshot(
                id,
                vec![
                    Message::new(MessageRole::System, "system prompt"),
                    Message::new(MessageRole::User, "reset it"),
                    Message::new(MessageRole::Assistant, "asking first"),
                ],
                Some(payload(false, None)),
            ),
        );
        assert_eq!(state.session_id, id);
        assert_eq!(state.project, "billing-api");
        assert_eq!(state.label, "Fix⟨U+202E⟩test");
        assert_eq!(state.turn_state, "waiting");
        assert_eq!(
            state
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            ["user", "assistant"]
        );
        assert_eq!(
            state.pending_approval.map(|a| a.remote_options),
            Some(vec![
                RemoteDecision::AllowOnce,
                RemoteDecision::Deny,
                RemoteDecision::DenyWithNote
            ])
        );
        assert_eq!(state.queue, ["next prompt"]);
        assert!(state
            .waiting_on
            .as_deref()
            .is_some_and(|w| w.contains("OPENAI_API_KEY")));
        let json = serde_json::to_string(&session_state("p", &snapshot(id, vec![], None))).unwrap();
        assert!(!json.contains("/work/repo"), "no local paths: {json}");
    }
}
