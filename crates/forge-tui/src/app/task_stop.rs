//! Named background interruption, with retained evidence and a safe default.

use super::*;
use crate::selection::cell_inside;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

impl TuiApp {
    fn return_from_task_stop(
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
        self.task_stop_paint = None;
        self.normalize_focus();
    }

    async fn decide_task_stop(&mut self) {
        let Some(Overlay::TaskStop {
            owner,
            task,
            stop,
            return_view,
            ..
        }) = self.overlay.clone()
        else {
            return;
        };
        if !stop {
            self.return_from_task_stop(owner, return_view);
            return;
        }
        if owner != self.selected_session_id
            || !self.task_stop_paint.as_ref().is_some_and(|paint| {
                paint.owner == owner && paint.id == task.id && paint.run_id == task.run_id
            })
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "inspect this parent's named target before stopping",
            );
            return;
        }
        let current = self
            .selected_background_tasks()
            .into_iter()
            .find(|current| current.id == task.id);
        let Some(current) = current.filter(|current| {
            current.run_id == task.run_id && current.child_session_id == task.child_session_id
        }) else {
            self.return_from_task_stop(owner, return_view);
            self.set_feedback(
                FeedbackSeverity::Warn,
                "target changed or was removed · nothing stopped",
            );
            return;
        };
        if current.status.is_terminal() {
            self.return_from_task_stop(owner, return_view);
            self.set_feedback(
                FeedbackSeverity::Info,
                format!(
                    "{} is {} · nothing stopped",
                    current.label,
                    super::turn::background_task_state(&current.status).to_lowercase()
                ),
            );
            return;
        }
        self.return_from_task_stop(owner, return_view);
        if self.selected_is_supervised() {
            self.submit_session_command(forge_session::SupervisorCommand::CancelBackgroundRun {
                session_id: owner,
                task_id: task.id,
                run_id: task.run_id,
            });
        } else if !self
            .session_runtime
            .background_control()
            .cancel_run(task.id, task.run_id)
        {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "task already finished · nothing stopped",
            );
            return;
        }
        self.set_feedback(
            FeedbackSeverity::Warn,
            format!(
                "stop requested for {} · available output is retained",
                task.label
            ),
        );
    }

    pub(super) async fn handle_task_stop_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::TaskStop {
            owner, return_view, ..
        }) = self.overlay.as_ref()
        else {
            return Ok(false);
        };
        let (owner, return_view) = (*owner, *return_view);
        if owner != self.selected_session_id {
            self.return_from_task_stop(owner, return_view);
            self.set_feedback(FeedbackSeverity::Warn, "parent changed · nothing stopped");
            return Ok(true);
        }
        if let Some(Overlay::TaskStop { scroll, stop, .. }) = self.overlay.as_mut() {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.return_from_task_stop(owner, return_view)
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab if key.modifiers.is_empty() => {
                    *stop = !*stop
                }
                KeyCode::BackTab if key.modifiers == KeyModifiers::SHIFT => *stop = !*stop,
                KeyCode::PageUp if key.modifiers.is_empty() => *scroll = scroll.saturating_sub(8),
                KeyCode::PageDown if key.modifiers.is_empty() => *scroll = scroll.saturating_add(8),
                KeyCode::Home if key.modifiers.is_empty() => *scroll = 0,
                KeyCode::End if key.modifiers.is_empty() => *scroll = usize::MAX,
                KeyCode::Enter if key.modifiers.is_empty() => self.decide_task_stop().await,
                _ => {}
            }
        }
        Ok(true)
    }

    pub(super) async fn handle_task_stop_mouse(
        &mut self,
        event: MouseEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::TaskStop { owner, .. }) = self.overlay.as_ref() else {
            return Ok(false);
        };
        if *owner != self.selected_session_id {
            return Ok(true);
        }
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if let Some(Overlay::TaskStop { scroll, .. }) = self.overlay.as_mut() {
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
                    .task_stop_paint
                    .as_ref()
                    .filter(|paint| paint.owner == self.selected_session_id)
                    .and_then(|paint| {
                        paint
                            .choices
                            .iter()
                            .position(|area| cell_inside(*area, event.column, event.row))
                    });
                if let Some(choice) = choice {
                    if let Some(Overlay::TaskStop { stop, .. }) = self.overlay.as_mut() {
                        *stop = choice == 1;
                    }
                    self.decide_task_stop().await;
                }
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) fn render_task_stop(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(Overlay::TaskStop {
            owner,
            task,
            scroll,
            stop,
            ..
        }) = self.overlay.as_ref()
        else {
            return;
        };
        let (owner, task, scroll, stop) = (*owner, task.clone(), *scroll, *stop);
        self.task_stop_paint = None;
        if owner != self.selected_session_id {
            return;
        }
        let current = self
            .selected_background_tasks()
            .into_iter()
            .find(|current| current.id == task.id);
        let status = current
            .as_ref()
            .map(|current| &current.status)
            .unwrap_or(&task.status);
        let waiting = matches!(
            status,
            forge_core::BackgroundTaskStatus::WaitingForApproval { .. }
        );
        let terminal = status.is_terminal();
        let (kind, invocation) = match &task.kind {
            forge_core::BackgroundTaskKind::Shell { command } => (
                "job",
                format!(
                    "Exact command:\n{command}\nLaunch cwd: {}",
                    task.shell
                        .as_ref()
                        .map(|shell| shell.cwd.display().to_string())
                        .unwrap_or_else(|| "unavailable".into())
                ),
            ),
            forge_core::BackgroundTaskKind::Subagent { .. } => {
                ("agent", super::turn::child_execution_text(&task))
            }
        };
        let text = format!("Parent: {owner}\n{kind} #{}: {}\nState: {}\n\n{invocation}\n\nStopping interrupts this {kind}. Available output and partial findings are retained.\nOther work and queued prompts are unaffected. No checkout is removed.", task.id.0, task.label, super::turn::background_task_state(status));
        let labels = [
            if terminal {
                "Return"
            } else if waiting {
                "Keep waiting"
            } else {
                "Keep running"
            }
            .to_string(),
            format!("Stop {kind}"),
        ];
        let mut scroll = scroll;
        let Some(choices) = crate::decision::choice_surface(
            area,
            buf,
            &format!("Stop {kind}"),
            &text,
            &mut scroll,
            [&labels[0], &labels[1]],
            usize::from(stop),
        ) else {
            return;
        };
        if let Some(Overlay::TaskStop { scroll: stored, .. }) = self.overlay.as_mut() {
            *stored = scroll;
        }
        self.task_stop_paint = Some(TaskStopPaint {
            owner,
            id: task.id,
            run_id: task.run_id,
            choices,
        });
    }
}
