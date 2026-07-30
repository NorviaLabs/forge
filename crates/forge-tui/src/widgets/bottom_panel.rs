use crate::activity::ActivityFeed;
use crate::theme;
use crate::widgets::BusyPhase;
use crate::{RunExecutionMode, RunState, RunStateModel};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomPanelTab {
    Run,
    Diagnostics,
    Terminal,
    Activity,
}

impl BottomPanelTab {
    pub const ALL: [Self; 4] = [Self::Run, Self::Diagnostics, Self::Terminal, Self::Activity];

    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
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
    pub run: &'a RunStateModel,
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
        let title = Line::from(
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
        );
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(if self.focused {
                theme::brand()
            } else {
                theme::border_muted()
            })
            .style(theme::panel())
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);
        let lines = match self.model.state.active {
            BottomPanelTab::Run => run_lines(self.model.run),
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

fn run_lines(run: &RunStateModel) -> Vec<Line<'_>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("RUN ", theme::muted()),
        Span::styled(
            if run.editing { "editing" } else { "manual" },
            theme::text(),
        ),
    ])];
    if let Some(error) = run.error.as_ref() {
        lines.push(Line::styled(format!("Error: {error}"), theme::warn()));
    }
    lines.push(Line::styled(
        format!("> {}", run.draft.command_input),
        theme::text(),
    ));
    lines.push(Line::styled(
        format!("Directory: {}", run.draft.working_directory.display()),
        theme::muted(),
    ));
    lines.push(Line::styled(
        format!("Mode: {}", mode_label(run.draft.execution_mode)),
        theme::muted(),
    ));
    match run.draft.invocation() {
        Ok(invocation) => {
            lines.push(Line::styled("Invocation preview", theme::muted()));
            lines.push(Line::styled(
                format!("  Executable: {}", invocation.executable),
                theme::text(),
            ));
            lines.push(Line::styled(
                format!("  Arguments:  {:?}", invocation.arguments),
                theme::text(),
            ));
            if invocation.execution_mode == RunExecutionMode::Shell {
                lines.push(Line::styled(
                    format!(
                        "  Shell command: {}",
                        invocation.shell_command.unwrap_or_default()
                    ),
                    theme::text(),
                ));
            }
            let env = if invocation.environment_delta.is_empty() {
                "inherited".into()
            } else {
                format!(
                    "inherited + {} overrides",
                    invocation.environment_delta.len()
                )
            };
            lines.push(Line::styled(format!("  Environment: {env}"), theme::text()));
            lines.push(Line::styled("  Source: Manual", theme::text()));
        }
        Err(error) => lines.push(Line::styled(
            format!("Preview unavailable: {error}"),
            theme::warn(),
        )),
    }
    if let Some(current) = run.current.as_ref() {
        lines.push(Line::styled(
            format!(
                "Current: {} · {} · {:?}",
                state_label(&current.state),
                current.invocation.summary(),
                current.provenance
            ),
            theme::text(),
        ));
        if let Some(code) = current.exit_status {
            lines.push(Line::styled(format!("Exit status: {code}"), theme::muted()));
        }
        if current.state == RunState::StartFailed {
            lines.push(Line::styled(
                format!("Executable: {}", current.invocation.executable),
                theme::muted(),
            ));
            lines.push(Line::styled(
                format!("Arguments: {:?}", current.invocation.arguments),
                theme::muted(),
            ));
            lines.push(Line::styled(
                format!(
                    "Directory: {}",
                    current.invocation.working_directory.display()
                ),
                theme::muted(),
            ));
            if let Some(error) = current.spawn_error.as_deref() {
                lines.push(Line::styled(format!("Cause: {error}"), theme::danger()));
            }
        }
    }
    if !run.recent.is_empty() || !run.legacy.is_empty() {
        lines.push(Line::styled("Recent", theme::muted()));
        for record in run.recent.iter().chain(run.legacy.iter()).take(3) {
            lines.push(Line::styled(
                format!(
                    "  {} · {} · {:?}",
                    state_label(&record.state),
                    record.invocation.summary(),
                    record.provenance
                ),
                theme::text(),
            ));
        }
    }
    lines.push(Line::styled(
        "i edit command · d dir · m mode · Enter run/cancel · r rerun · e edit rerun",
        theme::muted(),
    ));
    lines
}

fn mode_label(mode: RunExecutionMode) -> &'static str {
    match mode {
        RunExecutionMode::Direct => "direct",
        RunExecutionMode::Shell => "shell",
    }
}

fn state_label(state: &RunState) -> &'static str {
    match state {
        RunState::Queued => "Queued",
        RunState::Running => "Running",
        RunState::Succeeded => "Succeeded",
        RunState::Failed => "Failed",
        RunState::Cancelled => "Cancelled",
        RunState::StartFailed => "Could not start",
        RunState::CaptureFailed => "Capture failed",
    }
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
        assert_eq!(state.active, BottomPanelTab::Run);
        state.previous_tab();
        assert_eq!(state.active, BottomPanelTab::Activity);
    }

    #[test]
    fn renders_purpose_tabs_without_bottom_label_or_shortcut_manual() {
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
            run: &run_model(),
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
        };

        let rendered = rendered_text(model, true);
        assert!(rendered.contains("Diagnostics"));
        assert!(rendered.contains("Run"));
        assert!(rendered.contains("Terminal"));
        assert!(rendered.contains("Activity"));
        assert!(!rendered.contains("BOTTOM"));
        assert!(!rendered.contains("Ctrl+P close"));
    }

    #[test]
    fn renders_each_tab_without_panicking() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
            active: BottomPanelTab::Run,
        };
        for tab in BottomPanelTab::ALL {
            let mut state = state.clone();
            state.active = tab;
            let model = BottomPanelModel {
                state: &state,
                busy_phase: &BusyPhase::Idle,
                activity: &activity,
                run: &run_model(),
                terminal_title: None,
                terminal_content: "",
                terminal_truncated: false,
            };
            let rendered = rendered_text(model, false);
            assert!(rendered.contains(tab.label()));
        }
    }

    #[test]
    fn renders_run_invocation_preview() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
            active: BottomPanelTab::Run,
        };
        let run = run_model();
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            run: &run,
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
        };
        let rendered = rendered_text(model, false);
        assert!(rendered.contains("Invocation preview"));
        assert!(rendered.contains("Executable: cargo"));
        assert!(rendered.contains("test"));
    }

    #[test]
    fn renders_shell_mode_preview() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
            active: BottomPanelTab::Run,
        };
        let mut run = run_model();
        run.draft.command_input = "echo hi | wc".into();
        run.draft.execution_mode = RunExecutionMode::Shell;
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            run: &run,
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
        };
        let rendered = rendered_text(model, false);
        assert!(rendered.contains("Mode: shell"));
        assert!(rendered.contains("Shell command: echo hi | wc"));
    }

    #[test]
    fn renders_direct_mode_shell_syntax_error() {
        let activity = ActivityFeed::default();
        let state = BottomPanelState {
            open: true,
            focused: false,
            active: BottomPanelTab::Run,
        };
        let mut run = run_model();
        run.draft.command_input = "echo hi | wc".into();
        let model = BottomPanelModel {
            state: &state,
            busy_phase: &BusyPhase::Idle,
            activity: &activity,
            run: &run,
            terminal_title: None,
            terminal_content: "",
            terminal_truncated: false,
        };
        let rendered = rendered_text(model, false);
        assert!(rendered.contains("Direct mode does not evaluate shell syntax"));
    }
}
