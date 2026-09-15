//! Quit-all: the pre-flight summary shown when quitting would discard work in
//! other sessions, and the sweep that follows it.
//!
//! Quitting Forge has always torn every session down — the process exit runs
//! `SupervisorCommand::Shutdown`, which retires every actor. What the operator
//! never had was a chance to see the cost first: turns running in sessions
//! they are not looking at, prompts still queued behind them, approvals and
//! agent questions waiting on an answer, and unsaved editor buffers in other
//! session views. This module owns that summary, the gate that decides whether
//! it is worth a dialog, and the fan-out that closes everything before the
//! process goes away.

use super::*;

impl TuiApp {
    /// What quitting right now would discard.
    ///
    /// Read from the supervisor's roster rather than from the focused view, so
    /// the numbers describe every session and not just the visible one.
    pub(super) fn quit_all_summary(&self) -> QuitAllSummary {
        let Some(supervisor) = self.supervisor.as_ref() else {
            return QuitAllSummary::default();
        };
        let mut summary = QuitAllSummary::default();
        for snapshot in supervisor.snapshots.values() {
            if snapshot.task.lifecycle != forge_session::SessionLifecycle::Active {
                continue;
            }
            summary.sessions += 1;
            if matches!(
                snapshot.task.turn_state,
                forge_session::SupervisorTurnState::Running
                    | forge_session::SupervisorTurnState::Queued
            ) {
                summary.in_flight += 1;
            }
            summary.queued_prompts += snapshot.queued_prompts.len();
            if snapshot.session.pending_hitl.is_some()
                || snapshot.session.pending_question.is_some()
            {
                summary.pending_requests += 1;
            }
            if self.session_view_state_is_dirty(snapshot.task.session_id) {
                summary
                    .dirty_sessions
                    .push(Self::session_display_label(snapshot));
            }
        }
        summary
    }

    /// The label a session shows in the navigator, so the dialog names the
    /// same thing the operator sees there. An unnamed session is created
    /// before its first prompt, and shows the same placeholder.
    pub(super) fn session_display_label(snapshot: &SessionRuntimeSnapshot) -> String {
        if snapshot.task.label.is_empty() {
            "session".into()
        } else {
            snapshot.task.label.clone()
        }
    }

    /// The one gate every quit passes through: `Ctrl+D`, `/quit` on the
    /// primary session, and the exit that follows a resolved dirty buffer.
    ///
    /// Asking to quit is always a request to quit Forge; the only question is
    /// whether the operator is told what that costs.
    pub(super) fn begin_quit(&mut self) {
        // A sweep is already under way (the roster is shrinking under it), so
        // a second request has nothing left to ask about.
        if self.quitting {
            return;
        }
        let summary = self.quit_all_summary();
        if !summary.needs_confirmation() {
            self.start_quit_all();
            return;
        }
        self.explorer_dialog.show(ExplorerDialog::QuitAll {
            summary,
            choice: QuitAllChoice::default(),
        });
    }

    /// Stop managing every session, then let the process exit.
    ///
    /// The app exits on either outcome. A session that will not retire is
    /// reported through the exit summary, never allowed to hold an app the
    /// operator asked to close open.
    pub(super) fn start_quit_all(&mut self) {
        self.quitting = true;
        let summary = self.quit_all_summary();
        if summary.sessions == 0 {
            self.exit.request();
            self.status_state.message = "quitting…".into();
            return;
        }
        self.status_state.message =
            format!("closing {}…", counted_noun(summary.sessions, "session"));
        if !self.submit_session_command_tracked(
            forge_session::SupervisorCommand::CloseAllSessions,
            CommandFollowUp::QuitAll,
        ) {
            // The command never reached the supervisor. The process exit still
            // retires the actors, so leaving is still the honest outcome.
            self.exit.request();
            self.status_state.message = "quitting…".into();
        }
    }
}
