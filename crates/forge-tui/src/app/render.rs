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

impl TuiApp {
    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        // Advance the off-thread repo-header refresh. Cheap (a `try_recv` plus an
        // elapsed check); every draw path funnels through here, including the
        // streaming and `drain_pending_*` loops that bypass `run_loop`'s polls.
        self.poll_repo_header();
        let area = frame.area();
        if is_too_small(area) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
            self.file_explorer.focused = false;
            self.bottom_panel.focused = false;
            self.source_viewer.focused = false;
            self.invalidate_hit_regions();
            frame.render_widget(
                Paragraph::new("Terminal too small — resize to at least 40x18"),
                area,
            );
            return;
        }
        self.begin_hit_frame();
        crate::theme::fill(area, frame.buffer_mut(), crate::theme::canvas());
        let fb_h = if self.feedback.is_empty() { 0 } else { 1 };
        let slash_mode = self.overlay.is_none() && self.input.text.starts_with('/');
        let input_h = (self.input.visual_lines() + 2).clamp(3, 8);
        let panel_h = if self.bottom_panel.open { 8 } else { 0 };
        let contextual_hint = self.contextual_hint();
        let hint_h = u16::from(contextual_hint.is_some());
        let regions = split_areas_with_chrome(
            area,
            fb_h,
            input_h,
            !slash_mode && self.files_visible,
            !slash_mode && self.sidebar_visible,
            0,
            panel_h,
            hint_h,
        );
        // Layout can hide a requested side/bottom panel. Focus must follow the
        // rendered geometry rather than leaving an invisible key owner behind.
        let available = FocusAvailability {
            files: regions.files.is_some(),
            inspector: regions.sidebar.is_some(),
            bottom_panel: self.bottom_panel.open && regions.bottom_panel.height > 0,
        };
        if !available.contains(self.focus.block) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
        }
        self.normalize_focus();
        self.register_pane_hit_regions(&regions);
        let connected = self.is_provider_connected();
        let status = self.refresh_status_model_with_connected(connected);
        frame.render_widget(StatusBar { model: &status }, regions.status);
        if let Some(files) = regions.files {
            frame.render_widget(
                FileExplorerWidget {
                    explorer: &mut self.file_explorer,
                    focused: self.focus.block == FocusBlock::Files,
                },
                files,
            );
            self.register_file_hit_regions(files);
        }
        self.file_explorer.git_status.poll();

        let stream_wait = if self.busy && self.pending_prompt.is_none() {
            let elapsed = if !self.stream_thinking.is_empty() {
                // Thinking timer runs from first thinking token
                self.thinking_started
                    .or(self.turn_started)
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
            } else {
                self.turn_started
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
            };
            // After answer tokens start, drop the wait/think status line.
            if !self.stream_preview.is_empty() {
                None
            } else if !self.stream_thinking.is_empty() {
                Some((StreamWaitPhase::Thinking, elapsed))
            } else {
                Some((StreamWaitPhase::Waiting, elapsed))
            }
        } else {
            None
        };
        let opts = ConversationViewOpts {
            busy: self.busy,
            // Don't force-expand finished thinking just because busy (answer may be streaming)
            tool_expanded: self.tool_expanded,
            compact: false,
            stream_wait,
            stream_thought_secs: self.thought_secs,
        };
        // `/clear` only clears the viewport; the full session remains available to the model.
        let visible_messages =
            &self.session.messages[self.chat_message_start.min(self.session.messages.len())..];
        let visible_events =
            &self.session.events[self.chat_event_start.min(self.session.events.len())..];
        let activity_summary = self.activity_summary();
        let activity_summary_key = self.activity_summary_cache_key();
        let key = ConversationRenderKey {
            session_id: self.session.session_id,
            width: regions.chat.width,
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
            banners: self.ui_banners.len(),
            queue: self.session.queue.len(),
            queue_selected: self.queue_selected,
            chat_message_start: self.chat_message_start,
            chat_event_start: self.chat_event_start,
            busy: self.busy,
            busy_phase: self.busy_phase.label(),
            activity_summary: activity_summary_key,
            tool_expanded: self.tool_expanded,
            splash_dismissed: self.splash_dismissed,
            slash_mode,
            status: self.session.active_task.lifecycle,
            theme_id: crate::theme::active(),
        };
        if self.conversation_cache.as_ref().map(|cache| &cache.key) != Some(&key) {
            let mut conv = ConversationModel::from_messages(
                visible_messages,
                visible_events,
                self.session.active_task.lifecycle,
                ConversationViewOpts {
                    busy: false,
                    stream_wait: None,
                    stream_thought_secs: None,
                    ..opts.clone()
                },
            )
            .with_extra_banners(self.ui_banners.iter().cloned());
            if !self.splash_dismissed {
                conv = conv.with_brand(self.runtime.version.clone());
            }
            if !slash_mode && !self.splash_dismissed {
                conv = conv.with_home(
                    self.runtime.cwd.display().to_string(),
                    self.session.loaded_skills_count(),
                );
            }
            if let Some(summary) = activity_summary {
                conv =
                    conv.with_activity_summary(summary.label, summary.action_label, summary.kind);
            }
            conv = conv.with_queued_messages(
                self.session
                    .queue
                    .visible()
                    .map(|item| item.text.clone())
                    .collect::<Vec<_>>(),
                self.queue_selected,
            );
            if let BusyPhase::Tool { name } = &self.busy_phase {
                if name != "run" {
                    conv = conv.with_running_tool(name.clone());
                }
            }
            if let Some(payload) = self.session.pending_hitl() {
                let args = payload
                    .args_redacted
                    .get("command")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| payload.args_redacted.to_string());
                conv = conv.with_blocked_tool(payload.tool.clone(), args);
            }
            let width = regions.chat.width.saturating_sub(2) as usize;
            self.conversation_cache = Some(ConversationRenderCache {
                key,
                lines: Arc::new(conv.lines_for_width(width)),
            });
        }
        let width = regions.chat.width.saturating_sub(2) as usize;
        let live_lines = if self.busy && self.pending_prompt.is_none() {
            ConversationModel::from_messages(
                &[],
                &[],
                self.session.active_task.lifecycle,
                ConversationViewOpts { busy: true, ..opts },
            )
            .with_streaming_preview(self.stream_thinking.clone(), self.stream_preview.clone())
            .lines_for_width(width)
        } else {
            Vec::new()
        };
        let cached = self
            .conversation_cache
            .as_ref()
            .expect("conversation cache populated");
        // Clones the shared handle, not the line data. This exists so the
        // immutable borrow of `conversation_cache` ends before
        // `register_activity_summary_region` takes `&mut self` below.
        let cached_lines = Arc::clone(&cached.lines);
        match self.workspace_navigation.current.clone() {
            WorkspaceView::Conversation => {
                let conversation_area = ratatui::layout::Rect {
                    x: regions.chat.x.saturating_add(2.min(regions.chat.width)),
                    y: regions.chat.y.saturating_add(1.min(regions.chat.height)),
                    width: regions.chat.width.saturating_sub(2.min(regions.chat.width)),
                    height: regions
                        .chat
                        .height
                        .saturating_sub(1.min(regions.chat.height)),
                };
                frame.render_widget(
                    crate::conversation::ConversationLinesWidget {
                        lines: &cached_lines,
                        tail_lines: &live_lines,
                        scroll: self.chat_scroll,
                        follow: self.chat_follow,
                    },
                    conversation_area,
                );
                self.register_activity_summary_region(
                    conversation_area,
                    &cached_lines,
                    &live_lines,
                );
            }
            WorkspaceView::File(_) => {
                self.last_editor_height = regions.chat.height;
                frame.render_widget(
                    SourceViewerWidget {
                        viewer: &mut self.source_viewer,
                        focused: self.focus.block == FocusBlock::Workspace,
                    },
                    regions.chat,
                );
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => {
                self.render_diff_workspace(regions.chat, frame.buffer_mut());
            }
            WorkspaceView::Run(id) => {
                self.render_run_workspace(&id, regions.chat, frame.buffer_mut());
            }
        }
        if let Some(sidebar_area) = regions.sidebar {
            let activity = self
                .activity
                .recent(8)
                .iter()
                .map(|item| item.summary.clone())
                .collect::<Vec<_>>();
            let mut sidebar = SidebarModel::from_session_with_activity(&self.session, &activity);
            sidebar.provider = self.runtime.provider.clone();
            sidebar.model = self.runtime.model_label.clone();
            sidebar.effort = self.reasoning_effort.label().to_string();
            sidebar.route = self.connect.profile.clone();
            sidebar.busy = self.busy;
            sidebar.step = match &self.busy_phase {
                BusyPhase::Model => "model_stream",
                BusyPhase::Tool { .. } => "tool_execution",
                BusyPhase::Connect => "connect",
                BusyPhase::Other(step) => step,
                BusyPhase::Idle => "idle",
            }
            .into();
            sidebar.context_reset = self.context_reset_snapshot;
            sidebar.session_allows = self
                .hitl_session_allow
                .iter()
                .map(ApprovalIdentity::label)
                .collect();
            let header = self.repo_header();
            sidebar.repo_name = header.repo_name;
            sidebar.branch = header.branch;
            let gs = &self.file_explorer.git_status;
            sidebar.git_status_loading = gs.loading;
            sidebar.git_status_error = gs.error.is_some();
            sidebar.files_changed = Some(gs.status.len());
            sidebar.validation = self.run.current.as_ref().map(|record| {
                format!(
                    "Run {}",
                    match record.state {
                        RunState::Queued => "queued",
                        RunState::Running => "running",
                        RunState::Succeeded => "succeeded",
                        RunState::Failed => "failed",
                        RunState::Cancelled => "cancelled",
                        RunState::StartFailed => "start failed",
                        RunState::CaptureFailed => "capture failed",
                    }
                )
            });
            sidebar.elapsed = self
                .turn_started
                .or(self.thinking_started)
                .map(|started| format_elapsed_tenths(started.elapsed().as_secs_f64()));
            frame.render_widget(
                SidebarWidget {
                    model: &sidebar,
                    view: self.inspector_view,
                    focused: self.focus.block == FocusBlock::Inspector,
                },
                sidebar_area,
            );
        }

        frame.render_widget(
            BottomPanel {
                model: BottomPanelModel {
                    state: &self.bottom_panel,
                    busy_phase: &self.busy_phase,
                    activity: &self.activity,
                    run: &self.run,
                    background: &self.session.background,
                    tasks_selected: self.tasks_selected,
                    terminal_title: self.terminal_capture.title.as_deref(),
                    terminal_content: &self.terminal_capture.content,
                    terminal_truncated: self.terminal_capture.truncated,
                },
                focused: self.focus.block == FocusBlock::BottomPanel,
            },
            regions.bottom_panel,
        );

        // Notices (help, connect list, multi-line status) just above input
        if !self.notices.is_empty() && self.overlay.is_none() {
            let notice_h = (self.notices.len() as u16).min(18).saturating_add(1);
            // Render into bottom of chat area
            let chat = regions.chat;
            if chat.height > notice_h {
                let notice_area = ratatui::layout::Rect {
                    x: chat.x,
                    y: chat.y + chat.height.saturating_sub(notice_h),
                    width: chat.width,
                    height: notice_h,
                };
                let text = self
                    .notices
                    .iter()
                    .take(18)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                frame.render_widget(
                    Paragraph::new(text).style(theme::muted()).block(
                        ratatui::widgets::Block::default()
                            .borders(ratatui::widgets::Borders::TOP)
                            .style(theme::panel())
                            .title(ratatui::text::Span::styled(" notices ", theme::muted())),
                    ),
                    notice_area,
                );
            }
        }

        // Inline slash autocomplete above the input bar — full list with scroll window
        if self.overlay.is_none() {
            let suggestions = self.slash_suggestions();
            if !suggestions.is_empty() && self.input.text.starts_with('/') {
                let input = regions.input;
                let n = suggestions.len();
                let idx = self.slash_suggest_idx.min(n.saturating_sub(1));
                // Use as much space above the input as possible (cap for readability).
                let max_list = input.y.saturating_sub(2).clamp(1, 16) as usize;
                let visible = n.min(max_list);
                // Scroll so the highlighted row stays on screen.
                let start = if n <= visible || idx < visible / 2 {
                    0
                } else if idx + (visible - visible / 2) >= n {
                    n - visible
                } else {
                    idx - visible / 2
                };
                let h = (visible as u16).saturating_add(3); // borders + selected help
                if input.y >= h {
                    let sug_area = ratatui::layout::Rect {
                        x: input.x,
                        y: input.y.saturating_sub(h),
                        width: input.width,
                        height: h,
                    };
                    // Pad rows so background fill spans the panel width (visible selection).
                    let inner_w = sug_area.width.saturating_sub(2) as usize;
                    let mut lines: Vec<ratatui::text::Line> = suggestions
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(visible)
                        .map(|(i, it)| {
                            let marker = if i == idx { "▶ " } else { "  " };
                            let raw = format!("{marker}{:<14} {}", it.cmd, it.desc);
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
                    lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                        format!("  {}", suggestions[idx].desc),
                        theme::dim(),
                    )));
                    let title = if n > visible {
                        format!(
                            " commands {}–{}/{} · Tab · ↑↓ ",
                            start + 1,
                            start + visible,
                            n
                        )
                    } else {
                        format!(" commands ({n}) · ↑↓ select · Tab complete · Enter run ")
                    };
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

        // Phase 10 / TUI-08 — always-visible feedback strip
        if !self.feedback.is_empty() && regions.feedback.height > 0 {
            frame.render_widget(
                FeedbackBar {
                    model: &self.feedback,
                },
                regions.feedback,
            );
        }

        let attachment_label = self.pending_attachment.as_ref().map(|a| a.label());
        let glyph = ACTIVE_GLYPH;
        let composer_content_width = regions
            .input
            .width
            .saturating_sub(gutter_prefix_width(glyph) as u16)
            .max(1) as usize;
        let composer_rows = self.composer_layout_cache.rows(
            self.input.layout_revision,
            &self.input.text,
            composer_content_width,
        );
        frame.render_widget(
            InputBar {
                model: &self.input,
                rows: composer_rows,
                attachment: attachment_label.as_deref(),
                dimmed: self.busy && self.input.text.is_empty(),
                not_connected: !connected,
                focused: self.focus.mode == FocusMode::Navigation
                    && self.focus.block == FocusBlock::Composer,
            },
            regions.input,
        );

        let footer = FooterModel {
            hints: contextual_hint.unwrap_or_default(),
            ..FooterModel::default()
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);

        if let Some(ref dialog) = self.explorer_dialog {
            self.render_explorer_dialog(dialog, area, frame.buffer_mut());
            self.register_overlay_hit_regions(area);
        } else if let Some(ref ov) = self.overlay {
            match ov {
                Overlay::Help => self.render_help_overlay(area, frame.buffer_mut()),
                _ => frame.render_widget(OverlayWidget { overlay: ov }, area),
            }
            self.register_overlay_hit_regions(area);
        }
    }

    fn render_diff_workspace(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let gs = &self.file_explorer.git_status;
        if gs.loading && gs.status.is_empty() && !self.diff_snapshot.stale {
            Paragraph::new("Loading changes…")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }
        if gs.error.is_some() && !self.diff_snapshot.stale {
            Paragraph::new("Changes unavailable\n\nGit status could not be read.\nThe rest of Forge remains usable.")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }
        if gs.status.is_empty() && !self.diff_snapshot.stale {
            Paragraph::new("No changes\n\nThe working tree is clean.")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }

        let changed = gs.changed_files();
        let review_paths = if self.diff_snapshot.stale && !self.diff_snapshot.paths.is_empty() {
            self.diff_snapshot.paths.clone()
        } else {
            changed.iter().map(|f| f.path.clone()).collect()
        };
        let selected = self.diff_selected.min(review_paths.len().saturating_sub(1));
        let selected_path = review_paths.get(selected);

        let mut lines = vec![Line::from(Span::styled("CHANGES", theme::brand()))];
        if self.diff_snapshot.stale {
            lines.push(Line::styled(
                "Stale review · changes updated externally · press r to Refresh",
                theme::warn(),
            ));
            lines.push(Line::styled(
                "Apply disabled until refresh.",
                theme::disabled(),
            ));
        }
        lines.push(Line::from(""));

        for (i, path) in review_paths.iter().enumerate() {
            let marker = if i == selected { "▶ " } else { "  " };
            let status = changed
                .iter()
                .find(|file| file.path == *path)
                .and_then(|file| file.unstaged)
                .map(GitStatusKind::marker)
                .unwrap_or("!");
            lines.push(Line::from(format!("{marker}{status} {}", path.display())));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("UNSTAGED DIFF", theme::info())));

        if let Some(path) = selected_path {
            match gs.get_unstaged_diff(&self.runtime.cwd, path) {
                Ok(diff) => {
                    for line in diff.lines().take(20) {
                        let style = if line.starts_with('+') {
                            theme::ok()
                        } else if line.starts_with('-') {
                            theme::danger()
                        } else if line.starts_with("@@") {
                            theme::warn()
                        } else {
                            theme::muted()
                        };
                        lines.push(Line::styled(line.to_string(), style));
                    }
                }
                Err(e) => {
                    lines.push(Line::styled(
                        format!("Unable to load diff: {}", e),
                        theme::danger(),
                    ));
                }
            }
        } else {
            lines.push(Line::from("No unstaged file selected."));
        }

        Paragraph::new(lines)
            .style(theme::text())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::muted())
                    .style(theme::panel()),
            )
            .render(area, buf);
    }

    fn render_run_workspace(
        &self,
        id: &str,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let current = self.run.current.as_ref().filter(|record| record.id == id);
        let mut lines = Vec::new();
        if let Some(record) = current {
            lines.push(Line::from(vec![
                Span::styled("Run ", theme::muted()),
                Span::styled(record.invocation.summary(), theme::text()),
            ]));
            lines.push(Line::styled(
                format!(
                    "State: {}",
                    match record.state {
                        RunState::Queued => "Queued",
                        RunState::Running => "Running",
                        RunState::Succeeded => "Succeeded",
                        RunState::Failed => "Failed",
                        RunState::Cancelled => "Cancelled",
                        RunState::StartFailed => "Could not start",
                        RunState::CaptureFailed => "Capture failed",
                    }
                ),
                theme::text(),
            ));
            if let Some(code) = record.exit_status {
                lines.push(Line::styled(format!("Exit status: {code}"), theme::muted()));
            }
            if record.state == RunState::StartFailed {
                lines.push(Line::styled(
                    format!("Executable: {}", record.invocation.executable),
                    theme::muted(),
                ));
                lines.push(Line::styled(
                    format!("Arguments: {:?}", record.invocation.arguments),
                    theme::muted(),
                ));
                lines.push(Line::styled(
                    format!(
                        "Directory: {}",
                        record.invocation.working_directory.display()
                    ),
                    theme::muted(),
                ));
                if let Some(error) = record.spawn_error.as_deref() {
                    lines.push(Line::styled(format!("Cause: {error}"), theme::danger()));
                }
            }
        } else {
            lines.push(Line::styled("Run is no longer available.", theme::warn()));
        }
        if !self.terminal_capture.content.is_empty() {
            lines.push(Line::styled("Output", theme::muted()));
            for line in self.terminal_capture.content.lines().take(12) {
                lines.push(Line::styled(line.to_string(), theme::text()));
            }
            if self.terminal_capture.truncated {
                lines.push(Line::styled("Output truncated", theme::muted()));
            }
        } else if let Some(record) = current {
            lines.push(Line::styled(
                format!(
                    "Directory: {}",
                    record.invocation.working_directory.display()
                ),
                theme::muted(),
            ));
        }
        lines.push(Line::styled(
            "Back · Enter cancel while running · r rerun · e edit rerun",
            theme::muted(),
        ));

        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(if self.focus.block == FocusBlock::Workspace {
                        theme::active_panel_border()
                    } else {
                        theme::inactive_panel_border()
                    })
                    .title(Span::styled(" Run ", theme::active_panel_title())),
            )
            .render(area, buf);
    }
}
