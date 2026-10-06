use crate::theme;
use forge_types::TaskLifecycle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
    pub branch: String,
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

impl Widget for TaskStrip<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut used = 0;
        let mut spans = Vec::new();
        let mut hidden = self.overflow;
        for (position, item) in self.items.iter().enumerate() {
            if position > 0 {
                let separator = " · ";
                if used + separator.len() >= area.width as usize {
                    hidden += self.items.len().saturating_sub(position);
                    break;
                }
                spans.push(Span::styled(separator, theme::border_muted()));
                used += separator.len();
            }
            let style = if item.focused && self.focused {
                theme::focused_selection_style()
            } else if item.selected {
                // The active session keeps the accent even when the strip is
                // unfocused or the cursor rests on a sibling, so the user can
                // always see which tab they are viewing.
                theme::brand()
            } else {
                theme::metadata_style()
            };
            let mut item_spans = vec![
                Span::styled("[", theme::border_muted()),
                Span::styled(format!(" {}", item.label), style),
            ];
            item_spans.push(Span::styled("]", theme::border_muted()));
            let item_width = item_spans.iter().map(Span::width).sum::<usize>();
            let remaining = (area.width as usize).saturating_sub(used);
            if item_width > remaining {
                // Keep a focused item discoverable even when neighboring
                // tasks consume the strip; its label is truncated by cell
                // width rather than clipped by the terminal buffer.
                if !(item.focused && self.focused) {
                    hidden += 1;
                    hidden += self.items.len().saturating_sub(position + 1);
                    break;
                }
                let text = format!("[ {} ]", item.label);
                let reserve = if self.items.len() > 1 && remaining > 10 {
                    10
                } else {
                    0
                };
                let shown = truncate(&text, remaining.saturating_sub(reserve));
                spans.push(Span::styled(shown, style));
                hidden += self.items.len().saturating_sub(1);
                break;
            }
            used += item_width;
            spans.extend(item_spans);
        }
        if hidden > 0 {
            if !self.items.is_empty() {
                spans.push(Span::styled(" · ", theme::border_muted()));
            }
            spans.push(Span::styled(format!("+{} more", hidden), theme::brand()));
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    format!("{}…", text.chars().take(width - 1).collect::<String>())
}
