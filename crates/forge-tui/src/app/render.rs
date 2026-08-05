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

fn composer_input_height(input: &InputModel, area: ratatui::layout::Rect) -> u16 {
    let content_width = crate::layout::estimate_composer_content_width(area)
        .saturating_sub(gutter_prefix_width(ACTIVE_GLYPH))
        .max(1);
    (input.visual_lines_for_width(content_width) + 2).clamp(3, crate::layout::MAX_COMPOSER_INPUT_H)
}

impl TuiApp {
    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        if crate::theme::refresh_system() {
            self.render_cache.conversation = None;
        }
        if self.workspace_files.explorer.git_status.poll() {
            self.reconcile_diff_staleness();
        }
        // Advance the off-thread repo-header refresh. Cheap (a `try_recv` plus an
        // elapsed check); every draw path funnels through here, including the
        // streaming and `drain_pending_*` loops that bypass `run_loop`'s polls.
        self.poll_repo_header();
        let area = frame.area();
        if is_too_small(area) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
            self.workspace_files.explorer.focused = false;
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
        let theme_picking = matches!(self.overlay, Some(Overlay::Theme { .. }));
        let input_h = if theme_picking {
            crate::layout::THEME_DOCK_H
        } else {
            composer_input_height(&self.input, area)
        };
        let panel_h = if self.bottom_panel.open { 16 } else { 0 };
        let contextual_hint = self.contextual_hint();
        let connected = self.is_provider_connected();
        let (vendor_label, _route_label) = self
            .connect
            .profile
            .as_deref()
            .map(|id| self.vendor_route_labels(id))
            .unwrap_or((None, None));
        // The footer row is otherwise idle whenever there's no contextual
        // hint stealing it — that's exactly when the persistent
        // [vendor] [model] [effort] control should occupy it, and only
        // when there's an actual vendor/model to show (e.g. not for the
        // mock/offline provider, which has no `connect.profile`). Still
        // capped at height 1 by `split_areas_with_chrome` below: this never
        // becomes a second footer row, just a reason to keep the one row
        // that already exists.
        let footer_has_compact_control = contextual_hint.is_none()
            && connected
            && vendor_label.is_some()
            && !self.runtime.model_label.is_empty();
        let hint_h = u16::from(contextual_hint.is_some() || footer_has_compact_control);
        let regions = split_areas_with_chrome(
            area,
            fb_h,
            input_h,
            !slash_mode && self.workspace_files.visible,
            0,
            panel_h,
            hint_h,
            true,
            0,
        );
        // Layout can hide a requested side/bottom panel. Focus must follow the
        // rendered geometry rather than leaving an invisible key owner behind.
        let available = FocusAvailability {
            files: regions.files.is_some(),
            sidebar: regions.sidebar.is_some(),
            bottom_panel: self.bottom_panel.open && regions.bottom_panel.height > 0,
        };
        if self.bottom_panel.open && regions.bottom_panel.height > 1 {
            self.resize_interactive_terminal(
                regions.bottom_panel.width,
                regions.bottom_panel.height.saturating_sub(1),
            );
        }
        if !available.contains(self.focus.block) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
        }
        self.normalize_focus();
        self.register_pane_hit_regions(&regions);
        if let Some(files) = regions.files {
            frame.render_widget(
                FileExplorerWidget {
                    explorer: &mut self.workspace_files.explorer,
                    focused: self.focus.block == FocusBlock::Files,
                },
                files,
            );
            self.register_file_hit_regions(files);
        }
        let stream_wait = if self.busy_state.active && self.pending_turn.prompt.is_none() {
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
        let opts = ConversationViewOpts {
            busy: self.busy_state.active,
            // Don't force-expand finished thinking just because busy (answer may be streaming)
            tool_expanded: self.tool_detail.expanded,
            compact: false,
            stream_wait,
            stream_thought_secs: self.timing.thought_secs,
        };
        // `/clear` only clears the viewport; the full session remains available to the model.
        let visible_messages = &self.session.messages[self
            .conversation_view
            .message_start
            .min(self.session.messages.len())..];
        let visible_events = &self.session.events[self
            .conversation_view
            .event_start
            .min(self.session.events.len())..];
        let activity_summary = self.activity_summary();
        let activity_summary_key = self.activity_summary_cache_key();
        let sidebar_width = regions.sidebar.map(|r| r.width).unwrap_or(0);
        let key = ConversationRenderKey {
            session_id: self.session.session_id,
            width: sidebar_width,
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
            banners: self.banner_state.items.len(),
            queue: self.session.queue().len(),
            queue_selected: self.task_selection.queue,
            chat_message_start: self.conversation_view.message_start,
            chat_event_start: self.conversation_view.event_start,
            busy: self.busy_state.active,
            busy_phase: self.busy_state.phase.label(),
            activity_summary: activity_summary_key,
            tool_expanded: self.tool_detail.expanded,
            splash_dismissed: self.conversation_view.splash_dismissed,
            slash_mode,
            status: self.session.active_task.lifecycle,
            theme_id: crate::theme::active(),
            pending_hitl: self
                .session
                .pending_hitl()
                .map(|payload| payload.call_id.clone()),
        };
        if self
            .render_cache
            .conversation
            .as_ref()
            .map(|cache| &cache.key)
            != Some(&key)
        {
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
            .with_extra_banners(self.banner_state.items.iter().cloned());
            if !self.conversation_view.splash_dismissed {
                conv = conv.with_brand(self.runtime.version.clone());
            }
            if !slash_mode && !self.conversation_view.splash_dismissed {
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
                    .queue()
                    .visible()
                    .map(|item| item.text.clone())
                    .collect::<Vec<_>>(),
                self.task_selection.queue,
            );
            if let BusyPhase::Tool { name } = &self.busy_state.phase {
                if name != "run" {
                    conv = conv.with_running_tool(name.clone());
                }
            }
            if let Some(payload) = self.session.pending_hitl() {
                conv = conv.with_pending_approval(
                    payload,
                    self.session.workspace_root().display().to_string(),
                );
            }
            let width = sidebar_width.saturating_sub(2) as usize;
            self.render_cache.conversation = Some(ConversationRenderCache {
                key,
                lines: Arc::new(conv.lines_for_width(width)),
            });
        }
        let width = sidebar_width.saturating_sub(2) as usize;
        let live_lines = if self.busy_state.active && self.pending_turn.prompt.is_none() {
            ConversationModel::from_messages(
                &[],
                &[],
                self.session.active_task.lifecycle,
                ConversationViewOpts { busy: true, ..opts },
            )
            .with_streaming_preview(self.stream.thinking.clone(), self.stream.preview.clone())
            .lines_for_width(width)
        } else {
            Vec::new()
        };
        let cached = self
            .render_cache
            .conversation
            .as_ref()
            .expect("conversation cache populated");
        // Clones the shared handle, not the line data. This exists so the
        // immutable borrow of `conversation_cache` ends before
        // `register_activity_summary_region` takes `&mut self` below.
        let cached_lines = Arc::clone(&cached.lines);
        // The sidebar always shows the conversation, regardless of what the
        // center pane shows — it's no longer one of the `WorkspaceView`
        // options.
        if let Some(sidebar) = regions.sidebar {
            let conversation_area = ratatui::layout::Rect {
                x: sidebar.x.saturating_add(2.min(sidebar.width)),
                y: sidebar.y.saturating_add(1.min(sidebar.height)),
                width: sidebar.width.saturating_sub(2.min(sidebar.width)),
                height: sidebar.height.saturating_sub(1.min(sidebar.height)),
            };
            frame.render_widget(
                crate::conversation::ConversationLinesWidget {
                    lines: &cached_lines,
                    tail_lines: &live_lines,
                    scroll: self.conversation_view.scroll,
                    follow: self.conversation_view.follow,
                },
                conversation_area,
            );
            self.register_activity_summary_region(conversation_area, &cached_lines, &live_lines);
        }
        // The approval decision now lives in the conversation itself (inline
        // transcript item) and the composer, so the center pane gets its full
        // height — no docked card carving out a strip at its bottom.
        let chat_area = regions.chat;
        match self.workspace_navigation.current.clone() {
            None => {
                self.render_empty_workspace(chat_area, frame.buffer_mut());
            }
            Some(WorkspaceView::File(_)) => {
                self.editor_viewport.height = chat_area.height;
                frame.render_widget(
                    SourceViewerWidget {
                        viewer: &mut self.source_viewer,
                        focused: self.focus.block == FocusBlock::Workspace,
                    },
                    chat_area,
                );
            }
            Some(WorkspaceView::Diff(DiffCommandContext::Current)) => {
                self.render_diff_workspace(chat_area, frame.buffer_mut());
            }
            Some(WorkspaceView::Run(id)) => {
                self.render_run_workspace(&id, chat_area, frame.buffer_mut());
            }
        }

        let interactive_terminal_output = self
            .interactive_terminal
            .as_ref()
            .map(|terminal| terminal.display_output());
        let interactive_terminal = self.interactive_terminal.as_ref();
        frame.render_widget(
            BottomPanel {
                model: BottomPanelModel {
                    state: &self.bottom_panel,
                    busy_phase: &self.busy_state.phase,
                    activity: &self.activity,
                    run: &self.run,
                    terminal_title: self.terminal_capture.title.as_deref(),
                    terminal_content: interactive_terminal_output
                        .as_deref()
                        .unwrap_or(&self.terminal_capture.content),
                    terminal_truncated: self.terminal_capture.truncated,
                    terminal_running: interactive_terminal.is_some_and(|terminal| terminal.running),
                    terminal_shell: interactive_terminal.map(|terminal| terminal.shell.as_str()),
                },
                focused: self.focus.block == FocusBlock::BottomPanel,
            },
            regions.bottom_panel,
        );

        // Notices (help, connect list, multi-line status) just above input
        if !self.notice_state.items.is_empty() && self.overlay.is_none() {
            let notice_h = (self.notice_state.items.len() as u16)
                .min(18)
                .saturating_add(1);
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
                    .notice_state
                    .items
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
                let idx = self.slash_suggestions.selected.min(n.saturating_sub(1));
                // Use as much space above the input as possible (cap for readability).
                let max_list = input.y.saturating_sub(2).clamp(1, 8) as usize;
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

        let attachment_label = self.attachment.pending.as_ref().map(|a| a.label());
        if theme_picking {
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
                );
            }
        } else {
            frame.render_widget(
                InputBar {
                    model: &self.input,
                    attachment: attachment_label.as_deref(),
                    dimmed: self.busy_state.active && self.input.text.is_empty(),
                    not_connected: !connected,
                    focused: self.focus.mode == FocusMode::Navigation
                        && self.focus.block == FocusBlock::Composer,
                },
                regions.input,
            );
        }

        let effort_label = self
            .reasoning_effort
            .value
            .display_label(&self.runtime.model_label)
            .to_string();
        let token_report = self.session.token_usage_report();
        let footer = FooterModel {
            hints: contextual_hint.unwrap_or_default(),
            connected,
            provider: vendor_label.unwrap_or_default(),
            model: self.runtime.model_label.clone(),
            effort: effort_label,
            ctx_used: token_report.context_tokens_est,
            ctx_total: token_report.context_capacity,
            ctx_pct: self.session.context_usage_ratio(),
            ..FooterModel::default()
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);
        self.register_footer_control_hit_regions(&footer, regions.footer);

        if let Some(ref dialog) = self.explorer_dialog.current {
            self.render_explorer_dialog(dialog, area, frame.buffer_mut());
            self.register_overlay_hit_regions(area);
        } else if let Some(ref ov) = self.overlay {
            match ov {
                Overlay::Help => self.render_help_overlay(area, frame.buffer_mut()),
                // Theme dock already replaced the composer band above.
                Overlay::Theme { .. } => {}
                _ => frame.render_widget(OverlayWidget { overlay: ov }, area),
            }
            self.register_overlay_hit_regions(area);
        }
    }

    /// Center-pane placeholder for when nothing is open — `workspace_navigation.current`
    /// is `None`, since conversation no longer fills this pane by default.
    fn render_empty_workspace(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        // Top-aligned with consistent padding so switching Editor <-> Diff has no visual jump.
        let text = "\nNo file open\n\nSelect one from the explorer.";
        Paragraph::new(text)
            .style(theme::muted())
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::inactive_panel_border())
                    .style(theme::panel()),
            )
            .render(area, buf);
    }

    fn render_diff_workspace(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let gs = &self.workspace_files.explorer.git_status;
        if gs.loading && gs.status.is_empty() && !self.diff_view.snapshot.stale {
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
        if gs.error.is_some() && !self.diff_view.snapshot.stale {
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
        if gs.status.is_empty() && !self.diff_view.snapshot.stale {
            // Leading newline keeps identical top padding with Editor empty state.
            Paragraph::new("\nNo changes\n\nThe working tree is clean.")
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
        let review_paths =
            if self.diff_view.snapshot.stale && !self.diff_view.snapshot.paths.is_empty() {
                self.diff_view.snapshot.paths.clone()
            } else {
                changed.iter().map(|f| f.path.clone()).collect()
            };
        let selected = self
            .diff_view
            .selected
            .min(review_paths.len().saturating_sub(1));
        let selected_path = review_paths.get(selected);

        let mut lines = vec![Line::from(Span::styled("CHANGES", theme::brand()))];
        if self.diff_view.snapshot.stale {
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

#[cfg(test)]
mod tests {
    use super::composer_input_height;
    use crate::widgets::InputModel;
    use ratatui::layout::Rect;

    #[test]
    fn wrapped_composer_grows_before_the_second_visual_line_is_clipped() {
        let mut input = InputModel::default();
        input.set_text(
            std::iter::repeat_n("word", 40)
                .collect::<Vec<_>>()
                .join(" "),
        );

        assert_eq!(composer_input_height(&input, Rect::new(0, 0, 120, 40)), 8);
    }
}
