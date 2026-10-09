//! Mouse input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per the `input.rs` precedent (#19). v1 scope is
//! **vertical wheel only**, routed by focus (not pointer position). Selection
//! support (drag-to-select + right-click context menu, v1: the Editor pane) is
//! routed by pointer position within the editor rect. Pointer motion enables
//! hover highlights (`EnableMouseCapture` includes any-motion tracking), so
//! `Moved` events preview rows without ever moving keyboard focus.

use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use ratatui::layout::Rect;
use std::time::{Duration, Instant};

use crate::clipboard;
use crate::selection::{self, cell_inside, Cell, ContextMenuItem, CopyPane};

/// Two clicks within this window at the same cell count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Conversation/explorer rows moved per plain wheel notch.
///
/// Terminals report a wheel notch rather than a pixel delta. Moving a single
/// row makes scrolling feel stalled, especially in the conversation pane, so
/// use the conventional three-row wheel increment.
const WHEEL_NOTCH: isize = 3;
/// File-tree rows moved per shift+wheel.
///
/// The conversation's shift+wheel is a page instead — it uses the measured
/// [`TuiApp::conversation_page_rows`], because a list selection has no page
/// size to match.
const WHEEL_PAGE: isize = 5;

impl TuiApp {
    /// Route a terminal mouse event.
    pub(crate) async fn handle_mouse(&mut self, event: MouseEvent) -> Result<(), TuiError> {
        // A context menu owns the pointer while it is open.
        if self.context_menu.is_some() {
            self.handle_mouse_context_menu(&event);
            return Ok(());
        }
        if !self.explorer_dialog.is_open()
            && (self.handle_child_approval_mouse(event).await?
                || self.handle_task_stop_mouse(event).await?
                || self.handle_tasks_view_mouse(event).await?)
        {
            return Ok(());
        }

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.dispatch_mouse_scroll(-1, event.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::ScrollDown => {
                self.dispatch_mouse_scroll(1, event.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.pointer_blocked() && self.start_mouse_resize(event.column, event.row) {
                    return Ok(());
                }
                self.mouse_click(event.column, event.row).await?;
                self.mouse_start_selection(event.column, event.row);
            }
            MouseEventKind::Drag(_) => {
                if self.drag_mouse_resize(event.column, event.row) {
                    return Ok(());
                }
                self.mouse_update_selection(event.column, event.row);
            }
            MouseEventKind::Moved => {
                self.mouse_hover(event.column, event.row);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if !self.finish_mouse_resize() {
                    self.mouse_finish_selection();
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.mouse_open_context_menu(event.column, event.row);
            }
            // The right-button release completes opening the menu; it must not
            // be interpreted as an outside click against the newly-created menu.
            MouseEventKind::Up(MouseButton::Right) => {}
            // Horizontal wheel / other buttons are ignored cheaply.
            _ => {}
        }
        Ok(())
    }

    /// Block click/selection routing when a modal or transient surface owns the
    /// pointer (mirrors the wheel guard's precedence in `dispatch_mouse_scroll`).
    fn pointer_blocked(&self) -> bool {
        self.explorer_dialog.is_open()
            || self.session_view.pending_hitl.is_some()
            || self.overlay.is_some()
            // The search/jump-to-line prompts take every keystroke, so a click
            // landing behind one cannot act on what it names: a caret placed
            // under a prompt nobody can type into is a caret the keyboard can
            // no longer reach. `dispatch_mouse_scroll` already refuses the wheel
            // for the same reason.
            || matches!(
                self.focus.mode(),
                FocusMode::Transient(TransientOwner::SourceSearch | TransientOwner::JumpToLine)
            )
    }

    /// Focus the block under the pointer and act on the hit row/tab. A click
    /// never starts a drag by itself; `mouse_start_selection` runs after and
    /// only engages on the copyable panes.
    async fn mouse_click(&mut self, col: u16, row: u16) -> Result<(), TuiError> {
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(at, c, r)| {
            now.duration_since(at) <= DOUBLE_CLICK && c == col && r == row
        });
        self.last_click = Some((now, col, row));

        // The inline approval/question card is live even while it pends — the
        // pointer guard below is for text selection under modal surfaces, not
        // for the card's own option rows.
        if let Some(index) = self.option_at(col, row) {
            self.click_option(index, double).await?;
            return Ok(());
        }
        // Overlay list rows (model picker, resume, sessions, themes) are
        // mouse-actionable too: same grammar as the approval card.
        if let Some(hit) = self.overlay_row_at(col, row) {
            self.click_overlay_row(hit, double).await?;
            return Ok(());
        }
        if self.overlay.is_none()
            && !self.explorer_dialog.is_open()
            && !matches!(self.focus.mode(), FocusMode::Transient(_))
            && self.dock_paint.owner == self.selected_session_id
        {
            if let Some((filter, _)) = self
                .dock_paint
                .chips
                .iter()
                .find(|(_, area)| cell_inside(*area, col, row))
            {
                self.open_tasks_view(Some(*filter));
                return Ok(());
            }
        }
        if self.pointer_blocked() {
            return Ok(());
        }
        // The composer popups float above the input, over the sidebar column,
        // so their rows have to be claimed before the surface they cover.
        if let Some(index) = self
            .slash_popup_rows
            .iter()
            .find(|(_, rect)| cell_inside(*rect, col, row))
            .map(|(index, _)| *index)
        {
            self.slash_suggestions.selected = index;
            self.enter_chat_composer();
            // The second click accepts, the way `Tab` does.
            if double {
                self.complete_slash_suggestion();
            }
            return Ok(());
        }
        if let Some(index) = self
            .inline_search_rows
            .iter()
            .find(|(_, rect)| cell_inside(*rect, col, row))
            .map(|(index, _)| *index)
        {
            if let Some(state) = self.inline_search.as_mut() {
                state.selected = index;
            }
            self.enter_chat_composer();
            // The second click accepts, the way `Enter` does.
            if double {
                self.insert_inline_search_selection();
            }
            return Ok(());
        }
        if let Some((index, _)) = self
            .start_prompt_rows
            .iter()
            .find(|(_, area)| cell_inside(*area, col, row))
        {
            self.use_start_prompt(*index);
            return Ok(());
        }
        if let Some((tab, _)) = self
            .workspace_tab_areas
            .iter()
            .find(|(_, area)| cell_inside(*area, col, row))
        {
            self.select_workspace_tab(*tab);
            return Ok(());
        }
        // With the navigator column collapsed, the status-bar sessions chip is
        // the explicit entry into the session chooser.
        if let Some(area) = self.sessions_chip_area {
            if cell_inside(area, col, row) {
                self.open_session_switcher();
                return Ok(());
            }
        }
        // The `+` cell is checked before the tab row: it shares the `Sessions`
        // tab's right edge and the `Files` tab's left edge, so the tab branch
        // would otherwise claim the click as a tab switch.
        if let Some(area) = self.navigator_new_session_area {
            if cell_inside(area, col, row) {
                self.click_navigator_new_session();
                return Ok(());
            }
        }
        if let Some(area) = self.navigator_tabs_area {
            if cell_inside(area, col, row) {
                self.click_navigator_tab(col, area);
                return Ok(());
            }
        }
        if let Some(area) = self.sessions_list_area {
            if cell_inside(area, col, row) {
                self.navigator_tab_row_focused = false;
                self.click_session_row(row, area, double).await?;
                return Ok(());
            }
        }
        if let Some(area) = self.navigator_list_area {
            if cell_inside(area, col, row) {
                self.horizontal_session_strip_focused = false;
                // Same rule as the tab click above: a click on the row's own
                // surface hands the keyboard to the pane the row sits above,
                // so the tab row stops holding it. Without this the click's
                // focus change is invisible — the pane paints unfocused and
                // every bare key still goes to the row.
                self.navigator_tab_row_focused = false;
                if self.workspace_navigation.selected_tab() == WorkspaceTab::Agent {
                    self.click_session_row(row, area, double).await?;
                } else {
                    self.click_file_row(row, area, double).await?;
                }
                return Ok(());
            }
        }
        if let Some(area) = self.task_strip_area {
            if cell_inside(area, col, row) {
                let Some(index) = self
                    .task_strip_chips
                    .iter()
                    .find(|(_, rect)| cell_inside(*rect, col, row))
                    .map(|(index, _)| *index)
                else {
                    return Ok(());
                };
                self.task_strip_selection = index;
                self.navigator_tab_row_focused = false;
                self.horizontal_session_strip_focused = true;
                self.focus_block(FocusBlock::TaskStrip);
                if double {
                    self.handle_task_strip_key(crossterm::event::KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    ))
                    .await?;
                }
                return Ok(());
            }
        }
        if let Some(area) = self.composer_area {
            if cell_inside(area, col, row) {
                self.enter_chat_composer();
                return Ok(());
            }
        }
        if let Some(area) = self.queue_area {
            if cell_inside(area, col, row) {
                self.click_queue_row(col, row, area);
                return Ok(());
            }
        }
        if let Some(area) = self.background_area {
            if cell_inside(area, col, row) {
                self.click_background_row(col, row, area, double).await?;
                return Ok(());
            }
        }
        if let Some(area) = self.footer_area {
            if cell_inside(area, col, row) {
                self.focus_block(FocusBlock::Footer);
                if let Some(chip) = self.footer_chip_at(col, row) {
                    self.composer_chip_focus = Some(chip);
                    // The notes chip is an action, not a picker: clicking it
                    // toggles the scratchpad rather than opening a menu, and it
                    // never takes footer focus (so Enter still sends).
                    if chip == 2 {
                        if self.scratchpad.is_some() {
                            self.close_scratchpad();
                        } else {
                            self.open_scratchpad();
                        }
                        return Ok(());
                    }
                    let focus = if chip == 0 {
                        FooterFocus::Llm
                    } else {
                        FooterFocus::Effort
                    };
                    self.activate_composer_chip(focus).await?;
                }
                return Ok(());
            }
        }
        if let Some(area) = self.conversation_area {
            if cell_inside(area, col, row) {
                self.focus_block(FocusBlock::Sidebar);
                return Ok(());
            }
        }
        if let Some(area) = self.editor_area {
            if cell_inside(area, col, row) {
                self.focus_block(FocusBlock::Workspace);
                self.click_place_caret(col, row, area);
                return Ok(());
            }
        }
        if let Some(area) = self.terminal_area {
            if cell_inside(area, col, row) {
                self.focus_block(FocusBlock::BottomPanel);
                return Ok(());
            }
        }
        Ok(())
    }

    /// A click on a queued-message row selects it. Select is all it does: the
    /// strip's only per-row verb is destructive (`CancelSelectedQueueMessage`),
    /// and `EditLastQueuedMessage` always edits the *last* message rather than
    /// the selected one, so there is no non-destructive act a second click
    /// could perform.
    fn click_queue_row(&mut self, col: u16, row: u16, area: Rect) {
        if self.dock_paint.owner != self.selected_session_id {
            return;
        }
        let len = self.dock_paint.queue_ids.len();
        if let Some(index) = crate::widgets::queued_messages::message_index_at(
            len,
            self.dock_paint.queue_selected,
            area,
            col,
            row,
        ) {
            self.task_selection
                .select_queue(self.selected_session_id, self.dock_paint.queue_ids[index]);
        }
    }

    /// A click on a background-task row selects it and takes the keyboard,
    /// matching the strip's own grammar. A double click opens the matching
    /// live task filter without inserting a result or resolving a request.
    async fn click_background_row(
        &mut self,
        col: u16,
        row: u16,
        area: Rect,
        double: bool,
    ) -> Result<(), TuiError> {
        self.focus_block(FocusBlock::Sidebar);
        if self.dock_paint.owner != self.selected_session_id {
            return Ok(());
        }
        if let Some(index) = crate::widgets::background_strip::row_index_at(
            &self.dock_paint.background.rows,
            area,
            col,
            row,
        ) {
            self.task_selection.select_task(
                self.selected_session_id,
                self.dock_paint.background.rows[index].id,
            );
            if double {
                let filter = if self.dock_paint.background.rows[index].subagent {
                    crate::tasks_strip::TaskFilter::Agents
                } else {
                    crate::tasks_strip::TaskFilter::Jobs
                };
                self.open_tasks_view(Some(filter));
            }
        }
        Ok(())
    }

    /// Use the editor's native viewport mapping. Transformed previews
    /// have no source caret mapping and never move the hidden editor cursor.
    fn click_place_caret(&mut self, col: u16, row: u16, area: Rect) {
        if self.source_viewer.markdown_preview || self.source_viewer.text_preview {
            return;
        }
        let body = self.source_viewer.rendered_text.area;
        if row < body.y || row >= body.bottom() || !cell_inside(area, col, row) {
            return;
        }
        if let Some(editor) = self.editor_session.as_mut() {
            editor.place_cursor_at(col.max(body.x).min(body.right().saturating_sub(1)), row);
            self.source_viewer.current_line = editor.cursor_row();
            return;
        }
        let line = self.source_viewer.top_line + (row - body.y) as usize;
        if line < self.source_viewer.lines.len() {
            self.source_viewer.current_line = line;
        }
    }

    /// A click on the navigator's `Sessions` heading hands the keyboard to the
    /// session list. Files and Git are main-workspace tabs, not navigator tabs.
    fn click_navigator_tab(&mut self, col: u16, area: Rect) {
        let sessions_width = crate::widgets::navigator::SESSIONS_TAB_WIDTH.min(area.width);
        if col >= area.x.saturating_add(sessions_width) {
            return;
        }
        // A click hands the keyboard to the pane, so the row stops holding it.
        self.navigator_tab_row_focused = false;
        self.focus_block(FocusBlock::TaskStrip);
    }

    /// A click on the navigator row's `+` cell creates a session: the same verb
    /// the row's `Enter` runs on that cell, and the same one `n` runs in the
    /// Sessions list.
    fn click_navigator_new_session(&mut self) {
        // A click stops the row holding the keyboard, exactly as a tab click
        // does, so the pane on screen takes the keys back with the click.
        self.navigator_tab_row_focused = false;
        self.create_session_now();
    }

    /// Session rows are two visual lines each. Peek expansion shifts later
    /// rows, so the mapping is approximate below the focused row — good
    /// enough for a click, and Enter remains the exact path.
    async fn click_session_row(
        &mut self,
        row: u16,
        area: Rect,
        double: bool,
    ) -> Result<(), TuiError> {
        let index = row.saturating_sub(area.y) as usize / 2;
        if index >= self.session_chrome.len() {
            return Ok(());
        }
        self.focus_block(FocusBlock::TaskStrip);
        self.task_strip_selection = index;
        if double {
            self.handle_task_strip_key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))
            .await?;
        }
        Ok(())
    }

    /// Select a file row in either the Files tree or grouped Git changes.
    async fn click_file_row(&mut self, row: u16, area: Rect, double: bool) -> Result<(), TuiError> {
        if matches!(
            self.workspace_navigation.current(),
            Some(WorkspaceView::GithubIssues)
        ) {
            if self.github_view.loading || row < area.y + 2 {
                return Ok(());
            }
            let local = self
                .github_view
                .list_start(area.height.saturating_sub(3) as usize)
                + row.saturating_sub(area.y + 2) as usize;
            if let Some(index) = self.github_view.visible().get(local).copied() {
                self.github_view.selected = index;
                self.github_view
                    .details(self.session_view.workspace_root().to_path_buf());
                self.focus_block(if double {
                    FocusBlock::Workspace
                } else {
                    FocusBlock::Files
                });
            }
            return Ok(());
        }
        match self.workspace_navigation.selected_tab() {
            WorkspaceTab::Files => {
                let tree_top = area.y + crate::file_explorer::TREE_ROW_OFFSET;
                if row < tree_top {
                    if row > area.y {
                        self.focus_block(FocusBlock::Search);
                    }
                    return Ok(());
                }
                if row >= area.bottom().saturating_sub(1) {
                    return Ok(());
                }
                let index = self.workspace_files.explorer.scroll + (row - tree_top) as usize;
                if index < self.workspace_files.explorer.visible_nodes().len() {
                    self.focus_block(FocusBlock::Files);
                    self.workspace_files.explorer.select_visible_row(index);
                    self.execute_semantic_command(SemanticCommand::OpenSelectedEntry)
                        .await?;
                }
            }
            WorkspaceTab::Git => {
                let local = row.saturating_sub(area.y) as usize;
                if let Some(index) = crate::widgets::git_changes::GitChangesList::absolute_file_at(
                    &self.diff_view.entries,
                    local,
                    self.diff_view.selected,
                    area.height as usize,
                ) {
                    self.diff_view.select(index);
                    self.focus_block(FocusBlock::Files);
                    // A git row selects on the first click and acts on the
                    // second, like a session row: the patch lives in the
                    // Workspace block, so acting means handing the keyboard
                    // there. `Enter` is that same move (`git_review_key`).
                    if double {
                        self.git_review_key(crossterm::event::KeyEvent::new(
                            KeyCode::Enter,
                            KeyModifiers::NONE,
                        ))
                        .await?;
                    }
                }
            }
            WorkspaceTab::Agent => {}
        }
        Ok(())
    }

    /// Pointer motion updates hover highlights only — it never moves focus and
    /// never starts a selection. Terminals that do not report motion simply
    /// never send `Moved`; the highlight is then never triggered.
    fn mouse_hover(&mut self, col: u16, row: u16) {
        self.hover_start_prompt = None;
        self.hover_session = None;
        self.hover_file = None;
        self.hover_chip = None;
        self.hover_navigator_tab = None;
        self.hover_navigator_new_session = false;
        self.hover_queue = None;
        self.hover_background = None;
        self.hover_option = self.option_at(col, row);
        self.hover_overlay = self.overlay_row_at(col, row);
        self.hover_resize = None;
        if self.pointer_blocked() {
            return;
        }
        // Floating composer menus cover navigation and starter rows too.
        if self
            .slash_popup_rows
            .iter()
            .chain(self.inline_search_rows.iter())
            .any(|(_, area)| cell_inside(*area, col, row))
        {
            return;
        }
        // The seam itself is the target, so the grip lights up for the whole
        // ±1-cell grab zone the click-to-drag path already accepts.
        self.hover_resize = self.resize_boundary_at(col, row);
        self.hover_start_prompt = self
            .start_prompt_rows
            .iter()
            .find(|(_, area)| cell_inside(*area, col, row))
            .map(|(index, _)| *index);
        if let Some(area) = self.queue_area {
            self.hover_queue = crate::widgets::queued_messages::message_index_at(
                self.dock_paint.queue_ids.len(),
                self.dock_paint.queue_selected,
                area,
                col,
                row,
            );
        }
        if let Some(area) = self.background_area {
            self.hover_background = crate::widgets::background_strip::row_index_at(
                &self.dock_paint.background.rows,
                area,
                col,
                row,
            );
        }
        if let Some(area) = self.footer_area {
            if cell_inside(area, col, row) {
                self.hover_chip = self.footer_chip_at(col, row);
            }
        }
        if let Some(area) = self.navigator_new_session_area {
            if cell_inside(area, col, row) {
                self.hover_navigator_new_session = true;
            }
        }
        if !self.hover_navigator_new_session {
            if let Some(area) = self.navigator_tabs_area {
                if cell_inside(area, col, row) {
                    self.hover_navigator_tab = navigator_tab_at(col, area);
                }
            }
        }
        if let Some(area) = self.sessions_list_area {
            if cell_inside(area, col, row) {
                let index = row.saturating_sub(area.y) as usize / 2;
                if index < self.session_chrome.len() {
                    self.hover_session = Some(index);
                }
                return;
            }
        }
        let Some(area) = self.navigator_list_area else {
            return;
        };
        if !cell_inside(area, col, row) {
            return;
        }
        match self.effective_navigator_tab() {
            crate::widgets::NavigatorTab::Sessions => {
                let index = row.saturating_sub(area.y) as usize / 2;
                if index < self.session_chrome.len() {
                    self.hover_session = Some(index);
                }
            }
            crate::widgets::NavigatorTab::Git => {
                let local = row.saturating_sub(area.y) as usize;
                self.hover_file = crate::widgets::git_changes::GitChangesList::absolute_file_at(
                    &self.diff_view.entries,
                    local,
                    self.diff_view.selected,
                    area.height as usize,
                );
            }
            crate::widgets::NavigatorTab::Files => {
                let tree_top = area.y + crate::file_explorer::TREE_ROW_OFFSET;
                if row >= tree_top && row < area.bottom().saturating_sub(1) {
                    let index = self.workspace_files.explorer.scroll + (row - tree_top) as usize;
                    if index < self.workspace_files.explorer.visible_nodes().len() {
                        self.hover_file = Some(index);
                    }
                }
            }
        }
    }

    /// Which footer chip (0 = model, 1 = effort) sits under a pointer cell,
    /// if any. Uses the x-ranges captured during the last footer paint.
    fn footer_chip_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.footer_area?;
        if row != area.y {
            return None;
        }
        let ranges = self.footer_chip_rects?;
        if (ranges[0].0..ranges[0].1).contains(&col) {
            Some(0)
        } else if (ranges[1].0..ranges[1].1).contains(&col) {
            Some(1)
        } else if ranges[2].0 < ranges[2].1 && (ranges[2].0..ranges[2].1).contains(&col) {
            Some(2)
        } else {
            None
        }
    }

    /// The overlay list row under a pointer cell, if any. Uses the row rects
    /// captured during the overlay paint, so hover and click share one
    /// geometry with the renderer.
    fn overlay_row_at(&self, col: u16, row: u16) -> Option<crate::overlays::OverlayRow> {
        self.overlay.as_ref()?;
        self.overlay_rows
            .borrow()
            .iter()
            .find(|(_, rect)| cell_inside(*rect, col, row))
            .map(|(hit, _)| *hit)
    }

    /// Select an overlay list row. A single click moves the highlight (and the
    /// focused column for the unified picker); a double-click confirms with the
    /// same Enter path the keyboard uses, so side effects stay in one place.
    async fn click_overlay_row(
        &mut self,
        hit: crate::overlays::OverlayRow,
        double: bool,
    ) -> Result<(), TuiError> {
        use crate::overlays::{ConnectModelColumn, Overlay, OverlayRow};

        let theme_preview = {
            let Some(overlay) = self.overlay.as_mut() else {
                return Ok(());
            };
            match (hit, &mut *overlay) {
                (
                    OverlayRow::Provider(index),
                    Overlay::ConnectModel {
                        provider_cursor,
                        focus,
                        ..
                    },
                ) => {
                    *focus = ConnectModelColumn::Providers;
                    *provider_cursor = index;
                }
                (
                    OverlayRow::Model(index),
                    Overlay::ConnectModel {
                        model_selected,
                        focus,
                        ..
                    },
                ) => {
                    *focus = ConnectModelColumn::Models;
                    *model_selected = index;
                }
                (
                    OverlayRow::Effort(index),
                    Overlay::ConnectModel {
                        effort_selected,
                        focus,
                        ..
                    },
                ) => {
                    *focus = ConnectModelColumn::Effort;
                    *effort_selected = index;
                }
                (OverlayRow::Resume(index), Overlay::ResumePicker { selected, .. }) => {
                    *selected = index;
                }
                (OverlayRow::Session(index), Overlay::SessionSwitcher { selected, .. }) => {
                    *selected = index;
                }
                (OverlayRow::Theme(index), Overlay::Theme { selected, .. }) => {
                    *selected = index;
                }
                (OverlayRow::Issue(index), Overlay::GithubIssues { selected, .. }) => {
                    // Only the issue selection moves, exactly as `Up`/`Down`
                    // do: the action menu keeps its own highlight and the index
                    // it last had, so clicking an issue can never silently
                    // retarget a choice the operator already made.
                    *selected = index;
                }
                (OverlayRow::IssueAction(index), Overlay::GithubIssues { action, .. }) => {
                    *action = index;
                }
                (OverlayRow::CommitSuggest(index), Overlay::GitCommitSuggest { selected }) => {
                    *selected = index;
                }
                (OverlayRow::Branch(index), Overlay::GitBranch { selected, .. }) => {
                    *selected = index;
                }
                (OverlayRow::FileExplorer(index), Overlay::FileExplorer { selected, .. }) => {
                    *selected = index;
                }
                _ => return Ok(()),
            }
            matches!(overlay, Overlay::Theme { .. })
        };

        if theme_preview {
            // Moving the theme cursor must re-theme, exactly as Up/Down does.
            let action = self
                .overlay
                .as_mut()
                .map(|overlay| crate::overlays::theme_preview_action(overlay))
                .unwrap_or(OverlayAction::None);
            self.apply_overlay_action(action).await?;
        }
        if double {
            let action = self
                .overlay
                .as_mut()
                .map(|overlay| handle_overlay_key(overlay, OverlayKey::Enter))
                .unwrap_or(OverlayAction::None);
            self.apply_overlay_action(action).await?;
        }
        Ok(())
    }

    /// The option index of the pending approval/question card under the
    /// pointer, if any. Uses the rects captured during the conversation paint.
    fn option_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.overlay.is_some() || self.explorer_dialog.is_open() || self.inline_search.is_some()
        {
            return None;
        }
        if !(self.session_view.is_awaiting_approval() || self.session_view.is_awaiting_question()) {
            return None;
        }
        self.option_rects
            .iter()
            .find(|(_, rect)| cell_inside(*rect, col, row))
            .map(|(index, _)| *index)
    }

    /// Click an option row: first click moves the highlight, a double-click
    /// confirms (same as Enter). Driven through the existing menu handlers so
    /// the card's own focus/selection invariants stay in one place.
    async fn click_option(&mut self, index: usize, double: bool) -> Result<(), TuiError> {
        let approval = self.session_view.is_awaiting_approval();
        let question = self.session_view.is_awaiting_question();
        if approval {
            self.sync_approval_focus();
        } else if question {
            self.sync_question_focus();
        } else {
            return Ok(());
        }
        let current = if approval {
            self.approval_menu_selected()
        } else {
            self.question_menu_indexes().1
        };
        self.step_option(current, index, approval).await?;
        if double {
            let key = crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            if approval {
                self.handle_approval_menu_key(key).await?;
            } else {
                self.handle_question_menu_key(key).await?;
            }
        }
        Ok(())
    }

    /// Walk the card's highlight to `target` with the same Up/Down keys the
    /// keyboard uses, so clamping and sync run exactly once per step. Stops if
    /// a step does not move (clamped) to avoid a spin.
    async fn step_option(
        &mut self,
        current: usize,
        target: usize,
        approval: bool,
    ) -> Result<(), TuiError> {
        let mut index = current;
        while index != target {
            let code = if target > index {
                KeyCode::Down
            } else {
                KeyCode::Up
            };
            let key = crossterm::event::KeyEvent::new(code, KeyModifiers::NONE);
            if approval {
                self.handle_approval_menu_key(key).await?;
            } else {
                self.handle_question_menu_key(key).await?;
            }
            let next = if approval {
                self.approval_menu_selected()
            } else {
                self.question_menu_indexes().1
            };
            if next == index {
                break;
            }
            index = next;
        }
        Ok(())
    }

    fn mouse_start_selection(&mut self, col: u16, row: u16) {
        let pane = if self
            .editor_area
            .is_some_and(|area| cell_inside(area, col, row))
            && self.current_workspace_is_file()
        {
            Some(CopyPane::Editor)
        } else if self
            .conversation_area
            .is_some_and(|area| cell_inside(area, col, row))
        {
            Some(CopyPane::Conversation)
        } else if self
            .terminal_area
            .is_some_and(|area| cell_inside(area, col, row))
        {
            Some(CopyPane::Terminal)
        } else {
            None
        };
        if self.pointer_blocked() || pane.is_none() {
            self.selection.clear();
            return;
        }
        self.selection.start_in(pane.unwrap(), Cell { row, col });
    }

    fn mouse_update_selection(&mut self, col: u16, row: u16) {
        if !self.selection.is_dragging() {
            return;
        }
        if self.selection.pane == Some(CopyPane::Conversation) {
            if let Some(area) = self.conversation_area {
                let before = self.conversation_view.scroll;
                if row <= area.y {
                    self.scroll_conversation_up(WHEEL_NOTCH as u16);
                } else if row >= area.bottom().saturating_sub(1) {
                    self.scroll_conversation_down(WHEEL_NOTCH as u16);
                }
                let delta = self.conversation_view.scroll as i32 - before as i32;
                self.selection.shift_rows(delta);
                self.conversation_copy_top = self
                    .conversation_copy_top
                    .saturating_add_signed(-(delta as isize));
            }
        }
        self.selection.update(Cell { row, col });
    }

    fn mouse_finish_selection(&mut self) {
        // Guard on `is_dragging`, not `is_active`: a spurious duplicate Up
        // event after the drag already finished must be a no-op, not
        // re-derive and re-copy the same text again.
        if !self.selection.is_dragging() {
            return;
        }
        // A click without a drag (anchor == current) is not a selection —
        // treat it as "click elsewhere to deselect" rather than copying a
        // single cell.
        let dragged = self
            .selection
            .rect()
            .is_some_and(|r| r.row_start != r.row_end || r.start_col != r.end_col);
        if !dragged {
            self.selection.clear();
            return;
        }
        let text = match self.selection.pane {
            Some(CopyPane::Editor) => self
                .source_viewer
                .rendered_text
                .selection_text(&self.selection, false),
            Some(CopyPane::Conversation) => match self.conversation_area {
                Some(area) => self.conversation_selection_text(area),
                None => String::new(),
            },
            Some(CopyPane::Terminal) => match self.terminal_area {
                Some(area) => {
                    if self.terminal_text.area == area {
                        self.terminal_text.selection_text(&self.selection, false)
                    } else {
                        selection::visible_rows_selection_text(
                            &self.terminal_rows,
                            area,
                            &self.selection,
                            false,
                        )
                    }
                }
                None => String::new(),
            },
            None => String::new(),
        };
        self.selection.finish(text.clone());
        if text.is_empty() {
            return;
        }
        let lines = text.lines().count().max(1);
        match clipboard::write_osc52(&text) {
            Ok(()) => {
                let noun = if lines == 1 { "line" } else { "lines" };
                self.set_feedback(
                    crate::widgets::FeedbackSeverity::Ok,
                    format!("Copied {lines} {noun}"),
                );
            }
            Err(error) => self.set_feedback(
                crate::widgets::FeedbackSeverity::Error,
                format!("Copy failed: {error}"),
            ),
        }
    }

    pub(super) fn conversation_selection_text(&self, area: Rect) -> String {
        if self.conversation_all_rows.is_empty() {
            return selection::visible_rows_selection_text(
                &self.conversation_rows,
                area,
                &self.selection,
                true,
            );
        }
        selection::rows_selection_text(
            &self.conversation_all_rows,
            self.conversation_copy_top,
            area,
            &self.selection,
            true,
        )
    }

    fn mouse_open_context_menu(&mut self, col: u16, row: u16) {
        if self.pointer_blocked() {
            return;
        }
        let x = col.saturating_add(1);
        let y = row.saturating_add(1);
        let mut menu = selection::ContextMenu::new(x, y);
        menu.fit(self.pane_resize.frame_area);
        self.context_menu = Some(menu);
    }

    fn handle_mouse_context_menu(&mut self, event: &MouseEvent) {
        let (col, row) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right) => {
                let inside = self
                    .context_menu
                    .as_ref()
                    .is_some_and(|menu| menu.index_at(col, row).is_some());
                if !inside {
                    self.context_menu = None;
                } else if let Some(menu) = self.context_menu.as_mut() {
                    if let Some(i) = menu.index_at(col, row) {
                        menu.selected = i;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let action = self
                    .context_menu
                    .as_ref()
                    .and_then(|menu| menu.index_at(col, row))
                    .map(|i| menu_item_at(&self.context_menu, i));
                if let Some(action) = action {
                    self.activate_context_menu(action);
                }
            }
            _ => {}
        }
    }

    fn activate_context_menu(&mut self, action: ContextMenuItem) {
        match action {
            ContextMenuItem::Copy => {
                if self.selection.text.is_empty() {
                    self.set_feedback(
                        crate::widgets::FeedbackSeverity::Error,
                        "Nothing selected to copy",
                    );
                } else {
                    let text = self.selection.text.clone();
                    match clipboard::write_osc52(&text) {
                        Ok(()) => self.set_feedback(
                            crate::widgets::FeedbackSeverity::Ok,
                            "Copied selection to clipboard",
                        ),
                        Err(error) => self.set_feedback(
                            crate::widgets::FeedbackSeverity::Error,
                            format!("Copy failed: {error}"),
                        ),
                    }
                    self.selection.clear();
                }
                self.context_menu = None;
            }
            ContextMenuItem::ClearSelection => {
                self.selection.clear();
                self.context_menu = None;
            }
        }
    }

    pub(super) fn handle_context_menu_key(&mut self, key: event::KeyEvent) {
        match key.code {
            KeyCode::Esc => self.context_menu = None,
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.selected = (menu.selected + 1) % menu.items.len();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.selected = menu
                        .selected
                        .checked_sub(1)
                        .unwrap_or(menu.items.len().saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                let action = self
                    .context_menu
                    .as_ref()
                    .map(|menu| menu_item_at(&self.context_menu, menu.selected));
                if let Some(action) = action {
                    self.activate_context_menu(action);
                }
            }
            _ => {}
        }
    }

    fn dispatch_mouse_scroll(&mut self, direction: isize, shift: bool) {
        // Mirror `handle_key`'s guard precedence: while a modal or transient
        // surface is active the wheel must not scroll a pane the user cannot
        // see (or the transcript hidden beneath an overlay).
        if self.explorer_dialog.is_open()
            || self.session_view.pending_hitl.is_some()
            || self.overlay.is_some()
            || matches!(
                self.focus.mode(),
                FocusMode::Transient(TransientOwner::SourceSearch | TransientOwner::JumpToLine)
            )
        {
            return;
        }

        match self.focus.block() {
            // Composer and Sidebar are the two owners of the conversation
            // column. The composer is its resting focus, and the wheel over it
            // scrolls the transcript behind it; Sidebar is what a click on the
            // transcript focuses, and §8.6 gives the focused pane's content to
            // the wheel. Both page identically.
            FocusBlock::Composer | FocusBlock::Sidebar => {
                self.mouse_scroll_conversation(direction, shift);
            }
            // Workspace is the CHAT panel; it hosts the source viewer when a
            // file is open and the conversation otherwise.
            FocusBlock::Workspace
                if matches!(
                    self.workspace_navigation.current(),
                    Some(WorkspaceView::GithubIssues)
                ) =>
            {
                self.github_view.scroll = self.github_view.scroll.saturating_add_signed(
                    (direction * if shift { WHEEL_PAGE } else { WHEEL_NOTCH }) as i16,
                );
            }
            FocusBlock::Files | FocusBlock::Search
                if matches!(
                    self.workspace_navigation.current(),
                    Some(WorkspaceView::GithubIssues)
                ) =>
            {
                if self.github_view.loading {
                    return;
                }
                let visible = self.github_view.visible();
                let at = visible
                    .iter()
                    .position(|index| *index == self.github_view.selected)
                    .unwrap_or(0);
                if let Some(index) = visible
                    .get(
                        at.saturating_add_signed(direction)
                            .min(visible.len().saturating_sub(1)),
                    )
                    .copied()
                {
                    self.github_view.selected = index;
                    self.github_view
                        .details(self.session_view.workspace_root().to_path_buf());
                }
            }
            FocusBlock::Workspace if self.current_workspace_is_file() => {
                self.mouse_scroll_source_viewer(direction, shift);
            }
            FocusBlock::Workspace => self.mouse_scroll_conversation(direction, shift),
            FocusBlock::Files | FocusBlock::Search => {
                let step = if shift { WHEEL_PAGE } else { WHEEL_NOTCH };
                self.workspace_files
                    .explorer
                    .move_selection(direction * step);
            }
            FocusBlock::BottomPanel => {
                if let Some(terminal) = self.interactive_terminal.as_mut() {
                    let step = if shift { WHEEL_PAGE } else { WHEEL_NOTCH };
                    terminal.scroll(-direction * step);
                }
            }
            // Approval has no scroll target of its own. TaskStrip is one
            // too — the session navigator moves its selection on ↑↓ and takes
            // no wheel, unlike the file tree beside it.
            _ => {}
        }
    }

    fn mouse_scroll_conversation(&mut self, direction: isize, shift: bool) {
        // Shift pages by the same amount keyboard `PageUp`/`PageDown` use in
        // this pane. It cannot reuse the file tree's fixed `WHEEL_PAGE`: a page
        // is measured from the drawn pane, not assumed.
        let amount = if shift {
            self.conversation_page_rows()
        } else {
            WHEEL_NOTCH as u16
        };
        if direction < 0 {
            self.scroll_conversation_up(amount);
        } else {
            self.scroll_conversation_down(amount);
        }
    }

    fn mouse_scroll_source_viewer(&mut self, direction: isize, shift: bool) {
        // Preview navigation takes precedence over its editable backing buffer,
        // just as it does in `handle_editor_key` / `handle_preview_key`.
        if self.source_viewer.markdown_preview || self.source_viewer.text_preview {
            let height = self.editor_viewport.height.saturating_sub(4).max(1) as usize;
            let step = if shift { height as isize } else { WHEEL_NOTCH };
            self.source_viewer
                .scroll_preview(if direction < 0 { -step } else { step }, height);
            return;
        }
        // Page height matches the editor key handler so shift+wheel ==
        // PageUp/PageDown exactly.
        let page = self.editor_viewport.height.saturating_sub(2) as isize;
        let delta = if shift {
            if direction < 0 {
                -page
            } else {
                page
            }
        } else {
            direction * WHEEL_NOTCH
        };
        if let Some(editor) = self.editor_session.as_mut() {
            let key = if shift {
                if direction < 0 {
                    crossterm::event::KeyCode::PageUp
                } else {
                    crossterm::event::KeyCode::PageDown
                }
            } else if delta < 0 {
                crossterm::event::KeyCode::Up
            } else {
                crossterm::event::KeyCode::Down
            };
            let steps = if shift {
                1
            } else {
                delta.unsigned_abs().max(1)
            };
            for _ in 0..steps {
                editor.handle_key(crossterm::event::KeyEvent::new(
                    key,
                    crossterm::event::KeyModifiers::NONE,
                ));
            }
            self.source_viewer.current_line = editor.cursor_row();
            return;
        }
        self.source_viewer
            .move_cursor_vertical(delta, page.max(1) as usize);
    }
}

/// The sidebar renders only Sessions; workspace tabs have their own hit areas.
fn navigator_tab_at(col: u16, area: Rect) -> Option<crate::widgets::NavigatorTab> {
    crate::widgets::navigator::navigator_tab_at(col, area, false)
        .filter(|tab| *tab == crate::widgets::NavigatorTab::Sessions)
}

/// Resolve the selected menu item by index (indirection to sidestep borrow
/// conflicts between `self.context_menu` reads and `&mut self` mutation).
fn menu_item_at(menu: &Option<selection::ContextMenu>, index: usize) -> ContextMenuItem {
    menu.as_ref()
        .and_then(|m| m.items.get(index).copied())
        .unwrap_or(ContextMenuItem::ClearSelection)
}
