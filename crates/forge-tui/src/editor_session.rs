//! Forge-owned state around the embedded [`edtui`] editing core.
//!
//! This module deliberately does not know about paths, filesystem writes, or
//! TUI focus. It owns only the editor buffer, Vim event handler, and the
//! accepted text needed to report dirty state to the rest of Forge.

#![allow(dead_code)] // The session is introduced before the rendering/input migration.

use crossterm::event::KeyEvent;
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, LineNumbers, Lines,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

#[derive(Clone)]
pub(crate) struct EditorSession {
    state: EditorState,
    event_handler: EditorEventHandler,
    accepted_text: String,
    revision: u64,
}

impl EditorSession {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            state: EditorState::new(Lines::from(text)),
            event_handler: EditorEventHandler::vim_mode(),
            accepted_text: text.to_string(),
            revision: 0,
        }
    }

    /// Route one terminal key through edtui and report whether the buffer changed.
    pub(crate) fn handle_key(&mut self, event: KeyEvent) -> bool {
        let before = self.text();
        self.event_handler.on_key_event(event, &mut self.state);
        let changed = before != self.text();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn text(&self) -> String {
        self.state.lines.to_string()
    }

    pub(crate) fn mode(&self) -> EditorMode {
        self.state.mode
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.text() != self.accepted_text
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Mark the current editor text as the accepted on-disk version.
    pub(crate) fn accept_current_text(&mut self) {
        self.accepted_text = self.text();
    }

    /// Replace the buffer with text accepted from disk and reset Vim state.
    pub(crate) fn replace_text(&mut self, text: &str) {
        self.state = EditorState::new(Lines::from(text));
        self.event_handler = EditorEventHandler::vim_mode();
        self.accepted_text = text.to_string();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Render only the editor surface. Forge-owned chrome stays outside this
    /// method so the edtui widget cannot change the surrounding layout.
    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        EditorView::new(&mut self.state)
            .theme(EditorTheme::default().hide_status_line())
            .line_numbers(LineNumbers::Absolute)
            .tab_width(4)
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn starts_in_normal_mode_with_clean_accepted_text() {
        let session = EditorSession::new("hello");

        assert_eq!(session.mode(), EditorMode::Normal);
        assert_eq!(session.text(), "hello");
        assert!(!session.is_dirty());
        assert_eq!(session.revision(), 0);
    }

    #[test]
    fn vim_input_changes_text_and_tracks_dirty_state() {
        let mut session = EditorSession::new("");

        assert!(!session.handle_key(key(KeyCode::Char('i'))));
        assert_eq!(session.mode(), EditorMode::Insert);
        assert!(session.handle_key(key(KeyCode::Char('x'))));
        assert_eq!(session.text(), "x");
        assert!(session.is_dirty());
        assert_eq!(session.revision(), 1);

        session.handle_key(key(KeyCode::Esc));
        assert_eq!(session.mode(), EditorMode::Normal);
    }

    #[test]
    fn accepting_and_replacing_text_reset_dirty_state() {
        let mut session = EditorSession::new("before");
        session.handle_key(key(KeyCode::Char('i')));
        session.handle_key(key(KeyCode::Char('x')));
        assert!(session.is_dirty());

        session.accept_current_text();
        assert!(!session.is_dirty());

        session.replace_text("after");
        assert_eq!(session.text(), "after");
        assert_eq!(session.mode(), EditorMode::Normal);
        assert!(!session.is_dirty());
    }

    #[test]
    fn renders_the_editor_surface_without_an_embedded_status_line() {
        let mut session = EditorSession::new("hello");
        let area = Rect::new(0, 0, 20, 4);
        let mut buffer = Buffer::empty(area);

        session.render(area, &mut buffer);

        let rendered: String = (0..area.width)
            .map(|x| buffer.cell((x, 0)).unwrap().symbol().to_string())
            .collect();
        assert!(rendered.contains("hello"));
    }
}
