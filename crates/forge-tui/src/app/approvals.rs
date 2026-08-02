//! Human-in-the-loop tool approval for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. A tool call that governance defers surfaces as
//! an approval overlay; these methods build its identity, decide whether a
//! session-scoped allowance already covers it, and apply the operator's
//! decision.
//!
//! [`ApprovalIdentity`] lives here too, so the definition of what makes two tool
//! calls "the same" for approval purposes sits beside the code that acts on it.
//! It stays `pub(super)`: `TuiApp` holds a set of them, and the overlay renderer
//! reads their labels.
//!
//! Methods and the type are moved verbatim.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ApprovalIdentity {
    executable: String,
    arguments: Vec<String>,
    working_directory: String,
    environment_delta: String,
    workspace_identity: String,
    session_id: String,
}

impl ApprovalIdentity {
    pub(super) fn label(&self) -> String {
        std::iter::once(self.executable.as_str())
            .chain(self.arguments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect()
    }
}

impl TuiApp {
    fn approval_state_for_payload(&self, payload: &HitlPayload) -> ApprovalOverlayState {
        ApprovalOverlayState::for_payload(
            payload,
            self.session.workspace_root().display().to_string(),
        )
    }

    pub(super) fn approval_identity_for_payload(
        &self,
        payload: &HitlPayload,
    ) -> Option<ApprovalIdentity> {
        let approval = self.approval_state_for_payload(payload);
        if approval.mode != ApprovalExecutionMode::Direct || !approval.remember_eligible {
            return None;
        }
        Some(ApprovalIdentity {
            executable: approval.executable_or_shell,
            arguments: approval.arguments,
            working_directory: approval.working_directory,
            environment_delta: approval.environment_delta,
            workspace_identity: self.repository_or_workspace_id(),
            session_id: self.session.session_id.to_string(),
        })
    }

    pub(super) fn open_hitl_overlay(&mut self, payload: HitlPayload) {
        self.overlay = Some(Overlay::hitl_with_working_directory(
            payload,
            self.session.workspace_root().display().to_string(),
        ));
    }

    pub async fn drain_pending_hitl(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let Some(decision) = self.pending_interaction.hitl_decision.take() else {
            return Ok(());
        };
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        self.session.resolve_hitl(decision.clone(), "tui").await?;
        self.status_message = match decision {
            HitlDecision::Approve => "Action approved".into(),
            HitlDecision::Deny => "Action denied".into(),
            // `HitlDecision` is `#[non_exhaustive]`. Report an unrecognised decision as
            // denied so the operator sees what `resolve_hitl` actually did with it.
            _ => "Action denied".into(),
        };
        self.push_notice(vec![self.status_message.clone()]);
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    pub(super) async fn resolve_hitl_overlay(
        &mut self,
        decision: HitlDecision,
        remember_exact_direct: bool,
    ) -> Result<(), TuiError> {
        let Some(payload) = self.session.pending_hitl().cloned() else {
            self.overlay = None;
            return Ok(());
        };

        let identity_to_remember = if remember_exact_direct {
            let Some(identity) = self.approval_identity_for_payload(&payload) else {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "this approval cannot be remembered; use Allow once or Deny",
                );
                return Ok(());
            };
            Some(identity)
        } else {
            None
        };

        self.session.resolve_hitl(decision.clone(), "tui").await?;
        if let Some(identity) = identity_to_remember {
            self.hitl_session_allow.insert(identity);
        }
        self.overlay = None;
        match decision {
            HitlDecision::Approve if remember_exact_direct => {
                self.push_toast("remembered exact Direct invocation");
            }
            HitlDecision::Approve => self.push_toast("approved once"),
            HitlDecision::Deny => self.push_toast("denied"),
            // `HitlDecision` is `#[non_exhaustive]`; an unrecognised decision is denied.
            _ => self.push_toast("denied"),
        }
        self.resume_turn_after_hitl();
        Ok(())
    }

    /// `resolve_hitl` only executes the tool (or records a denial) and
    /// transitions the session lifecycle back to `Working` — it does not, by
    /// itself, make the follow-up model call. `drain_pending_prompt` already
    /// exited (via `ApplyOutcome::Hitl`) and cleared `busy` before the
    /// approval overlay was ever shown, so without this, nothing re-enters
    /// the turn loop: the header keeps reading "Working" (from the core's
    /// lifecycle) forever while the TUI itself sits fully idle, and no
    /// amount of waiting, Ctrl+C's graceful interrupt, or Esc can act on a
    /// turn that isn't actually running. Re-arm the same continuation flag
    /// `dequeue_and_send_next` uses so `run_loop` restarts the model call on
    /// its next tick.
    fn resume_turn_after_hitl(&mut self) {
        self.pending_turn.continue_turn = true;
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.timing.started = Some(Instant::now());
        self.stream_preview.clear();
        self.stream_thinking.clear();
    }

    pub fn maybe_open_hitl(&mut self) {
        if self.overlay.is_none() {
            if let Some(p) = self.session.pending_hitl() {
                if self
                    .approval_identity_for_payload(p)
                    .is_some_and(|identity| self.hitl_session_allow.contains(&identity))
                {
                    // Will be drained by `drain_auto_hitl` in the event loop.
                    return;
                }
                self.open_hitl_overlay(p.clone());
            }
        }
    }

    /// Auto-approve HITL for exact Direct invocations remembered this session.
    pub async fn drain_auto_hitl(&mut self) -> Result<(), TuiError> {
        if let Some(ref p) = self.session.pending_hitl().cloned() {
            if let Some(identity) = self.approval_identity_for_payload(p) {
                if !self.hitl_session_allow.contains(&identity) {
                    return Ok(());
                }
                self.session
                    .resolve_hitl(HitlDecision::Approve, "tui-session")
                    .await?;
                self.push_toast(format!("auto-approved {}", identity.label()));
                self.resume_turn_after_hitl();
            }
        }
        Ok(())
    }
}
