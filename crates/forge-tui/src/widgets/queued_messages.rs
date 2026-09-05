use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

const MAX_DISPLAYED_MESSAGES: usize = 3;

pub struct QueuedMessages<'a> {
    pub messages: &'a [String],
    pub selected: Option<usize>,
}

impl Widget for QueuedMessages<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.messages.is_empty() {
            return;
        }

        let visible = self.messages.len().min(MAX_DISPLAYED_MESSAGES);
        let start = self
            .selected
            .unwrap_or(0)
            .saturating_sub(visible.saturating_sub(1))
            .min(self.messages.len().saturating_sub(visible));
        let title = Line::from(vec![
            Span::styled("Queued", theme::metadata_style()),
            Span::styled(" · ↑ edit last", theme::dim()),
        ]);
        buf.set_line(area.x, area.y, &title, area.width);

        for (offset, message) in self.messages.iter().skip(start).take(visible).enumerate() {
            let index = start + offset;
            let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
            let prefix = format!("  {}. ", index + 1);
            let available = area.width.saturating_sub(prefix.chars().count() as u16) as usize;
            let preview = truncate(&normalized, available);
            let style = if self.selected == Some(index) {
                theme::focused_selection_style()
            } else {
                theme::metadata_style()
            };
            let line = Line::from(vec![
                Span::styled(prefix, theme::dim()),
                Span::styled(preview, style),
            ]);
            let row = area.y.saturating_add(1 + offset as u16);
            if row < area.bottom() {
                buf.set_line(area.x, row, &line, area.width);
            }
        }

        if self.messages.len() > MAX_DISPLAYED_MESSAGES {
            let row = area.y.saturating_add(1 + visible as u16);
            if row < area.bottom() {
                let overflow = format!(
                    "  … ({}–{} of {} · {} hidden)",
                    start + 1,
                    start + visible,
                    self.messages.len(),
                    self.messages.len() - visible
                );
                buf.set_line(
                    area.x,
                    row,
                    &Line::from(Span::styled(overflow, theme::dim())),
                    area.width,
                );
            }
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    text.chars()
        .take(width - 1)
        .collect::<String>()
        .trim_end()
        .to_string()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(messages: &[String], selected: Option<usize>, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(QueuedMessages { messages, selected }, frame.area());
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_bounded_normalized_queue_with_overflow() {
        let messages = vec![
            "first   message".into(),
            "second\tmessage".into(),
            "third\nmessage".into(),
            "fourth message".into(),
        ];
        let output = render(&messages, Some(1), 40, 5);
        assert!(output.contains("Queued · ↑ edit last"));
        assert!(output.contains("first message"));
        assert!(output.contains("second message"));
        assert!(output.contains("third message"));
        assert!(output.contains("1–3 of 4 · 1 hidden"));
        assert!(!output.contains("fourth message"));
    }

    #[test]
    fn truncates_previews_to_the_available_width() {
        let output = render(&["a very long queued message".into()], None, 18, 2);
        assert!(output.contains("a very long…"));
    }

    #[test]
    fn selected_message_is_kept_inside_the_visible_window() {
        let messages = (0..5).map(|i| format!("message {i}")).collect::<Vec<_>>();
        let output = render(&messages, Some(4), 40, 5);
        assert!(output.contains("message 4"));
        assert!(output.contains("3–5 of 5"));
        assert!(!output.contains("message 0"));
    }
}
