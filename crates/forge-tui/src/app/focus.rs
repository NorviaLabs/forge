//! Keyboard focus and block navigation for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Normalizes focus across visible blocks, cycles
//! Tab order, restores focus after closing panels, and surfaces transient hints.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn focus_availability(&self) -> FocusAvailability {
        FocusAvailability {
            files: self.workspace_files.visible,
            // No standalone preference flag — the sidebar only ever hides
            // via the layout's own narrow-width defensive floor, which this
            // preference-only check can't see (see render.rs's geometry-based
            // FocusAvailability for that case).
            sidebar: true,
            bottom_panel: self.bottom_panel.open,
        }
    }

    pub(super) fn normalize_focus(&mut self) {
        let available = self.focus_availability();
        if !available.contains(self.focus.block) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
            self.focus.return_block = Some(FocusBlock::Workspace);
        }
        if self.source_viewer.search.open {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Transient(TransientOwner::SourceSearch);
        } else if self.source_viewer.jump.open {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Transient(TransientOwner::JumpToLine);
        }
        self.workspace_files.explorer.focused = self.focus.block == FocusBlock::Files
            && self.focus.mode == FocusMode::Navigation
            && self.workspace_files.visible;
        self.bottom_panel.focused = self.focus.block == FocusBlock::BottomPanel
            && self.focus.mode == FocusMode::Navigation
            && self.bottom_panel.open;
        self.source_viewer.focused = self.focus.block == FocusBlock::Workspace
            && self.current_workspace_is_file()
            && matches!(
                self.focus.mode,
                FocusMode::Navigation | FocusMode::Transient(_)
            );
    }

    pub(crate) fn focus_block(&mut self, block: FocusBlock) {
        if self.focus.block != block {
            self.focus.previous_block = Some(self.focus.block);
        }
        self.focus.block = block;
        self.focus.mode = FocusMode::Navigation;
        self.focus.return_block = Some(block);
        self.normalize_focus();
    }

    pub(super) fn enter_chat_composer(&mut self) {
        self.focus_block(FocusBlock::Composer);
        self.normalize_focus();
    }

    pub(super) fn enter_transient(&mut self, owner: TransientOwner) {
        self.focus.block = FocusBlock::Workspace;
        self.focus.mode = FocusMode::Transient(owner);
        self.focus.return_block = Some(FocusBlock::Workspace);
        self.normalize_focus();
    }

    pub(super) fn restore_focus_after_closing(&mut self, closed: FocusBlock) {
        let previous = self
            .focus
            .previous_block
            .filter(|block| *block != closed && self.focus_availability().contains(*block))
            .unwrap_or(FocusBlock::Workspace);
        self.focus.block = previous;
        self.focus.mode = FocusMode::Navigation;
        self.focus.return_block = Some(previous);
    }

    pub(super) fn cycle_focus_block(&mut self, forward: bool) {
        let available = self.focus_availability();
        let current = FocusBlock::ORDER
            .iter()
            .position(|block| *block == self.focus.block)
            .unwrap_or(1);
        for offset in 1..=FocusBlock::ORDER.len() {
            let index = if forward {
                (current + offset) % FocusBlock::ORDER.len()
            } else {
                (current + FocusBlock::ORDER.len() - offset) % FocusBlock::ORDER.len()
            };
            let next = FocusBlock::ORDER[index];
            if available.contains(next) {
                self.focus_block(next);
                break;
            }
        }
    }

    pub(super) fn escape_navigation(&mut self) {
        match self.focus.block {
            FocusBlock::Workspace => {}
            FocusBlock::Composer => {
                let previous = self
                    .focus
                    .previous_block
                    .filter(|block| *block != FocusBlock::Composer)
                    .filter(|block| self.focus_availability().contains(*block))
                    .unwrap_or(FocusBlock::Workspace);
                self.focus_block(previous);
            }
            block => self.restore_focus_after_closing(block),
        }
        self.normalize_focus();
    }

    pub(super) fn open_bottom_panel(&mut self) {
        self.bottom_panel.open = true;
        if self.interactive_terminal.is_none() {
            match crate::interactive_terminal::InteractiveTerminal::spawn(
                self.session.workspace_root(),
                80,
                8,
            ) {
                Ok(terminal) => self.interactive_terminal = Some(terminal),
                Err(error) => self.set_feedback(
                    FeedbackSeverity::Error,
                    format!("could not start terminal: {error}"),
                ),
            }
        }
        self.focus_block(FocusBlock::BottomPanel);
    }

    pub(super) fn contextual_hint(&self) -> Option<String> {
        if self.explorer_dialog.current.is_some() {
            return Some("Enter confirm · Esc cancel".into());
        }
        if self.session.pending_hitl().is_some() {
            return Some(match self.hitl_session.menu.phase {
                ApprovalMenuPhase::DenyFeedback => {
                    "⏸ deny note · type feedback · Enter submit · Esc back".into()
                }
                ApprovalMenuPhase::Choose => {
                    "⏸ approval · ↑↓ select · Enter confirm · Esc deny".into()
                }
            });
        }
        if let Some(overlay) = self.overlay.as_ref() {
            return match overlay {
                Overlay::TurnLimit { .. } => Some("Enter confirm · Esc cancel".into()),
                Overlay::ConnectApiKey { .. } => Some("Enter confirm · Esc cancel".into()),
                Overlay::ConnectOauth { .. } => Some("Enter continue · Esc cancel".into()),
                _ => None,
            };
        }
        match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                Some("Enter next · ⇧Enter previous · Esc cancel".into())
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                Some("Enter jump · Esc cancel".into())
            }
            FocusMode::Navigation => None,
        }
    }
}
