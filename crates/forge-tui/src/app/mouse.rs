//! Pointer input and hit-region bookkeeping for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Render code registers clickable regions each
//! frame; `handle_mouse` resolves a pointer event against them. Kept separate
//! from `app/input.rs` because the register/resolve pair is its own concern with
//! its own lifecycle. Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn begin_hit_frame(&mut self) {
        self.pointer.frame_generation = self.pointer.frame_generation.saturating_add(1);
        if self.pointer.frame_generation == 0 {
            self.pointer.frame_generation = 1;
        }
        self.pointer.hit_regions.clear();
    }

    pub(super) fn invalidate_hit_regions(&mut self) {
        self.pointer.frame_generation = self.pointer.frame_generation.saturating_add(1);
        self.pointer.hit_regions.clear();
        self.pointer.pending_double_click = None;
    }

    pub(super) fn clear_pending_double_click(&mut self) {
        self.pointer.pending_double_click = None;
    }

    fn register_hit_region(
        &mut self,
        area: ratatui::layout::Rect,
        target: HitTarget,
        z_order: u16,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.pointer.hit_regions.push(HitRegion {
            area,
            target,
            generation: self.pointer.frame_generation,
            z_order,
        });
    }

    pub(super) fn register_pane_hit_regions(&mut self, regions: &crate::layout::LayoutRegions) {
        if let Some(area) = regions.files {
            self.register_hit_region(area, HitTarget::Pane(FocusBlock::Files), 1);
        }
        self.register_hit_region(regions.chat, HitTarget::Pane(FocusBlock::Workspace), 1);
        if let Some(area) = regions.sidebar {
            self.register_hit_region(area, HitTarget::Pane(FocusBlock::Inspector), 1);
        }
        if self.bottom_panel.open && regions.bottom_panel.height > 0 {
            self.register_hit_region(
                regions.bottom_panel,
                HitTarget::Pane(FocusBlock::BottomPanel),
                1,
            );
            self.register_bottom_panel_tab_regions(regions.bottom_panel);
        }
        self.register_hit_region(regions.input, HitTarget::Composer, 5);
    }

    pub(super) fn register_file_hit_regions(&mut self, area: ratatui::layout::Rect) {
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(area);
        let height = inner.height.saturating_sub(1) as usize;
        let error_shown = self.file_explorer.git_status.error.is_some();
        let list_height = height.saturating_sub(error_shown as usize);
        let y_offset = error_shown as u16;
        let visible = self.file_explorer.visible_nodes();
        for (row, node) in visible
            .iter()
            .skip(self.file_explorer.scroll)
            .take(list_height)
            .enumerate()
        {
            let y = inner.y.saturating_add(y_offset).saturating_add(row as u16);
            let row_area = ratatui::layout::Rect::new(inner.x, y, inner.width, 1);
            self.register_hit_region(row_area, HitTarget::FileEntry(node.path.clone()), 20);
            if node.kind == FileKind::Directory {
                let chevron_x = inner
                    .x
                    .saturating_add((node.depth as u16).saturating_mul(2));
                if chevron_x < inner.x.saturating_add(inner.width) {
                    self.register_hit_region(
                        ratatui::layout::Rect::new(chevron_x, y, 1, 1),
                        HitTarget::DirectoryChevron(node.path.clone()),
                        30,
                    );
                }
            }
        }
    }

    fn register_bottom_panel_tab_regions(&mut self, area: ratatui::layout::Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let mut x = area.x;
        let y = area.y;
        for (idx, tab) in BottomPanelTab::ALL.into_iter().enumerate() {
            let width = format!(" {} {} ", idx + 1, tab.label()).chars().count() as u16;
            if x >= area.x.saturating_add(area.width) {
                break;
            }
            let clamped_width = width.min(area.x.saturating_add(area.width).saturating_sub(x));
            self.register_hit_region(
                ratatui::layout::Rect::new(x, y, clamped_width, 1),
                HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(tab)),
                25,
            );
            x = x.saturating_add(width).saturating_add(1);
        }
    }

    pub(super) fn register_activity_summary_region(
        &mut self,
        area: ratatui::layout::Rect,
        lines: &[Line<'static>],
        tail_lines: &[Line<'static>],
    ) {
        let Some(summary) = self.activity_summary() else {
            return;
        };
        if summary.action.is_none() {
            return;
        }
        let total = lines.len().saturating_add(tail_lines.len());
        let max_scroll = total.saturating_sub(area.height as usize);
        let scroll = if self.chat_follow {
            max_scroll
        } else {
            max_scroll.saturating_sub((self.chat_scroll as usize).min(max_scroll))
        };
        let end = scroll.saturating_add(area.height as usize).min(total);
        for index in scroll..end {
            let line = if index < lines.len() {
                &lines[index]
            } else {
                &tail_lines[index - lines.len()]
            };
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            if text.contains(&summary.label) {
                let y = area.y.saturating_add(index.saturating_sub(scroll) as u16);
                self.register_hit_region(
                    ratatui::layout::Rect::new(area.x, y, area.width, 1),
                    HitTarget::ActivitySummary,
                    25,
                );
                return;
            }
        }
    }

    pub(super) fn register_overlay_hit_regions(&mut self, area: ratatui::layout::Rect) {
        if self.explorer_dialog.is_some() {
            self.register_hit_region(
                area,
                HitTarget::VisibleControl(SemanticCommand::CloseOverlay),
                900,
            );
            return;
        }
        let hitl = self.overlay.as_ref().and_then(|overlay| {
            if let Overlay::Hitl {
                approval, expanded, ..
            } = overlay
            {
                Some((approval.remember_eligible, *expanded))
            } else {
                None
            }
        });
        if self.overlay.is_none() {
            return;
        }
        self.register_hit_region(
            area,
            HitTarget::VisibleControl(SemanticCommand::CloseOverlay),
            900,
        );
        if let Some((remember_eligible, expanded)) = hitl {
            let overlay_area =
                centered_capped_rect_for_mouse(area, 78, if expanded { 30 } else { 22 });
            let inner = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .inner(overlay_area);
            let action_y = inner.y.saturating_add(10);
            self.register_hit_region(
                ratatui::layout::Rect::new(inner.x, action_y, 12, 1),
                HitTarget::OverlayAction(OverlayAction::HitlApprove),
                1000,
            );
            self.register_hit_region(
                ratatui::layout::Rect::new(inner.x.saturating_add(14), action_y, 8, 1),
                HitTarget::OverlayAction(OverlayAction::HitlDeny),
                1000,
            );
            if remember_eligible {
                self.register_hit_region(
                    ratatui::layout::Rect::new(inner.x, inner.y.saturating_add(12), inner.width, 1),
                    HitTarget::OverlayAction(OverlayAction::HitlApproveSession),
                    1000,
                );
            }
        }
    }

    pub(super) fn scroll_conversation_up(&mut self, amount: u16) {
        self.chat_follow = false;
        self.chat_scroll = self.chat_scroll.saturating_add(amount);
    }

    pub(super) fn scroll_conversation_down(&mut self, amount: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(amount);
        if self.chat_scroll == 0 {
            self.chat_follow = true;
        }
    }

    fn resolve_hit_target(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.pointer
            .hit_regions
            .iter()
            .filter(|region| region.generation == self.pointer.frame_generation)
            .filter(|region| rect_contains(region.area, x, y))
            .max_by_key(|region| region.z_order)
            .map(|region| region.target.clone())
    }

    fn double_click_target_for(target: &HitTarget) -> Option<DoubleClickTarget> {
        match target {
            HitTarget::FileEntry(path) => Some(DoubleClickTarget::FileEntry(path.clone())),
            HitTarget::Pane(_)
            | HitTarget::DirectoryChevron(_)
            | HitTarget::ActivitySummary
            | HitTarget::VisibleControl(_)
            | HitTarget::Composer
            | HitTarget::OverlayAction(_) => None,
        }
    }

    fn file_entry_kind(&self, path: &Path) -> Option<FileKind> {
        self.file_explorer
            .visible_nodes()
            .iter()
            .find(|node| node.path == path)
            .map(|node| node.kind)
    }

    fn double_click_target_exists(&self, target: &DoubleClickTarget) -> bool {
        match target {
            DoubleClickTarget::FileEntry(path) => self.file_entry_kind(path).is_some(),
        }
    }

    fn is_qualifying_double_click(
        &self,
        target: &DoubleClickTarget,
        button: MouseButton,
        now: Instant,
    ) -> bool {
        let Some(pending) = self.pointer.pending_double_click.as_ref() else {
            return false;
        };
        pending.button == button
            && pending.target == *target
            && now.duration_since(pending.timestamp) <= DOUBLE_CLICK_THRESHOLD
            && pending.frame_generation <= self.pointer.frame_generation
            && self.double_click_target_exists(&pending.target)
            && self.double_click_target_exists(target)
    }

    async fn activate_double_click_target(
        &mut self,
        target: DoubleClickTarget,
    ) -> Result<(), TuiError> {
        match target {
            DoubleClickTarget::FileEntry(path) => match self.file_entry_kind(&path) {
                Some(FileKind::Directory) => {
                    self.execute_semantic_command(SemanticCommand::ToggleDirectory(path))
                        .await?;
                }
                Some(FileKind::File | FileKind::Symlink) => {
                    self.execute_semantic_command(SemanticCommand::OpenFile(path))
                        .await?;
                }
                Some(FileKind::Unknown) => {}
                None => {}
            },
        }
        Ok(())
    }

    fn remember_double_click_candidate(
        &mut self,
        target: DoubleClickTarget,
        button: MouseButton,
        timestamp: Instant,
    ) {
        self.pointer.pending_double_click = Some(PendingDoubleClick {
            target,
            button,
            timestamp,
            frame_generation: self.pointer.frame_generation,
        });
    }

    fn pane_target_at(&self, x: u16, y: u16) -> Option<FocusBlock> {
        match self.resolve_hit_target(x, y) {
            Some(HitTarget::Pane(block)) => Some(block),
            Some(HitTarget::FileEntry(_)) | Some(HitTarget::DirectoryChevron(_)) => {
                Some(FocusBlock::Files)
            }
            Some(HitTarget::Composer) => Some(FocusBlock::Composer),
            Some(HitTarget::ActivitySummary) => Some(FocusBlock::Workspace),
            Some(HitTarget::VisibleControl(_)) | Some(HitTarget::OverlayAction(_)) => None,
            None => None,
        }
    }

    fn scroll_files(&mut self, up: bool, amount: usize) {
        let visible_len = self.file_explorer.visible_nodes().len();
        if up {
            self.file_explorer.scroll = self.file_explorer.scroll.saturating_sub(amount);
        } else {
            self.file_explorer.scroll = self
                .file_explorer
                .scroll
                .saturating_add(amount)
                .min(visible_len.saturating_sub(1));
        }
    }

    fn scroll_workspace_under_pointer(&mut self, up: bool) {
        match self.workspace_navigation.current {
            WorkspaceView::Conversation => {
                if up {
                    self.scroll_conversation_up(3);
                } else {
                    self.scroll_conversation_down(3);
                }
            }
            WorkspaceView::File(_) => {
                let height = self.last_editor_height.saturating_sub(2) as usize;
                let delta = if up { -3 } else { 3 };
                self.source_viewer.move_cursor_vertical(delta, height);
            }
            WorkspaceView::Diff(_) | WorkspaceView::Run(_) => {}
        }
    }

    async fn activate_hit_target(&mut self, target: HitTarget) -> Result<(), TuiError> {
        match target {
            HitTarget::Pane(block) => {
                self.execute_semantic_command(SemanticCommand::FocusPane(block))
                    .await?;
            }
            HitTarget::FileEntry(path) => {
                self.execute_semantic_command(SemanticCommand::SelectEntry(path))
                    .await?;
            }
            HitTarget::DirectoryChevron(path) => {
                self.execute_semantic_command(SemanticCommand::ToggleDirectory(path))
                    .await?;
            }
            HitTarget::ActivitySummary => {
                self.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
                    .await?;
            }
            HitTarget::VisibleControl(command) => {
                self.execute_semantic_command(command).await?;
            }
            HitTarget::Composer => {
                self.execute_semantic_command(SemanticCommand::FocusComposer)
                    .await?;
            }
            HitTarget::OverlayAction(action) => {
                self.apply_overlay_action(action).await?;
            }
        }
        Ok(())
    }

    pub async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<(), TuiError> {
        if !self.runtime.mouse_capture {
            return Ok(());
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(target) = self.resolve_hit_target(mouse.column, mouse.row) else {
                    self.clear_pending_double_click();
                    return Ok(());
                };
                if (self.overlay.is_some() || self.explorer_dialog.is_some())
                    && !matches!(target, HitTarget::OverlayAction(_))
                {
                    self.clear_pending_double_click();
                    return Ok(());
                }
                let now = Instant::now();
                let double_click_target = Self::double_click_target_for(&target);
                if let Some(double_click_target) = double_click_target.clone() {
                    if self.is_qualifying_double_click(&double_click_target, MouseButton::Left, now)
                    {
                        self.clear_pending_double_click();
                        self.activate_double_click_target(double_click_target)
                            .await?;
                        self.invalidate_hit_regions();
                        return Ok(());
                    }
                } else {
                    self.clear_pending_double_click();
                }
                self.activate_hit_target(target).await?;
                if let Some(double_click_target) = double_click_target {
                    self.remember_double_click_candidate(
                        double_click_target,
                        MouseButton::Left,
                        now,
                    );
                } else {
                    self.invalidate_hit_regions();
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.clear_pending_double_click();
                if self.overlay.is_some() || self.explorer_dialog.is_some() {
                    return Ok(());
                }
                let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                match self.pane_target_at(mouse.column, mouse.row) {
                    Some(FocusBlock::Files) => self.scroll_files(up, 3),
                    Some(FocusBlock::Workspace) => self.scroll_workspace_under_pointer(up),
                    Some(
                        FocusBlock::Composer | FocusBlock::Inspector | FocusBlock::BottomPanel,
                    )
                    | None => {}
                }
            }
            _ => {
                self.clear_pending_double_click();
            }
        }
        Ok(())
    }
}

fn centered_capped_rect_for_mouse(
    area: ratatui::layout::Rect,
    max_width: u16,
    max_height: u16,
) -> ratatui::layout::Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn rect_contains(area: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && y >= area.y
        && x < area.x.saturating_add(area.width)
        && y < area.y.saturating_add(area.height)
}
