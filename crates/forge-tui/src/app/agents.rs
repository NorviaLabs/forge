//! Child agents as the conversation tells them.
//!
//! A child spawned by this session appears three ways in the parent
//! transcript, all derived from the background registry rather than from any
//! event the TUI remembers: a roster row where the spawn happened, a receipt
//! under the turn it finished in, and, while it waits on the operator, its
//! request as an inline approval card. The card is passive: the composer keeps
//! focus, one Tab reaches the card, and `a`/`d` decide from there.

use super::*;
use crate::conversation::{
    format_elapsed_compact, AgentReceiptKind, AgentReceiptPresentation, AgentRosterRow,
    AgentRowState, ApprovalMenuRow,
};
use chrono::{DateTime, Utc};
use forge_core::{BackgroundTaskKind, BackgroundTaskStatus};
use forge_session::BackgroundTaskSnapshot;

/// Longest error excerpt a roster row or receipt carries.
const AGENT_ERROR_MAX_CHARS: usize = 60;

fn is_child(task: &BackgroundTaskSnapshot) -> bool {
    matches!(task.kind, BackgroundTaskKind::Subagent { .. })
}

fn first_line(text: &str, max_chars: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max_chars {
        let cut: String = line.chars().take(max_chars).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_string()
    }
}

/// The request as one line: the tool, then the command when the redacted
/// arguments carry one.
fn request_line(payload: &forge_types::HitlPayload) -> String {
    let command = payload
        .args_redacted
        .get("command")
        .and_then(|value| value.as_str())
        .map(|command| first_line(command, 80))
        .filter(|command| !command.is_empty());
    match command {
        Some(command) => format!("{} {command}", payload.tool),
        None => payload.tool.clone(),
    }
}

fn elapsed_for(task: &BackgroundTaskSnapshot, now: DateTime<Utc>) -> String {
    let end = task.finished_at.unwrap_or(now);
    let secs = (end - task.started_at).num_milliseconds() as f64 / 1000.0;
    format_elapsed_compact(secs)
}

fn row_state(task: &BackgroundTaskSnapshot) -> AgentRowState {
    match &task.status {
        BackgroundTaskStatus::WaitingForApproval { payload } => AgentRowState::Waiting {
            request: request_line(payload),
        },
        BackgroundTaskStatus::Succeeded { .. } => AgentRowState::Finished,
        BackgroundTaskStatus::Failed { error } => AgentRowState::Failed {
            error: first_line(error, AGENT_ERROR_MAX_CHARS),
        },
        BackgroundTaskStatus::Cancelled => AgentRowState::Cancelled,
        BackgroundTaskStatus::Queued | BackgroundTaskStatus::Running => AgentRowState::Running,
    }
}

impl TuiApp {
    /// One roster row per child this session spawned, in spawn order.
    pub(super) fn agent_roster_rows(
        &self,
        tasks: &[BackgroundTaskSnapshot],
        now: DateTime<Utc>,
    ) -> Vec<AgentRosterRow> {
        let mut children: Vec<&BackgroundTaskSnapshot> =
            tasks.iter().filter(|task| is_child(task)).collect();
        children.sort_by_key(|task| (task.started_at, task.id.0));
        children
            .into_iter()
            .map(|task| AgentRosterRow {
                task_id: task.id.0,
                label: task.label.clone(),
                mode: task
                    .child
                    .as_ref()
                    .and_then(|child| child.mode)
                    .map(|mode| format!("{mode:?}")),
                state: row_state(task),
                elapsed: elapsed_for(task, now),
                child_session_id: task.child_session_id.map(|id| id.to_string()),
                turn_ordinal: self
                    .agents
                    .roster_anchors
                    .iter()
                    .find(|(id, _)| *id == task.id)
                    .map(|(_, ordinal)| *ordinal),
            })
            .collect()
    }

    /// Remember the turn each child was first seen in, so a roster row with
    /// no spawn tool row in view has a fixed place to stand.
    pub(super) fn anchor_agent_rosters(
        &mut self,
        ids: impl IntoIterator<Item = forge_types::BackgroundTaskId>,
    ) {
        let Some(ordinal) = self.current_turn_ordinal() else {
            return;
        };
        for id in ids {
            if !self
                .agents
                .roster_anchors
                .iter()
                .any(|(task, _)| *task == id)
            {
                self.agents.roster_anchors.push((id, ordinal));
            }
        }
    }

    /// A receipt for every child that has reached a terminal state, plus one
    /// for every request decided from the inline card, each under the turn
    /// it happened in. Ordered by turn, then by when it happened.
    pub(super) fn agent_receipts(
        &self,
        tasks: &[BackgroundTaskSnapshot],
    ) -> Vec<AgentReceiptPresentation> {
        let anchor_for = |id: forge_types::BackgroundTaskId| {
            self.agents
                .receipt_anchors
                .iter()
                .find(|(task, _)| *task == id)
                .map(|(_, ordinal)| *ordinal)
        };
        let mut receipts: Vec<(Option<u64>, DateTime<Utc>, AgentReceiptPresentation)> = Vec::new();
        for task in tasks.iter().filter(|task| is_child(task)) {
            let kind = match &task.status {
                BackgroundTaskStatus::Succeeded { .. } => AgentReceiptKind::Finished,
                BackgroundTaskStatus::Failed { error } => AgentReceiptKind::Failed {
                    error: first_line(error, AGENT_ERROR_MAX_CHARS),
                },
                BackgroundTaskStatus::Cancelled => AgentReceiptKind::Cancelled,
                _ => continue,
            };
            let finished = task.finished_at.unwrap_or(task.started_at);
            let summary = task
                .latest_message
                .as_deref()
                .or(match &task.status {
                    BackgroundTaskStatus::Succeeded { summary } => Some(summary.as_str()),
                    _ => None,
                })
                .map(|text| first_line(text, 120))
                .filter(|text| !text.is_empty());
            let ordinal = anchor_for(task.id);
            receipts.push((
                ordinal,
                finished,
                AgentReceiptPresentation {
                    task_id: task.id.0,
                    label: task.label.clone(),
                    kind,
                    elapsed: elapsed_for(task, finished),
                    summary,
                    branch: task.worktree_branch.clone(),
                    turn_ordinal: ordinal,
                },
            ));
        }
        for (index, decision) in self.agents.decisions.iter().enumerate() {
            // Decisions are already in the order they were taken; a fixed
            // timestamp far in the past keeps them ahead of finishes within
            // the same turn, which is also the order they happened in.
            let when = DateTime::<Utc>::from_timestamp(index as i64, 0).unwrap_or_default();
            receipts.push((
                Some(decision.turn_ordinal),
                when,
                AgentReceiptPresentation {
                    task_id: decision.task_id.0,
                    label: decision.label.clone(),
                    kind: AgentReceiptKind::Decided {
                        allowed: decision.allowed,
                    },
                    elapsed: String::new(),
                    summary: None,
                    branch: None,
                    turn_ordinal: Some(decision.turn_ordinal),
                },
            ));
        }
        receipts.sort_by(|a, b| {
            let turn = |o: Option<u64>| o.unwrap_or(u64::MAX);
            turn(a.0).cmp(&turn(b.0)).then(a.1.cmp(&b.1))
        });
        receipts
            .into_iter()
            .map(|(_, _, receipt)| receipt)
            .collect()
    }

    /// The child whose request the inline card shows: the oldest one waiting.
    pub(super) fn first_waiting_child(
        &self,
        tasks: &[BackgroundTaskSnapshot],
    ) -> Option<BackgroundTaskSnapshot> {
        if self.child_view.is_some() {
            return None;
        }
        tasks
            .iter()
            .filter(|task| is_child(task))
            .filter(|task| {
                matches!(task.status, BackgroundTaskStatus::WaitingForApproval { .. })
                    && task.pending_approval.is_some()
            })
            .min_by_key(|task| (task.started_at, task.id.0))
            .cloned()
    }

    /// Whether an inline child card is on screen, which is what puts the
    /// Approval block in the Tab cycle when the session has no decision of
    /// its own pending.
    pub(super) fn child_card_available(&self) -> bool {
        self.first_waiting_child(&self.selected_background_tasks())
            .is_some()
    }

    /// The two rows of a child's card. The safe choice leads and is the
    /// default; the letters are the ones the Tasks view uses for the same
    /// decision.
    pub(super) fn child_card_options() -> Vec<ApprovalMenuRow> {
        vec![
            ApprovalMenuRow {
                label: "Don't run".into(),
                detail: None,
                help: Some(
                    "Refuses this invocation; the child continues with that refusal. \
                     Other work and queued prompts are unaffected."
                        .into(),
                ),
                key: Some("d".into()),
            },
            ApprovalMenuRow {
                label: "Allow once".into(),
                detail: None,
                help: Some(
                    "Retries this one invocation outside the sandbox. Existing runtime \
                     permission checks still apply."
                        .into(),
                ),
                key: Some("a".into()),
            },
        ]
    }

    /// Remember the request the card was drawn with, so a decision key acts
    /// on what the operator saw.
    pub(super) fn paint_child_card(&mut self, task: Option<&BackgroundTaskSnapshot>) {
        self.agents.card_paint = task.and_then(|task| {
            task.pending_approval
                .as_ref()
                .map(|request| ChildCardPaint {
                    owner: self.selected_session_id,
                    task_id: task.id,
                    request: request.clone(),
                })
        });
    }

    /// 0-based ordinal of the turn in progress or just finished, for the
    /// direct runtime. Supervised sessions anchor nothing; their receipts
    /// land at the end.
    fn current_turn_ordinal(&self) -> Option<u64> {
        match self.selected_runtime() {
            SelectedRuntime::Direct => Some(
                self.session_runtime
                    .messages
                    .iter()
                    .filter(|m| m.role == forge_types::MessageRole::User)
                    .count()
                    .saturating_sub(1) as u64,
            ),
            SelectedRuntime::Supervised(_) => None,
        }
    }

    /// Pin a child's receipt under the turn in which the poller saw it finish.
    pub(super) fn anchor_agent_receipt(&mut self, id: forge_types::BackgroundTaskId) {
        if self
            .agents
            .receipt_anchors
            .iter()
            .any(|(task, _)| *task == id)
        {
            return;
        }
        if let Some(ordinal) = self.current_turn_ordinal() {
            self.agents.receipt_anchors.push((id, ordinal));
        }
    }

    /// Keys while the inline child card holds focus. Returns `true` when the
    /// key was the card's.
    pub(super) async fn handle_child_card_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if self.overlay.is_some()
            || self.scratchpad.is_some()
            || self.focus.block() != FocusBlock::Approval
            || self.selected_pending_hitl().is_some()
            || self.selected_pending_question().is_some()
        {
            return Ok(false);
        }
        let Some(task) = self.first_waiting_child(&self.selected_background_tasks()) else {
            return Ok(false);
        };
        if !key.modifiers.is_empty() {
            return Ok(false);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                self.agents.card_selected = 1 - self.agents.card_selected.min(1);
                Ok(true)
            }
            KeyCode::Esc => {
                // Leaving is not deciding: the child's request does not block
                // this session's own work.
                self.focus_block(FocusBlock::Composer);
                Ok(true)
            }
            KeyCode::Char('a') => {
                self.decide_child_card(&task, true).await;
                Ok(true)
            }
            KeyCode::Char('d') => {
                self.decide_child_card(&task, false).await;
                Ok(true)
            }
            KeyCode::Enter => {
                let allow = self.agents.card_selected == 1;
                self.decide_child_card(&task, allow).await;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Decide the request the card shows. Refuses when the card has not been
    /// painted with this exact request, or when the child moved on since.
    async fn decide_child_card(&mut self, task: &BackgroundTaskSnapshot, allow: bool) {
        let owner = self.selected_session_id;
        let Some(request) = task.pending_approval.clone() else {
            return;
        };
        let painted = self.agents.card_paint.as_ref().is_some_and(|paint| {
            paint.owner == owner && paint.task_id == task.id && paint.request.matches(&request)
        });
        if !painted {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "inspect this child's full request before deciding",
            );
            return;
        }
        let decision = if allow {
            HitlDecision::Approve
        } else {
            HitlDecision::Deny
        };
        let sent = if self.selected_is_supervised() {
            self.submit_session_command(
                forge_session::SupervisorCommand::ResolveBackgroundRequest {
                    session_id: owner,
                    task_id: task.id,
                    request: Box::new(request.clone()),
                    decision,
                },
            )
        } else {
            self.session_runtime
                .background_control()
                .resolve_request(task.id, &request, decision)
        };
        if !sent {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "child request changed · nothing decided",
            );
            return;
        }
        let already_recorded = self
            .agents
            .decisions
            .iter()
            .any(|decision| decision.request_id == request.id);
        if let Some(turn_ordinal) = self.current_turn_ordinal().filter(|_| !already_recorded) {
            self.agents.decisions.push(ChildDecisionRecord {
                task_id: task.id,
                request_id: request.id,
                label: task.label.clone(),
                allowed: allow,
                turn_ordinal,
            });
        }
        self.agents.card_selected = 0;
        self.agents.card_paint = None;
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "{} sent to {}",
                if allow { "allow once" } else { "don't run" },
                task.label
            ),
        );
        // The card settles into its receipt; with nothing left to decide the
        // Approval block leaves the cycle and focus returns to the composer.
        if !self.child_card_available() {
            self.focus_block(FocusBlock::Composer);
        }
    }
}
