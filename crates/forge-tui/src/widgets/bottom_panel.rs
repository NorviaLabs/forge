use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::BusyPhase;
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
}

pub struct BottomPanel<'a> {
    pub model: BottomPanelModel<'a>,
}

impl Widget for BottomPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || !self.model.state.open {
            return;
        }
        let title = Line::from(
            BottomPanelTab::ALL
                .into_iter()
                .enumerate()
                .flat_map(|(idx, tab)| {
                    let style = if tab == self.model.state.active {
                        theme::brand()
                    } else {
                        theme::muted()
                    };
                    [
                        Span::styled(format!(" Alt+{} {} ", idx + 1, tab.label()), style),
                        Span::styled(" ", theme::muted()),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(title)
            .title_bottom(Line::from("Ctrl+P close"));
        let inner = block.inner(area);
        block.render(area, buf);
        let lines = match self.model.state.active {
            BottomPanelTab::Tests => vec![
                Line::styled(
                    "No structured test results are available yet.",
                    theme::text(),
                ),
                Line::styled(
                    "Command output remains available in Terminal.",
                    theme::muted(),
                ),
            ],
            BottomPanelTab::Diagnostics => vec![
                Line::styled(
                    "No structured diagnostics are available yet.",
                    theme::text(),
                ),
                Line::styled(
                    "Compiler and tool output remains available in Terminal.",
                    theme::muted(),
                ),
            ],
            BottomPanelTab::Terminal => terminal_lines(self.model.busy_phase, self.model.activity),
            BottomPanelTab::Activity => activity_lines(self.model.activity),
        };
        Paragraph::new(lines).render(inner, buf);
    }
}

fn terminal_lines<'a>(busy_phase: &'a BusyPhase, activity: &'a ActivityFeed) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Command output view", theme::text()),
        Span::styled(" · existing captured output only", theme::muted()),
    ])];
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
        lines.push(Line::styled(
            "No captured command output yet.",
            theme::muted(),
        ));
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
        return vec![Line::styled("No technical activity yet.", theme::muted())];
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
}
