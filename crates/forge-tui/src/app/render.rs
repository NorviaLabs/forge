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
        .saturating_sub(2) // side borders
        .saturating_sub(crate::widgets::input::TEXT_INSET as usize)
        .max(1);
    // +2 borders, +2 padding (one row above/below the content) so short
    // content has room to actually be vertically centered — with only 1
    // spare row, integer-division centering has nowhere to put the second
    // half and just sits at the top.
    (input.visual_lines_for_width(content_width) + 4).min(crate::layout::MAX_COMPOSER_INPUT_H)
}

impl TuiApp {
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

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        // One read of the session per frame. Everything below renders from
        // this, so a frame is internally consistent and the ~40 scattered
        // `self.session.*` reads it replaces cost one capture instead.
        self.session_view = SessionSnapshot::capture(&self.session);
        self.transcript_view.refresh(&self.session);
        let area = frame.area();
        if is_too_small(area) {
            self.focus.reset_to_workspace();
            self.workspace_files.explorer.focused = false;
            self.bottom_panel.focused = false;
            self.source_viewer.focused = false;
            frame.render_widget(
                Paragraph::new("Terminal too small — resize to at least 40x18"),
                area,
            );
            return;
        }
        crate::theme::fill(area, frame.buffer_mut(), crate::theme::canvas());
        let fb_h = 0;
        let slash_mode = self.overlay.is_none() && self.input.text.starts_with('/');
        let theme_picking = matches!(self.overlay, Some(Overlay::Theme { .. }));
        let input_h = if theme_picking {
            crate::layout::THEME_DOCK_H
        } else {
            composer_input_height(&self.input, area)
        };
        let panel_h = if self.bottom_panel.open { 16 } else { 0 };
        let contextual_hint = self.contextual_hint();
        // The event-loop tick refreshes this cache; drawing only reads it.
        let connected = self.provider_connected_cached();
        let (vendor_label, _route_label) = self
            .connect
            .profile
            .as_deref()
            .map(|id| self.vendor_route_labels(id))
            .unwrap_or((None, None));
        // Model/vendor/effort live on the footer chip row; the composer band
        // is text-only and the footer always reserves two rows — a thin
        // divider rule plus the chip/activity content row. The focused
        // footer's hint shares the content row (replacing the right-side
        // activity while focused), never adds a third row.
        let hint_h: u16 = 2;
        // An open file occupies the center workspace pane. Anything else
        // (home / empty) expands conversation into that pane and there is
        // no Workspace block to focus.
        let expand_conversation = !matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::File(_))
        );
        let regions = if expand_conversation {
            split_areas_with_expanded_conversation(
                area,
                fb_h,
                input_h,
                self.workspace_files.visible,
                0,
                panel_h,
                hint_h,
                true,
                0,
            )
        } else {
            split_areas_with_chrome(
                area,
                fb_h,
                input_h,
                self.workspace_files.visible,
                0,
                panel_h,
                hint_h,
                true,
                0,
            )
        };
        // Remember the rendered editor rect so mouse events (which arrive
        // between frames) can be hit-tested against it for selection.
        self.editor_area = if self.current_workspace_is_file() {
            Some(regions.chat)
        } else {
            None
        };
        self.conversation_area = None;
        self.terminal_area = None;
        self.conversation_rows.clear();
        self.terminal_rows.clear();
        // Layout can hide a requested side/bottom panel. Focus must follow the
        // rendered geometry rather than leaving an invisible key owner behind.
        let available = FocusAvailability {
            search: regions.files.is_some(),
            files: regions.files.is_some(),
            sidebar: regions.sidebar.is_some(),
            bottom_panel: self.bottom_panel.open && regions.bottom_panel.height > 0,
            approval: self.session_view.is_awaiting_approval(),
        };
        if self.bottom_panel.open && regions.bottom_panel.height > 1 {
            self.resize_interactive_terminal(
                regions.bottom_panel.width,
                regions.bottom_panel.height.saturating_sub(1),
            );
        }
        if !available.contains(self.focus.block()) {
            self.focus.reset_to_workspace();
        }
        if expand_conversation && self.focus.block() == FocusBlock::Workspace {
            self.focus.set_navigation(FocusBlock::Sidebar);
        }
        self.normalize_focus();
        let status = self.refresh_status_model_with_connected(connected);
        frame.render_widget(StatusBar { model: &status }, regions.status);
        if let Some(files) = regions.files {
            frame.render_widget(
                FileExplorerWidget {
                    explorer: &mut self.workspace_files.explorer,
                    focused: matches!(self.focus.block(), FocusBlock::Files | FocusBlock::Search),
                    search_active: self.focus.block() == FocusBlock::Search,
                },
                files,
            );
        }
        let stream_wait = if self.busy_state.is_active() && !self.pending_turn.has_prompt() {
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
            busy: self.busy_state.is_active(),
            // Don't force-expand finished thinking just because busy (answer may be streaming)
            tool_expanded: self.tool_detail.is_expanded(),
            compact: false,
            stream_wait,
            stream_thought_secs: self.timing.thought_secs,
        };
        // `/clear` only clears the viewport; the full session remains available to the model.
        let all_messages = self.transcript_view.messages();
        let all_events = self.transcript_view.events();
        let visible_messages =
            &all_messages[self.conversation_view.message_start.min(all_messages.len())..];
        let visible_events =
            &all_events[self.conversation_view.event_start.min(all_events.len())..];
        let activity_summary = self.activity_summary();
        let activity_summary_key = self.activity_summary_cache_key();
        let sidebar_width = regions.sidebar.map(|r| r.width).unwrap_or(0);
        let sidebar_inner_h = regions
            .sidebar
            .map(|r| r.height.saturating_sub(2) as usize)
            .unwrap_or(0);
        // Follow-mode only paints the viewport plus overscan. Scrolling up
        // raises the window so earlier blocks are materialized on demand.
        const TRANSCRIPT_OVERSCAN: usize = 64;
        let keep_from_end = if self.conversation_view.follow {
            sidebar_inner_h.saturating_add(TRANSCRIPT_OVERSCAN)
        } else {
            sidebar_inner_h
                .saturating_add(self.conversation_view.scroll as usize)
                .saturating_add(TRANSCRIPT_OVERSCAN)
        }
        .max(1);
        let key = ConversationRenderKey {
            session_id: self.session_view.session_id,
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
            queue: self.session_view.queue_len,
            queue_selected: self.task_selection.queue(),
            chat_message_start: self.conversation_view.message_start,
            chat_event_start: self.conversation_view.event_start,
            keep_from_end,
            activity_summary: activity_summary_key,
            tool_expanded: self.tool_detail.is_expanded(),
            splash_dismissed: self.conversation_view.splash_dismissed,
            slash_mode,
            status: self.session_view.lifecycle,
            theme_id: crate::theme::active(),
            pending_hitl: self
                .session
                .pending_hitl()
                .map(|payload| payload.call_id.clone()),
            approval_menu_selected: self.approval_menu_selected(),
            approval_focused: self.focus.block() == FocusBlock::Approval,
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
                self.session_view.lifecycle,
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
                    self.session_view.loaded_skills_count,
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
                self.task_selection.queue(),
            );
            self.sync_approval_menu();
            if let Some(payload) = self.session_view.pending_hitl.clone() {
                let rows = self.approval_menu_rows();
                let selected = self.approval_menu_selected();
                let approval_focused = self.focus.block() == FocusBlock::Approval;
                let cwd = self.session_view.workspace_root().display().to_string();
                let request = crate::overlays::ApprovalOverlayState::request_view(&payload, cwd);
                conv = conv.with_pending_approval(request, rows, selected, approval_focused);
            }
            let width = sidebar_width.saturating_sub(2) as usize;
            self.render_cache.conversation = Some(ConversationRenderCache {
                key,
                lines: Arc::new(conv.lines_for_width_from_end(width, keep_from_end)),
            });
        }
        let width = sidebar_width.saturating_sub(2) as usize;
        // Rebuilding the live preview re-parses the whole accumulated markdown
        // (and re-highlights its code blocks), so it is rate-limited rather than
        // run once per streaming frame: at token rate it is O(n^2) over a turn.
        const STREAM_PREVIEW_RENDER_INTERVAL: Duration = Duration::from_millis(150);
        let live_lines = if self.busy_state.is_active() && !self.pending_turn.has_prompt() {
            let key = (
                width as u16,
                self.stream.thinking.len(),
                self.stream.preview.len(),
            );
            let key_matches = self
                .stream
                .live_lines
                .as_ref()
                .map(|(w, t, p, _)| (*w, *t, *p) == key)
                .unwrap_or(false);
            let ready = self
                .stream
                .last_preview_render
                .map(|at| at.elapsed() >= STREAM_PREVIEW_RENDER_INTERVAL)
                .unwrap_or(true);
            if key_matches || !ready {
                self.stream
                    .live_lines
                    .as_ref()
                    .map(|(.., lines)| Arc::clone(lines))
                    .unwrap_or_else(|| Arc::new(Vec::new()))
            } else {
                let lines = Arc::new(
                    ConversationModel::from_messages(
                        &[],
                        &[],
                        self.session_view.lifecycle,
                        ConversationViewOpts { busy: true, ..opts },
                    )
                    .with_streaming_preview(
                        self.stream.thinking.clone(),
                        self.stream.preview.clone(),
                    )
                    .lines_for_width(width),
                );
                self.stream.live_lines = Some((key.0, key.1, key.2, Arc::clone(&lines)));
                self.stream.last_preview_render = Some(Instant::now());
                lines
            }
        } else {
            Arc::new(Vec::new())
        };
        let cached = self
            .render_cache
            .conversation
            .as_ref()
            .expect("conversation cache populated");
        let cached_lines = Arc::clone(&cached.lines);
        // The sidebar always shows the conversation, regardless of what the
        // center pane shows — it's no longer one of the `WorkspaceView`
        // options.
        if let Some(sidebar) = regions.sidebar {
            let sidebar_focused = self.focus.block() == FocusBlock::Sidebar;
            let sidebar_block = Block::default()
                .borders(Borders::ALL)
                .padding(ratatui::widgets::Padding::horizontal(1))
                .border_style(if sidebar_focused {
                    theme::active_panel_border()
                } else {
                    theme::inactive_panel_border()
                })
                .style(theme::panel());
            let conversation_area = sidebar_block.inner(sidebar);
            self.conversation_area = Some(conversation_area);
            let bottom_padding = if self.session_view.is_awaiting_approval() {
                0
            } else if theme_picking {
                1
            } else {
                input_h.saturating_add(1)
            };
            self.conversation_rows = visible_conversation_copy_rows(
                &cached_lines,
                &live_lines,
                self.conversation_view.scroll,
                self.conversation_view.follow,
                bottom_padding,
                conversation_area,
            );
            sidebar_block.render(sidebar, frame.buffer_mut());
            frame.render_widget(
                crate::conversation::ConversationLinesWidget {
                    lines: &cached_lines,
                    tail_lines: &live_lines,
                    scroll: self.conversation_view.scroll,
                    follow: self.conversation_view.follow,
                    bottom_padding,
                },
                conversation_area,
            );
        }
        // The approval decision now lives in the conversation itself (inline
        // transcript item) and the composer, so the center pane gets its full
        // height — no docked card carving out a strip at its bottom.
        if !expand_conversation {
            let chat_area = regions.chat;
            match self.workspace_navigation.current().clone() {
                None => {
                    self.render_empty_workspace(chat_area, frame.buffer_mut());
                }
                Some(WorkspaceView::File(_)) => {
                    self.editor_viewport.height = chat_area.height;
                    frame.render_widget(
                        SourceViewerWidget {
                            viewer: &mut self.source_viewer,
                            focused: self.focus.block() == FocusBlock::Workspace,
                            editor: self.editor_session.as_mut(),
                            editor_command: self.editor_command.as_deref(),
                            editor_message: self.editor_message.as_deref(),
                        },
                        chat_area,
                    );
                }
            }
        }

        // Highlight a live drag-selection over the editor (sorted after the
        // source viewer so reverse-video is applied on top of its spans).
        if self.selection.active && self.current_workspace_is_file() {
            let line_count = self
                .editor_session
                .as_ref()
                .map(EditorSession::line_count)
                .unwrap_or(self.source_viewer.lines.len());
            paint_editor_selection(
                frame.buffer_mut(),
                &self.selection,
                regions.chat,
                line_count,
            );
        }
        if self.selection.active
            && matches!(
                self.selection.pane,
                Some(
                    crate::selection::CopyPane::Conversation | crate::selection::CopyPane::Terminal
                )
            )
        {
            let area = match self.selection.pane {
                Some(crate::selection::CopyPane::Conversation) => self.conversation_area,
                Some(crate::selection::CopyPane::Terminal) => self.terminal_area,
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
        frame.render_widget(
            BottomPanel {
                model: BottomPanelModel {
                    state: &self.bottom_panel,
                    busy_phase: &self.busy_state.phase(),
                    activity: &self.activity,
                    terminal_content: interactive_terminal_output.unwrap_or(""),
                    terminal_running: interactive_terminal.is_some_and(|terminal| terminal.running),
                    terminal_shell: interactive_terminal.map(|terminal| terminal.shell.as_str()),
                    terminal_cursor: interactive_terminal.map(InteractiveTerminal::cursor_position),
                },
                focused: self.focus.block() == FocusBlock::BottomPanel,
            },
            regions.bottom_panel,
        );
        if regions.bottom_panel.height > 1 && self.bottom_panel.open {
            self.terminal_area = Some(ratatui::layout::Rect {
                x: regions.bottom_panel.x,
                y: regions.bottom_panel.y.saturating_add(1),
                width: regions.bottom_panel.width,
                height: regions.bottom_panel.height.saturating_sub(1),
            });
            self.terminal_rows = terminal_copy_rows(
                interactive_terminal_output.unwrap_or(""),
                self.terminal_area.unwrap().height,
            );
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
        let effort_label = self
            .reasoning_effort
            .value
            .display_label(&self.runtime.model_label)
            .to_string();
        let llm_label = format!(
            "{}/{}",
            vendor_label.as_deref().unwrap_or("model"),
            footer_short_model_id(&self.runtime.model_label)
        );
        // Only three focusable footer controls now (which-LLM, effort, mode).
        if let Some(idx) = self.composer_chip_focus {
            self.composer_chip_focus = Some(idx.min(2));
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
            let composer_focused = self.focus.mode() == FocusMode::Navigation
                && self.focus.block() == FocusBlock::Composer;
            frame.render_widget(
                InputBar {
                    model: &self.input,
                    attachment: attachment_label.as_deref(),
                    dimmed: (self.busy_state.is_active() && self.input.text.is_empty())
                        || self.session_view.is_awaiting_approval(),
                    not_connected: !connected,
                    focused: composer_focused,
                    waiting: self.session_view.is_awaiting_approval(),
                    permission_mode: self.session.permission_mode(),
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

        let footer = FooterModel {
            hints: contextual_hint.unwrap_or_default(),
            // The footer's own per-chip hint shares the row; every other
            // hint source (HITL/dialog/transient) is blocking and takes the
            // whole row.
            hint_replaces_row: !(self.focus.mode() == FocusMode::Navigation
                && self.focus.block() == FocusBlock::Footer),
            llm_label,
            llm_connected: connected,
            effort_label,
            mode_label: self.session.permission_mode().label().to_string(),
            focus: self.composer_chip_focus.map(|idx| match idx {
                0 => FooterFocus::Llm,
                1 => FooterFocus::Effort,
                _ => FooterFocus::Mode,
            }),
            dimmed: self.session_view.is_awaiting_approval(),
            lifecycle: status.turn_lifecycle(),
            ctx_pct: status.ctx_pct,
            prompt_tokens: self.session_view.prompt_tokens,
            completion_tokens: self.session_view.completion_tokens,
            prompt_cache_reads: self.session_view.prompt_cache_hits,
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);

        if let Some(dialog) = self.explorer_dialog.current() {
            self.render_explorer_dialog(dialog, area, frame.buffer_mut());
        } else if let Some(ref ov) = self.overlay {
            match ov {
                Overlay::Help => self.render_help_overlay(area, frame.buffer_mut()),
                // Theme dock already replaced the composer band above.
                Overlay::Theme { .. } => {}
                _ => frame.render_widget(OverlayWidget { overlay: ov }, area),
            }
        }

        if !self.feedback.is_empty() {
            let width = area.width.saturating_sub(2).clamp(32, 56);
            let content_width = width.saturating_sub(4).max(1) as usize;
            let line_count = self
                .feedback
                .text
                .lines()
                .map(|line| line.chars().count().max(1).div_ceil(content_width))
                .sum::<usize>()
                .max(1) as u16;
            let height = line_count
                .saturating_add(2)
                .min(area.height.saturating_sub(2));
            if height > 0 && width > 0 {
                let notice_area = ratatui::layout::Rect {
                    x: area.x + area.width.saturating_sub(width).saturating_sub(1),
                    y: area.y.saturating_add(1),
                    width,
                    height,
                };
                frame.render_widget(
                    FeedbackBar {
                        model: &self.feedback,
                    },
                    notice_area,
                );
            }
        }

        // The right-click context menu is the topmost layer.
        if let Some(menu) = self.context_menu.as_ref() {
            render_context_menu(frame.buffer_mut(), menu);
        }
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
            theme::inactive_panel_border(),
        );
    }
}

fn visible_conversation_copy_rows(
    lines: &[Line<'static>],
    tail_lines: &[Line<'static>],
    scroll_from_bottom: u16,
    follow: bool,
    bottom_padding: u16,
    area: ratatui::layout::Rect,
) -> Vec<String> {
    let content_len = lines.len().saturating_add(tail_lines.len());
    let total = content_len.saturating_add(bottom_padding as usize);
    let max_scroll = total.saturating_sub(area.height as usize);
    let scroll = if follow {
        max_scroll
    } else {
        max_scroll.saturating_sub((scroll_from_bottom as usize).min(max_scroll))
    };
    let end = scroll.saturating_add(area.height as usize).min(total);
    (scroll..end)
        .map(|index| {
            // Borrow the line: it was previously deep-cloned (spans and all)
            // only to concatenate the text back out and drop the copy.
            let line = if index < lines.len() {
                Some(&lines[index])
            } else if index < content_len {
                Some(&tail_lines[index - lines.len()])
            } else {
                None
            };
            line.map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .unwrap_or_default()
        })
        .collect()
}

fn terminal_copy_rows(content: &str, height: u16) -> Vec<String> {
    let mut rows: Vec<String> = content.lines().map(str::to_string).collect();
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

/// Paint a live drag-selection over the editor pane as reverse video. Only the
/// visible selection rows within the editor body are touched; any part dragged
/// outside the pane is ignored.
fn paint_editor_selection(
    buf: &mut ratatui::buffer::Buffer,
    sel: &crate::selection::MouseSelection,
    area: ratatui::layout::Rect,
    line_count: usize,
) {
    use ratatui::style::Modifier;
    let body = crate::selection::editor_body(area);
    let Some(rect) = sel.rect() else {
        return;
    };
    if body.height == 0 || body.width == 0 {
        return;
    }
    let total = line_count.max(1);
    let gutter = (total.to_string().len().max(3) + 3) as u16;
    let content_x = body.x.saturating_add(gutter);
    let right_edge = body.x.saturating_add(body.width).saturating_sub(1);
    for row in rect.row_start..=rect.row_end {
        if row < body.y || row >= body.y.saturating_add(body.height) {
            continue;
        }
        let left = if row == rect.row_start {
            rect.start_col.max(content_x)
        } else {
            content_x
        };
        let right = if row == rect.row_end {
            rect.end_col.min(right_edge)
        } else {
            right_edge
        };
        if left > right {
            continue;
        }
        for col in left..=right {
            let cell = &mut buf[(col, row)];
            let style = cell.style();
            cell.set_style(style.add_modifier(Modifier::REVERSED));
        }
    }
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
        for col in left..=right {
            let cell = &mut buf[(col, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

/// Draw the right-click context menu as a small popover list.
fn render_context_menu(buf: &mut ratatui::buffer::Buffer, menu: &crate::selection::ContextMenu) {
    let rect = menu.rect();
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

        assert_eq!(composer_input_height(&input, Rect::new(0, 0, 120, 40)), 10);
    }

    #[test]
    fn paint_rows_selection_uses_stream_shape_not_rectangle() {
        use crate::selection::{Cell, CopyPane, MouseSelection};
        use ratatui::style::Modifier;

        let area = Rect::new(0, 0, 10, 3);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut sel = MouseSelection::default();
        sel.start_in(CopyPane::Conversation, Cell { row: 0, col: 5 });
        sel.update(Cell { row: 2, col: 3 });
        super::paint_rows_selection(&mut buf, &sel, area);

        let reversed = |x: u16, y: u16| {
            buf[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };

        // First row: selection starts mid-row (col 5), so only cols 5..=9
        // (the right edge) are highlighted — not the whole row.
        for x in 0..5 {
            assert!(!reversed(x, 0), "col {x} row 0 should not be selected");
        }
        for x in 5..10 {
            assert!(reversed(x, 0), "col {x} row 0 should be selected");
        }
        // Interior row: highlighted in full.
        for x in 0..10 {
            assert!(reversed(x, 1), "col {x} row 1 should be selected");
        }
        // Last row: selection ends mid-row (col 3), so only cols 0..=3 (the
        // left edge onward) are highlighted — a rectangle would also
        // highlight cols 4..9 here, which is exactly the bug this guards.
        for x in 0..=3 {
            assert!(reversed(x, 2), "col {x} row 2 should be selected");
        }
        for x in 4..10 {
            assert!(!reversed(x, 2), "col {x} row 2 should not be selected");
        }
    }

    #[test]
    fn empty_state_text_renders_fully_vertically_centered() {
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        super::render_centered_text(
            area,
            &mut buf,
            "No file open\n\nSelect one from the explorer.",
            crate::theme::muted(),
            crate::theme::inactive_panel_border(),
        );
        let rendered: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();

        assert!(rendered.iter().any(|row| row.contains("No file open")));
        assert!(
            rendered
                .iter()
                .any(|row| row.contains("Select one from the explorer.")),
            "all lines must render, not just the first:\n{}",
            rendered.join("\n")
        );
        assert!(
            rendered.iter().any(|row| row.trim().starts_with('└')),
            "bottom border must render:\n{}",
            rendered.join("\n")
        );
    }
}
