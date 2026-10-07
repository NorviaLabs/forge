//! Live owner-scoped Jobs / Agents / Queue inspection.

use super::*;
use crate::decision::{literal_lines, preview};
use crate::selection::cell_inside;
use crate::tasks_strip::{ordered_tasks, row_for, StripState, TaskFilter};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, Padding};

impl TuiApp {
    pub(super) fn open_tasks_view(&mut self, filter: Option<TaskFilter>) {
        let filter = filter.unwrap_or_else(|| {
            let tasks = self.selected_background_tasks();
            let selected = self.task_selection.task(self.selected_session_id);
            if tasks.iter().any(|task| {
                Some(task.id) == selected
                    && matches!(task.kind, forge_core::BackgroundTaskKind::Subagent { .. })
            }) {
                TaskFilter::Agents
            } else if tasks
                .iter()
                .any(|task| matches!(task.kind, forge_core::BackgroundTaskKind::Shell { .. }))
            {
                TaskFilter::Jobs
            } else if !tasks.is_empty() {
                TaskFilter::Agents
            } else if !self.selected_queue_items().is_empty() {
                TaskFilter::Queue
            } else {
                TaskFilter::Jobs
            }
        });
        self.overlay = Some(Overlay::Tasks {
            owner: self.selected_session_id,
            filter,
            scroll: 0,
        });
        self.select_task_filter(filter);
    }

    fn filtered_tasks(&self, filter: TaskFilter) -> Vec<forge_session::BackgroundTaskSnapshot> {
        let tasks = self.selected_background_tasks();
        ordered_tasks(&tasks)
            .into_iter()
            .filter(|(_, _, task)| match filter {
                TaskFilter::Jobs => {
                    matches!(task.kind, forge_core::BackgroundTaskKind::Shell { .. })
                }
                TaskFilter::Agents => {
                    matches!(task.kind, forge_core::BackgroundTaskKind::Subagent { .. })
                }
                TaskFilter::Queue => false,
            })
            .map(|(_, _, task)| task.clone())
            .collect()
    }

    fn select_task_filter(&mut self, filter: TaskFilter) {
        if filter == TaskFilter::Queue {
            self.clamp_queue_selection();
        } else {
            let tasks = self.filtered_tasks(filter);
            let selected = self.task_selection.task(self.selected_session_id);
            if !tasks.iter().any(|task| Some(task.id) == selected) {
                if let Some(task) = tasks.first() {
                    self.task_selection
                        .select_task(self.selected_session_id, task.id);
                } else {
                    self.task_selection.clear_tasks();
                }
            }
        }
    }

    fn move_filtered_task(&mut self, filter: TaskFilter, delta: i32) {
        let tasks = self.filtered_tasks(filter);
        if tasks.is_empty() {
            return;
        }
        let selected = self.task_selection.task(self.selected_session_id);
        let index = tasks
            .iter()
            .position(|task| Some(task.id) == selected)
            .unwrap_or(0) as i64;
        let next = (index + i64::from(delta)).rem_euclid(tasks.len() as i64) as usize;
        self.task_selection
            .select_task(self.selected_session_id, tasks[next].id);
    }

    fn scroll_task_details(&mut self, delta: isize) {
        if let Some(Overlay::Tasks { scroll, .. }) = self.overlay.as_mut() {
            *scroll = scroll.saturating_add_signed(delta);
        }
    }

    pub(super) async fn handle_tasks_view_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::Tasks { owner, filter, .. }) = self.overlay else {
            return Ok(false);
        };
        if owner != self.selected_session_id {
            self.dismiss_overlay();
            self.set_feedback(
                FeedbackSeverity::Warn,
                "task view owner changed; reopen /tasks",
            );
            return Ok(true);
        }
        let plain = key.modifiers.is_empty();
        match key.code {
            KeyCode::Esc if plain => self.dismiss_overlay(),
            KeyCode::Tab if plain => self.open_tasks_view(Some(filter.next(false))),
            KeyCode::BackTab => self.open_tasks_view(Some(filter.next(true))),
            KeyCode::PageUp if plain => self.scroll_task_details(-8),
            KeyCode::PageDown if plain => self.scroll_task_details(8),
            KeyCode::Home | KeyCode::End if plain => {
                if let Some(Overlay::Tasks { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = if key.code == KeyCode::Home {
                        0
                    } else {
                        usize::MAX
                    };
                }
            }
            KeyCode::Up | KeyCode::Down
                if filter == TaskFilter::Queue && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_queue_selection(if key.code == KeyCode::Up { -1 } else { 1 });
                if let Some(Overlay::Tasks { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = 0;
                }
            }
            KeyCode::Backspace
                if filter == TaskFilter::Queue && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.cancel_selected_queue().await
            }
            KeyCode::Up if filter == TaskFilter::Queue && plain => {
                if self.input.text.is_empty() {
                    self.edit_last_queued_message().await;
                    self.dismiss_overlay();
                } else {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        "finish or clear your draft before editing queued text",
                    );
                }
            }
            KeyCode::Up | KeyCode::Down if filter != TaskFilter::Queue && plain => {
                self.move_filtered_task(filter, if key.code == KeyCode::Up { -1 } else { 1 });
                if let Some(Overlay::Tasks { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = 0;
                }
            }
            KeyCode::Right | KeyCode::Enter if filter == TaskFilter::Agents && plain => {
                self.dismiss_overlay();
                self.open_selected_child_session().await;
            }
            KeyCode::Right if filter == TaskFilter::Jobs && plain => {
                self.set_feedback(
                    FeedbackSeverity::Info,
                    "shell job has no child session · i inserts its available result",
                );
            }
            KeyCode::Char('i') if filter != TaskFilter::Queue && plain => {
                self.close_child_session();
                self.dismiss_overlay();
                self.attach_selected_task().await;
            }
            KeyCode::Char('a') | KeyCode::Char('d') if filter == TaskFilter::Agents && plain => {
                self.open_selected_child_decision();
            }
            KeyCode::Char('x') if filter != TaskFilter::Queue && plain => {
                self.cancel_selected_task().await
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) async fn handle_tasks_view_mouse(
        &mut self,
        event: MouseEvent,
    ) -> Result<bool, TuiError> {
        let Some(Overlay::Tasks { owner, filter, .. }) = self.overlay else {
            return Ok(false);
        };
        if owner != self.selected_session_id
            || self.task_view_paint.owner != owner
            || self.task_view_paint.filter != filter
        {
            return Ok(true);
        }
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_task_details(-3),
            MouseEventKind::ScrollDown => self.scroll_task_details(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((tab, _)) = self
                    .task_view_paint
                    .tabs
                    .iter()
                    .find(|(_, area)| cell_inside(*area, event.column, event.row))
                {
                    self.open_tasks_view(Some(*tab));
                } else if let Some((target, _)) = self
                    .task_view_paint
                    .rows
                    .iter()
                    .find(|(_, area)| cell_inside(*area, event.column, event.row))
                {
                    match *target {
                        TaskViewTarget::Prompt(id) => self.task_selection.select_queue(owner, id),
                        TaskViewTarget::Task(id) => self.task_selection.select_task(owner, id),
                    }
                    if let Some(Overlay::Tasks { scroll, .. }) = self.overlay.as_mut() {
                        *scroll = 0;
                    }
                }
            }
            _ => {}
        }
        Ok(true)
    }

    pub(super) fn render_tasks_view(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(Overlay::Tasks {
            owner,
            filter,
            scroll,
        }) = self.overlay
        else {
            return;
        };
        if owner != self.selected_session_id {
            return;
        }
        self.select_task_filter(filter);
        let all_tasks = self.selected_background_tasks();
        let queue = self.selected_queue_items();
        let tasks = self.filtered_tasks(filter);
        let count = if filter == TaskFilter::Queue {
            queue.len()
        } else {
            tasks.len()
        };
        let selected = if filter == TaskFilter::Queue {
            let id = self.task_selection.queue(owner);
            queue.iter().position(|item| Some(item.id) == id)
        } else {
            let id = self.task_selection.task(owner);
            tasks.iter().position(|task| Some(task.id) == id)
        };
        let details = if let Some(index) = selected {
            if filter == TaskFilter::Queue {
                let id = match queue[index].id {
                    forge_session::QueuedPromptId::Durable(id) => format!("Prompt #{id}"),
                    forge_session::QueuedPromptId::Session(id) => {
                        format!("Queued message #{}", id.0)
                    }
                };
                format!(
                    "{id} · {} of {count}\n{}\n\nParent: {owner}\nFIFO · starts at a turn boundary",
                    index + 1,
                    queue[index].text
                )
            } else {
                let task = &tasks[index];
                match &task.kind {
                    forge_core::BackgroundTaskKind::Shell { .. } => format!(
                        "{} of {count} · Parent: {owner}\n{}",
                        index + 1,
                        super::turn::shell_execution_text(task),
                    ),
                    forge_core::BackgroundTaskKind::Subagent { .. } => format!(
                        "{} of {count} · Parent: {owner}\n{}",
                        index + 1,
                        super::turn::child_execution_text(task)
                    ),
                }
            }
        } else {
            format!(
                "No {} for this parent session.",
                filter.label().to_lowercase()
            )
        };
        let width = crate::overlays::centered_content_rect(area, 96, 7, 32)
            .width
            .saturating_sub(2 + 2 * crate::design::MODAL_PAD_X);
        let detail_lines = literal_lines(&details, width, theme::text());
        let height =
            6 + count.min(6) as u16 + u16::from(count > 0) + detail_lines.len().min(14) as u16;
        let r = crate::overlays::centered_content_rect(area, 96, height, 32);
        Clear.render(r, buf);
        theme::fill(r, buf, theme::panel());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .style(theme::panel())
            .padding(Padding::horizontal(crate::design::MODAL_PAD_X))
            .title(theme::modal_title("Tasks"));
        let inner = block.inner(r);
        block.render(r, buf);
        if inner.height < 5 {
            return;
        }
        self.task_view_paint = TaskViewPaintState {
            owner,
            filter,
            ..Default::default()
        };
        let counts = [
            all_tasks
                .iter()
                .filter(|task| matches!(task.kind, forge_core::BackgroundTaskKind::Shell { .. }))
                .count(),
            all_tasks
                .iter()
                .filter(|task| matches!(task.kind, forge_core::BackgroundTaskKind::Subagent { .. }))
                .count(),
            queue.len(),
        ];
        let mut x = inner.x;
        for (tab, count) in [TaskFilter::Jobs, TaskFilter::Agents, TaskFilter::Queue]
            .into_iter()
            .zip(counts)
        {
            let label = format!(
                "{}{} {count}  ",
                if tab == filter { "> " } else { "  " },
                tab.label()
            );
            let width = (Span::raw(&label).width() as u16).min(inner.right().saturating_sub(x));
            buf.set_line(
                x,
                inner.y,
                &Line::styled(
                    label,
                    if tab == filter {
                        theme::accent_style().add_modifier(Modifier::BOLD)
                    } else {
                        theme::metadata_style()
                    },
                ),
                width,
            );
            self.task_view_paint
                .tabs
                .push((tab, Rect::new(x, inner.y, width, 1)));
            x = x.saturating_add(width);
        }
        let label = self
            .session_chrome
            .iter()
            .find(|session| session.session_id == owner)
            .map(|session| session.label.as_str())
            .unwrap_or("parent session");
        buf.set_line(
            inner.x,
            inner.y + 1,
            &Line::styled(
                preview(&format!("Owner: {label}"), inner.width as usize),
                theme::metadata_style(),
            ),
            inner.width,
        );
        let body = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 4);
        let row_cap = if area.height < 28 { 2 } else { 6 };
        let visible = count.min(row_cap).min((body.height / 2).max(1) as usize);
        let start = selected
            .unwrap_or(0)
            .saturating_sub(visible.saturating_sub(1))
            .min(count.saturating_sub(visible));
        for offset in 0..visible {
            let index = start + offset;
            let (target, text) = if filter == TaskFilter::Queue {
                (
                    TaskViewTarget::Prompt(queue[index].id),
                    format!("{}. {}", index + 1, queue[index].text),
                )
            } else {
                let row = row_for(
                    &tasks[index],
                    chrono::Utc::now(),
                    StripState::of(&tasks[index].status),
                );
                (
                    TaskViewTarget::Task(row.id),
                    format!(
                        "{} {} {} · {}",
                        row.marker(),
                        row.glyph(),
                        row.label,
                        row.elapsed
                    ),
                )
            };
            let rect = Rect::new(body.x, body.y + offset as u16, body.width, 1);
            let style = if selected == Some(index) {
                theme::focused_selection_style()
            } else {
                theme::metadata_style()
            };
            if selected == Some(index) {
                theme::fill(rect, buf, style);
            }
            let line = Line::styled(
                preview(
                    &format!(
                        "{}{}",
                        if selected == Some(index) { "> " } else { "  " },
                        text
                    ),
                    body.width as usize,
                ),
                style,
            );
            buf.set_line(rect.x, rect.y, &line, rect.width);
            self.task_view_paint.rows.push((target, rect));
        }
        let details_area = Rect::new(
            body.x,
            body.y + visible as u16 + u16::from(visible > 0),
            body.width,
            body.height
                .saturating_sub(visible as u16 + u16::from(visible > 0)),
        );
        let scroll = scroll.min(
            detail_lines
                .len()
                .saturating_sub(details_area.height as usize),
        );
        if let Some(Overlay::Tasks { scroll: stored, .. }) = self.overlay.as_mut() {
            *stored = scroll;
        }
        Paragraph::new(
            detail_lines
                .into_iter()
                .skip(scroll)
                .take(details_area.height as usize)
                .collect::<Vec<_>>(),
        )
        .render(details_area, buf);
        let hints = if count == 0 {
            ["Tab filter · Shift+Tab previous filter", "Esc return"]
        } else if filter == TaskFilter::Queue {
            [
                "Tab filter · Ctrl+↑/↓ select · Ctrl+Backspace cancel",
                "↑ edit last · PgUp/PgDn/Home/End details · Esc return",
            ]
        } else if filter == TaskFilter::Jobs {
            [
                "Tab filter · ↑↓ select · i insert · x stop/dismiss",
                "PgUp/PgDn/Home/End details · Esc return",
            ]
        } else {
            [
                "Tab filter · ↑↓ select · → child · i insert · x stop/dismiss",
                "a/d decision · PgUp/PgDn/Home/End details · Esc return",
            ]
        };
        for (offset, hint) in hints.into_iter().enumerate() {
            buf.set_line(
                inner.x,
                inner.bottom() - 2 + offset as u16,
                &Line::styled(hint, theme::metadata_style()),
                inner.width,
            );
        }
    }
}
