//! Human-in-the-loop tool approval for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. A tool call that governance defers surfaces as
//! an inline conversation prompt; these methods build its identity, decide
//! whether a session-scoped allowance already covers it, and apply the
//! operator's decision.
//!
//! [`ApprovalIdentity`] lives here too, so the definition of what makes two tool
//! calls "the same" for approval purposes sits beside the code that acts on it.
//! It stays `pub(super)`: `TuiApp` holds a set of them, and the overlay renderer
//! reads their labels.
//!
//! Methods and the type are moved verbatim.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalMenuKind {
    AllowOnce,
    AllowPattern,
    Deny,
}

#[derive(Debug, Clone, Default)]
struct ApprovalMenuState {
    /// `call_id` of the pending payload this menu was built for.
    call_id: Option<String>,
    selected: usize,
}

/// All session-scoped approval state. Its fields intentionally remain private:
/// approval policy must not be changed by unrelated TUI modules.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApprovalSessionState {
    menu: ApprovalMenuState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ApprovalIdentity {
    executable: String,
    arguments: Vec<String>,
    raw_args: serde_json::Value,
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

/// Build the `ToolCall` a `HitlPayload` represents, for matching against
/// session-scoped pattern-allow rules (which are keyed on the call shape,
/// not an exact-invocation identity like `ApprovalIdentity`).
fn tool_call_for_payload(payload: &HitlPayload) -> forge_types::ToolCall {
    forge_types::ToolCall {
        id: payload.call_id.clone(),
        name: payload.tool.clone(),
        arguments: payload.args_redacted.clone(),
    }
}

impl TuiApp {
    pub(super) fn approval_menu_selected(&self) -> usize {
        self.approval_session.menu.selected
    }

    pub(super) fn remembered_approval_count(&self) -> usize {
        self.session.session_exact_allow_count()
    }

    pub(super) fn clear_session_approvals(&mut self) {
        self.session.clear_session_exact_allows();
        self.approval_session.menu = ApprovalMenuState::default();
    }

    #[cfg(test)]
    pub(super) fn is_approval_remembered(&self, identity: &ApprovalIdentity) -> bool {
        self.session
            .session_exact_allows(&forge_governance::SessionExactAllow {
                tool: identity.executable.clone(),
                arguments: identity.raw_args.clone(),
                working_directory: identity.working_directory.clone(),
                environment_delta: identity.environment_delta.clone(),
            })
    }

    fn approval_state_for_payload(&self, payload: &HitlPayload) -> ApprovalOverlayState {
        ApprovalOverlayState::for_payload(
            payload,
            self.session_view.workspace_root().display().to_string(),
        )
    }

    /// Reset menu selection when the pending HITL call changes or clears.
    pub(super) fn sync_approval_menu(&mut self) {
        match self.session.pending_hitl() {
            None => {
                self.approval_session.menu = ApprovalMenuState::default();
            }
            Some(payload) => {
                if self.approval_session.menu.call_id.as_deref() != Some(payload.call_id.as_str()) {
                    self.approval_session.menu = ApprovalMenuState {
                        call_id: Some(payload.call_id.clone()),
                        selected: 0,
                    };
                }
                let n = self.approval_menu_kinds().len();
                if n > 0 {
                    self.approval_session.menu.selected =
                        self.approval_session.menu.selected.min(n - 1);
                }
            }
        }
    }

    /// When a new approval arrives, claim focus on the approval card and
    /// scroll it into view — once. Runs from the event loop (never from
    /// render, so a draw can't move focus). After the user Tabs away, the
    /// transition is over and this must not re-grab focus.
    pub(super) fn sync_approval_focus(&mut self) {
        let Some(payload) = self.session.pending_hitl() else {
            return;
        };
        if self.approval_session.menu.call_id.as_deref() != Some(payload.call_id.as_str()) {
            self.focus_block(FocusBlock::Approval);
            self.conversation_view.follow = true;
        }
    }

    fn approval_menu_kinds(&self) -> Vec<ApprovalMenuKind> {
        let Some(payload) = self.session.pending_hitl() else {
            return Vec::new();
        };
        let approval = self.approval_state_for_payload(payload);
        let mut kinds = vec![ApprovalMenuKind::AllowOnce];
        if approval.pattern_allow_eligible {
            kinds.push(ApprovalMenuKind::AllowPattern);
        }
        kinds.push(ApprovalMenuKind::Deny);
        kinds
    }

    pub(super) fn approval_menu_rows(&self) -> Vec<crate::conversation::ApprovalMenuRow> {
        let Some(payload) = self.session.pending_hitl() else {
            return Vec::new();
        };
        let remembered = forge_governance::exact_pattern(&tool_call_for_payload(payload));
        self.approval_menu_kinds()
            .into_iter()
            .map(|kind| match kind {
                ApprovalMenuKind::AllowOnce => crate::conversation::ApprovalMenuRow {
                    label: "Allow once".into(),
                    detail: None,
                },
                ApprovalMenuKind::AllowPattern => crate::conversation::ApprovalMenuRow {
                    label: "Allow pattern".into(),
                    detail: Some(remembered.clone()),
                },
                ApprovalMenuKind::Deny => crate::conversation::ApprovalMenuRow {
                    label: "Deny".into(),
                    detail: None,
                },
            })
            .collect()
    }

    /// Handle keys for the inline approval menu. Returns true if consumed.
    /// Only active while the approval card itself holds focus — the operator
    /// may Tab elsewhere mid-approval (file navigation, panels), and there
    /// the menu keys must not hijack normal block navigation.
    pub(super) async fn handle_approval_menu_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if self.session.pending_hitl().is_none() {
            return Ok(false);
        }
        if self.focus.block() != FocusBlock::Approval {
            return Ok(false);
        }
        self.sync_approval_menu();
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                let n = self.approval_menu_kinds().len().max(1);
                self.approval_session.menu.selected =
                    (self.approval_session.menu.selected + n - 1) % n;
                Ok(true)
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                let n = self.approval_menu_kinds().len().max(1);
                self.approval_session.menu.selected = (self.approval_session.menu.selected + 1) % n;
                Ok(true)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.resolve_approval_line(ApprovalMenuKind::Deny).await?;
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let kinds = self.approval_menu_kinds();
                let Some(kind) = kinds.get(self.approval_session.menu.selected).copied() else {
                    return Ok(true);
                };
                match kind {
                    ApprovalMenuKind::AllowOnce => {
                        self.resolve_approval_line(ApprovalMenuKind::AllowOnce)
                            .await?;
                    }
                    ApprovalMenuKind::AllowPattern => {
                        self.resolve_approval_line(ApprovalMenuKind::AllowPattern)
                            .await?;
                    }
                    ApprovalMenuKind::Deny => {
                        self.resolve_approval_line(ApprovalMenuKind::Deny).await?;
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn session_exact_for_payload(
        &self,
        payload: &HitlPayload,
    ) -> Option<forge_governance::SessionExactAllow> {
        let approval = self.approval_state_for_payload(payload);
        if !approval.pattern_allow_eligible {
            return None;
        }
        Some(forge_governance::SessionExactAllow::from_call(
            &tool_call_for_payload(payload),
            self.session.workspace_root(),
        ))
    }

    pub(super) fn approval_identity_for_payload(
        &self,
        payload: &HitlPayload,
    ) -> Option<ApprovalIdentity> {
        let approval = self.approval_state_for_payload(payload);
        if !approval.pattern_allow_eligible {
            return None;
        }
        Some(ApprovalIdentity {
            executable: payload.tool.clone(),
            arguments: approval.arguments,
            raw_args: payload.args_redacted.clone(),
            working_directory: approval.working_directory,
            environment_delta: approval.environment_delta,
            workspace_identity: self.repository_or_workspace_id(),
            session_id: self.session.session_id.to_string(),
        })
    }

    /// Apply a menu choice to the pending HITL request. Eligibility for
    /// `remember`/`always` is checked here, not in the menu builder, so an
    /// ineligible choice is warned about without acting.
    pub(super) async fn resolve_approval_line(
        &mut self,
        action: ApprovalMenuKind,
    ) -> Result<(), TuiError> {
        match action {
            ApprovalMenuKind::AllowOnce => {
                self.resolve_hitl_overlay(HitlDecision::Approve, false)
                    .await
            }
            ApprovalMenuKind::AllowPattern => {
                self.resolve_hitl_overlay(HitlDecision::Approve, true).await
            }
            ApprovalMenuKind::Deny => self.resolve_hitl_overlay(HitlDecision::Deny, false).await,
        }
    }

    pub async fn drain_pending_hitl(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let Some(decision) = self.pending_interaction.take_hitl_decision() else {
            return Ok(());
        };
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        self.session.resolve_hitl(decision.clone(), "tui").await?;
        self.status_state.message = match decision {
            HitlDecision::Approve => "Action approved".into(),
            HitlDecision::Deny => "Action denied".into(),
            // `HitlDecision` is `#[non_exhaustive]`. Report an unrecognised decision as
            // denied so the operator sees what `resolve_hitl` actually did with it.
            _ => "Action denied".into(),
        };
        self.push_notice(vec![self.status_state.message.clone()]);
        self.busy_state.set_phase(BusyPhase::Idle);
        self.enter_chat_composer();
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
            return Ok(());
        };

        if remember_exact_direct {
            let Some(grant) = self.session_exact_for_payload(&payload) else {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "this call has no session pattern to remember; use Allow once or Deny",
                );
                return Ok(());
            };
            self.session.allow_exact_for_session(grant);
        }
        self.session.resolve_hitl(decision.clone(), "tui").await?;
        match decision {
            HitlDecision::Approve if remember_exact_direct => {
                self.push_toast("allowed this command for the session");
            }
            HitlDecision::Approve => self.push_toast("approved once"),
            HitlDecision::Deny => self.push_toast("denied"),
            // `HitlDecision` is `#[non_exhaustive]`; an unrecognised decision is denied.
            _ => self.push_toast("denied"),
        }
        self.resume_turn_after_hitl();
        self.enter_chat_composer();
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
        self.pending_turn.request_continue();
        self.busy_state.start(BusyPhase::Model);
        self.timing.started = Some(Instant::now());
        self.stream.preview.clear();
        self.stream.thinking.clear();
    }

    /// Auto-approve HITL for exact invocations remembered this session.
    /// The session authorizer already skips a matching call; this covers
    /// a prompt that was already on screen when the grant was recorded.
    pub async fn drain_auto_hitl(&mut self) -> Result<(), TuiError> {
        let Some(payload) = self.session.pending_hitl().cloned() else {
            return Ok(());
        };
        let identity = self.approval_identity_for_payload(&payload);
        let identity_allowed = self
            .session_exact_for_payload(&payload)
            .is_some_and(|grant| self.session.session_exact_allows(&grant));
        if !identity_allowed {
            return Ok(());
        }
        self.session
            .resolve_hitl(HitlDecision::Approve, "tui-session")
            .await?;
        let label = identity
            .map(|identity| identity.label())
            .unwrap_or(payload.tool);
        self.push_toast(format!("auto-approved {label}"));
        self.resume_turn_after_hitl();
        // The card just disappeared under the user; drop back to the composer
        // rather than letting normalize_focus strand focus on a ghost block.
        if self.focus.block() == FocusBlock::Approval {
            self.enter_chat_composer();
        }
        Ok(())
    }
}
