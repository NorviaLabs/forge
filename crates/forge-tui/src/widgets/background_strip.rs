//! Sidebar "BACKGROUND" strip — relocated from the bottom panel's Tasks
//! tab (see round-2 layout PR). Straight relocation: same minimal
//! one-line-per-task rendering, no new capability.

use crate::theme;
use forge_core::{BackgroundTaskRegistry, BackgroundTaskStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

pub struct BackgroundStripWidget<'a> {
    pub background: &'a BackgroundTaskRegistry,
    pub selected: Option<usize>,
}

impl Widget for BackgroundStripWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        Paragraph::new(background_lines(self.background, self.selected)).render(area, buf);
    }
}

/// Number of lines `background_lines` needs for the current task set,
/// including the "BACKGROUND" header — callers use this to size the
/// sidebar's background region before rendering.
pub fn background_strip_height(background: &BackgroundTaskRegistry) -> u16 {
    background_lines(background, None).len() as u16
}

fn background_lines(background: &BackgroundTaskRegistry, selected: Option<usize>) -> Vec<Line<'_>> {
    let mut lines = vec![Line::styled("BACKGROUND", theme::muted())];
    let mut items: Vec<_> = background.list().collect();
    if items.is_empty() {
        lines.push(Line::styled("Nothing running", theme::muted()));
        return lines;
    }
    items.sort_by_key(|task| task.id.0);
    let any_waiting = items
        .iter()
        .any(|t| matches!(t.status, BackgroundTaskStatus::WaitingForApproval { .. }));
    for (idx, task) in items.into_iter().enumerate() {
        let (label, style) = match &task.status {
            BackgroundTaskStatus::Queued => ("queued".to_string(), theme::muted()),
            BackgroundTaskStatus::Running => ("running".to_string(), theme::info()),
            BackgroundTaskStatus::WaitingForApproval { payload } => (
                format!("waiting for approval — {}", payload.tool),
                theme::warn(),
            ),
            BackgroundTaskStatus::Succeeded { .. } => ("succeeded".to_string(), theme::ok()),
            BackgroundTaskStatus::Failed { error } => {
                (format!("failed: {}", truncate_line(error)), theme::danger())
            }
            BackgroundTaskStatus::Cancelled => ("cancelled".to_string(), theme::muted()),
        };
        let marker = if selected == Some(idx) { "> " } else { "  " };
        let marker_style = if selected == Some(idx) {
            theme::brand()
        } else {
            theme::muted()
        };
        lines.push(Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(task.label.clone(), theme::text()),
            Span::styled(format!(" — {label}"), style),
        ]));
        if let Some(branch) = &task.worktree_branch {
            lines.push(Line::from(vec![
                Span::styled("    worktree: ", theme::muted()),
                Span::styled(branch.clone(), theme::muted()),
            ]));
        }
        if let Ok(latest) = task.latest_message.lock() {
            if let Some(text) = latest.as_deref() {
                // The selected row gets the full message (the sidebar's
                // Paragraph wraps it), not just a one-line tail.
                let shown = if selected == Some(idx) {
                    text.trim().to_string()
                } else {
                    truncate_line(text)
                };
                if !shown.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("    ", theme::muted()),
                        Span::styled(shown, theme::dim()),
                    ]));
                }
            }
        }
    }
    if any_waiting {
        lines.push(Line::styled(
            "Up/Down select · x cancel · a approve · d deny (on a waiting row)",
            theme::muted(),
        ));
    } else {
        lines.push(Line::styled("Up/Down select · x cancel", theme::muted()));
    }
    lines
}

fn truncate_line(s: &str) -> String {
    let first_line = s.lines().next().unwrap_or("");
    if first_line.chars().count() > 60 {
        format!("{}…", first_line.chars().take(60).collect::<String>())
    } else {
        first_line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn rendered_text(background: &BackgroundTaskRegistry, selected: Option<usize>) -> String {
        rendered_text_at_width(background, selected, 40)
    }

    fn rendered_text_at_width(
        background: &BackgroundTaskRegistry,
        selected: Option<usize>,
        width: u16,
    ) -> String {
        let area = Rect::new(0, 0, width, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    BackgroundStripWidget {
                        background,
                        selected,
                    },
                    area,
                );
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
    fn empty_strip_shows_a_hint_instead_of_a_blank_panel() {
        let background = BackgroundTaskRegistry::default();
        let rendered = rendered_text(&background, None);
        assert!(rendered.contains("BACKGROUND"));
        assert!(rendered.contains("Nothing running"));
        assert_eq!(background_strip_height(&background), 2);
    }

    #[test]
    fn strip_renders_label_and_status_for_each_background_task() {
        use forge_core::BackgroundTaskKind;
        use forge_types::TaskId;
        use tokio_util::sync::CancellationToken;

        let mut background = BackgroundTaskRegistry::new();
        let running = background.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "cargo test".into(),
            },
            "cargo test",
            TaskId(1),
            CancellationToken::new(),
            None,
        );
        background.mark_running(running);
        let failed = background.spawn_slot(
            BackgroundTaskKind::Shell {
                command: "cargo check".into(),
            },
            "cargo check",
            TaskId(1),
            CancellationToken::new(),
            None,
        );
        background.set_status(
            failed,
            BackgroundTaskStatus::Failed {
                error: "exit code 1".into(),
            },
        );

        let rendered = rendered_text(&background, None);
        assert!(rendered.contains("cargo test"));
        assert!(rendered.contains("running"));
        assert!(rendered.contains("cargo check"));
        assert!(rendered.contains("failed"));
    }

    #[test]
    fn strip_renders_subagent_worktree_branch_and_latest_message() {
        use forge_core::BackgroundTaskKind;
        use forge_types::TaskId;
        use tokio_util::sync::CancellationToken;

        let mut background = BackgroundTaskRegistry::new();
        let id = background.spawn_slot(
            BackgroundTaskKind::Subagent {
                role: "test-fixer".into(),
                prompt: "fix the tests".into(),
            },
            "test-fixer",
            TaskId(1),
            CancellationToken::new(),
            Some(uuid::Uuid::new_v4()),
        );
        background.mark_running(id);
        background.set_worktree(
            id,
            std::path::PathBuf::from("/repo/.forge/local/worktrees/subagent-1-test-fixer"),
            "forge/subagent/subagent-1-test-fixer".into(),
        );
        *background.get(id).unwrap().latest_message.lock().unwrap() =
            Some("Running the test suite now".into());

        // Wide enough that the worktree branch line doesn't wrap.
        let rendered = rendered_text_at_width(&background, None, 60);
        assert!(rendered.contains("test-fixer"), "{rendered}");
        assert!(
            rendered.contains("forge/subagent/subagent-1-test-fixer"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Running the test suite now"),
            "{rendered}"
        );
    }

    #[test]
    fn selecting_a_task_row_shows_its_full_latest_message_not_just_a_tail() {
        use forge_core::BackgroundTaskKind;
        use forge_types::TaskId;
        use tokio_util::sync::CancellationToken;

        let long_message = "a".repeat(120);
        let mut background = BackgroundTaskRegistry::new();
        let id = background.spawn_slot(
            BackgroundTaskKind::Subagent {
                role: "explorer".into(),
                prompt: "go".into(),
            },
            "explorer",
            TaskId(1),
            CancellationToken::new(),
            Some(uuid::Uuid::new_v4()),
        );
        *background.get(id).unwrap().latest_message.lock().unwrap() = Some(long_message.clone());

        let rendered = rendered_text(&background, None);
        assert!(!rendered.contains(&long_message), "{rendered}");

        // Wide enough that ratatui doesn't itself wrap/clip the long line —
        // isolates "did background_lines emit the full text" from layout wrapping.
        let area = Rect::new(0, 0, 200, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    BackgroundStripWidget {
                        background: &background,
                        selected: Some(0),
                    },
                    area,
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let rendered_wide = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered_wide.contains(&long_message), "{rendered_wide}");
    }
}
