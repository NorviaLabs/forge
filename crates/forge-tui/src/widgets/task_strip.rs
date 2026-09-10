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
        let title = if self.focused {
            " ● TASKS "
        } else {
            " TASKS "
        };
        let title_style = if self.focused {
            theme::active_panel_border().add_modifier(Modifier::BOLD)
        } else {
            theme::metadata_style()
        };
        let mut spans = vec![Span::styled(title, title_style)];
        let mut used = title.chars().count();
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn strip_renders_session_names_and_overflow() {
        let items = vec![TaskStripItem {
            slot: Some(1),
            label: "parser-fix".into(),
            branch: "forge/parser-fix-1".into(),
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
        assert!(!text.contains("forge/parser-fix-1"));
        assert!(!text.contains("M"));
        assert!(text.contains("+3 more"));
    }

    #[test]
    fn selecting_a_tab_does_not_reorder_the_strip() {
        let items = vec![
            TaskStripItem {
                slot: Some(1),
                label: "alpha".into(),
                branch: String::new(),
                state: TaskStripState::Idle,
                secondary: None,
                selected: false,
                focused: false,
                attention: false,
            },
            TaskStripItem {
                slot: Some(2),
                label: "beta".into(),
                branch: String::new(),
                state: TaskStripState::Idle,
                secondary: None,
                selected: true,
                focused: false,
                attention: false,
            },
        ];
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TaskStrip {
                        items: &items,
                        overflow: 0,
                        focused: false,
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
        let alpha = text.find("alpha").expect("alpha rendered");
        let beta = text.find("beta").expect("beta rendered");
        assert!(alpha < beta, "active tab moved: {text:?}");
    }

    #[test]
    fn active_tab_uses_the_theme_accent() {
        let items = vec![TaskStripItem {
            slot: Some(1),
            label: "alpha".into(),
            branch: String::new(),
            state: TaskStripState::Idle,
            secondary: None,
            selected: true,
            focused: false,
            attention: false,
        }];
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TaskStrip {
                        items: &items,
                        overflow: 0,
                        focused: false,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let accent = crate::theme::accent_color();
        let highlighted = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| "alpha".contains(cell.symbol()) && cell.style().fg == Some(accent));
        assert!(highlighted, "active tab should carry the accent colour");
    }

    #[test]
    fn narrow_strip_truncates_and_reports_hidden_tasks() {
        // Order is stable: a focused tab past the visible width is counted in
        // the overflow rather than hoisted to the front, so keep the focused
        // tab first to exercise the elision path itself.
        let items = (0..4)
            .map(|index| TaskStripItem {
                slot: Some(index + 1),
                label: format!("task-with-a-very-long-name-{index}"),
                branch: "feature/long-branch-name".into(),
                state: TaskStripState::Running,
                secondary: None,
                selected: index == 0,
                focused: index == 0,
                attention: false,
            })
            .collect::<Vec<_>>();
        let backend = TestBackend::new(36, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TaskStrip {
                        items: &items,
                        overflow: 0,
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
        assert!(text.contains("…"), "long task should be elided: {text:?}");
        assert!(
            text.contains("+"),
            "hidden task count should be shown: {text:?}"
        );
    }
}
