//! Keyboard and pointer interaction for resizing persistent layout panes.

use super::*;

impl TuiApp {
    pub(super) fn begin_resize_mode(&mut self) {
        self.pane_resize.interaction = Some(ResizeInteraction {
            boundary: None,
            initial: self.pane_resize.preferences,
            drag_start: None,
        });
        self.set_feedback(
            FeedbackSeverity::Info,
            "resize mode · use arrow keys · Enter save · Esc cancel",
        );
    }

    pub(super) fn reset_pane_layout(&mut self) {
        self.pane_resize.preferences = forge_config::PaneLayoutPreferences::default();
        self.pane_resize.interaction = None;
        match self.pane_resize.store.reset() {
            Ok(()) => self.set_feedback(FeedbackSeverity::Info, "pane layout reset"),
            Err(error) => self.set_feedback(
                FeedbackSeverity::Warn,
                format!("pane layout reset in memory; could not persist: {error}"),
            ),
        }
    }

    fn persist_pane_layout(&mut self) {
        if let Err(error) = self.pane_resize.store.save(self.pane_resize.preferences) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("pane layout kept for this session; could not persist: {error}"),
            );
        }
    }

    pub(super) fn cancel_pane_resize(&mut self) {
        if let Some(interaction) = self.pane_resize.interaction.take() {
            self.pane_resize.preferences = interaction.initial;
            self.set_feedback(FeedbackSeverity::Info, "pane resize cancelled");
        }
    }

    pub(super) fn commit_pane_resize(&mut self) {
        if self.pane_resize.interaction.take().is_some() {
            self.persist_pane_layout();
            if self.feedback.severity != FeedbackSeverity::Warn {
                self.set_feedback(FeedbackSeverity::Info, "pane layout saved");
            }
        }
    }

    pub(super) fn handle_resize_key(&mut self, key: event::KeyEvent) -> bool {
        if self.pane_resize.interaction.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => self.cancel_pane_resize(),
            KeyCode::Enter if key.modifiers.is_empty() => self.commit_pane_resize(),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if key.modifiers.is_empty() =>
            {
                if let Some((boundary, delta)) = self.resize_target_for_key(key.code) {
                    if let Some(interaction) = self.pane_resize.interaction.as_mut() {
                        interaction.boundary = Some(boundary);
                    }
                    self.adjust_boundary(boundary, delta);
                } else {
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "no resizable boundary in that direction",
                    );
                }
            }
            _ => {}
        }
        true
    }

    fn resize_target_for_key(&self, key: KeyCode) -> Option<(ResizeBoundary, i16)> {
        use FocusBlock::*;
        use KeyCode::*;
        match (self.focus.block(), key) {
            (TaskStrip | Search | Files, Right) if self.pane_resize.files_separator.is_some() => {
                Some((ResizeBoundary::Files, 1))
            }
            (Workspace, Left) if self.pane_resize.files_separator.is_some() => {
                Some((ResizeBoundary::Files, -1))
            }
            (Workspace, Right) if self.pane_resize.conversation_separator.is_some() => {
                Some((ResizeBoundary::Conversation, -1))
            }
            (Sidebar | Composer, Left) if self.pane_resize.conversation_separator.is_some() => {
                Some((ResizeBoundary::Conversation, 1))
            }
            (Sidebar | Composer, Left) if self.pane_resize.files_separator.is_some() => {
                Some((ResizeBoundary::Files, -1))
            }
            (Workspace, Down) if self.pane_resize.bottom_separator.is_some() => {
                Some((ResizeBoundary::BottomPanel, -1))
            }
            (BottomPanel, Up) if self.pane_resize.bottom_separator.is_some() => {
                Some((ResizeBoundary::BottomPanel, 1))
            }
            _ => None,
        }
    }

    pub(super) fn adjust_boundary(&mut self, boundary: ResizeBoundary, cells: i16) {
        let area = self.pane_resize.frame_area;
        let content_width = area
            .width
            .saturating_sub(crate::design::FRAME_INSET_X * 2)
            .max(1);
        let height = area.height.max(1);
        let (ratio, denominator, minimum, maximum) = match boundary {
            ResizeBoundary::Files => {
                let current = self
                    .pane_resize
                    .preferences
                    .files_width_ratio
                    .unwrap_or_else(|| {
                        self.pane_resize
                            .files_separator
                            .map(|separator| {
                                f64::from(
                                    separator
                                        .x
                                        .saturating_sub(area.x + crate::design::FRAME_INSET_X),
                                ) / f64::from(content_width)
                            })
                            .unwrap_or(0.25)
                    });
                (current, content_width, 28, content_width.saturating_sub(45))
            }
            ResizeBoundary::Conversation => {
                let current = self
                    .pane_resize
                    .preferences
                    .conversation_width_ratio
                    .unwrap_or_else(|| {
                        self.pane_resize
                            .conversation_separator
                            .map(|separator| {
                                f64::from(
                                    area.right()
                                        .saturating_sub(crate::design::FRAME_INSET_X)
                                        .saturating_sub(separator.x + 1),
                                ) / f64::from(content_width)
                            })
                            .unwrap_or(0.25)
                    });
                (current, content_width, 32, content_width.saturating_sub(45))
            }
            ResizeBoundary::BottomPanel => {
                let current = self
                    .pane_resize
                    .preferences
                    .bottom_panel_height_ratio
                    .unwrap_or(16.0 / f64::from(height));
                (current, height, 3, 32.min(height.saturating_sub(3)))
            }
        };
        let current_cells = (ratio * f64::from(denominator)).round() as i32;
        let cells = (current_cells + i32::from(cells)).clamp(minimum, i32::from(maximum));
        let ratio = f64::from(cells) / f64::from(denominator);
        match boundary {
            ResizeBoundary::Files => self.pane_resize.preferences.files_width_ratio = Some(ratio),
            ResizeBoundary::Conversation => {
                self.pane_resize.preferences.conversation_width_ratio = Some(ratio)
            }
            ResizeBoundary::BottomPanel => {
                self.pane_resize.preferences.bottom_panel_height_ratio = Some(ratio)
            }
        }
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "resize {} · {cells} cells · Enter save · Esc cancel",
                boundary.label()
            ),
        );
    }

    pub(super) fn resize_boundary_at(&self, column: u16, row: u16) -> Option<ResizeBoundary> {
        [
            (ResizeBoundary::Files, self.pane_resize.files_separator),
            (
                ResizeBoundary::Conversation,
                self.pane_resize.conversation_separator,
            ),
            (
                ResizeBoundary::BottomPanel,
                self.pane_resize.bottom_separator,
            ),
        ]
        .into_iter()
        .find_map(|(boundary, area)| {
            let area = area?;
            let hit = match boundary {
                ResizeBoundary::Files | ResizeBoundary::Conversation => ratatui::layout::Rect::new(
                    area.x.saturating_sub(1),
                    area.y,
                    area.width.saturating_add(2),
                    area.height,
                ),
                ResizeBoundary::BottomPanel => ratatui::layout::Rect::new(
                    area.x,
                    area.y.saturating_sub(1),
                    area.width,
                    area.height.saturating_add(2),
                ),
            };
            crate::selection::cell_inside(hit, column, row).then_some(boundary)
        })
    }

    pub(super) fn start_mouse_resize(&mut self, column: u16, row: u16) -> bool {
        let Some(boundary) = self.resize_boundary_at(column, row) else {
            return false;
        };
        self.pane_resize.interaction = Some(ResizeInteraction {
            boundary: Some(boundary),
            initial: self.pane_resize.preferences,
            drag_start: Some((column, row)),
        });
        true
    }

    pub(super) fn drag_mouse_resize(&mut self, column: u16, row: u16) -> bool {
        let Some(interaction) = self.pane_resize.interaction else {
            return false;
        };
        let (Some(boundary), Some((start_column, start_row))) =
            (interaction.boundary, interaction.drag_start)
        else {
            return false;
        };
        self.pane_resize.preferences = interaction.initial;
        let delta = match boundary {
            ResizeBoundary::Files => i32::from(column) - i32::from(start_column),
            ResizeBoundary::Conversation => i32::from(start_column) - i32::from(column),
            ResizeBoundary::BottomPanel => i32::from(start_row) - i32::from(row),
        }
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        self.adjust_boundary(boundary, delta);
        true
    }

    pub(super) fn finish_mouse_resize(&mut self) -> bool {
        if self
            .pane_resize
            .interaction
            .is_some_and(|interaction| interaction.drag_start.is_some())
        {
            self.commit_pane_resize();
            true
        } else {
            false
        }
    }
}

impl ResizeBoundary {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Conversation => "conversation",
            Self::BottomPanel => "panel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::helpers::focus_test_app;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[tokio::test]
    async fn keyboard_resize_can_commit_or_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let (_workspace, mut app) = focus_test_app().await;
        app.pane_resize.store = forge_config::PaneLayoutStore::new(dir.path().join("config.toml"));
        app.pane_resize.frame_area = ratatui::layout::Rect::new(0, 0, 160, 50);
        app.pane_resize.files_separator = Some(ratatui::layout::Rect::new(40, 3, 1, 40));
        app.focus_block(FocusBlock::Files);

        app.begin_resize_mode();
        app.handle_resize_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let changed = app.pane_resize.preferences;
        assert!(changed.files_width_ratio.is_some());
        app.handle_resize_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.pane_resize.preferences,
            forge_config::PaneLayoutPreferences::default()
        );

        app.begin_resize_mode();
        app.handle_resize_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_resize_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.pane_resize.store.load(), changed);
    }

    #[tokio::test]
    async fn separator_hitbox_extends_one_cell_each_side() {
        let (_dir, mut app) = focus_test_app().await;
        app.pane_resize.files_separator = Some(ratatui::layout::Rect::new(40, 3, 1, 20));
        assert_eq!(app.resize_boundary_at(39, 10), Some(ResizeBoundary::Files));
        assert_eq!(app.resize_boundary_at(41, 10), Some(ResizeBoundary::Files));
        assert_eq!(app.resize_boundary_at(38, 10), None);
    }

    #[tokio::test]
    async fn mouse_drag_updates_live_and_escape_restores_starting_size() {
        let (_dir, mut app) = focus_test_app().await;
        app.pane_resize.frame_area = ratatui::layout::Rect::new(0, 0, 160, 50);
        app.pane_resize.files_separator = Some(ratatui::layout::Rect::new(40, 3, 1, 20));

        assert!(app.start_mouse_resize(40, 10));
        assert!(app.drag_mouse_resize(45, 10));
        assert!(app.pane_resize.preferences.files_width_ratio.is_some());
        app.cancel_pane_resize();

        assert_eq!(
            app.pane_resize.preferences,
            forge_config::PaneLayoutPreferences::default()
        );
    }
}
