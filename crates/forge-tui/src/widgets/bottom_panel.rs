use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::panel;
use crate::widgets::BusyPhase;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget};

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
        // The body shares the pane text origin (`TEXT_INSET`); the title keeps
        // one cell before the rule so it never touches the fill.
        let label = if self.model.terminal_shell.is_some() && !self.model.terminal_running {
            "Terminal · shell exited · hide/reopen to restart"
        } else {
            "Terminal"
        };
        let mut title = panel::title(self.focused, false, label);
        title.spans.push(Span::raw(" "));
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
                theme::panel_border()
            })
            .style(theme::panel())
            .padding(Padding::horizontal(crate::widgets::input::TEXT_INSET))
            .title(title);
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
                let rendered_y = inner.y.saturating_add(cursor_y).saturating_sub(scroll);
                let rendered_x = inner.x.saturating_add(cursor_x);
                if rendered_x < inner.right() && rendered_y < inner.bottom() {
                    buf[(rendered_x, rendered_y)].set_style(theme::caret());
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
    if terminal_shell.is_some() {
        return terminal_content
            .lines()
            .map(|line| Line::styled(line, theme::text()))
            .collect();
    }
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
        for line in &content {
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

    #[test]
    fn default_panel_is_closed() {
        let state = BottomPanelState::default();
        assert!(!state.open);
        assert!(!state.focused);
    }
}
