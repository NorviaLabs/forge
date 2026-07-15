//! Outbound message queue pane — read-only list with keyboard selection.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone)]
pub struct QueueModel {
    pub items: Vec<String>,
    /// 0-based selected row.
    pub selected: Option<usize>,
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
                let preview: String = t.chars().take(inner.width.saturating_sub(6) as usize).collect();
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
