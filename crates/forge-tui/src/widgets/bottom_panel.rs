use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::BusyPhase;
use crate::{validation_command_text, ValidationSnapshot, ValidationStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomPanelTab {
    Tests,
    Diagnostics,
    Terminal,
    Activity,
}

impl BottomPanelTab {
    pub const ALL: [Self; 4] = [
        Self::Tests,
        Self::Diagnostics,
        Self::Terminal,
        Self::Activity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tests => "Tests",
            Self::Diagnostics => "Diagnostics",
            Self::Terminal => "Terminal",
            Self::Activity => "Activity",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BottomPanelState {
    pub open: bool,
    pub focused: bool,
    pub active: BottomPanelTab,
}

impl Default for BottomPanelState {
    fn default() -> Self {
        Self {
            open: false,
            focused: false,
            active: BottomPanelTab::Terminal,
        }
    }
}

impl BottomPanelState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.focused = self.open;
    }

    pub fn open_tab(&mut self, tab: BottomPanelTab) {
        self.active = tab;
        self.open = true;
        self.focused = true;
    }

    pub fn next_tab(&mut self) {
        let index = BottomPanelTab::ALL
            .iter()
            .position(|tab| *tab == self.active)
            .unwrap_or(0);
        self.active = BottomPanelTab::ALL[(index + 1) % BottomPanelTab::ALL.len()];
    }

    pub fn previous_tab(&mut self) {
        let index = BottomPanelTab::ALL
            .iter()
            .position(|tab| *tab == self.active)
            .unwrap_or(0);
        self.active = BottomPanelTab::ALL
            [(index + BottomPanelTab::ALL.len() - 1) % BottomPanelTab::ALL.len()];
    }
}

pub struct BottomPanelModel<'a> {
    pub state: &'a BottomPanelState,
    pub busy_phase: &'a BusyPhase,
    pub activity: &'a ActivityFeed,
    pub validation: &'a ValidationSnapshot,
    pub terminal_title: Option<&'a str>,
    pub terminal_content: &'a str,
    pub terminal_truncated: bool,
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
        let style = if self.focused {
            theme::brand()
        } else {
            theme::muted()
        };
        let title = Line::from(
            vec![Span::styled(
                if self.focused {
                    " BOTTOM · NAV ".to_string()
                } else {
                    " BOTTOM ".to_string()
                },
                style,
            )]
            .into_iter()
            .chain(
                BottomPanelTab::ALL
                    .into_iter()
                    .enumerate()
                    .flat_map(|(idx, tab)| {
                        let tab_style = if tab == self.model.state.active {
                            if self.focused {
                                theme::brand().add_modifier(ratatui::style::Modifier::UNDERLINED)
                            } else {
                                theme::text().add_modifier(ratatui::style::Modifier::BOLD)
                            }
                        } else {
                            theme::muted()
                        };
                        [
                            Span::styled(format!(" {} {} ", idx + 1, tab.label()), tab_style),
                            Span::styled(" ", theme::muted()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            )
            .collect::<Vec<_>>(),
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focused {
                theme::brand()
            } else {
                theme::border_muted()
            })
            .title(title)
            .title_bottom(Line::from("⇧←/⇧→ tab · Esc back · Ctrl+P close"));
        let inner = block.inner(area);
        block.render(area, buf);
        let lines = match self.model.state.active {
            BottomPanelTab::Tests => validation_lines(self.model.validation),
            BottomPanelTab::Diagnostics => vec![
                Line::styled("No diagnostics", theme::text()),
                Line::styled("Compiler and tool output is in Terminal.", theme::muted()),
            ],
            BottomPanelTab::Terminal => terminal_lines(
                self.model.busy_phase,
                self.model.activity,
                self.model.terminal_title,
                self.model.terminal_content,
                self.model.terminal_truncated,
            ),
            BottomPanelTab::Activity => activity_lines(self.model.activity),
        };
        Paragraph::new(lines).render(inner, buf);
    }
}

fn validation_lines(validation: &ValidationSnapshot) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Validation ", theme::muted()),
        Span::styled(validation.display_status(), theme::text()),
    ]));
    match validation.status {
        ValidationStatus::NotConfigured => {
            lines.push(Line::styled(
                "Set [validation].command in forge.toml.",
                theme::muted(),
            ));
        }
        ValidationStatus::NotRun => {
            if let Some(command) = &validation.command {
                lines.push(Line::styled(
                    validation_command_text(command),
                    theme::text(),
                ));
            }
            lines.push(Line::styled(
                "Enter to run · raw output in Terminal",
                theme::muted(),
            ));
        }
        ValidationStatus::Running => {
            if let Some(command) = &validation.command {
                lines.push(Line::styled(
                    validation_command_text(command),
                    theme::text(),
                ));
            }
            lines.push(Line::styled(
                "Running… · Enter cancels when focused",
                theme::muted(),
            ));
        }
        ValidationStatus::Passed | ValidationStatus::Failed | ValidationStatus::Cancelled => {
            if let Some(command) = &validation.command {
                lines.push(Line::styled(
                    validation_command_text(command),
                    theme::text(),
                ));
            }
            if validation.stale {
                lines.push(Line::styled(
                    "Workspace changed after that run.",
                    theme::muted(),
                ));
            }
        }
    }
    if let Some(duration) = validation.duration {
        lines.push(Line::styled(
            format!("Duration: {:?}", duration),
            theme::muted(),
        ));
    }
    if let Some(code) = validation.exit_code {
        lines.push(Line::styled(format!("Exit: {code}"), theme::muted()));
    }
    if let Some(output_ref) = &validation.output_ref {
        lines.push(Line::styled(
            format!("Output: {output_ref}"),
            theme::muted(),
        ));
    }
    lines
}

fn terminal_lines<'a>(
    busy_phase: &'a BusyPhase,
    activity: &'a ActivityFeed,
    terminal_title: Option<&'a str>,
    terminal_content: &'a str,
    terminal_truncated: bool,
) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Command output view", theme::text()),
        Span::styled(" · existing captured output only", theme::muted()),
    ])];
    if let Some(title) = terminal_title {
        lines.push(Line::styled(title.to_string(), theme::text()));
    }
    if !terminal_content.is_empty() {
        for line in terminal_content.lines().take(3) {
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

fn activity_lines(activity: &ActivityFeed) -> Vec<Line<'_>> {
    if activity.is_empty() {
        return vec![Line::styled("No activity yet", theme::muted())];
    }
    activity
        .all()
        .iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|item| {
            Line::from(vec![
                Span::styled(format!("{:?} ", item.kind), theme::muted()),
                Span::styled(item.summary.as_str(), theme::text()),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ValidationSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn rendered_text(model: BottomPanelModel<'_>, focused: bool) -> String {
        let area = Rect::new(0, 0, 80, 8);
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

    #[test]
    fn default_panel_is_closed_on_terminal() {
        let state = BottomPanelState::default();
        assert!(!state.open);
        assert!(!state.focused);
        assert_eq!(state.active, BottomPanelTab::Terminal);
    }

    #[test]
    fn opening_tab_preserves_selected_mode() {
        let mut state = BottomPanelState::default();
        state.open_tab(BottomPanelTab::Activity);
        assert!(state.open);
        assert!(state.focused);
        assert_eq!(state.active, BottomPanelTab::Activity);
        state.toggle();
        assert!(!state.open);
        assert!(!state.focused);
        assert_eq!(state.active, BottomPanelTab::Activity);
    }

    #[test]
    fn tabs_cycle_in_both_directions() {
        let mut state = BottomPanelState::default();
        state.previous_tab();
        assert_eq!(state.active, BottomPanelTab::Diagnostics);
        state.next_tab();
        assert_eq!(state.active, BottomPanelTab::Terminal);
    }

    #[test]
    fn tabs_wrap_across_the_full_cycle() {
        let mut state = BottomPanelState::default();
        state.active = BottomPanelTab::Activity;
        state.next_tab();
        assert_eq!(state.active, BottomPanelTab::Tests);
        state.previous_tab();
        assert_eq!(state.active, BottomPanelTab::Activity);
    }

    #[test]
    fn renders_active_tab_and_focus_state_in_title() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: true,
            active: BottomPanelTab::Diagnostics,
        };
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            validation: &ValidationSnapshot::default(),
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
        };

        let rendered = rendered_text(model, true);
        assert!(rendered.contains("BOTTOM · NAV"));
        assert!(rendered.contains("Diagnostics"));
        assert!(rendered.contains("⇧←/⇧→ tab"));
        assert!(rendered.contains("Ctrl+P close"));
    }

    #[test]
    fn renders_each_tab_without_panicking() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
            active: BottomPanelTab::Tests,
        };
        for tab in BottomPanelTab::ALL {
            let mut state = state.clone();
            state.active = tab;
            let model = BottomPanelModel {
                state: &state,
                busy_phase: &BusyPhase::Idle,
                activity: &activity,
                validation: &ValidationSnapshot::default(),
                terminal_title: None,
                terminal_content: "",
                terminal_truncated: false,
            };
            let rendered = rendered_text(model, false);
            assert!(rendered.contains(tab.label()));
        }
    }
}
