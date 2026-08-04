use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::BusyPhase;
use crate::RunStateModel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BottomPanelState {
    pub open: bool,
    pub focused: bool,
}

pub struct BottomPanelModel<'a> {
    pub state: &'a BottomPanelState,
    pub busy_phase: &'a BusyPhase,
    pub activity: &'a ActivityFeed,
    #[allow(dead_code)]
    pub run: &'a RunStateModel,
    pub terminal_title: Option<&'a str>,
    pub terminal_content: &'a str,
    pub terminal_truncated: bool,
    pub terminal_running: bool,
    pub terminal_shell: Option<&'a str>,
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
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(if self.focused {
                theme::active_panel_border()
            } else {
                theme::inactive_panel_border()
            })
            .style(theme::panel())
            .title(Line::from(Span::styled(" Terminal ", theme::text())));
        let inner = block.inner(area);
        block.render(area, buf);
        let lines = terminal_lines(
            self.model.busy_phase,
            self.model.activity,
            self.model.terminal_title,
            self.model.terminal_content,
            self.model.terminal_truncated,
            self.model.terminal_running,
            self.model.terminal_shell,
        );
        Paragraph::new(lines).render(inner, buf);
    }
}

fn terminal_lines<'a>(
    busy_phase: &'a BusyPhase,
    activity: &'a ActivityFeed,
    terminal_title: Option<&'a str>,
    terminal_content: &'a str,
    terminal_truncated: bool,
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
        lines.push(Line::styled(format!("$ {shell} -l"), theme::muted()));
    }
    if let Some(title) = terminal_title {
        lines.push(Line::styled(title.to_string(), theme::text()));
    }
    if !terminal_content.is_empty() {
        for line in terminal_content.lines().take(20) {
            lines.push(Line::styled(line.to_string(), theme::muted()));
        }
        if terminal_truncated {
            lines.push(Line::styled("Output truncated", theme::muted()));
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
    use crate::RunStateModel;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

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

    fn run_model() -> RunStateModel {
        let mut run = RunStateModel::new(PathBuf::from("/repo"), None);
        run.draft.command_input = "cargo test -p forge-tui".into();
        run
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
            run: &run_model(),
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
            terminal_running: false,
            terminal_shell: None,
        };

        let rendered = rendered_text(model, true);
        assert!(rendered.contains("Terminal"));
        assert!(!rendered.contains("BOTTOM"));
        assert!(!rendered.contains("Ctrl+P close"));
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
            run: &run_model(),
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
            terminal_running: false,
            terminal_shell: None,
        };
        let rendered = rendered_text(model, false);
        assert!(rendered.contains("Terminal"));
    }
}
