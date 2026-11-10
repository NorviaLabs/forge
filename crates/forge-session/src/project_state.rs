//! One project's state as a frontend needs it to route attention: which
//! sessions exist, which of them wait on you, and how many run slots are in
//! use.
//!
//! It is published on a `watch` channel rather than the event broadcast. The
//! broadcast also carries every streamed token, so a slow reader can lag and
//! lose events; a `watch` keeps only the latest value, which a reader always
//! sees. The value is derived from the actors' snapshots, never from the
//! events themselves, so a lost event can delay an update but never lose one.

use chrono::{DateTime, Utc};
use forge_types::SessionId;

use crate::{RepositorySession, SessionLifecycle, SessionRuntimeSnapshot, SupervisorTurnState};

/// What a session is waiting on you for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttentionKind {
    Approval,
    Question,
    /// An approval asked by one of the session's background child agents.
    ChildApproval,
}

/// One request waiting on you. `(session_id, kind, call_id)` identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionItem {
    pub session_id: SessionId,
    pub kind: AttentionKind,
    pub call_id: String,
    /// When this state first saw the request. Kept across updates, so the
    /// same request never looks new twice.
    pub since: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub label: String,
    pub lifecycle: SessionLifecycle,
    pub turn_state: SupervisorTurnState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    /// Increases by one each time any other field changes.
    pub version: u64,
    pub sessions: Vec<SessionSummary>,
    pub attention: Vec<AttentionItem>,
    /// Sessions whose turn is running.
    pub running: usize,
    /// The project's run slots.
    pub limit: usize,
}

/// One session's contribution: its summary and the requests it waits on.
pub(crate) struct SessionView {
    summary: SessionSummary,
    waiting: Vec<(AttentionKind, String)>,
}

impl SessionView {
    /// Reads only the small fields, never the transcript.
    pub(crate) fn of(snapshot: &SessionRuntimeSnapshot) -> Self {
        let session_id = snapshot.task.session_id;
        let mut waiting = Vec::new();
        if let Some(hitl) = &snapshot.session.pending_hitl {
            waiting.push((AttentionKind::Approval, hitl.call_id.clone()));
        }
        if let Some(question) = &snapshot.session.pending_question {
            waiting.push((AttentionKind::Question, question.call_id.clone()));
        }
        if let Some(details) = &snapshot.details {
            waiting.extend(details.background.iter().filter_map(|task| {
                task.pending_approval.as_ref().map(|request| {
                    (
                        AttentionKind::ChildApproval,
                        request.payload.call_id.clone(),
                    )
                })
            }));
        }
        Self {
            summary: SessionSummary {
                session_id,
                label: snapshot.task.label.clone(),
                lifecycle: snapshot.task.lifecycle,
                turn_state: snapshot.task.turn_state,
            },
            waiting,
        }
    }
}

impl SessionView {
    /// A session with no live actor (its workspace is unavailable): it can't
    /// be waiting on anything.
    pub(crate) fn of_task(task: &RepositorySession) -> Self {
        Self {
            summary: SessionSummary {
                session_id: task.session_id,
                label: task.label.clone(),
                lifecycle: task.lifecycle,
                turn_state: task.turn_state,
            },
            waiting: Vec::new(),
        }
    }
}

/// The next state from the current views. `since` carries over for requests
/// already in `previous`; `version` increases only when something changed.
pub(crate) fn next_state(
    previous: &ProjectState,
    views: Vec<SessionView>,
    limit: usize,
    now: DateTime<Utc>,
) -> ProjectState {
    let mut attention = Vec::new();
    let mut sessions = Vec::with_capacity(views.len());
    for view in views {
        for (kind, call_id) in view.waiting {
            let session_id = view.summary.session_id;
            let since = previous
                .attention
                .iter()
                .find(|item| {
                    item.session_id == session_id && item.kind == kind && item.call_id == call_id
                })
                .map_or(now, |item| item.since);
            attention.push(AttentionItem {
                session_id,
                kind,
                call_id,
                since,
            });
        }
        sessions.push(view.summary);
    }
    attention.sort_by(|a, b| a.since.cmp(&b.since).then(a.call_id.cmp(&b.call_id)));
    let running = sessions
        .iter()
        .filter(|session| session.turn_state == SupervisorTurnState::Running)
        .count();
    let mut next = ProjectState {
        version: previous.version,
        sessions,
        attention,
        running,
        limit,
    };
    if next != *previous {
        next.version = previous.version + 1;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        session_id: SessionId,
        turn_state: SupervisorTurnState,
        waiting: &[&str],
    ) -> SessionView {
        SessionView {
            summary: SessionSummary {
                session_id,
                label: "s".into(),
                lifecycle: SessionLifecycle::Active,
                turn_state,
            },
            waiting: waiting
                .iter()
                .map(|call| (AttentionKind::Approval, (*call).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_request_keeps_its_first_seen_time_and_unchanged_state_keeps_its_version() {
        let id = SessionId::new_v4();
        let t0 = Utc::now();
        let t1 = t0 + chrono::Duration::seconds(5);
        let first = next_state(
            &ProjectState::default(),
            vec![view(id, SupervisorTurnState::Waiting, &["call-1"])],
            4,
            t0,
        );
        assert_eq!(first.version, 1);
        assert_eq!(first.attention[0].since, t0);

        let again = next_state(
            &first,
            vec![view(id, SupervisorTurnState::Waiting, &["call-1"])],
            4,
            t1,
        );
        assert_eq!(again, first, "nothing changed, so no new version");

        let second = next_state(
            &again,
            vec![view(
                id,
                SupervisorTurnState::Waiting,
                &["call-1", "call-2"],
            )],
            4,
            t1,
        );
        assert_eq!(second.version, 2);
        assert_eq!(second.attention.len(), 2);
        assert_eq!(second.attention[0].since, t0);
        assert_eq!(second.attention[1].since, t1);
    }

    #[test]
    fn running_counts_only_running_turns() {
        let state = next_state(
            &ProjectState::default(),
            vec![
                view(SessionId::new_v4(), SupervisorTurnState::Running, &[]),
                view(SessionId::new_v4(), SupervisorTurnState::Waiting, &["c"]),
                view(SessionId::new_v4(), SupervisorTurnState::Idle, &[]),
            ],
            4,
            Utc::now(),
        );
        assert_eq!((state.running, state.limit), (1, 4));
        assert_eq!(state.sessions.len(), 3);
    }
}
