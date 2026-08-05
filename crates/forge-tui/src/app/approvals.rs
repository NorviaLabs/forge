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
    fn approval_state_for_payload(&self, payload: &HitlPayload) -> ApprovalOverlayState {
        ApprovalOverlayState::for_payload(
            payload,
            self.session.workspace_root().display().to_string(),
        )
    }

    /// Reset menu selection when the pending HITL call changes or clears.
    pub(super) fn sync_approval_menu(&mut self) {
        match self.session.pending_hitl() {
            None => {
                self.hitl_session.menu = ApprovalMenuState::default();
            }
            Some(payload) => {
                if self.hitl_session.menu.call_id.as_deref() != Some(payload.call_id.as_str()) {
                    self.hitl_session.menu = ApprovalMenuState {
                        call_id: Some(payload.call_id.clone()),
                        selected: 0,
                        phase: ApprovalMenuPhase::Choose,
                    };
                }
                let n = self.approval_menu_kinds().len();
                if n > 0 {
                    self.hitl_session.menu.selected = self.hitl_session.menu.selected.min(n - 1);
                }
            }
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
        if approval.remember_eligible {
            kinds.push(ApprovalMenuKind::Remember);
        }
        kinds.push(ApprovalMenuKind::Deny);
        kinds.push(ApprovalMenuKind::DenyWithNote);
        kinds
    }

    pub(super) fn approval_menu_rows(&self) -> Vec<crate::conversation::ApprovalMenuRow> {
        let Some(payload) = self.session.pending_hitl() else {
            return Vec::new();
        };
        let approval = self.approval_state_for_payload(payload);
        self.approval_menu_kinds()
            .into_iter()
            .map(|kind| match kind {
                ApprovalMenuKind::AllowOnce => crate::conversation::ApprovalMenuRow {
                    label: "Allow once".into(),
                    detail: None,
                },
                ApprovalMenuKind::AllowPattern => crate::conversation::ApprovalMenuRow {
                    label: "Allow pattern going forward".into(),
                    detail: Some(approval.suggested_pattern.clone()),
                },
                ApprovalMenuKind::Remember => crate::conversation::ApprovalMenuRow {
                    label: "Remember exact (session)".into(),
                    detail: None,
                },
                ApprovalMenuKind::Deny => crate::conversation::ApprovalMenuRow {
                    label: "Deny".into(),
                    detail: None,
                },
                ApprovalMenuKind::DenyWithNote => crate::conversation::ApprovalMenuRow {
                    label: "Deny with note…".into(),
                    detail: None,
                },
            })
            .collect()
    }

    /// Handle keys for the inline approval menu. Returns true if consumed.
    pub(super) async fn handle_approval_menu_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if self.session.pending_hitl().is_none() {
            return Ok(false);
        }
        self.sync_approval_menu();
        match self.hitl_session.menu.phase {
            ApprovalMenuPhase::DenyFeedback => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.hitl_session.menu.phase = ApprovalMenuPhase::Choose;
                    self.input.clear();
                    Ok(true)
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let note = self.input.take();
                    self.hitl_session.menu.phase = ApprovalMenuPhase::Choose;
                    self.resolve_approval_line(input_route::ApprovalAction::DenyWithFeedback(note))
                        .await?;
                    Ok(true)
                }
                _ => Ok(false), // let composer edit the note
            },
            ApprovalMenuPhase::Choose => match key.code {
                KeyCode::Up if key.modifiers.is_empty() => {
                    let n = self.approval_menu_kinds().len().max(1);
                    self.hitl_session.menu.selected = (self.hitl_session.menu.selected + n - 1) % n;
                    Ok(true)
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    let n = self.approval_menu_kinds().len().max(1);
                    self.hitl_session.menu.selected = (self.hitl_session.menu.selected + 1) % n;
                    Ok(true)
                }
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.resolve_approval_line(input_route::ApprovalAction::Deny)
                        .await?;
                    Ok(true)
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let kinds = self.approval_menu_kinds();
                    let Some(kind) = kinds.get(self.hitl_session.menu.selected).copied() else {
                        return Ok(true);
                    };
                    match kind {
                        ApprovalMenuKind::AllowOnce => {
                            self.resolve_approval_line(input_route::ApprovalAction::Approve)
                                .await?;
                        }
                        ApprovalMenuKind::AllowPattern => {
                            self.resolve_approval_line(input_route::ApprovalAction::AllowPattern)
                                .await?;
                        }
                        ApprovalMenuKind::Remember => {
                            self.resolve_approval_line(input_route::ApprovalAction::Remember)
                                .await?;
                        }
                        ApprovalMenuKind::Deny => {
                            self.resolve_approval_line(input_route::ApprovalAction::Deny)
                                .await?;
                        }
                        ApprovalMenuKind::DenyWithNote => {
                            self.hitl_session.menu.phase = ApprovalMenuPhase::DenyFeedback;
                            self.input.clear();
                            self.enter_chat_composer();
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            },
        }
    }

    /// If Allow once should offer a follow-up pattern persist nudge, return
    /// the suggested pattern. Skips when not eligible or already covered.
    fn pattern_nudge_after_allow_once(&self) -> Option<String> {
        let payload = self.session.pending_hitl()?;
        let approval = self.approval_state_for_payload(payload);
        if !approval.pattern_allow_eligible {
            return None;
        }
        let call = tool_call_for_payload(payload);
        if self
            .hitl_session
            .pattern_allow
            .iter()
            .any(|rule| rule.matches(&call))
        {
            return None;
        }
        let pattern = approval.suggested_pattern;
        if pattern.trim().is_empty() {
            return None;
        }
        Some(pattern)
    }

    /// Handle keys for the post-allow-once pattern nudge. Returns true if consumed.
    pub(super) fn handle_pattern_nudge_key(&mut self, key: event::KeyEvent) -> bool {
        let Some(nudge) = self.hitl_session.pattern_nudge.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                nudge.selected = 1 - nudge.selected.min(1);
                true
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.hitl_session.pattern_nudge = None;
                self.push_toast("pattern not saved");
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let selected = nudge.selected.min(1);
                let pattern = nudge.pattern.clone();
                self.hitl_session.pattern_nudge = None;
                if selected == 0 {
                    self.persist_pattern_allow(pattern);
                } else {
                    self.push_toast("pattern not saved");
                }
                true
            }
            _ => false,
        }
    }

    /// Persist an allow pattern without re-approving a tool call (nudge path).
    fn persist_pattern_allow(&mut self, pattern: String) {
        let Some(rule) = forge_governance::PatternRule::parse(&pattern) else {
            self.set_feedback(FeedbackSeverity::Warn, "could not parse the pattern rule");
            return;
        };
        if self
            .hitl_session
            .pattern_allow
            .iter()
            .any(|existing| existing.raw == rule.raw)
        {
            self.push_toast(format!("already allowed: {pattern}"));
            return;
        }
        if let Err(error) = forge_config::append_user_allow_rule(&pattern) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("pattern rule not saved to disk ({error}); active for this session only"),
            );
        }
        self.hitl_session.pattern_allow.push(rule);
        self.push_toast(format!("allowed going forward: {pattern}"));
    }

    /// Whether a session-scoped pattern rule (added this session via "allow
    /// this pattern going forward") already covers `payload`. Distinct from
    /// `Governance.pattern_allow`, which is fixed for the session from the
    /// permissions file at startup — this lets a rule added mid-session take
    /// effect immediately without a restart.
    fn pattern_allows(&self, payload: &HitlPayload) -> bool {
        let call = tool_call_for_payload(payload);
        self.hitl_session
            .pattern_allow
            .iter()
            .any(|rule| rule.matches(&call))
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

    /// Apply a parsed approval line to the pending HITL request. Eligibility
    /// for `remember`/`always` is checked here, not in the composer parser,
    /// so an ineligible verb is warned about without acting.
    pub(super) async fn resolve_approval_line(
        &mut self,
        action: input_route::ApprovalAction,
    ) -> Result<(), TuiError> {
        match action {
            input_route::ApprovalAction::Approve => {
                let nudge_pattern = self.pattern_nudge_after_allow_once();
                self.resolve_hitl_overlay(HitlDecision::Approve, false)
                    .await?;
                if let Some(pattern) = nudge_pattern {
                    self.hitl_session.pattern_nudge = Some(PatternNudgeState {
                        pattern,
                        selected: 0,
                    });
                }
                Ok(())
            }
            input_route::ApprovalAction::Remember => {
                self.resolve_hitl_overlay(HitlDecision::Approve, true).await
            }
            input_route::ApprovalAction::AllowPattern => {
                let Some(payload) = self.session.pending_hitl() else {
                    return Ok(());
                };
                let approval = self.approval_state_for_payload(payload);
                if !approval.pattern_allow_eligible {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        "this call has no allow pattern to persist; use yes or no",
                    );
                    return Ok(());
                }
                self.resolve_hitl_overlay_with_pattern(approval.suggested_pattern)
                    .await
            }
            input_route::ApprovalAction::Deny => {
                self.resolve_hitl_overlay(HitlDecision::Deny, false).await
            }
            input_route::ApprovalAction::DenyWithFeedback(feedback) => {
                self.resolve_hitl_overlay_with_feedback(feedback).await
            }
        }
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
        self.status_state.message = match decision {
            HitlDecision::Approve => "Action approved".into(),
            HitlDecision::Deny => "Action denied".into(),
            // `HitlDecision` is `#[non_exhaustive]`. Report an unrecognised decision as
            // denied so the operator sees what `resolve_hitl` actually did with it.
            _ => "Action denied".into(),
        };
        self.push_notice(vec![self.status_state.message.clone()]);
        self.busy_state.phase = BusyPhase::Idle;
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
            self.hitl_session.allowed.insert(identity);
        }
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
        self.busy_state.active = true;
        self.busy_state.phase = BusyPhase::Model;
        self.timing.started = Some(Instant::now());
        self.stream.preview.clear();
        self.stream.thinking.clear();
    }

    /// Auto-approve HITL for exact Direct invocations remembered this
    /// session, or calls matching a pattern rule added this session.
    pub async fn drain_auto_hitl(&mut self) -> Result<(), TuiError> {
        let Some(payload) = self.session.pending_hitl().cloned() else {
            return Ok(());
        };
        let identity = self.approval_identity_for_payload(&payload);
        let identity_allowed = identity
            .as_ref()
            .is_some_and(|identity| self.hitl_session.allowed.contains(identity));
        let pattern_allowed = self.pattern_allows(&payload);
        if !identity_allowed && !pattern_allowed {
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
        Ok(())
    }

    /// Approve the pending call and add `pattern` as a durable `allow` rule:
    /// persisted to the personal permissions file for future launches, and
    /// added to the in-session pattern-allow list so it takes effect
    /// immediately without a restart. A write failure still lets the
    /// approval through — the user made a decision either way — but is
    /// surfaced so they know it won't outlive this session.
    pub(super) async fn resolve_hitl_overlay_with_pattern(
        &mut self,
        pattern: String,
    ) -> Result<(), TuiError> {
        if self.session.pending_hitl().is_none() {
            return Ok(());
        }
        self.hitl_session.pattern_nudge = None;
        self.persist_pattern_allow(pattern);
        // persist_pattern_allow toasts; still need to approve the pending call.
        if self.session.pending_hitl().is_none() {
            return Ok(());
        }
        self.session
            .resolve_hitl(HitlDecision::Approve, "tui")
            .await?;
        self.resume_turn_after_hitl();
        Ok(())
    }

    /// Deny the pending call, optionally carrying a short note back to the
    /// agent as tool-result context for what to do instead (opencode's
    /// `CorrectedError`). Blank feedback behaves like a plain deny.
    pub(super) async fn resolve_hitl_overlay_with_feedback(
        &mut self,
        feedback: String,
    ) -> Result<(), TuiError> {
        if self.session.pending_hitl().is_none() {
            return Ok(());
        }
        let trimmed = feedback.trim();
        let feedback_opt = (!trimmed.is_empty()).then_some(trimmed);
        self.session
            .resolve_hitl_with_feedback(HitlDecision::Deny, "tui", feedback_opt)
            .await?;
        self.push_toast(if feedback_opt.is_some() {
            "denied with feedback"
        } else {
            "denied"
        });
        self.resume_turn_after_hitl();
        Ok(())
    }
}
