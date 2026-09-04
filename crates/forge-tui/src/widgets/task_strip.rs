use crate::status_glyph::{status_indicator_now, Status};
use crate::theme;
use forge_types::TaskLifecycle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStripState {
    Idle,
    Running,
    Waiting,
    Completed,
    Failed,
    Interrupted,
    Unavailable,
}

impl From<TaskLifecycle> for TaskStripState {
    fn from(value: TaskLifecycle) -> Self {
        match value {
            TaskLifecycle::Ready => Self::Idle,
            TaskLifecycle::Working => Self::Running,
            TaskLifecycle::Waiting => Self::Waiting,
            TaskLifecycle::Completed => Self::Completed,
            TaskLifecycle::Failed => Self::Failed,
            TaskLifecycle::Cancelled => Self::Idle,
            TaskLifecycle::Interrupted => Self::Interrupted,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStripItem {
    pub slot: Option<u8>,
    pub label: String,
    pub state: TaskStripState,
    pub secondary: Option<String>,
    pub selected: bool,
    pub focused: bool,
    pub attention: bool,
}

pub struct TaskStrip<'a> {
    pub items: &'a [TaskStripItem],
    pub overflow: usize,
    pub focused: bool,
}

impl<'a> TaskStrip<'a> {
    fn state_status(state: TaskStripState) -> Status {
        match state {
            TaskStripState::Idle => Status::Info,
            TaskStripState::Running => Status::Info,
            TaskStripState::Waiting => Status::Warning,
            TaskStripState::Completed => Status::Success,
            TaskStripState::Failed | TaskStripState::Interrupted | TaskStripState::Unavailable => {
                Status::Error
            }
        }
    }
}

impl Widget for TaskStrip<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut spans = vec![Span::styled(" Tasks ", theme::metadata_style())];
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(" · ", theme::border_muted()));
            }
            let status = Self::state_status(item.state);
            let indicator = status_indicator_now(status);
            let base = if let Some(slot) = item.slot {
                format!("{slot} {}", item.label)
            } else {
                item.label.clone()
            };
            let style = if item.focused && self.focused {
                theme::focused_selection_style()
            } else if item.selected {
                theme::text().add_modifier(Modifier::BOLD)
            } else {
                theme::metadata_style()
            };
            let mut item_spans = vec![
                Span::styled("[", theme::border_muted()),
                indicator,
                Span::styled(format!(" {base}"), style),
            ];
            if let Some(secondary) = &item.secondary {
                item_spans.push(Span::styled(
                    format!(" {secondary}"),
                    theme::metadata_style(),
                ));
            }
            if item.attention {
                item_spans.push(Span::styled(" !", theme::warn()));
            }
            item_spans.push(Span::styled("]", theme::border_muted()));
            spans.extend(item_spans);
        }
        if self.overflow > 0 {
            if !self.items.is_empty() {
                spans.push(Span::styled(" · ", theme::border_muted()));
            }
            spans.push(Span::styled(
                format!("+{} more", self.overflow),
                theme::brand(),
            ));
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn lifecycle_states_map_to_non_colliding_semantic_statuses() {
        assert_eq!(
            TaskStrip::<'static>::state_status(TaskStripState::Completed),
            Status::Success
        );
        assert_eq!(
            TaskStrip::<'static>::state_status(TaskStripState::Waiting),
            Status::Warning
        );
        assert_eq!(
            TaskStrip::<'static>::state_status(TaskStripState::Failed),
            Status::Error
        );
    }

    #[test]
    fn strip_renders_slots_labels_and_overflow() {
        let items = vec![TaskStripItem {
            slot: Some(1),
            label: "parser-fix".into(),
            state: TaskStripState::Running,
            secondary: Some("M".into()),
            selected: true,
            focused: true,
            attention: false,
        }];
        let backend = TestBackend::new(80, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TaskStrip {
                        items: &items,
                        overflow: 3,
                        focused: true,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains("parser-fix"));
        assert!(text.contains("+3 more"));
    }
}
