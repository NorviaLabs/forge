//! Frame rendering for [`TuiApp`].
//!
//! `draw` is the single entry point every render path funnels through, and at
//! ~480 lines it was the largest method in `app.rs`. It is moved here verbatim:
//! no logic, no signature and no field access changed.
//!
//! An inherent `impl` block may live in any module of the crate that defines the
//! type, and a child module can reach its parent's private items, so nothing had
//! to be made more visible to make this compile.

use super::*;

use crate::tasks_strip;

/// Most rows the slash palette will draw, however much room is above the
/// composer. Past this a palette stops being a menu and becomes a page.
const SLASH_PALETTE_MAX_ROWS: u16 = 16;

/// Fold the selected session's background tasks into the footer's counts-only
/// second row. The registry prunes terminal tasks, so `done`/`failed` count
/// recent history, not everything the session ever ran.
fn footer_activity(tasks: &[forge_session::BackgroundTaskSnapshot]) -> FooterActivity {
    use forge_core::{BackgroundTaskKind, BackgroundTaskStatus};
    let mut activity = FooterActivity::default();
    for task in tasks {
        let (active, need, failed, done, cancelled) = match task.kind {
            BackgroundTaskKind::Shell { .. } => (
                &mut activity.jobs_active,
                &mut activity.jobs_need,
                &mut activity.jobs_failed,
                &mut activity.jobs_done,
                &mut activity.jobs_cancelled,
            ),
            BackgroundTaskKind::Subagent { .. } => (
                &mut activity.agents_active,
                &mut activity.agents_need,
                &mut activity.agents_failed,
                &mut activity.agents_done,
                &mut activity.agents_cancelled,
            ),
        };
        match task.status {
            BackgroundTaskStatus::Queued | BackgroundTaskStatus::Running => *active += 1,
            BackgroundTaskStatus::WaitingForApproval { .. } => {
                *active += 1;
                *need += 1;
            }
            BackgroundTaskStatus::Failed { .. } => *failed += 1,
            BackgroundTaskStatus::Succeeded { .. } => *done += 1,
            BackgroundTaskStatus::Cancelled => *cancelled += 1,
        }
    }
    activity
}

fn composer_input_height(
    input: &InputModel,
    area: ratatui::layout::Rect,
    show_files: bool,
    expanded_conversation: bool,
    navigator_active: bool,
    preferences: forge_config::PaneLayoutPreferences,
    start: bool,
) -> u16 {
    let composer_width = crate::layout::estimate_composer_region_width(
        area,
        show_files,
        expanded_conversation,
        navigator_active,
        preferences,
    );
    let content_width = if start {
        composer_width.min(crate::design::HOME_MAX_WIDTH as usize)
    } else {
        composer_width
    }
    .saturating_sub(2 * crate::widgets::input::TEXT_INSET as usize)
    .max(1);
    // Use the same width and border budget as the rendered composer.
    let compact = area.height < crate::design::COMPACT_FRAME_H;
    input.visual_lines_for_width(content_width).clamp(
        if compact { 1 } else { 3 },
        if compact {
            crate::design::COMPACT_COMPOSER_INPUT_H
        } else {
            crate::layout::MAX_COMPOSER_INPUT_H
        },
    ) + crate::design::COMPOSER_BORDER_H
}

impl TuiApp {
    pub(super) fn use_start_prompt(&mut self, index: usize) {
        if let Some(prompt) = crate::widgets::start::STARTERS.get(index) {
            self.input.append_paste(prompt);
            self.enter_chat_composer();
        }
    }

    fn render_workspace_tabs(
        &mut self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        modal_open: bool,
    ) {
        if area.is_empty() {
            return;
        }
        let resource = self.workspace_navigation.current().map(|view| match view {
            WorkspaceView::File(path) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let dirty = self
                    .editor_session
                    .as_ref()
                    .is_some_and(|editor| editor.is_dirty());
                (name.into_owned(), dirty)
            }
            WorkspaceView::Diff => ("Git review".into(), false),
            WorkspaceView::GithubIssues => ("GitHub issues".into(), false),
        });
        let hint = if resource.is_some() {
            "F6 switch"
        } else {
            "Ctrl+P files · / commands"
        };
        let hint_width = hint.chars().count() as u16;
        let tabs_width = area.width.saturating_sub(hint_width + 2);
        let conversation = self
            .child_view
            .as_ref()
            .map(|view| format!("Conversation ‹ {}", view.label))
            .unwrap_or_else(|| "Conversation".into());
        let conversation_width = if resource.is_some() && self.child_view.is_some() {
            40.min(tabs_width / 2)
        } else if resource.is_some() {
            18.min(tabs_width)
        } else {
            if self.child_view.is_some() { 40 } else { 18 }.min(tabs_width)
        };
        let mut tabs = vec![(FocusBlock::Sidebar, conversation, false, conversation_width)];
        if let Some((label, dirty)) = resource {
            tabs.push((
                FocusBlock::Workspace,
                label,
                dirty,
                tabs_width.saturating_sub(conversation_width),
            ));
        }
        // The navbar is a band at least as thick as the composer; its labels sit
        // on the band's centre row and the selected ground fills the whole tile.
        let center_y = area.y + area.height.saturating_sub(1) / 2;
        crate::theme::fill(area, frame.buffer_mut(), theme::panel());
        let mut x = area.x;
        for (block, label, dirty, width) in tabs {
            if width < 4 {
                continue;
            }
            let tile = ratatui::layout::Rect::new(x, area.y, width, area.height);
            let rect = ratatui::layout::Rect::new(x, center_y, width, 1);
            let focused = !modal_open && self.focus.block() == block;
            let selected =
                (block == FocusBlock::Workspace) == self.workspace_navigation.resource_selected();
            let label = crate::path_display::elide_middle(
                &label
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect::<String>(),
                width.saturating_sub(3 + u16::from(dirty) * 2) as usize,
            );
            let label_style = if focused || selected {
                theme::text().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                theme::muted()
            };
            let label_style = if selected {
                label_style
                    .patch(theme::accent_style())
                    .bg(theme::accent_soft_bg())
            } else {
                label_style
            };
            crate::theme::fill(
                tile,
                frame.buffer_mut(),
                if selected {
                    theme::panel().bg(theme::accent_soft_bg())
                } else {
                    theme::panel()
                },
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if focused { "> " } else { "  " },
                        if focused {
                            theme::accent_style()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(label, label_style),
                    Span::styled(if dirty { " *" } else { "" }, theme::warn()),
                ]))
                .style(if selected {
                    theme::panel().bg(theme::accent_soft_bg())
                } else {
                    theme::panel()
                }),
                rect,
            );
            self.workspace_tab_areas.push((block, tile));
            x = x.saturating_add(width);
        }
        frame.render_widget(
            Paragraph::new(Line::styled(hint, theme::muted())),
            ratatui::layout::Rect::new(
                area.right().saturating_sub(hint_width),
                center_y,
                hint_width,
                1,
            ),
        );
    }

    /// Drive the live streaming preview from a test.
    ///
    /// The preview is the one part of the draw path whose cost grows with the
    /// turn rather than the viewport, and it is only reachable while a turn is
    /// in flight. Exposing it keeps the measurement in `render_perf` — where
    /// the counting allocator lives — instead of forcing a mock provider and a
    /// real turn loop just to grow a string.
    #[doc(hidden)]
    pub fn stream_preview_for_tests(&mut self, text: &str) {
        self.busy_state
            .start(crate::widgets::status::BusyPhase::Model);
        self.stream.preview.push_str(text);
        // The renderer rate-limits itself; tests measure the rebuild, so clear
        // the throttle rather than sleep 150ms per sample.
        self.stream.last_preview_render = None;
        self.stream.live_lines = None;
    }

    /// Drive streamed reasoning through the same preview path as production.
    #[doc(hidden)]
    pub fn stream_thinking_for_tests(&mut self, text: &str) {
        self.stream.thinking.push_str(text);
        self.stream_preview_for_tests("");
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        self.kitty_image_area = None;
        // One read of the active session per frame. Sibling sessions are
        // immutable supervisor snapshots; selecting one must not be undone by
        // the primary-session refresh that happens on every draw.
        if let Some(session) = self.session_runtime.as_ref() {
            self.session_view = SessionSnapshot::capture(session);
            if let Some(view) = self.child_view.as_mut() {
                view.parent_transcript.refresh(session);
            } else {
                self.transcript_view.refresh(session);
            }
        } else if let Some(snapshot) = self
            .supervisor
            .as_ref()
            .and_then(|supervisor| supervisor.snapshots.get(&self.selected_session_id))
        {
            self.session_view = snapshot.session.clone();
            self.set_parent_transcript(snapshot.transcript.clone());
        }
        let area = frame.area();
        self.last_frame_width = area.width;
        self.pane_resize.frame_area = area;
        if is_too_small(area) {
            self.focus.reset_to_workspace();
            self.workspace_files.explorer.focused = false;
            self.bottom_panel.focused = false;
            self.source_viewer.focused = false;
            frame.render_widget(
                Paragraph::new(format!(
                    "Terminal too small.\n\nForge needs at least {}x{} — this one is {}x{}.",
                    crate::layout::MIN_WIDTH,
                    crate::layout::MIN_HEIGHT,
                    area.width,
                    area.height
                ))
                .wrap(ratatui::widgets::Wrap { trim: true }),
                area,
            );
            return;
        }
        crate::theme::fill(area, frame.buffer_mut(), crate::theme::canvas());
        let fb_h = 0;
        // DESIGN-004: an open modal owns the keyboard, so background panes
        // suppress their focus markers/borders while it is open. Closing it
        // restores the previous valid owner via `restore_focus_after_closing`.
        let modal_open = self.overlay.is_some();
        let slash_mode = !modal_open && self.input.text.starts_with('/');
        let theme_picking = matches!(self.overlay, Some(Overlay::Theme { .. }));
        // Resolve this before measuring the composer so wrapping uses the
        // same horizontal geometry that the layout will paint.
        let expand_conversation = !matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::File(_) | WorkspaceView::Diff | WorkspaceView::GithubIssues)
        );
        let task_mode = self.supervisor.is_some();
        let decision_pending = self.child_view.is_none()
            && (self.session_view.is_awaiting_approval()
                || self.session_view.is_awaiting_question());
        let navigator_active = !decision_pending
            && matches!(
                self.focus.block(),
                FocusBlock::TaskStrip | FocusBlock::Search | FocusBlock::Files
            );
        let show_start = expand_conversation
            && !decision_pending
            && !self.bottom_panel.open
            && self.child_view.is_none()
            && self
                .transcript_view
                .messages()
                .iter()
                .all(|message| message.role == forge_types::MessageRole::System)
            && !self.busy_state.is_active()
            && !self.pending_turn.has_prompt()
            && !self.pending_turn.continue_requested()
            && self.selected_queue_messages().is_empty()
            && self.selected_background_tasks().is_empty();
        let show_files = self.workspace_files.visible || task_mode;
        let input_h = if theme_picking {
            crate::layout::THEME_DOCK_H
        } else {
            composer_input_height(
                &self.input,
                area,
                show_files,
                expand_conversation,
                navigator_active,
                self.pane_resize.preferences,
                show_start,
            )
        };
        // At compact heights the active body needs the terminal's rows for
        // navigation, inspection or a decision. Keep the shell open; focusing
        // Panel reveals it again without losing its input or scroll state.
        let panel_h = if self.bottom_panel.open
            && (area.height >= crate::design::AIRY_MIN_ROWS
                || self.focus.block() == FocusBlock::BottomPanel)
        {
            16
        } else {
            0
        };
        let (queued_ids, queued_messages): (Vec<_>, Vec<_>) = self
            .selected_queue_items()
            .into_iter()
            .map(|item| (item.id, item.text))
            .unzip();
        let compact_dock = area.height < 28;
        let queue_cap = if compact_dock { 1 } else { 3 };
        let current_queue_id = self.task_selection.queue(self.selected_session_id);
        if !queued_ids.iter().any(|id| Some(*id) == current_queue_id) {
            if let Some(id) = queued_ids.first() {
                self.task_selection
                    .select_queue(self.selected_session_id, *id);
            } else {
                self.task_selection.clear_queue();
            }
        }
        let queue_selected = queued_ids
            .iter()
            .position(|id| Some(*id) == self.task_selection.queue(self.selected_session_id));
        let queue_h = if queued_messages.is_empty() {
            0
        } else {
            1 + queued_messages.len().min(queue_cap) as u16
        };
        let background_tasks = self.selected_background_tasks();
        if let Some(dismissed) = self.dismissed_background.get_mut(&self.selected_session_id) {
            dismissed.retain(|id| {
                background_tasks
                    .iter()
                    .any(|task| task.id == *id && task.status.is_terminal())
            });
        }
        let contextual_hint = self.contextual_hint();
        // The event-loop tick refreshes this cache; drawing only reads it.
        let connected = self.provider_connected_cached();
        let (provider, _connect_profile, vendor_label, _route_label) =
            self.selected_provider_display();
        // Model/vendor/effort live on the footer chip row; the composer band
        // is text-only and the footer always reserves two rows — a thin
        // DESIGN-012: the footer is a single content row with no separator.
        // The focused footer's hint shares that row (replacing the
        // right-side activity while focused), never adds another.
        let hint_h = crate::design::FOOTER_H
            + u16::from(!queued_messages.is_empty() || !background_tasks.is_empty());
        // One full-width warning row directly under the status row while
        // approve-all is on. Session state, so it is never scrollable away.
        let approve_all_warning_h: u16 = u16::from(self.approve_all);
        // An open resource occupies the inspector; otherwise conversation
        // expands across the work surface and there is no inspector to focus.
        // The strip's height is requested before the split because `layout.rs`
        // has to clamp it against the transcript's floor, and it is built after
        // the split because only then is the height that actually fit known.
        let strip_now = chrono::Utc::now();
        let ordered = self.ordered_dock_tasks(&background_tasks, strip_now);
        let selected = self.task_selection.task(self.selected_session_id);
        if !matches!(self.overlay, Some(Overlay::Tasks { .. }))
            && !ordered.iter().any(|(_, id, _)| Some(*id) == selected)
        {
            if let Some((_, id, _)) = ordered.first() {
                self.task_selection
                    .select_task(self.selected_session_id, *id);
            } else {
                self.task_selection.clear_tasks();
            }
        }
        let strip_live = tasks_strip::BackgroundStrip::window(
            &background_tasks,
            strip_now,
            if compact_dock {
                1
            } else {
                tasks_strip::STRIP_ROW_CAP
            },
            if compact_dock { 2 } else { 5 },
            self.task_selection.task(self.selected_session_id),
            self.dismissed_background.get(&self.selected_session_id),
        );
        let background_h = if strip_live.is_empty() {
            0
        } else {
            strip_live.height()
        };
        let mut regions = split_areas_with_preferences(
            area,
            fb_h,
            input_h,
            show_files,
            queue_h,
            panel_h,
            hint_h,
            true,
            background_h,
            approve_all_warning_h,
            expand_conversation,
            task_mode,
            self.workspace_navigation.resource_selected() && !decision_pending,
            navigator_active,
            self.pane_resize.preferences,
        );
        let start_group = (show_start && !theme_picking && regions.sidebar.is_some()).then(|| {
            let body = ratatui::layout::Rect::new(
                regions.input.x,
                regions.workspace_tabs.y,
                regions.input.width,
                regions
                    .input
                    .bottom()
                    .saturating_sub(regions.workspace_tabs.y),
            );
            let group = crate::widgets::start::StartGroup::new(body, input_h, area.height);
            regions.input = group.input;
            regions.sidebar = None;
            regions.workspace_tabs.height = 0;
            group
        });
        self.pane_resize.files_separator = regions
            .files
            .filter(|files| files.right() < regions.input.x)
            .map(|files| ratatui::layout::Rect::new(files.right(), files.y, 1, files.height));
        self.pane_resize.conversation_separator = (regions.chat.width > 0)
            .then_some(regions.sidebar)
            .flatten()
            .map(|sidebar| {
                ratatui::layout::Rect::new(sidebar.right(), sidebar.y, 1, sidebar.height)
            });
        self.pane_resize.bottom_separator = (regions.bottom_panel.height > 0).then(|| {
            ratatui::layout::Rect::new(
                regions.bottom_panel.x,
                regions.bottom_panel.y.saturating_sub(1),
                regions.bottom_panel.width,
                1,
            )
        });
        // Remember the rendered editor rect so mouse events (which arrive
        // between frames) can be hit-tested against it for selection.
        self.editor_area = if self.current_workspace_is_file() && regions.chat.width > 0 {
            Some(regions.chat)
        } else {
            None
        };
        self.conversation_area = None;
        self.workspace_tab_areas.clear();
        self.terminal_area = None;
        self.navigator_tabs_area = None;
        self.navigator_new_session_area = None;
        self.navigator_list_area = None;
        self.task_strip_area = None;
        self.task_strip_chips.clear();
        self.footer_area = None;
        self.background_area = None;
        self.dock_paint = DockPaintState {
            owner: self.selected_session_id,
            ..Default::default()
        };
        self.slash_popup_rows.clear();
        self.inline_search_rows.clear();
        self.option_rects.clear();
        self.overlay_rows.borrow_mut().clear();
        self.conversation_rows.clear();
        self.start_prompt_rows.clear();
        self.terminal_rows.clear();
        // Layout can hide a requested side/bottom panel. Focus must follow the
        // rendered geometry rather than leaving an invisible key owner behind.
        // Questions reuse `FocusBlock::Approval` (same inline transcript
        // prompt as HITL); omitting them here kicks focus off the menu on the
        // first frame, so ↑↓ never move the selection.
        let navigator_tab = if regions.files.is_some() {
            self.effective_navigator_tab()
        } else {
            crate::widgets::NavigatorTab::Files
        };
        let navigator_sessions = navigator_tab == crate::widgets::NavigatorTab::Sessions;
        let available = FocusAvailability {
            task_strip: regions.task_strip.height > 0
                || (navigator_sessions && regions.files.is_some()),
            search: regions.files.is_some()
                && !navigator_sessions
                && navigator_tab != crate::widgets::NavigatorTab::Git,
            files: regions.files.is_some() && !navigator_sessions,
            workspace: regions.chat.width > 0,
            sidebar: regions.sidebar.is_some() || start_group.is_some(),
            bottom_panel: self.bottom_panel.open && regions.bottom_panel.height > 0,
            approval: decision_pending && regions.sidebar.is_some(),
            composer: self.child_view.is_none(),
        };
        if self.bottom_panel.open && regions.bottom_panel.height > 1 {
            self.resize_interactive_terminal(
                regions
                    .bottom_panel
                    .width
                    .saturating_sub(2 * crate::widgets::input::TEXT_INSET),
                regions.bottom_panel.height.saturating_sub(1),
            );
        }
        if !available.contains(self.focus.block()) {
            self.focus_block(if decision_pending {
                FocusBlock::Approval
            } else if regions.sidebar.is_some() {
                FocusBlock::Sidebar
            } else {
                FocusBlock::Composer
            });
        }
        // The tab row holds the keyboard only while the navigator column is on
        // screen: a collapsed column (or a mode without tabs) must not leave it
        // owning keys nothing is drawing.
        if regions.files.is_none() {
            self.navigator_tab_row_focused = false;
        }
        self.normalize_focus();
        self.render_workspace_tabs(frame, regions.workspace_tabs, modal_open);
        let status = self.refresh_status_model_with_connected(connected);
        // When the navigator column is collapsed there is no session list on
        // screen; a one-line chip in the status row keeps sessions reachable.
        let sessions_chip: Option<String> = if task_mode && regions.files.is_none() {
            let need = self
                .session_chrome
                .iter()
                .filter(|task| task.attention)
                .count();
            let working = self
                .session_chrome
                .iter()
                .filter(|task| task.is_working())
                .count();
            // The collapsed column is one row wide, so the one thing worth
            // keeping there is whether work is in flight — the same spinner
            // frame the session rows step, on the same clock (`§482`).
            let frame = crate::widgets::turn_line::running_marker(
                self.session_row_step,
                self.runtime.reduced_motion,
            );
            match (need, working) {
                (0, 0) => None,
                (n, 0) => Some(format!("⌄ {n} need")),
                (0, w) => Some(format!("⌄ {frame} {w} working")),
                (n, w) => Some(format!("⌄ {n} need · {frame} {w} working")),
            }
        } else {
            None
        };
        frame.render_widget(
            StatusBar {
                model: &status,
                sessions_chip: sessions_chip.as_deref(),
            },
            regions.status,
        );
        if regions.approve_all_warning.height > 0 {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(ratatui::text::Line::from(
                    ratatui::text::Span::styled(
                        "⚠ SANDBOX OFF · approvals, filesystem and network unconfined · this session only · /approve-all to re-enable",
                        theme::danger(),
                    ),
                ))
                .style(theme::panel()),
                regions.approve_all_warning,
            );
        }
        if regions.task_strip.height > 0 {
            self.task_strip_area = Some(regions.task_strip);
            // An unnamed task (created with one key, before its first prompt
            // names it) shows an ordinal instead of a hole. Numbered among
            // unnamed tasks, not by strip position, so the first unnamed
            // task is always `task 1` even with the primary row ahead of it.
            let mut unnamed = 0usize;
            let strip_items: Vec<TaskStripItem> = self
                .session_chrome
                .iter()
                .enumerate()
                .map(|(index, task)| TaskStripItem {
                    slot: task.slot,
                    label: if task.label.is_empty() {
                        unnamed += 1;
                        format!("session {unnamed}")
                    } else {
                        task.label.clone()
                    },
                    branch: task.branch.clone(),
                    state: task.lifecycle.into(),
                    secondary: task.secondary.clone(),
                    selected: task.selected,
                    focused: self.task_strip_selection == index,
                    attention: task.attention,
                })
                .collect();
            let strip = TaskStrip {
                items: &strip_items,
                overflow: 0,
                focused: self.focus.block() == FocusBlock::TaskStrip
                    && !self.navigator_tab_row_focused
                    && !modal_open
                    && (self.horizontal_session_strip_focused || regions.files.is_none()),
            };
            self.task_strip_chips = strip.chip_rects(regions.task_strip);
            frame.render_widget(strip, regions.task_strip);
        }
        if let Some(files) = regions.files {
            let active_file = match self.workspace_navigation.current() {
                Some(WorkspaceView::File(path)) => Some(path),
                _ => None,
            };
            let navigator_focused = self.focus.block() == FocusBlock::TaskStrip
                && !modal_open
                && !self.navigator_tab_row_focused
                && !self.horizontal_session_strip_focused;
            {
                // The left column is the navigator: a tab bar over either the
                // session list or the file explorer (FORGE-DESIGN §7.7).
                // One shared content origin; only the right seam separates panes.
                let shell = Block::default()
                    .borders(if regions.chat.width > 0 || regions.sidebar.is_some() {
                        Borders::RIGHT
                    } else {
                        Borders::NONE
                    })
                    .border_style(theme::panel_border())
                    .padding(ratatui::widgets::Padding::left(1))
                    .style(theme::panel());
                let content = shell.inner(files);
                frame.render_widget(shell, files);
                let rows = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Length(crate::design::NAVIGATOR_TAB_H),
                        ratatui::layout::Constraint::Min(0),
                    ])
                    .split(content);
                let tabs_area = rows[0];
                self.navigator_tabs_area = Some(tabs_area);
                self.navigator_new_session_area =
                    crate::widgets::navigator::new_session_cell(tabs_area);
                self.navigator_list_area = Some(rows[1]);
                let needs_you = self.session_chrome.iter().filter(|t| t.attention).count();
                if navigator_sessions {
                    let list_area = rows[1];
                    self.navigator_list_area = Some(list_area);
                    let mut unnamed = 0usize;
                    let session_rows: Vec<crate::widgets::SessionRow> = self
                        .session_chrome
                        .iter()
                        .enumerate()
                        .map(|(index, task)| {
                            let label = if task.label.is_empty() {
                                unnamed += 1;
                                format!("session {unnamed}")
                            } else {
                                task.label.clone()
                            };
                            let need = task.attention;
                            let working = task.is_working();
                            // Precedence: a turn stopped for the operator
                            // outranks a live one, and both outrank the
                            // lifecycle. Reading the marker from the flags
                            // alone — as this did — renders every finished
                            // session as idle, so a failed turn and a clean one
                            // were the same glyph.
                            let state = if need {
                                crate::widgets::SessionRowState::Waiting
                            } else if task.is_queued() {
                                crate::widgets::SessionRowState::Queued
                            } else if working {
                                crate::widgets::SessionRowState::Working
                            } else {
                                crate::widgets::SessionRowState::from_lifecycle(task.lifecycle)
                            };
                            let qualifier =
                                format!("{} · {}", state.label(), relative_age(task.updated_at));
                            crate::widgets::SessionRow {
                                state,
                                label,
                                qualifier,
                                selected: task.selected,
                                focused: self.task_strip_selection == index,
                            }
                        })
                        .collect();
                    let peek_lines = self.navigator_peek.and_then(|id| {
                        if !navigator_focused {
                            return None;
                        }
                        let snapshot = self.supervisor.as_ref()?.snapshots.get(&id)?;
                        let last = snapshot.transcript.messages().iter().rev().find(|message| {
                            message.role == forge_types::MessageRole::Assistant
                                && !message.content.trim().is_empty()
                        });
                        let context = format!(
                            "branch {}\nbase {}\nworktree {}\nissue {}\n{}\n{}",
                            if snapshot.task.branch.is_empty() {
                                "not yet materialized"
                            } else {
                                &snapshot.task.branch
                            },
                            snapshot.task.base_sha.as_deref().unwrap_or("unknown"),
                            snapshot.task.workspace.display(),
                            snapshot
                                .task
                                .github_issue_number
                                .map(|n| format!("#{n}"))
                                .unwrap_or_else(|| "none".into()),
                            if id == self.selected_session_id {
                                self.git_sync_tag().unwrap_or_else(|| "sync unknown".into())
                            } else {
                                "sync: attach to inspect".into()
                            },
                            last.map(|m| m.content.trim()).unwrap_or("No answer yet")
                        );
                        // Wrap to the peek's own text column, so a line is
                        // never handed to the widget already too wide for it.
                        Some(wrap_to_width(
                            &context,
                            list_area
                                .width
                                .saturating_sub(crate::widgets::TEXT_COL as u16)
                                as usize,
                            6,
                        ))
                    });
                    let peek_panel = peek_lines.as_ref().map(|lines| crate::widgets::PeekPanel {
                        lines,
                        reply: &self.navigator_reply,
                        placeholder: "reply to this session…",
                    });
                    frame.render_widget(
                        crate::widgets::SessionList {
                            rows: &session_rows,
                            focused: navigator_focused,
                            peek: peek_panel.as_ref(),
                            hover: self.hover_session,
                            step: self.session_row_step,
                            reduced_motion: self.runtime.reduced_motion,
                        },
                        list_area,
                    );
                } else if matches!(
                    self.workspace_navigation.current(),
                    Some(WorkspaceView::GithubIssues)
                ) {
                    let visible = self.github_view.visible();
                    let lines: Vec<Line> = std::iter::once(Line::from(format!(
                        "Issues · / {}",
                        self.github_view.filter
                    )))
                    .chain(
                        visible
                            .iter()
                            .skip(
                                self.github_view
                                    .list_start(rows[1].height.saturating_sub(3) as usize),
                            )
                            .map(|index| {
                                let issue = &self.github_view.items[*index];
                                Line::styled(
                                    format!(
                                        "{} #{} {}",
                                        if *index == self.github_view.selected {
                                            ">"
                                        } else {
                                            " "
                                        },
                                        issue.number,
                                        issue
                                            .title
                                            .chars()
                                            .filter(|c| !c.is_control())
                                            .collect::<String>()
                                    ),
                                    if *index == self.github_view.selected
                                        && self.focus.block() == FocusBlock::Files
                                    {
                                        crate::theme::selected_row()
                                    } else {
                                        crate::theme::text()
                                    },
                                )
                            }),
                    )
                    .collect();
                    frame.render_widget(
                        Paragraph::new(lines).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(ratatui::widgets::BorderType::Rounded)
                                .border_style(crate::theme::panel_border()),
                        ),
                        rows[1],
                    );
                } else if navigator_tab == crate::widgets::NavigatorTab::Git {
                    let git_list_focused = self.focus.block() == FocusBlock::Files
                        && !modal_open
                        && !self.navigator_tab_row_focused;
                    let git_hover = git_list_focused.then_some(self.hover_file).flatten();
                    frame.render_widget(
                        crate::widgets::git_changes::GitChangesList {
                            entries: &self.diff_view.entries,
                            selected: self.diff_view.selected,
                            focused: self.focus.block() == FocusBlock::Files
                                && !modal_open
                                && !self.navigator_tab_row_focused,
                            hover: git_hover,
                        },
                        rows[1],
                    );
                    if self.diff_view.entries.is_empty() && rows[1].height > 3 {
                        frame.render_widget(
                            Paragraph::new(Line::styled(
                                "No staged or unstaged changes",
                                crate::theme::muted(),
                            )),
                            ratatui::layout::Rect {
                                x: rows[1].x + 1,
                                y: rows[1].y,
                                width: rows[1].width.saturating_sub(2),
                                height: 1,
                            },
                        );
                    }
                } else {
                    frame.render_widget(
                        FileExplorerWidget {
                            explorer: &mut self.workspace_files.explorer,
                            active_file: active_file.as_deref(),
                            show_search: true,
                            focused: crate::widgets::background_focused(
                                matches!(
                                    self.focus.block(),
                                    FocusBlock::Files | FocusBlock::Search
                                ),
                                modal_open,
                            ) && !self.navigator_tab_row_focused,
                            search_active: self.focus.block() == FocusBlock::Search
                                && !modal_open
                                && !self.navigator_tab_row_focused,
                            hover: self.hover_file,
                        },
                        rows[1],
                    );
                    // The search field bleeds one cell into the shell inset
                    // (`file_explorer.rs`); widen the pointer area to match so
                    // that gutter stays clickable.
                    let bleed = rows[1].x.min(crate::design::PANE_PAD_X);
                    self.navigator_list_area = Some(ratatui::layout::Rect::new(
                        rows[1].x.saturating_sub(bleed),
                        rows[1].y,
                        rows[1].width.saturating_add(bleed),
                        rows[1].height,
                    ));
                }
                // Paint tabs last so the list cannot erase the active ground
                // on their shared divider.
                frame.render_widget(
                    crate::widgets::NavigatorTabs {
                        tab: navigator_tab,
                        needs_you,
                        git: self.navigator_git_available(),
                        focused: self.navigator_tab_row_focused && !modal_open,
                        hover: self.hover_navigator_tab,
                        row_stop: self.navigator_row_stop,
                        hover_new_session: self.hover_navigator_new_session,
                    },
                    tabs_area,
                );
            }
        }
        // How long a turn must have been running before the pinned status
        // line (`Waiting for the model…` / `Thinking…` / `Running {tool}…`)
        // appears — long enough that a near-instant response never flashes
        // it. Gated on `turn_started` (survives step boundaries), not
        // `started` (resets per step), so the line doesn't flicker
        // off-and-back-on at each tool call within one turn. Hiding is
        // never debounced — only entrance.
        const BUSY_STATUS_DEBOUNCE: Duration = Duration::from_millis(150);
        let busy_long_enough = self
            .timing
            .turn_started
            .is_some_and(|t| t.elapsed() >= BUSY_STATUS_DEBOUNCE);
        let child_task = self.child_view.as_ref().and_then(|view| {
            background_tasks
                .iter()
                .find(|task| task.id == view.task_id && task.run_id == view.run_id)
        });
        let conversation_session_id = self
            .child_view
            .as_ref()
            .map(|view| view.session_id)
            .unwrap_or(self.session_view.session_id);
        let conversation_lifecycle = child_task
            .map(|task| match task.status {
                forge_core::BackgroundTaskStatus::Queued => forge_types::TaskLifecycle::Ready,
                forge_core::BackgroundTaskStatus::Running => forge_types::TaskLifecycle::Working,
                forge_core::BackgroundTaskStatus::WaitingForApproval { .. } => {
                    forge_types::TaskLifecycle::Waiting
                }
                forge_core::BackgroundTaskStatus::Succeeded { .. } => {
                    forge_types::TaskLifecycle::Completed
                }
                forge_core::BackgroundTaskStatus::Failed { .. } => {
                    forge_types::TaskLifecycle::Failed
                }
                forge_core::BackgroundTaskStatus::Cancelled => {
                    forge_types::TaskLifecycle::Cancelled
                }
            })
            .unwrap_or_else(|| {
                if self.child_view.is_some() {
                    forge_types::TaskLifecycle::Ready
                } else {
                    self.session_view.lifecycle
                }
            });
        let conversation_busy = self.child_view.is_none() && self.busy_state.is_active();
        let stream_wait =
            if conversation_busy && !self.pending_turn.has_prompt() && busy_long_enough {
                let elapsed = if !self.stream.thinking.is_empty() {
                    // Thinking timer runs from first thinking token
                    self.timing
                        .thinking_started
                        .or(self.timing.started)
                        .map(|t| t.elapsed().as_secs_f64())
                        .unwrap_or(0.0)
                } else {
                    self.timing
                        .started
                        .map(|t| t.elapsed().as_secs_f64())
                        .unwrap_or(0.0)
                };
                // After answer tokens start, drop the wait/think status line.
                if !self.stream.preview.is_empty() {
                    None
                } else if !self.stream.thinking.is_empty() {
                    Some((StreamWaitPhase::Thinking, elapsed))
                } else {
                    Some((StreamWaitPhase::Waiting, elapsed))
                }
            } else {
                None
            };
        // Airy rhythm needs room to breathe; a short pane falls back to the
        // compact spacing so 80×18 keeps its content budget (FORGE-DESIGN §7.5).
        let conversation_rows = regions
            .sidebar
            .map(|rect| rect.height)
            .unwrap_or(regions.chat.height);
        let compact_conversation = conversation_rows < crate::design::AIRY_MIN_ROWS;
        let opts = ConversationViewOpts {
            busy: conversation_busy,
            // Don't force-expand finished thinking just because busy (answer may be streaming)
            tool_expanded: self.tool_detail.is_expanded(),
            compact: compact_conversation,
            stream_wait,
            stream_thought_secs: self
                .child_view
                .is_none()
                .then_some(self.timing.thought_secs)
                .flatten(),
            pulse_dim: self.child_view.is_none()
                && !self.runtime.reduced_motion
                && conversation_busy
                && crate::conversation::plan_pulse_dim(self.busy_state.throbber()),
        };
        // `/clear` only clears the viewport; the full session remains available to the model.
        let all_messages = self.transcript_view.messages();
        let all_events = self.transcript_view.events();
        let message_start = self.conversation_view.message_start.min(all_messages.len());
        let visible_messages = &all_messages[message_start..];
        let visible_events =
            &all_events[self.conversation_view.event_start.min(all_events.len())..];
        // DESIGN-005: absolute user ordinal of the window start, so per-turn
        // summaries recorded against the full session land on their turn.
        let window_user_base = all_messages[..message_start]
            .iter()
            .filter(|message| message.role == forge_types::MessageRole::User)
            .count() as u64;
        let session_id_string = self.session_view.session_id.to_string();
        if self
            .render_cache
            .turn_summaries
            .as_ref()
            .is_some_and(|cache| {
                cache.session_id != self.session_view.session_id
                    || cache.revision != self.turn_summaries_revision
            })
        {
            self.render_cache.turn_summaries = None;
        }
        if self.render_cache.turn_summaries.is_none() {
            let summaries: Vec<_> = self
                .turn_summaries
                .iter()
                .filter(|record| record.key.session == session_id_string)
                .map(|record| (record.key.user_ordinal, record.summary.clone()))
                .collect();
            let digest = summaries
                .iter()
                .map(|(ordinal, summary)| {
                    (
                        *ordinal,
                        summary.secs.to_bits(),
                        summary.chars,
                        summary.tools,
                        summary.output_tokens,
                    )
                })
                .collect();
            self.render_cache.turn_summaries = Some(TurnSummaryRenderCache {
                session_id: self.session_view.session_id,
                revision: self.turn_summaries_revision,
                summaries,
                digest,
            });
        }
        let (turn_summaries, turn_summary_digest) = self
            .render_cache
            .turn_summaries
            .as_ref()
            .map(|cache| (cache.summaries.clone(), cache.digest.clone()))
            .expect("turn summary cache populated");
        // Computed here, while the message slice is still borrowed: the home
        // splash is a landing page and stays at the top, but once someone has
        // actually said something the pane behaves like a conversation and
        // hugs the composer.
        let anchor_bottom = visible_messages
            .iter()
            .any(|message| message.role == forge_types::MessageRole::User);
        let activity_summary = self
            .child_view
            .is_none()
            .then(|| self.activity_summary())
            .flatten();
        let activity_summary_key = self
            .child_view
            .is_none()
            .then(|| self.activity_summary_cache_key())
            .flatten();
        // Hiding a retained pane must not rebuild its history at zero width or
        // clamp its reading position against an invisible viewport.
        let sidebar_width = regions.sidebar.map(|r| r.width).unwrap_or_else(|| {
            self.render_cache
                .conversation
                .as_ref()
                .map(|cache| cache.key.width)
                .unwrap_or(60)
        });
        let sidebar_inner_h = regions.sidebar.map(|r| r.height as usize).unwrap_or(0);
        // Follow-mode only paints the viewport plus overscan. Scrolling up
        // raises the window so earlier blocks are materialized on demand. The
        // bucket keeps each small scroll from rebuilding the transcript.
        const TRANSCRIPT_OVERSCAN: usize = 64;
        const TRANSCRIPT_SCROLL_BUCKET: usize = 64;
        let requested_keep_from_end = if self.conversation_view.follow {
            sidebar_inner_h.saturating_add(TRANSCRIPT_OVERSCAN)
        } else {
            sidebar_inner_h
                .saturating_add(self.conversation_view.scroll as usize)
                .saturating_add(TRANSCRIPT_OVERSCAN)
        }
        .max(1);
        let window_keep_from_end = requested_keep_from_end
            .saturating_add(TRANSCRIPT_SCROLL_BUCKET - 1)
            / TRANSCRIPT_SCROLL_BUCKET
            * TRANSCRIPT_SCROLL_BUCKET;
        let (question_selected, question_custom) = self.question_selection_key();
        let mut key = ConversationRenderKey {
            session_id: conversation_session_id,
            transcript_revision: self.transcript_view.revision(),
            width: sidebar_width,
            compact: compact_conversation,
            messages: visible_messages.len(),
            last_message_content: visible_messages
                .last()
                .map_or(0, |message| message.content.len()),
            last_message_thinking: visible_messages
                .last()
                .and_then(|message| message.thinking.as_ref())
                .map_or(0, String::len),
            events: visible_events.len(),
            last_event_detail: visible_events.last().map_or(0, |event| event.detail.len()),
            banners: if self.child_view.is_none() {
                self.banner_state.items.len()
            } else {
                1
            },
            // DESIGN-005: per-turn summaries digest. Counting records is not
            // enough: a re-recorded turn leaves the count unchanged, so the
            // previous duration would stay on screen through the next one.
            turn_summaries: turn_summary_digest,
            chat_message_start: self.conversation_view.message_start,
            chat_event_start: self.conversation_view.event_start,
            keep_from_end: if (!self.conversation_view.follow && self.child_view.is_some())
                || self.conversation_view.restore_lines.is_some()
                || self.conversation_view.restore_top.is_some()
            {
                usize::MAX
            } else {
                window_keep_from_end
            },
            activity_summary: activity_summary_key,
            tool_expanded: self.tool_detail.is_expanded(),
            splash_dismissed: self.conversation_view.splash_dismissed,
            home_card: (!slash_mode && !self.conversation_view.splash_dismissed).then(|| {
                (
                    connected,
                    self.selected_model_label(),
                    vendor_label.clone().unwrap_or_else(|| provider.clone()),
                    self.session_view.loaded_skills_count,
                )
            }),
            slash_mode,
            status: conversation_lifecycle,
            theme_id: crate::theme::active(),
            pending_hitl: self
                .selected_pending_hitl()
                .map(|payload| payload.call_id.clone()),
            approval_menu_selected: self.approval_menu_selected(),
            approval_focused: self.focus.block() == FocusBlock::Approval,
            pending_question: self
                .selected_pending_question()
                .map(|payload| payload.call_id.clone()),
            question_idx: self.question_menu_indexes().0,
            question_option_idx: self.question_menu_indexes().1,
            question_selected,
            question_custom,
        };
        // A complete cache already contains every settled line. Keep it
        // complete while scrolling instead of rebuilding a smaller tail, but
        // only when all other inputs still match. A changed transcript may no
        // longer fit in the old complete cache.
        let same_complete_cache = self
            .render_cache
            .conversation
            .as_ref()
            .is_some_and(|cache| {
                if !cache.complete {
                    return false;
                }
                let mut previous_key = cache.key.clone();
                previous_key.keep_from_end = window_keep_from_end;
                previous_key == key
            });
        if same_complete_cache {
            key.keep_from_end = usize::MAX;
        }
        let cache_keep_from_end = key.keep_from_end;
        if self
            .render_cache
            .conversation
            .as_ref()
            .map(|cache| &cache.key)
            != Some(&key)
        {
            let projection_key = (
                conversation_session_id,
                self.transcript_view.revision(),
                message_start,
                visible_messages.len(),
                conversation_lifecycle,
            );
            let projection_opts = ConversationViewOpts {
                busy: false,
                stream_wait: None,
                stream_thought_secs: None,
                pulse_dim: false,
                ..opts.clone()
            };
            let mut projection = self
                .render_cache
                .projection
                .take()
                .filter(|cached| cached.key == projection_key)
                .unwrap_or_else(|| ConversationProjectionCache {
                    key: projection_key,
                    model: ConversationModel::from_messages(
                        visible_messages,
                        visible_events,
                        conversation_lifecycle,
                        projection_opts.clone(),
                    ),
                });
            projection.model.opts = projection_opts;
            let projected_len = projection.model.items.len();
            let mut conv = projection.model;
            if let Some(task) = child_task {
                conv = conv.with_extra_banners([ChatItem::Banner {
                    text: super::turn::child_reading_banner(
                        task,
                        conversation_text_width(sidebar_width),
                    ),
                    kind: BannerKind::Info,
                }]);
            } else if self.child_view.is_none() {
                conv = conv
                    .with_turn_summaries(turn_summaries, window_user_base)
                    .with_extra_banners(self.banner_state.items.iter().cloned());
            } else if let Some(view) = self.child_view.as_ref() {
                conv = conv.with_extra_banners([ChatItem::Banner {
                    text: format!("Read-only child inspection · {}\nParent: {}\nViewed execution unavailable or replaced · ← return to inspect the current task", view.label, view.owner),
                    kind: BannerKind::Warn,
                }]);
            }
            let decorated_len = conv.items.len();
            if !slash_mode && !self.conversation_view.splash_dismissed {
                conv = conv.with_home(
                    crate::widgets::status::shorten_home_path(&self.runtime.cwd),
                    self.session_view.loaded_skills_count,
                    self.selected_model_label(),
                    vendor_label.clone().unwrap_or_else(|| provider.clone()),
                    connected,
                );
            }
            let home_added = conv.items.len() - decorated_len;
            let mut activity_index = None;
            if let Some(summary) = activity_summary {
                conv =
                    conv.with_activity_summary(summary.label, summary.action_label, summary.kind);
                activity_index = conv.items.iter().position(|item| {
                    matches!(item, crate::conversation::ChatItem::ActivitySummary { .. })
                });
            }
            self.sync_approval_menu();
            self.sync_question_menu();
            if let Some(payload) = self.selected_pending_hitl().cloned() {
                let rows = self.approval_menu_rows();
                let selected = self.approval_menu_selected();
                let approval_focused = self.focus.block() == FocusBlock::Approval;
                let cwd = self.session_view.workspace_root().display().to_string();
                let request = crate::overlays::ApprovalOverlayState::request_view(&payload, cwd);
                conv = conv.with_pending_approval(request, rows, selected, approval_focused);
            }
            if let Some(presentation) = self.question_presentation() {
                conv = conv.with_pending_question(presentation);
            }
            let width = conversation_text_width(sidebar_width);
            let (lines, plan_dock, complete) =
                conv.lines_and_plan_dock_with_completeness(width, cache_keep_from_end);
            if let Some(index) = activity_index {
                conv.items.remove(index);
            }
            if home_added > 0 {
                if let Some(index) = conv
                    .items
                    .iter()
                    .position(|item| matches!(item, crate::conversation::ChatItem::Home { .. }))
                {
                    conv.items.remove(index);
                }
            }
            conv.items.truncate(projected_len);
            conv.turn_summaries.clear();
            projection.model = conv;
            self.render_cache.projection = Some(projection);
            let mut cache_key = key;
            if complete {
                cache_key.keep_from_end = usize::MAX;
            }
            self.render_cache.conversation = Some(ConversationRenderCache {
                key: cache_key,
                lines: Arc::new(lines),
                complete,
                plan_dock,
            });
        }

        if regions.queue.height > 0 {
            self.queue_area = Some(regions.queue);
            frame.render_widget(
                QueuedMessages {
                    messages: &queued_messages,
                    selected: queue_selected,
                    hover: self.hover_queue,
                },
                regions.queue,
            );
            self.dock_paint.queue_ids = queued_ids;
            self.dock_paint.queue_selected = queue_selected;
        } else {
            self.queue_area = None;
        }

        // The background strip. Only drawn when the layout gave it room: a
        // zero-height region means the transcript needed it more.
        if regions.background.height > 1 {
            self.dock_paint.background = tasks_strip::BackgroundStrip::window(
                &background_tasks,
                strip_now,
                if compact_dock {
                    1
                } else {
                    tasks_strip::STRIP_ROW_CAP
                },
                regions.background.height,
                self.task_selection.task(self.selected_session_id),
                self.dismissed_background.get(&self.selected_session_id),
            );
            self.background_area = Some(regions.background);
            frame.render_widget(
                BackgroundStripWidget {
                    strip: &self.dock_paint.background,
                    selected: self.task_selection.task(self.selected_session_id),
                    hover: self.hover_background,
                    focused: self.focus.block() == FocusBlock::Sidebar,
                },
                regions.background,
            );
        } else {
            self.background_area = None;
        }
        let width = conversation_text_width(sidebar_width);
        // Tail-only changes use `StreamMarkdownCache` and must become visible
        // on the frame that received them. Only width changes re-render the
        // settled prefix, so only resize churn is rate-limited.
        const STREAM_PREVIEW_RENDER_INTERVAL: Duration = Duration::from_millis(150);
        let live_lines = if self.child_view.is_none()
            && self.busy_state.is_active()
            && !self.pending_turn.has_prompt()
        {
            let key = (
                width as u16,
                self.stream.thinking.len(),
                // Every available delta is visible on this frame.
                self.stream.preview.len(),
            );
            let key_matches = self
                .stream
                .live_lines
                .as_ref()
                .map(|(w, t, p, _)| (*w, *t, *p) == key)
                .unwrap_or(false);
            let width_changed = self
                .stream
                .live_lines
                .as_ref()
                .is_some_and(|(cached_width, ..)| *cached_width != key.0);
            let resize_ready = self
                .stream
                .last_preview_render
                .map(|at| at.elapsed() >= STREAM_PREVIEW_RENDER_INTERVAL)
                .unwrap_or(true);
            if key_matches || (width_changed && !resize_ready) {
                self.stream
                    .live_lines
                    .as_ref()
                    .map(|(.., lines)| Arc::clone(lines))
                    .unwrap_or_else(|| Arc::new(Vec::new()))
            } else {
                let mut lines = crate::conversation::render_streaming_preview(
                    &self.stream.thinking,
                    &self.stream.preview,
                    opts.stream_thought_secs,
                    width,
                    window_keep_from_end,
                    &mut self.stream.markdown,
                    crate::conversation::transcript_density(compact_conversation),
                );
                open_preview_above_streamed_thinking(
                    &mut lines,
                    &self.stream.thinking,
                    self.render_cache
                        .conversation
                        .as_ref()
                        .and_then(|cache| cache.lines.last()),
                );
                let lines = Arc::new(lines);
                self.stream.live_lines = Some((key.0, key.1, key.2, Arc::clone(&lines)));
                if width_changed || self.stream.last_preview_render.is_none() {
                    self.stream.last_preview_render = Some(Instant::now());
                }
                lines
            }
        } else {
            Arc::new(Vec::new())
        };
        // The live turn line. Built fresh every frame — the elapsed tick
        // moves it, so the caches above (both keyed by content length)
        // would freeze it — and cheap enough to be: one line, no markdown,
        // no wrapping.
        let status_lines: Vec<crate::links::HyperlinkLine> = if self.child_view.is_none()
            && self.busy_state.is_active()
            && !self.pending_turn.has_prompt()
            && busy_long_enough
        {
            // The whole turn's age, not the current step's: `started` is reset
            // at every continuation, so a turn that ran three tools kept
            // restarting its clock.
            let elapsed = self
                .timing
                .turn_started
                .or(self.timing.started)
                .map(|at| at.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            // The live buffers are cleared at every tool call, so they say
            // what is arriving *now*. The turn's running total comes from the
            // counter that survives a step boundary — otherwise the volume
            // fell back to zero at each tool and the phase read "Waiting for
            // the model" for work that had already streamed.
            let heard_from_model = !self.stream.thinking.is_empty() || self.timing.chars > 0;
            let model = crate::widgets::TurnLineModel {
                verb: crate::widgets::phase_verb(
                    &self.busy_state.phase(),
                    heard_from_model,
                    !self.stream.preview.is_empty(),
                ),
                elapsed_secs: elapsed,
                // Esc interrupts a running turn; it does not interrupt a
                // turn that is already blocked waiting for the operator.
                interruptible: !self.session_view.is_awaiting_approval()
                    && !self.session_view.is_awaiting_question(),
                reduced_motion: self.runtime.reduced_motion,
            };
            let millis = self.animation_started.elapsed().as_millis();
            // The pane paints inside a bordered block with one column of
            // padding each side, so the line has 2 fewer columns than the
            // prose width — without this the interrupt hint was clipped to
            // "esc to interru".
            let line_width = width.saturating_sub(2);
            vec![
                Line::from("").into(),
                crate::widgets::turn_line(&model, line_width, millis).into(),
            ]
        } else {
            Vec::new()
        };
        let cached = self
            .render_cache
            .conversation
            .as_ref()
            .expect("conversation cache populated");
        let cached_lines = Arc::clone(&cached.lines);
        let cached_complete = cached.complete;
        let plan_dock = cached.plan_dock.clone();
        // The retained conversation is independent of the resource history.
        if let Some(sidebar) = regions.sidebar {
            let sidebar_focused = crate::widgets::background_focused(
                self.focus.block() == FocusBlock::Sidebar,
                modal_open,
            );
            // The tab row carries identity and focus. Content has no enclosing
            // outline; the scroll indicator occupies its right padding.
            let sidebar_block = Block::default().style(theme::canvas());
            let conversation_area = sidebar_block.inner(sidebar);
            self.conversation_area = Some(conversation_area);
            let restoring_reader = self.conversation_view.restore_lines.is_some()
                || self.conversation_view.restore_top.is_some();
            let bottom_padding = if self.session_view.is_awaiting_approval() {
                0
            } else if theme_picking {
                1
            } else {
                // §3: state-aware idle padding — never derived from composer
                // height, so content isn't displaced by input size. At small
                // heights (≤18) or during decisions, no trailing dead zone.
                1
            };
            if let Some(anchor) = self
                .conversation_view
                .restore_lines
                .take()
                .filter(|rows| !rows.is_empty())
            {
                let rows: Vec<_> = cached_lines
                    .iter()
                    .chain(live_lines.iter())
                    .chain(status_lines.iter())
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect();
                let mut best = None;
                for (top, _) in rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.trim_end() == anchor[0].trim_end())
                {
                    let matched = rows[top..]
                        .iter()
                        .zip(&anchor)
                        .take_while(|(row, old)| row.trim_end() == old.trim_end())
                        .count();
                    if best.is_none_or(|(count, _)| matched > count) {
                        best = Some((matched, top));
                    }
                }
                if let Some((_, top)) = best {
                    self.conversation_view.restore_top = Some(top);
                }
            }
            if let Some(top) = self
                .conversation_view
                .restore_top
                .take()
                .filter(|_| cached_complete)
            {
                let total = cached_lines.len()
                    + live_lines.len()
                    + status_lines.len()
                    + bottom_padding as usize;
                self.conversation_view.scroll = total
                    .saturating_sub(conversation_area.height as usize)
                    .saturating_sub(top)
                    .min(u16::MAX as usize) as u16;
            }
            if cached_complete && live_lines.is_empty() && status_lines.is_empty() {
                let total = cached_lines.len().saturating_add(bottom_padding as usize);
                let max_scroll = total
                    .saturating_sub(conversation_area.height as usize)
                    .min(u16::MAX as usize) as u16;
                self.conversation_view.scroll = self.conversation_view.scroll.min(max_scroll);
                if max_scroll == 0 {
                    self.conversation_view.follow = true;
                }
            }
            sidebar_block.render(sidebar, frame.buffer_mut());
            theme::fill(conversation_area, frame.buffer_mut(), theme::canvas());
            render_conversation_scrollbar(
                sidebar,
                conversation_area,
                cached_lines.len()
                    + live_lines.len()
                    + status_lines.len()
                    + bottom_padding as usize,
                self.conversation_view.scroll,
                sidebar_focused,
                frame.buffer_mut(),
            );
            let option_sink = std::cell::RefCell::new(Vec::new());
            frame.render_widget(
                crate::conversation::ConversationLinesWidget {
                    lines: &cached_lines,
                    tail_lines: &live_lines,
                    status_lines: &status_lines,
                    hyperlinks: crate::links::hyperlinks_enabled(),
                    anchor_bottom,
                    scroll: self.conversation_view.scroll,
                    follow: self.conversation_view.follow,
                    bottom_padding,
                    plan_dock: plan_dock.as_ref(),
                    option_sink: Some(&option_sink),
                    hover_option: self.hover_option,
                },
                conversation_area,
            );
            self.option_rects = option_sink.into_inner();
            if self.focus.block() == FocusBlock::Approval
                && self.selected_pending_hitl().is_some()
                && !modal_open
            {
                self.render_approval_surface(conversation_area, frame.buffer_mut());
            }
            let total = cached_lines.len()
                + live_lines.len()
                + status_lines.len()
                + bottom_padding as usize;
            let max_scroll = total.saturating_sub(conversation_area.height as usize);
            let previous_top = self.conversation_copy_top;
            self.conversation_copy_top = if self.conversation_view.follow {
                max_scroll
            } else {
                max_scroll.saturating_sub(self.conversation_view.scroll as usize)
            };
            let revision = {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                for line in cached_lines
                    .iter()
                    .chain(live_lines.iter())
                    .chain(status_lines.iter())
                {
                    line.spans.len().hash(&mut hash);
                    for span in &line.spans {
                        span.content.hash(&mut hash);
                    }
                }
                bottom_padding.hash(&mut hash);
                (if anchor_bottom {
                    (conversation_area.height as usize).saturating_sub(total)
                } else {
                    0
                })
                .hash(&mut hash);
                hash.finish()
            };
            let mut all_rows = if revision == self.conversation_text.revision {
                std::mem::take(&mut self.conversation_all_rows)
            } else {
                let mut rows = cached_lines
                    .iter()
                    .chain(live_lines.iter())
                    .chain(status_lines.iter())
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                rows.resize(total, String::new());
                if anchor_bottom && total < conversation_area.height as usize {
                    rows.splice(
                        0..0,
                        std::iter::repeat_n(
                            String::new(),
                            conversation_area.height as usize - total,
                        ),
                    );
                }
                rows
            };
            let mut text =
                crate::selection::RenderedText::capture(frame.buffer_mut(), conversation_area);
            text.revision = revision;
            if restoring_reader && text.same_cells(&self.conversation_text) {
                self.conversation_text.revision = revision;
                if self.selection.pane == Some(crate::selection::CopyPane::Conversation) {
                    let delta = (self.conversation_copy_top as i64 - previous_top as i64)
                        .clamp(i32::MIN as i64, i32::MAX as i64)
                        as i32;
                    self.selection.shift_rows(delta);
                }
            }
            let history_expanded = all_rows.len() >= self.conversation_all_rows.len()
                && !self.conversation_all_rows.is_empty()
                && all_rows[all_rows.len() - self.conversation_all_rows.len()..]
                    .iter()
                    .zip(&self.conversation_all_rows)
                    .all(|(new, old)| new.trim_end() == old.trim_end());
            // Autoscroll keeps logical transcript endpoints; other remappings
            // must not leave frozen copied text over newly painted content.
            if self.selection.is_dragging()
                && self.selection.pane == Some(crate::selection::CopyPane::Conversation)
                && ((text.revision != self.conversation_text.revision && !history_expanded)
                    || text.area != self.conversation_text.area)
            {
                self.selection.clear();
            } else if !self.selection.is_dragging() {
                text.validate_selection(
                    &self.conversation_text,
                    crate::selection::CopyPane::Conversation,
                    &mut self.selection,
                );
            }
            self.conversation_text = text;
            self.conversation_rows = self
                .conversation_text
                .rows
                .iter()
                .map(|cells| {
                    let mut row = String::with_capacity(conversation_area.width as usize);
                    for (_, _, text) in cells {
                        row.push_str(text.symbol());
                    }
                    row
                })
                .collect();
            // Capture the rendered viewport too: plan docks and approval/diff
            // cards can replace rows from the ordinary transcript line list.
            for (index, visible) in self.conversation_rows.iter().enumerate() {
                if let Some(row) = all_rows.get_mut(self.conversation_copy_top + index) {
                    row.clear();
                    row.push_str(visible);
                }
            }
            self.conversation_all_rows = all_rows;
        }
        // The approval decision now lives in the conversation itself (inline
        // transcript item) and the composer, so the center pane gets its full
        // height — no docked card carving out a strip at its bottom.
        if regions.chat.width > 0 && regions.chat.height > 0 {
            let chat_area = regions.chat;
            match self.workspace_navigation.current().clone() {
                None => {
                    self.render_empty_workspace(chat_area, frame.buffer_mut());
                }
                Some(WorkspaceView::File(_)) => {
                    let old_text = self.source_viewer.rendered_text.clone();
                    self.editor_viewport.height = chat_area.height;
                    if self.source_viewer.is_image() {
                        self.kitty_image_area = Some(ratatui::layout::Rect {
                            y: chat_area.y.saturating_add(1),
                            height: chat_area.height.saturating_sub(1),
                            ..chat_area
                        });
                    }
                    frame.render_widget(
                        SourceViewerWidget {
                            viewer: &mut self.source_viewer,
                            focused: crate::widgets::background_focused(
                                self.focus.block() == FocusBlock::Workspace,
                                modal_open,
                            ),
                            editor: self.editor_session.as_mut(),
                            editor_command: self.editor_command.as_deref(),
                            editor_message: self.editor_message.as_deref(),
                        },
                        chat_area,
                    );
                    self.source_viewer.rendered_text.validate_selection(
                        &old_text,
                        crate::selection::CopyPane::Editor,
                        &mut self.selection,
                    );
                }
                Some(WorkspaceView::GithubIssues) => {
                    let title = if self.focus.block() == FocusBlock::Workspace {
                        "> GitHub issues"
                    } else {
                        "GitHub issues"
                    };
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .title(title)
                        .border_style(if self.focus.block() == FocusBlock::Workspace {
                            crate::theme::active_panel_border()
                        } else {
                            crate::theme::panel_border()
                        });
                    let layout = ratatui::layout::Layout::vertical([
                        ratatui::layout::Constraint::Min(1),
                        ratatui::layout::Constraint::Length(2),
                    ])
                    .split(chat_area);
                    frame.render_widget(
                        Paragraph::new(self.github_view.text())
                            .style(crate::theme::text())
                            .wrap(ratatui::widgets::Wrap { trim: false })
                            .scroll((self.github_view.scroll, 0))
                            .block(block),
                        layout[0],
                    );
                    frame.render_widget(
                        Paragraph::new(
                            "↑↓ scroll · ←→ issue · / filter · r details · R reload
n preview task · p push + PR · l logs · a feedback · Esc back",
                        )
                        .style(crate::theme::metadata_style()),
                        layout[1],
                    );
                }
                Some(WorkspaceView::Diff) => {
                    // Read before the widget borrows `diff_view` mutably.
                    let sync = self.git_sync_tag();
                    let merge = self.git_merge_banner();
                    frame.render_widget(
                        crate::diff_view::DiffViewWidget {
                            view: &mut self.diff_view,
                            sync,
                            merge,
                            focused: crate::widgets::background_focused(
                                self.focus.block() == FocusBlock::Workspace,
                                modal_open,
                            ),
                        },
                        chat_area,
                    );
                }
            }
        }

        // Highlight a live drag-selection over the editor (sorted after the
        // source viewer so reverse-video is applied on top of its spans).
        if self.selection.active
            && self.selection.pane == Some(crate::selection::CopyPane::Editor)
            && self.current_workspace_is_file()
        {
            paint_rows_selection(
                frame.buffer_mut(),
                &self.selection,
                self.source_viewer.rendered_text.area,
            );
        }
        if self.selection.active
            && self.selection.pane == Some(crate::selection::CopyPane::Conversation)
        {
            let area = match self.selection.pane {
                Some(crate::selection::CopyPane::Conversation) => self.conversation_area,
                _ => None,
            };
            if let Some(area) = area {
                paint_rows_selection(frame.buffer_mut(), &self.selection, area);
            }
        }

        let interactive_terminal_output = self
            .interactive_terminal
            .as_ref()
            .map(|terminal| terminal.display_output());
        let interactive_terminal = self.interactive_terminal.as_ref();
        if regions.bottom_panel.height > 1 && self.bottom_panel.open {
            let terminal_area = ratatui::layout::Rect {
                x: regions
                    .bottom_panel
                    .x
                    .saturating_add(crate::widgets::input::TEXT_INSET),
                y: regions.bottom_panel.y.saturating_add(1),
                width: regions
                    .bottom_panel
                    .width
                    .saturating_sub(crate::widgets::input::TEXT_INSET.saturating_mul(2)),
                height: regions.bottom_panel.height.saturating_sub(1),
            };
            self.terminal_area = Some(terminal_area);
            self.terminal_rows = terminal_copy_rows(
                interactive_terminal_output.unwrap_or(""),
                terminal_area.height,
                interactive_terminal.is_some_and(|terminal| terminal.running),
                interactive_terminal.map(|terminal| terminal.shell.as_str()),
            );
        }
        frame.render_widget(
            BottomPanel {
                model: BottomPanelModel {
                    state: &self.bottom_panel,
                    busy_phase: &self.busy_state.phase(),
                    activity: &self.activity,
                    terminal_content: interactive_terminal_output.unwrap_or(""),
                    terminal_running: interactive_terminal.is_some_and(|terminal| terminal.running),
                    terminal_shell: interactive_terminal.map(|terminal| terminal.shell.as_str()),
                    terminal_cursor: interactive_terminal
                        .filter(|terminal| terminal.cursor_visible())
                        .map(InteractiveTerminal::cursor_position),
                },
                focused: crate::widgets::background_focused(
                    self.focus.block() == FocusBlock::BottomPanel,
                    modal_open,
                ),
            },
            regions.bottom_panel,
        );
        if let (Some(terminal), Some(area)) = (interactive_terminal, self.terminal_area) {
            terminal.render(
                area,
                frame.buffer_mut(),
                crate::widgets::background_focused(
                    self.focus.block() == FocusBlock::BottomPanel,
                    modal_open,
                ),
            );
        }
        if let Some(area) = self.terminal_area {
            let text = crate::selection::RenderedText::capture(frame.buffer_mut(), area);
            text.validate_selection(
                &self.terminal_text,
                crate::selection::CopyPane::Terminal,
                &mut self.selection,
            );
            self.terminal_text = text;
        }
        if self.selection.active {
            let visible = match self.selection.pane {
                Some(crate::selection::CopyPane::Editor) => {
                    self.editor_area.is_some() && !expand_conversation
                }
                Some(crate::selection::CopyPane::Conversation) => self.conversation_area.is_some(),
                Some(crate::selection::CopyPane::Terminal) => self.terminal_area.is_some(),
                None => false,
            };
            if !visible {
                self.selection.clear();
            }
        }
        if self.selection.active
            && self.selection.pane == Some(crate::selection::CopyPane::Terminal)
        {
            if let Some(area) = self.terminal_area {
                paint_rows_selection(frame.buffer_mut(), &self.selection, area);
            }
        }

        if let Some(group) = start_group.as_ref() {
            self.start_prompt_selected = self.start_prompt_selected.min(group.starters - 1);
            self.start_prompt_rows = group.render(
                frame.buffer_mut(),
                connected,
                self.start_prompt_selected,
                !modal_open && self.focus.block() == FocusBlock::Sidebar,
                self.hover_start_prompt,
            );
        }

        // Inline slash autocomplete above the input bar — full list with scroll window
        if self.overlay.is_none() && self.inline_search.is_none() {
            let suggestions = self.slash_suggestions();
            if !suggestions.is_empty() && self.input.text.starts_with('/') {
                let input = regions.input;
                let n = suggestions.len();
                let idx = self.slash_suggestions.selected.min(n.saturating_sub(1));
                // Use as much space above the input as possible. The cap was
                // eight, which showed 8 of 31 commands with twenty blank rows
                // above the palette on a normal-height terminal.
                let max_list = input.y.saturating_sub(2).clamp(1, SLASH_PALETTE_MAX_ROWS) as usize;
                let visible = n.min(max_list);
                // Scroll so the highlighted row stays on screen.
                let start = if n <= visible || idx < visible / 2 {
                    0
                } else if idx + (visible - visible / 2) >= n {
                    n - visible
                } else {
                    idx - visible / 2
                };
                // The detail row is only drawn when the selected row had to
                // truncate its description, so it only costs a row when it
                // has something to say.
                let inner_w = input.width.saturating_sub(2) as usize;
                let selected_row_width = format!(
                    "  {:<14} {}",
                    suggestions[idx].display_cmd(),
                    suggestions[idx].desc
                )
                .chars()
                .count();
                let truncated = selected_row_width > inner_w.saturating_sub(1);
                let h = (visible as u16).saturating_add(2 + u16::from(truncated));
                if input.y >= h {
                    let sug_area = ratatui::layout::Rect {
                        x: input.x,
                        y: input.y.saturating_sub(h),
                        width: input.width,
                        height: h,
                    };
                    // Pad rows so background fill spans the panel width (visible selection).
                    let mut lines: Vec<ratatui::text::Line> = suggestions
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(visible)
                        .map(|(i, it)| {
                            let marker = if i == idx { "> " } else { "  " };
                            let raw = format!("{marker}{:<14} {}", it.display_cmd(), it.desc);
                            let mut row = raw
                                .chars()
                                .take(inner_w.saturating_sub(1))
                                .collect::<String>();
                            while row.chars().count() < inner_w.saturating_sub(1) {
                                row.push(' ');
                            }
                            let style = if i == idx {
                                theme::selected_row()
                            } else {
                                theme::text()
                            };
                            ratatui::text::Line::from(ratatui::text::Span::styled(row, style))
                        })
                        .collect();
                    // The detail row exists to show a description the selected
                    // row had to truncate. When nothing was truncated it just
                    // repeats the line above it — and because it was never
                    // padded to the panel width, whatever it did not cover
                    // showed through from the transcript underneath.
                    if truncated {
                        let mut detail = format!("  {}", suggestions[idx].desc)
                            .chars()
                            .take(inner_w.saturating_sub(1))
                            .collect::<String>();
                        while detail.chars().count() < inner_w.saturating_sub(1) {
                            detail.push(' ');
                        }
                        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                            detail,
                            theme::dim(),
                        )));
                    }
                    let hint = crate::hints::hint_text(crate::hints::COMMANDS);
                    let title = if n > visible {
                        format!(
                            " Commands {}–{} of {} · {hint} ",
                            start + 1,
                            start + visible,
                            n
                        )
                    } else {
                        format!(" Commands ({n}) · {hint} ")
                    };
                    frame.render_widget(ratatui::widgets::Clear, sug_area);
                    // The palette floats over the transcript, so the click has
                    // to be claimed before the transcript's own area can take
                    // focus away from the composer.
                    let row_area = ratatui::layout::Rect {
                        x: sug_area.x.saturating_add(1),
                        width: sug_area.width.saturating_sub(2),
                        height: 1,
                        ..sug_area
                    };
                    for index in start..start + visible {
                        self.slash_popup_rows.push((
                            index,
                            ratatui::layout::Rect {
                                y: sug_area.y + 1 + (index - start) as u16,
                                ..row_area
                            },
                        ));
                    }
                    frame.render_widget(
                        Paragraph::new(lines).block(
                            ratatui::widgets::Block::default()
                                .borders(ratatui::widgets::Borders::ALL)
                                .border_style(theme::brand())
                                .style(theme::panel())
                                .title(ratatui::text::Span::styled(title, theme::brand())),
                        ),
                        sug_area,
                    );
                }
            }
        }

        let attachment_label = {
            let file = self.attachment.file().map(|a| a.label());
            let images = self.pending_image_label();
            match (file, images) {
                (Some(file), Some(images)) => Some(format!("{file} · {images}")),
                (Some(file), None) => Some(file),
                (None, Some(images)) => Some(images),
                (None, None) => None,
            }
        };
        let model_label = self.selected_model_label();
        let effort_label = self
            .reasoning_effort
            .value
            .display_label(&model_label)
            .to_string();
        // Disconnect and first-run states intentionally have no model. Do not
        // render that empty selection as the fake provider/model id `model/`.
        let llm_label = if model_label.trim().is_empty() {
            vendor_label.unwrap_or_else(|| "No model".into())
        } else {
            format!(
                "{}/{}",
                vendor_label.as_deref().unwrap_or("model"),
                footer_short_model_id(&model_label)
            )
        };
        // Only three focusable footer controls now (which-LLM, effort, mode).
        if let Some(idx) = self.composer_chip_focus {
            self.composer_chip_focus = Some(idx.min(1));
        }
        if theme_picking {
            self.composer_area = None;
            if let Some(Overlay::Theme {
                selected,
                current,
                items,
            }) = self.overlay.as_ref()
            {
                crate::overlays::render_theme_dock(
                    *selected,
                    current,
                    items,
                    regions.input,
                    frame.buffer_mut(),
                    Some(&self.overlay_rows),
                    self.hover_overlay,
                );
                if let Some((id, _)) = items.get(*selected) {
                    let host = if regions.chat.width >= 24 {
                        regions.chat
                    } else {
                        regions.sidebar.unwrap_or(area)
                    };
                    let preview_area = crate::overlays::theme_preview_card(host);
                    crate::theme_preview::render_theme_preview(
                        id,
                        preview_area,
                        frame.buffer_mut(),
                    );
                }
            }
        } else {
            self.composer_area = Some(regions.input);
            let composer_focused = crate::widgets::background_focused(
                self.child_view.is_none()
                    && self.focus.mode() == FocusMode::Navigation
                    && self.focus.block() == FocusBlock::Composer,
                modal_open,
            );
            frame.render_widget(
                InputBar {
                    model: &self.input,
                    attachment: attachment_label.as_deref(),
                    dimmed: (self.busy_state.is_active() && self.input.text.is_empty())
                        || self.session_view.is_awaiting_approval(),
                    focused: composer_focused,
                },
                regions.input,
            );
            if composer_focused {
                if let Some((x, y)) = composer_cursor_position(
                    &self.input,
                    regions.input,
                    attachment_label.as_deref(),
                ) {
                    frame.set_cursor_position((x, y));
                }
            }
        }
        // Floating search owns its cells after the task group and composer
        // have painted; otherwise the start screen covers the selected row.
        if self.overlay.is_none() && self.inline_search.is_some() {
            self.render_inline_search(frame, regions.input);
        }

        // Read before the immutable borrows that build the footer model. The
        // count is cached after the first read, so this is not a per-frame
        // filesystem hit.
        let scratchpad_chip = self
            .scratchpad_status()
            .map(|(lines, dirty)| crate::widgets::footer::ScratchpadChip { lines, dirty });

        let footer = FooterModel {
            hints: contextual_hint.unwrap_or_default(),
            // The footer's own per-chip hint and the task strip's session
            // hint share the row with the chips; every other hint source
            // (HITL/dialog/transient) is blocking and takes the whole row.
            hint_replaces_row: !((self.focus.mode() == FocusMode::Navigation
                && self.focus.block() == FocusBlock::Footer)
                || (self.focus.mode() == FocusMode::Navigation
                    && self.focus.block() == FocusBlock::TaskStrip)),
            hint_bold_keys: self.focus.mode() == FocusMode::Navigation
                && self.focus.block() == FocusBlock::TaskStrip,
            llm_label,
            llm_connected: connected,
            effort_label,
            focus: if modal_open {
                None
            } else {
                self.composer_chip_focus.map(|idx| match idx {
                    0 => FooterFocus::Llm,
                    _ => FooterFocus::Effort,
                })
            },
            dimmed: self.session_view.is_awaiting_approval(),
            lifecycle: status.turn_lifecycle(),
            lifecycle_detail: status.incomplete_checks.clone(),
            throbber: self.busy_state.throbber().clone(),
            ctx_pct: status.ctx_pct,
            prompt_tokens: self.session_view.prompt_tokens,
            completion_tokens: self.session_view.completion_tokens,
            prompt_cache_reads: self.session_view.prompt_cache_hits,
            activity: FooterActivity {
                queued: queued_messages.len(),
                ..footer_activity(&background_tasks)
            },
            hover_chip: self.hover_chip,
            scratchpad: scratchpad_chip,
        };
        self.footer_area = Some(regions.footer);
        self.dock_paint.chips =
            crate::widgets::footer::activity_chip_regions(&footer.activity, regions.footer);
        let footer_chip_sink = std::cell::RefCell::new(None);
        frame.render_widget(
            FooterBar {
                model: &footer,
                chip_sink: Some(&footer_chip_sink),
            },
            regions.footer,
        );
        self.footer_chip_rects = footer_chip_sink.into_inner();

        // Resize grips paint after every pane, so the marker lands on the seam
        // the panes left blank; dialogs, overlays, menus and the toast below
        // paint over them.
        self.render_resize_handles(frame);

        if let Some(dialog) = self.explorer_dialog.current() {
            self.render_explorer_dialog(dialog, area, frame.buffer_mut());
        } else if self.scratchpad.is_some() {
            self.render_scratchpad(area, frame.buffer_mut());
        } else if let Some(ref ov) = self.overlay {
            match ov {
                Overlay::Help => self.render_help_overlay(area, frame.buffer_mut()),
                Overlay::Tasks { .. } => self.render_tasks_view(area, frame.buffer_mut()),
                Overlay::TaskStop { .. } => self.render_task_stop(area, frame.buffer_mut()),
                Overlay::ChildApproval { .. } => {
                    self.render_child_approval(area, frame.buffer_mut())
                }
                // Theme dock already replaced the composer band above.
                Overlay::Theme { .. } => {}
                _ => frame.render_widget(
                    OverlayWidget {
                        overlay: ov,
                        row_sink: Some(&self.overlay_rows),
                        hover_row: self.hover_overlay,
                    },
                    area,
                ),
            }
        }

        // The right-click context menu is the topmost interactive layer.
        if let Some(menu) = self.context_menu.as_mut() {
            menu.fit(area);
            render_context_menu(frame.buffer_mut(), menu);
        }

        // Transient toast overlay paints last: notification only, never
        // focusable, never blocking. Positioned bottom-right by the engine.
        self.toast.render_overlay(area, frame.buffer_mut());
        theme::strip_colors_if_disabled(frame.buffer_mut());
    }

    /// Mark every boundary the operator can drag with a short dashed grip.
    ///
    /// The panes draw their own frames and each seam sits directly against
    /// one, so a full-length rule here would read as a second border
    /// (FORGE-DESIGN §9.2). A centred grip names the seam without drawing
    /// one. It brightens under the pointer and while that boundary is being
    /// dragged, so the affordance survives terminals that report no motion.
    fn render_resize_handles(&self, frame: &mut ratatui::Frame) {
        for (boundary, vertical) in [
            (ResizeBoundary::Files, true),
            (ResizeBoundary::Conversation, true),
            (ResizeBoundary::BottomPanel, false),
        ] {
            let Some(seam) = self.resize_seam(boundary) else {
                continue;
            };
            let targeted = self.pane_resize.interaction.and_then(|i| i.boundary) == Some(boundary)
                || self.hover_resize == Some(boundary);
            paint_resize_grip(frame.buffer_mut(), seam, vertical, targeted);
        }
    }

    /// The blank 1-cell gap that separates a resizable pane from its
    /// neighbour, from the last layout pass.
    pub(super) fn resize_seam(&self, boundary: ResizeBoundary) -> Option<ratatui::layout::Rect> {
        match boundary {
            ResizeBoundary::Files => self.pane_resize.files_separator,
            ResizeBoundary::Conversation => self.pane_resize.conversation_separator,
            ResizeBoundary::BottomPanel => self.pane_resize.bottom_separator,
        }
    }

    /// Inline commands+history fuzzy search, drawn in the same anchored band
    /// above the composer as the slash palette. Rows get a `Commands` and a
    /// `History` header; the highlighted row is marked and `search_match`
    /// highlights the contiguous query run in command/history text.
    fn render_inline_search(&mut self, frame: &mut ratatui::Frame, input: ratatui::layout::Rect) {
        let Some(state) = self.inline_search.as_ref() else {
            return;
        };
        let items = self.inline_search_items();
        let query = state.query.clone();
        let selected = state.selected;

        enum Row {
            Header(&'static str),
            Item(usize),
        }
        let mut rows: Vec<Row> = Vec::new();
        let (mut commands_header, mut history_header) = (false, false);
        for (index, item) in items.iter().enumerate() {
            match item {
                InlineSearchItem::Command(_) if !commands_header => {
                    rows.push(Row::Header("Commands"));
                    commands_header = true;
                }
                InlineSearchItem::History(_) if !history_header => {
                    rows.push(Row::Header("History"));
                    history_header = true;
                }
                _ => {}
            }
            rows.push(Row::Item(index));
        }
        if rows.is_empty() {
            return;
        }

        let inner_w = input.width.saturating_sub(2) as usize;
        let budget = inner_w.saturating_sub(1);
        let max_list = input.y.saturating_sub(2).clamp(1, SLASH_PALETTE_MAX_ROWS) as usize;
        let total = rows.len();
        let visible = total.min(max_list);
        let sel_pos = rows
            .iter()
            .position(|row| matches!(row, Row::Item(index) if *index == selected))
            .unwrap_or(0);
        let start = if total <= visible || sel_pos < visible / 2 {
            0
        } else if sel_pos + (visible - visible / 2) >= total {
            total - visible
        } else {
            sel_pos - visible / 2
        };
        let height = (visible as u16).saturating_add(2);
        if input.y < height {
            return;
        }
        let area = ratatui::layout::Rect {
            x: input.x,
            y: input.y.saturating_sub(height),
            width: input.width,
            height,
        };
        let lines: Vec<Line<'static>> = rows
            .iter()
            .skip(start)
            .take(visible)
            .map(|row| match row {
                Row::Header(title) => {
                    let mut text = format!(" {title}");
                    while text.chars().count() < budget {
                        text.push(' ');
                    }
                    Line::from(Span::styled(text, theme::metadata_style()))
                }
                Row::Item(index) => {
                    let is_selected = *index == selected;
                    let base = if is_selected {
                        theme::selected_row()
                    } else {
                        theme::text()
                    };
                    let marker = if is_selected { "> " } else { "  " };
                    let mut spans = vec![Span::styled(marker, base)];
                    match &items[*index] {
                        InlineSearchItem::Command(item) => {
                            let cmd = item.display_cmd();
                            spans.extend(crate::file_explorer::highlight_name_spans(
                                &cmd, &query, base,
                            ));
                            let used: usize =
                                spans.iter().map(|span| span.content.chars().count()).sum();
                            let desc: String = item
                                .desc
                                .chars()
                                .take(budget.saturating_sub(used + 2))
                                .collect();
                            if !desc.is_empty() {
                                spans.push(Span::styled(
                                    format!("  {desc}"),
                                    if is_selected { base } else { theme::muted() },
                                ));
                            }
                        }
                        InlineSearchItem::History(text) => {
                            let text: String =
                                text.chars().take(budget.saturating_sub(2)).collect();
                            spans.extend(crate::file_explorer::highlight_name_spans(
                                &text, &query, base,
                            ));
                        }
                    }
                    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
                    if used < budget {
                        spans.push(Span::styled(" ".repeat(budget - used), base));
                    }
                    Line::from(spans)
                }
            })
            .collect();

        let title = if query.is_empty() {
            format!(" Search · {} · Esc close ", items.len())
        } else {
            format!(" Search: {query} · Esc close ")
        };
        // Section headers are painted but not selectable, so only the item
        // rows are recorded; the click has to be claimed before the transcript
        // underneath can take focus off the composer.
        for (position, row) in rows.iter().skip(start).take(visible).enumerate() {
            if let Row::Item(index) = row {
                self.inline_search_rows.push((
                    *index,
                    ratatui::layout::Rect {
                        x: area.x.saturating_add(1),
                        y: area.y + 1 + position as u16,
                        width: area.width.saturating_sub(2),
                        height: 1,
                    },
                ));
            }
        }
        frame.render_widget(ratatui::widgets::Clear, area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::brand())
                    .style(theme::panel())
                    .title(Span::styled(title, theme::brand())),
            ),
            area,
        );
    }

    /// Center-pane placeholder for when nothing is open — `workspace_navigation.current()`
    /// is `None`, since conversation no longer fills this pane by default.
    fn render_empty_workspace(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        // Vertically centered empty-workspace placeholder.
        render_centered_text(
            area,
            buf,
            "No file open\n\nSelect one from the explorer.",
            theme::muted(),
            theme::panel_border(),
        );
    }
}

fn terminal_copy_rows(
    content: &str,
    height: u16,
    running: bool,
    shell: Option<&str>,
) -> Vec<String> {
    if shell.is_some() {
        let mut rows: Vec<_> = content.lines().map(str::to_string).collect();
        if rows.len() > height as usize {
            rows = rows.split_off(rows.len() - height as usize);
        }
        rows.resize(height as usize, String::new());
        return rows;
    }
    let mut rows = vec![format!(
        "Interactive shell{}",
        if running { " · running" } else { " · exited" }
    )];
    if let Some(shell) = shell {
        rows.push(format!("$ {shell} -il"));
    }
    if !content.is_empty() {
        rows.extend(
            content
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(str::to_string),
        );
    }
    let visible = height as usize;
    if rows.len() > visible {
        rows = rows.split_off(rows.len() - visible);
    }
    while rows.len() < visible {
        rows.insert(0, String::new());
    }
    rows
}

/// Render text vertically centered inside the given area.
/// Both Editor and Diff empty states call this for identical vertical alignment.
fn render_centered_text(
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    text: &str,
    style: ratatui::style::Style,
    border: ratatui::style::Style,
) {
    // The bordered block needs the text rows plus its top and bottom border rows.
    let line_count = text.lines().count() as u16;
    let block_height = line_count.saturating_add(2);
    let vertical_pad = area.height.saturating_sub(block_height) / 2;

    let inner = ratatui::layout::Rect {
        x: area.x,
        y: area.y + vertical_pad,
        width: area.width,
        height: block_height,
    };

    Paragraph::new(text)
        .style(style)
        .alignment(ratatui::layout::Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border)
                .style(theme::panel()),
        )
        .render(inner, buf);
}

/// Paint a live drag-selection over a rows-based pane (Conversation, Diff,
/// Terminal) as reverse video, matching `paint_editor_selection`'s stream
/// shape rather than a literal rectangle: the first row runs from the
/// anchor column to the row's right edge, interior rows are highlighted in
/// full, and the last row runs from the row's left edge to the current
/// column — the same shape most terminal apps use, and how the text is
/// actually extracted in `visible_rows_selection_text`.
fn paint_rows_selection(
    buf: &mut ratatui::buffer::Buffer,
    sel: &crate::selection::MouseSelection,
    area: ratatui::layout::Rect,
) {
    use ratatui::style::Modifier;
    let Some(rect) = sel.rect() else {
        return;
    };
    let left_edge = area.x;
    let right_edge = area.right().saturating_sub(1);
    let mapped = crate::selection::RenderedText::capture(buf, area);
    for row in rect.row_start..=rect.row_end {
        if row < area.y || row >= area.bottom() {
            continue;
        }
        let left = if row == rect.row_start {
            rect.start_col.max(left_edge)
        } else {
            left_edge
        };
        let right = if row == rect.row_end {
            rect.end_col.min(right_edge)
        } else {
            right_edge
        };
        if left > right {
            continue;
        }
        for (col, width, _) in &mapped.rows[(row - area.y) as usize] {
            if *col > right || col.saturating_add(*width) <= left {
                continue;
            }
            for col in *col..col.saturating_add(*width).min(area.right()) {
                let cell = &mut buf[(col, row)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// Dashed glyph marking a vertical resize seam.
const RESIZE_GRIP_VERTICAL: &str = "┊";
/// Dashed glyph marking a horizontal resize seam.
const RESIZE_GRIP_HORIZONTAL: &str = "┈";
/// Grip length in cells. Odd, so it centres on the seam without a bias.
const RESIZE_GRIP_LEN: u16 = 5;

/// Paint a centred dashed grip into a 1-cell-wide resize seam.
///
/// At rest the grip takes the same `border` weight as the pane frames it sits
/// between: `border_muted` is reserved for low-priority internal separators,
/// and a marker whose whole job is to name the seam must not be the faintest
/// thing in it. The boundary being dragged (or pointed at) takes the accent
/// and a weight step, so the state is legible without colour (§5.3). A seam
/// with no room for a margin on both sides stays blank rather than filling end
/// to end, which would read as the rule §9.2 forbids.
fn paint_resize_grip(
    buf: &mut ratatui::buffer::Buffer,
    seam: ratatui::layout::Rect,
    vertical: bool,
    targeted: bool,
) {
    use ratatui::style::Modifier;

    let style = if targeted {
        theme::accent_style().add_modifier(Modifier::BOLD)
    } else {
        theme::border()
    };
    let glyph = if vertical {
        RESIZE_GRIP_VERTICAL
    } else {
        RESIZE_GRIP_HORIZONTAL
    };
    let extent = if vertical { seam.height } else { seam.width };
    if extent < RESIZE_GRIP_LEN + 2 {
        return;
    }
    let start = (extent - RESIZE_GRIP_LEN) / 2;
    for step in 0..RESIZE_GRIP_LEN {
        let (x, y) = if vertical {
            (seam.x, seam.y + start + step)
        } else {
            (seam.x + start + step, seam.y)
        };
        buf.set_string(x, y, glyph, style);
    }
}

/// Draw the right-click context menu as a small popover list.
fn render_context_menu(buf: &mut ratatui::buffer::Buffer, menu: &crate::selection::ContextMenu) {
    let rect = menu.rect();
    ratatui::widgets::Clear.render(rect, buf);
    let max_y = buf.area().height;
    let max_x = buf.area().width;
    for (i, item) in menu.items.iter().enumerate() {
        let y = menu.y.saturating_add(i as u16);
        if y >= max_y {
            break;
        }
        let label = match item {
            crate::selection::ContextMenuItem::Copy => "Copy selection",
            crate::selection::ContextMenuItem::ClearSelection => "Clear selection",
        };
        let style = if i == menu.selected {
            theme::selected_row()
        } else {
            theme::panel()
        };
        let mut text = format!(" {label}");
        while (text.chars().count() as u16) < rect.width.saturating_sub(2) {
            text.push(' ');
        }
        for (x_off, ch) in text.chars().enumerate() {
            let x = menu.x.saturating_add(x_off as u16);
            if x >= max_x {
                break;
            }
            let cell = &mut buf[(x, y)];
            cell.set_symbol(&ch.to_string()).set_style(style);
        }
    }
}

/// Columns available to conversation text inside its borderless surface.
/// The scrollbar lives in the right padding, outside the measured text body.
pub(crate) fn conversation_text_width(sidebar_width: u16) -> usize {
    sidebar_width.saturating_sub(2 * crate::design::PANE_PAD_X) as usize
}

/// Thin scroll indicator for the conversation. Painted in the
/// sidebar's right padding so it never overlaps transcript text, and only when
/// the content actually overflows. `scroll_from_bottom` is the view's own
/// offset (0 = pinned to the newest line). A focused conversation takes the
/// solid thumb and accent track to reinforce the tab's focus marker.
fn render_conversation_scrollbar(
    sidebar: ratatui::layout::Rect,
    text_area: ratatui::layout::Rect,
    total: usize,
    scroll_from_bottom: u16,
    focused: bool,
    buf: &mut ratatui::buffer::Buffer,
) {
    if sidebar.width == 0 || text_area.height == 0 || sidebar.right() <= text_area.right() {
        return;
    }
    let max_scroll = total.saturating_sub(text_area.height as usize);
    if max_scroll == 0 {
        return;
    }
    let position = max_scroll.saturating_sub((scroll_from_bottom as usize).min(max_scroll));
    let track = ratatui::layout::Rect::new(sidebar.right() - 1, text_area.y, 1, text_area.height);
    let mut state = ratatui::widgets::ScrollbarState::new(total)
        .viewport_content_length(text_area.height as usize)
        .position(position);
    let (thumb, thumb_style, track_style) = if focused {
        ("█", theme::accent_style(), theme::accent_style())
    } else {
        ("▐", theme::muted(), theme::border_muted())
    };
    ratatui::widgets::StatefulWidget::render(
        ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(track_style)
            .thumb_symbol(thumb)
            .thumb_style(thumb_style),
        track,
        buf,
        &mut state,
    );
}

/// The live preview is painted directly below the settled transcript, but it
/// renders as an isolated model and never sees the line above it. The
/// "major block boundary" blank that separates the other blocks is therefore
/// never applied at its leading edge: streamed reasoning used to hug the row
/// above — usually the last railed tool row. Mirror `ensure_blank_line` at
/// the seam: open the preview with a blank line unless the settled line above
/// (or the preview itself) is already blank.
fn open_preview_above_streamed_thinking(
    preview: &mut Vec<crate::links::HyperlinkLine>,
    streaming_thoughts: &str,
    line_above: Option<&crate::links::HyperlinkLine>,
) {
    if streaming_thoughts.trim().is_empty() {
        return;
    }
    let is_blank =
        |line: &crate::links::HyperlinkLine| line.spans.iter().all(|span| span.content.is_empty());
    let above_blank = line_above.is_none_or(is_blank);
    let preview_blank = preview.first().is_none_or(is_blank);
    if !above_blank && !preview_blank {
        preview.insert(0, Line::from("").into());
    }
}

/// Greedy word wrap for the navigator peek; `max_lines` caps the height and
/// marks truncation with an ellipsis.
fn wrap_to_width(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(4);
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for (index, word) in words.iter().enumerate() {
        let extra = usize::from(!current.is_empty());
        if !current.is_empty() && current.chars().count() + extra + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
            if lines.len() == max_lines {
                if index < words.len() {
                    if let Some(last) = lines.last_mut() {
                        last.push('…');
                    }
                }
                return lines;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Compact age for a session row: `just now`, `2m`, `3h`, `4d`.
fn relative_age(then: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - then).num_seconds().max(0);
    if secs < 60 {
        "just now".into()
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {

    use super::relative_age;

    #[test]
    fn relative_age_buckets_by_magnitude() {
        let now = chrono::Utc::now();
        assert_eq!(relative_age(now), "just now");
        assert_eq!(relative_age(now - chrono::Duration::seconds(120)), "2m");
        assert_eq!(
            relative_age(now - chrono::Duration::seconds(3 * 3600)),
            "3h"
        );
        assert_eq!(
            relative_age(now - chrono::Duration::seconds(2 * 86_400)),
            "2d"
        );
    }

    #[test]
    fn terminal_copy_rows_matches_screen_coordinates() {
        assert_eq!(
            super::terminal_copy_rows("first\nsecond", 5, true, Some("sh")),
            vec![
                "first".to_string(),
                "second".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            ]
        );
        assert_eq!(
            super::terminal_copy_rows("one\ntwo\nthree", 3, false, None),
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn resize_grip_centres_a_dashed_run_and_leaves_a_margin() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 11));
        super::paint_resize_grip(
            &mut buf,
            ratatui::layout::Rect::new(1, 0, 1, 11),
            true,
            false,
        );
        let column: String = (0..11).map(|y| buf[(1, y)].symbol()).collect();
        assert_eq!(column, "   ┊┊┊┊┊   ");
        let style = buf[(1, 5)].style();
        assert_eq!(style.fg, crate::theme::border().fg);
        assert!(style.add_modifier.is_empty());
    }

    #[test]
    fn resize_grip_takes_the_accent_and_weight_while_targeted() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 11));
        super::paint_resize_grip(
            &mut buf,
            ratatui::layout::Rect::new(1, 0, 1, 11),
            true,
            true,
        );
        let style = buf[(1, 5)].style();
        assert_eq!(style.fg, Some(crate::theme::accent_color()));
        assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn resize_grip_marks_a_row_and_skips_a_seam_with_no_margin() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 11, 2));
        super::paint_resize_grip(
            &mut buf,
            ratatui::layout::Rect::new(0, 0, 11, 1),
            false,
            false,
        );
        let row: String = (0..11).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, "   ┈┈┈┈┈   ");

        // Six cells cannot hold the grip and a margin on each side, and a grip
        // that filled them end to end would read as the rule §9.2 forbids.
        super::paint_resize_grip(
            &mut buf,
            ratatui::layout::Rect::new(0, 1, 6, 1),
            false,
            false,
        );
        let short: String = (0..6).map(|x| buf[(x, 1)].symbol()).collect();
        assert_eq!(short, "      ");
    }
}
