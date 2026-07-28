//! Outbound message queue pane — read-only list with keyboard selection.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct QueueModel {
    pub items: Vec<String>,
    /// 0-based selected row.
    pub selected: Option<usize>,
}

#[allow(dead_code)]
pub struct QueueBar<'a> {
    pub model: &'a QueueModel,
}

impl Widget for QueueBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::warn())
            .title(Span::styled(
                " queue · read-only · ctrl+up/down select · ctrl+backspace cancel ",
                theme::warn().add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }

        let max_rows = inner.height as usize;
        let lines: Vec<Line> = self
            .model
            .items
            .iter()
            .enumerate()
            .take(max_rows)
            .map(|(i, t)| {
                let preview: String = t
                    .chars()
                    .take(inner.width.saturating_sub(6) as usize)
                    .collect();
                let ellipsis = if t.chars().count() > preview.chars().count() {
                    "…"
                } else {
                    ""
                };
                let text = format!(" {}. {}{}", i + 1, preview, ellipsis);
                let style = if self.model.selected == Some(i) {
                    theme::selected_row()
                } else {
                    theme::text()
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        Paragraph::new(lines).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(model: &QueueModel, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        QueueBar { model }.render(area, &mut buffer);
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn renders_rows_selection_and_truncation() {
        let model = QueueModel {
            items: vec!["first message".into(), "a very long second message".into()],
            selected: Some(1),
        };
        let text = rendered(&model, 18, 4);
        assert!(text.contains("1. first mess…"));
        assert!(text.contains("2. a very lon…"));
        assert!(text.contains("queue"));
    }

    #[test]
    fn zero_sized_and_border_only_areas_are_safe() {
        let model = QueueModel {
            items: vec!["message".into()],
            selected: None,
        };
        assert_eq!(rendered(&model, 0, 0), "");
        let text = rendered(&model, 10, 2);
        assert!(!text.contains("1. message"));
    }

    #[test]
    fn limits_rows_to_available_height() {
        let model = QueueModel {
            items: vec!["one".into(), "two".into(), "three".into()],
            selected: None,
        };
        let text = rendered(&model, 20, 3);
        assert!(text.contains("1. one"));
        assert!(!text.contains("2. two"));
    }
}
