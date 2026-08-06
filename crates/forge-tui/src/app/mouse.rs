//! Mouse input routing for [`TuiApp`].
//!
//! Split out of `app.rs` per the `input.rs` precedent (#19). v1 scope is
//! **vertical wheel only**, routed by focus (not pointer position). The
//! composer delegates to the conversation view; the interactive terminal and
//! overlays are no-ops. Horizontal wheel and click/drag/motion events are
//! ignored cheaply so they never consume the event-loop budget.

use super::*;
use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

/// Conversation/explorer rows moved per plain wheel notch.
const WHEEL_NOTCH: isize = 1;
/// Page size (rows) used for shift+wheel on the conversation and file explorer.
/// Mirrors the keyboard `PageUp`/`PageDown` step so both inputs stay consistent.
const WHEEL_PAGE: isize = 5;

impl TuiApp {
    /// Route a terminal mouse event. Only vertical wheel notches are handled in
    /// v1; everything else is a cheap no-op.
    pub(crate) async fn handle_mouse(&mut self, event: MouseEvent) -> Result<(), TuiError> {
        let direction = match event.kind {
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::ScrollDown => 1,
            // Horizontal wheel and click/drag/motion events are out of v1 scope.
            _ => return Ok(()),
        };
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        self.dispatch_mouse_scroll(direction, shift);
        Ok(())
    }

    fn dispatch_mouse_scroll(&mut self, direction: isize, shift: bool) {
        // Mirror `handle_key`'s guard precedence: while a modal or transient
        // surface is active the wheel must not scroll a pane the user cannot
        // see (or the transcript hidden beneath an overlay).
        if self.explorer_dialog.current.is_some()
            || self.hitl_session.pattern_nudge.is_some()
            || self.session.pending_hitl().is_some()
            || self.composer_chip_focus.is_some()
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
            FocusBlock::Files => {
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
        self.source_viewer
            .move_cursor_vertical(delta, page.max(1) as usize);
    }
}
