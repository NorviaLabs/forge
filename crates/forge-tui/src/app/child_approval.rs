//! A decision belongs to the displayed parent, child execution and request.

use super::*;
use crate::selection::cell_inside;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, layout::Rect};

impl TuiApp {
    fn return_from_child_approval(
        &mut self,
        owner: uuid::Uuid,
        view: Option<(crate::tasks_strip::TaskFilter, usize)>,
    ) {
        self.overlay =
            view.filter(|_| owner == self.selected_session_id)
                .map(|(filter, scroll)| Overlay::Tasks {
                    owner,
                    filter,
                    scroll,
                });
        self.child_approval_paint = None;
        self.normalize_focus();
    }

    async fn decide_child_approval(&mut self) {
        let Some(Overlay::ChildApproval {
            owner,
            task,
            request,
            allow,
            return_view,
            ..
        }) = self.overlay.clone()
        else {
            return;
        };
        if owner != self.selected_session_id {
            self.return_from_child_approval(owner, return_view);
            self.set_feedback(FeedbackSeverity::Warn, "parent changed · nothing decided");
            return;
        }
        if !self.child_approval_paint.as_ref().is_some_and(|paint| {
            paint.owner == owner && paint.task_id == task.id && paint.request.matches(&request)
        }) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "inspect this child's full request before deciding",
            );
            return;
        }
        let current = self
            .selected_background_tasks()
            .into_iter()
            .find(|current| current.id == task.id);
        if !current.is_some_and(|current| {
            current.run_id == task.run_id
                && current.child_session_id == task.child_session_id
                && matches!(
                    current.status,
                    forge_core::BackgroundTaskStatus::WaitingForApproval { .. }
                )
                && current
                    .pending_approval
                    .as_ref()
                    .is_some_and(|pending| pending.matches(&request))
        }) {
            self.return_from_child_approval(owner, return_view);
            self.set_feedback(
                FeedbackSeverity::Warn,
                "child request changed or finished · nothing decided",
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
                    request: Box::new(request),
                    decision,
                },
            )
        } else {
            self.session_runtime
                .background_control()
                .resolve_request(task.id, &request, decision)
        };
        self.return_from_child_approval(owner, return_view);
        if !sent {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "child request changed · nothing decided",
            );
            return;
        }
        // Explicit confirmation returns to this child's reader. Completion
        // itself never opens a view, changes the draft or takes focus.
        self.overlay = None;
        self.task_selection.select_task(owner, task.id);
        self.open_selected_child_session().await;
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "{} sent to {}",
                if allow { "allow once" } else { "don't run" },
                task.label
            ),
        );
    }

    pub(super) async fn handle_child_approval_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::ChildApproval {
            owner, return_view, ..
        }) = self.overlay.as_ref()
        else {
            return Ok(false);
        };
        let (owner, return_view) = (*owner, *return_view);
        if owner != self.selected_session_id {
            self.return_from_child_approval(owner, return_view);
            self.set_feedback(FeedbackSeverity::Warn, "parent changed · nothing decided");
            return Ok(true);
        }
        if let Some(Overlay::ChildApproval { scroll, allow, .. }) = self.overlay.as_mut() {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.return_from_child_approval(owner, return_view)
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab if key.modifiers.is_empty() => {
                    *allow = !*allow
                }
                KeyCode::BackTab if key.modifiers == KeyModifiers::SHIFT => *allow = !*allow,
                KeyCode::PageUp if key.modifiers.is_empty() => *scroll = scroll.saturating_sub(8),
                KeyCode::PageDown if key.modifiers.is_empty() => *scroll = scroll.saturating_add(8),
                KeyCode::Home if key.modifiers.is_empty() => *scroll = 0,
                KeyCode::End if key.modifiers.is_empty() => *scroll = usize::MAX,
                KeyCode::Enter if key.modifiers.is_empty() => self.decide_child_approval().await,
                _ => {}
            }
        }
        Ok(true)
    }

    pub(super) async fn handle_child_approval_mouse(
        &mut self,
        event: MouseEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::ChildApproval { owner, .. }) = self.overlay.as_ref() else {
            return Ok(false);
        };
        if *owner != self.selected_session_id {
            return Ok(true);
        }
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if let Some(Overlay::ChildApproval { scroll, .. }) = self.overlay.as_mut() {
                    *scroll =
                        scroll.saturating_add_signed(if event.kind == MouseEventKind::ScrollUp {
                            -3
                        } else {
                            3
                        });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let choice = self
                    .child_approval_paint
                    .as_ref()
                    .filter(|paint| paint.owner == self.selected_session_id)
                    .and_then(|paint| {
                        paint
                            .choices
                            .iter()
                            .position(|area| cell_inside(*area, event.column, event.row))
                    });
                if let Some(choice) = choice {
                    if let Some(Overlay::ChildApproval { allow, .. }) = self.overlay.as_mut() {
                        *allow = choice == 1;
                    }
                    self.decide_child_approval().await;
                }
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) fn render_child_approval(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(Overlay::ChildApproval {
            owner,
            task,
            request,
            scroll,
            allow,
            ..
        }) = self.overlay.as_ref()
        else {
            return;
        };
        let (owner, task, request, mut scroll, allow) =
            (*owner, task.clone(), request.clone(), *scroll, *allow);
        self.child_approval_paint = None;
        if owner != self.selected_session_id {
            return;
        }
        let workspace = task
            .child
            .as_ref()
            .map(|child| child.workspace.display().to_string())
            .unwrap_or_else(|| "unavailable".into());
        let view = ApprovalOverlayState::request_view(&request.payload, workspace);
        let consequence = if request.payload.sandbox_escalation {
            "Allow once retries this invocation outside the sandbox. Existing runtime permission checks still apply."
        } else {
            "Allow once approves this invocation for this child under existing runtime permission checks."
        };
        let text = format!("Parent: {owner}\nAgent #{}: {}\nRequest: {} · Child: {}\n\nExact invocation ({}):\n{}\nCwd: {}\nEnvironment: {}\n\n{}\nReason: {}\n{}\n\n{consequence}\nDon't run refuses this invocation; the child can continue with that refusal. Other work and queued prompts are unaffected.",
            task.id.0, task.label, request.payload.call_id, request.child_session_id, view.tool, view.command, view.cwd,
            view.env_delta,
            super::turn::child_execution_text(&task), view.reason.as_deref().unwrap_or("unavailable"),
            view.failure.map(|failure| format!("Reported failure: {failure}")).unwrap_or_default());
        let Some(choices) = crate::decision::choice_surface(
            area,
            buf,
            "Child decision",
            &text,
            &mut scroll,
            [
                "Don't run",
                if request.payload.sandbox_escalation {
                    "Allow once outside sandbox"
                } else {
                    "Allow once"
                },
            ],
            usize::from(allow),
        ) else {
            return;
        };
        if let Some(Overlay::ChildApproval { scroll: stored, .. }) = self.overlay.as_mut() {
            *stored = scroll;
        }
        self.child_approval_paint = Some(ChildApprovalPaint {
            owner,
            task_id: task.id,
            request,
            choices,
        });
    }
}
