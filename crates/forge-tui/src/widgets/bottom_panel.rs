use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::panel;
use crate::widgets::BusyPhase;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BottomPanelState {
    pub open: bool,
    pub focused: bool,
}

pub struct BottomPanelModel<'a> {
    pub state: &'a BottomPanelState,
    pub busy_phase: &'a BusyPhase,
    pub activity: &'a ActivityFeed,
    pub terminal_content: &'a str,
    pub terminal_running: bool,
    pub terminal_shell: Option<&'a str>,
    pub terminal_cursor: Option<(u16, u16)>,
}

pub struct BottomPanel<'a> {
    pub model: BottomPanelModel<'a>,
    pub focused: bool,
}

impl Widget for BottomPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || !self.model.state.open {
            return;
        }
        // Focus has to be legible without color: the panel is a single top
        // rule, so an active/inactive *style* swap alone is invisible in
        // low-color terminals and easy to miss even in full color. Match the
        // composer's structural cue — a thick border when focused — so
        // "where do my keystrokes go" is answerable from shape, and mark the
        // title with the shared `>` grammar, since the rule is only one
        // cell tall. Callers pass modal-suppressed focus (DESIGN-004).
        let block = Block::default()
            .borders(Borders::TOP)
            .border_type(if self.focused {
                BorderType::Thick
            } else {
                BorderType::Plain
            })
            .border_style(if self.focused {
                theme::active_panel_border()
            } else {
                theme::inactive_panel_border()
            })
            .style(theme::panel())
            .title(panel::title(self.focused, false, "Terminal"));
        let inner = block.inner(area);
        block.render(area, buf);
        let lines = terminal_lines(
            self.model.busy_phase,
            self.model.activity,
            self.model.terminal_content,
            self.model.terminal_running,
            self.model.terminal_shell,
        );
        let scroll = lines.len().saturating_sub(inner.height as usize) as u16;
        Paragraph::new(lines).scroll((scroll, 0)).render(inner, buf);
        if self.focused {
            if let Some((cursor_x, cursor_y)) = self.model.terminal_cursor {
                let header_lines = 1 + usize::from(self.model.terminal_shell.is_some());
                let content_lines = self.model.terminal_content.lines().count();
                let content_skip = content_lines.saturating_sub(20);
                let Some(content_row) = (cursor_y as usize).checked_sub(content_skip) else {
                    return;
                };
                let rendered_y = inner
                    .y
                    .saturating_add(header_lines as u16)
                    .saturating_add(content_row as u16)
                    .saturating_sub(scroll);
                let rendered_x = inner.x.saturating_add(cursor_x);
                if rendered_x < inner.right() && rendered_y < inner.bottom() {
                    theme::paint_caret(buf, rendered_x, rendered_y);
                }
            }
        }
    }
}

fn terminal_lines<'a>(
    busy_phase: &'a BusyPhase,
    activity: &'a ActivityFeed,
    terminal_content: &'a str,
    terminal_running: bool,
    terminal_shell: Option<&'a str>,
) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Interactive shell", theme::text()),
        Span::styled(
            if terminal_running {
                " · running"
            } else {
                " · exited"
            },
            theme::muted(),
        ),
    ])];
    if let Some(shell) = terminal_shell {
        lines.push(Line::styled(format!("$ {shell} -il"), theme::muted()));
    }

    if !terminal_content.is_empty() {
        let content = terminal_content.lines().collect::<Vec<_>>();
        for line in content.iter().rev().take(20).rev() {
            lines.push(Line::styled((*line).to_string(), theme::muted()));
        }
        return lines;
    }
    if let BusyPhase::Tool { name } = busy_phase {
        lines.push(Line::from(vec![
            Span::styled("running ", theme::muted()),
            Span::styled(name.as_str(), theme::tool_running_style()),
        ]));
    }
    let tool_items = activity
        .all()
        .iter()
        .rev()
        .filter(|item| item.kind == crate::activity::ActivityKind::Tool)
        .take(5)
        .collect::<Vec<_>>();
    if tool_items.is_empty() {
        lines.push(Line::styled("No command output yet", theme::muted()));
    } else {
        for item in tool_items.into_iter().rev() {
            lines.push(Line::from(vec![
                Span::styled("tool ", theme::tool()),
                Span::styled(item.summary.as_str(), theme::text()),
            ]));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn rendered_text(model: BottomPanelModel<'_>, focused: bool) -> String {
        let area = Rect::new(0, 0, 80, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(BottomPanel { model, focused }, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rendered_buffer(model: BottomPanelModel<'_>, focused: bool) -> Buffer {
        let area = Rect::new(0, 0, 30, 8);
        let mut buffer = Buffer::empty(area);
        BottomPanel { model, focused }.render(area, &mut buffer);
        buffer
    }

    #[test]
    fn default_panel_is_closed() {
        let state = BottomPanelState::default();
        assert!(!state.open);
        assert!(!state.focused);
    }

    #[test]
    fn renders_terminal_title_without_bottom_label_or_shortcut_manual() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: true,
        };
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            terminal_content: "",
            terminal_running: false,
            terminal_shell: None,
            terminal_cursor: None,
        };

        let rendered = rendered_text(model, true);
        assert!(rendered.contains("Terminal"));
        assert!(!rendered.contains("BOTTOM"));
        assert!(!rendered.contains("Ctrl+P close"));
    }

    #[test]
    fn terminal_focus_is_legible_without_color() {
        // Regression: focus was signalled only by swapping the border *style*
        // on a one-cell top rule, so a focused terminal was visually identical
        // to an unfocused one and the only way to find out where keystrokes
        // were going was to type and see what happened.
        let activity = ActivityFeed::default();
        let render = |focused: bool| {
            let state = BottomPanelState {
                open: true,
                focused,
            };
            rendered_text(
                BottomPanelModel {
                    state: &state,
                    busy_phase: &BusyPhase::Idle,
                    activity: &activity,
                    terminal_content: "",
                    terminal_running: false,
                    terminal_shell: None,
                    terminal_cursor: None,
                },
                focused,
            )
        };

        let focused = render(true);
        let unfocused = render(false);
        assert_ne!(
            focused, unfocused,
            "focused and unfocused terminals must differ in glyphs, not only color"
        );
        assert!(focused.contains("> Terminal"), "{focused}");
        assert!(!unfocused.contains("> Terminal"), "{unfocused}");
        // Thick top rule when focused, plain when not.
        assert!(focused.contains('━'), "{focused}");
        assert!(!unfocused.contains('━'), "{unfocused}");
    }

    #[test]
    fn renders_without_panicking() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
        };
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            terminal_content: "",
            terminal_running: false,
            terminal_shell: None,
            terminal_cursor: None,
        };
        let rendered = rendered_text(model, false);
        assert!(rendered.contains("Terminal"));
    }

    #[test]
    fn renders_latest_terminal_output_instead_of_oldest_lines() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: true,
        };
        let content = (0..12)
            .map(|index| format!("output-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            terminal_content: &content,
            terminal_running: true,
            terminal_shell: Some("sh"),
            terminal_cursor: None,
        };

        let rendered = rendered_text(model, true);
        assert!(rendered.contains("output-11"));
        assert!(!rendered.contains("output-0"));
    }

    #[test]
    fn focused_terminal_paints_block_at_pty_column_and_row() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: true,
        };
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            terminal_content: "prompt\nnext",
            terminal_running: true,
            terminal_shell: Some("sh"),
            terminal_cursor: Some((4, 1)),
        };

        let buffer = rendered_buffer(model, true);
        let cursor = &buffer[(4, 4)];
        assert_eq!(cursor.symbol(), theme::CURSOR_CELL);
        assert_eq!(cursor.style().bg, theme::caret().bg);
    }
}
