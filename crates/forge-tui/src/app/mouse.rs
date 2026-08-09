//! Mouse input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per the `input.rs` precedent (#19). v1 scope is
//! **vertical wheel only**, routed by focus (not pointer position). Selection
//! support (drag-to-select + right-click context menu, v1: the Editor pane) is
//! routed by pointer position within the editor rect. Hover `Moved` events are
//! not emitted unless `EnableMouseMotion` is also enabled (it is not).

use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::clipboard;
use crate::selection::{self, cell_inside, Cell, ContextMenuItem, CopyPane};

/// Conversation/explorer rows moved per plain wheel notch.
const WHEEL_NOTCH: isize = 1;
/// Page size (rows) used for shift+wheel on the conversation and file explorer.
/// Mirrors the keyboard `PageUp`/`PageDown` step so both inputs stay consistent.
const WHEEL_PAGE: isize = 5;

impl TuiApp {
    /// Route a terminal mouse event.
    pub(crate) async fn handle_mouse(&mut self, event: MouseEvent) -> Result<(), TuiError> {
        // A context menu owns the pointer while it is open.
        if self.context_menu.is_some() {
            self.handle_mouse_context_menu(&event);
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
                self.mouse_start_selection(event.column, event.row);
            }
            MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                self.mouse_update_selection(event.column, event.row);
            }
            MouseEventKind::Up(MouseButton::Left) => self.mouse_finish_selection(),
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
        self.explorer_dialog.current.is_some()
            || self.hitl_session.pattern_nudge.is_some()
            || self.session.pending_hitl().is_some()
            || self.overlay.is_some()
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
            .diff_area
            .is_some_and(|area| cell_inside(area, col, row))
        {
            Some(CopyPane::Diff)
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
        if self.selection.is_dragging() {
            self.selection.update(Cell { row, col });
        }
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
            Some(CopyPane::Editor) => match self.editor_area {
                Some(area) => {
                    let live_lines = self.editor_session.as_ref().map(|editor| {
                        editor
                            .text()
                            .split('\n')
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    });
                    let lines = live_lines.as_deref().unwrap_or(&self.source_viewer.lines);
                    selection::editor_selection_text(
                        lines,
                        self.source_viewer.top_line,
                        self.source_viewer.h_scroll,
                        area,
                        &self.selection,
                    )
                }
                None => String::new(),
            },
            Some(CopyPane::Conversation) => match self.conversation_area {
                Some(area) => selection::visible_rows_selection_text(
                    &self.conversation_rows,
                    area,
                    &self.selection,
                    true,
                ),
                None => String::new(),
            },
            Some(CopyPane::Diff) => match self.diff_area {
                Some(area) => selection::visible_rows_selection_text(
                    &self.diff_rows,
                    area,
                    &self.selection,
                    false,
                ),
                None => String::new(),
            },
            Some(CopyPane::Terminal) => match self.terminal_area {
                Some(area) => selection::visible_rows_selection_text(
                    &self.terminal_rows,
                    area,
                    &self.selection,
                    false,
                ),
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

    fn mouse_open_context_menu(&mut self, col: u16, row: u16) {
        if self.pointer_blocked() {
            return;
        }
        let x = col.saturating_add(1);
        let y = row.saturating_add(1);
        self.context_menu = Some(selection::ContextMenu::new(x, y));
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
        if self.explorer_dialog.current.is_some()
            || self.hitl_session.pattern_nudge.is_some()
            || self.session.pending_hitl().is_some()
            || self.overlay.is_some()
            || matches!(
                self.focus.mode,
                FocusMode::Transient(TransientOwner::SourceSearch | TransientOwner::JumpToLine)
            )
        {
            return;
        }

        match self.focus.block {
            // The composer is the resting focus; the wheel over it scrolls the
            // transcript behind it, same as PageUp/PageDown while composing.
            FocusBlock::Composer => self.mouse_scroll_conversation(direction, shift),
            // Workspace is the CHAT panel; it hosts the source viewer when a
            // file is open and the conversation otherwise.
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
            // Sidebar / Approval / interactive-terminal (BottomPanel) are v1
            // no-ops: focus-based routing has no scroll target there.
            _ => {}
        }
    }

    fn mouse_scroll_conversation(&mut self, direction: isize, shift: bool) {
        let amount = if shift { WHEEL_PAGE } else { WHEEL_NOTCH } as u16;
        if direction < 0 {
            self.scroll_conversation_up(amount);
        } else {
            self.scroll_conversation_down(amount);
        }
    }

    fn mouse_scroll_source_viewer(&mut self, direction: isize, shift: bool) {
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
            direction
        };
        if let Some(editor) = self.editor_session.as_mut() {
            let key = if shift {
                if delta < 0 {
                    crossterm::event::KeyCode::PageUp
                } else {
                    crossterm::event::KeyCode::PageDown
                }
            } else if delta < 0 {
                crossterm::event::KeyCode::Up
            } else {
                crossterm::event::KeyCode::Down
            };
            editor.handle_key(crossterm::event::KeyEvent::new(
                key,
                crossterm::event::KeyModifiers::NONE,
            ));
            self.source_viewer.current_line = editor.cursor_row();
            return;
        }
        self.source_viewer
            .move_cursor_vertical(delta, page.max(1) as usize);
    }
}

/// Resolve the selected menu item by index (indirection to sidestep borrow
/// conflicts between `self.context_menu` reads and `&mut self` mutation).
fn menu_item_at(menu: &Option<selection::ContextMenu>, index: usize) -> ContextMenuItem {
    menu.as_ref()
        .and_then(|m| m.items.get(index).copied())
        .unwrap_or(ContextMenuItem::ClearSelection)
}
