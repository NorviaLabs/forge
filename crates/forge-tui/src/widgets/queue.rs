//! Outbound message queue strip — click a row to cancel that item.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone)]
pub struct QueueModel {
    pub items: Vec<String>,
    /// 0-based row under mouse (optional highlight).
    pub hover: Option<usize>,
}

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
                " queue · click a message to cancel ",
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
                let preview: String = t.chars().take(inner.width.saturating_sub(6) as usize).collect();
                let ellipsis = if t.chars().count() > preview.chars().count() {
                    "…"
                } else {
                    ""
                };
                let text = format!(" {}. {}{}", i + 1, preview, ellipsis);
                let style = if self.model.hover == Some(i) {
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

/// Map a click inside `queue_area` to a 0-based item index, if any.
pub fn hit_test_queue_row(queue_area: Rect, col: u16, row: u16, item_count: usize) -> Option<usize> {
    if item_count == 0 || queue_area.height < 2 {
        return None;
    }
    // Inner content: skip border (1 cell each side/top/bottom when borders present).
    if col < queue_area.x.saturating_add(1)
        || col >= queue_area.x.saturating_add(queue_area.width.saturating_sub(1))
    {
        return None;
    }
    if row < queue_area.y.saturating_add(1)
        || row >= queue_area.y.saturating_add(queue_area.height.saturating_sub(1))
    {
        return None;
    }
    let inner_y = row.saturating_sub(queue_area.y.saturating_add(1));
    let idx = inner_y as usize;
    if idx < item_count {
        Some(idx)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_first_row() {
        let area = Rect::new(0, 10, 40, 5); // borders → inner y 11..13
        assert_eq!(hit_test_queue_row(area, 5, 11, 3), Some(0));
        assert_eq!(hit_test_queue_row(area, 5, 12, 3), Some(1));
        assert_eq!(hit_test_queue_row(area, 5, 10, 3), None); // border
    }
}
