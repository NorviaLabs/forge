//! Forge-owned state around the embedded [`edtui`] editing core.
//!
//! This module deliberately does not know about paths, filesystem writes, or
//! TUI focus. It owns only the editor buffer, Vim event handler, and the
//! accepted text needed to report dirty state to the rest of Forge.

#![allow(dead_code)] // The session is introduced before the rendering/input migration.

use crossterm::event::KeyEvent;
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, Highlight, Index2,
    LineNumbers, Lines,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use crate::theme;

#[derive(Clone)]
pub(crate) struct EditorSession {
    state: EditorState,
    event_handler: EditorEventHandler,
    accepted_text: String,
    syntax_language: Option<String>,
    syntax_theme: forge_syntax::HighlightTheme,
    revision: u64,
}

impl EditorSession {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            state: EditorState::new(Lines::from(text)),
            event_handler: EditorEventHandler::vim_mode(),
            accepted_text: text.to_string(),
            syntax_language: None,
            syntax_theme: forge_syntax::HighlightTheme::default(),
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
            self.refresh_syntax_highlights();
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
        self.refresh_syntax_highlights();
    }

    /// Configure the Forge grammar used for this buffer. `None` deliberately
    /// leaves edtui with no custom ranges, which is the plain-text fallback.
    pub(crate) fn set_syntax_language(&mut self, language: Option<&str>) {
        self.syntax_language = language.map(str::to_owned);
        self.refresh_syntax_highlights();
    }

    pub(crate) fn set_syntax_theme(&mut self, syntax_theme: forge_syntax::HighlightTheme) {
        self.syntax_theme = syntax_theme;
        self.refresh_syntax_highlights();
    }

    fn refresh_syntax_highlights(&mut self) {
        let Some(language) = self.syntax_language.as_deref() else {
            self.state.clear_highlights();
            return;
        };
        let source = self.text();
        let theme = self.syntax_theme;
        let spans = std::panic::catch_unwind(|| forge_syntax::highlight(language, &source, &theme))
            .unwrap_or_default();
        self.set_forge_highlights(&source, &spans, &theme);
    }

    /// Replace edtui's position-based highlights with ranges produced by
    /// Forge's Tree-sitter pipeline. Forge's spans are byte ranges and edtui
    /// expects zero-based character columns with an inclusive end position.
    pub(crate) fn set_forge_highlights(
        &mut self,
        source: &str,
        spans: &[forge_syntax::HighlightSpan],
        syntax_theme: &forge_syntax::HighlightTheme,
    ) {
        self.state
            .set_highlights(forge_highlights_to_edtui(source, spans, syntax_theme));
    }

    /// Render only the editor surface. Forge-owned chrome stays outside this
    /// method so the edtui widget cannot change the surrounding layout.
    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        EditorView::new(&mut self.state)
            .theme(
                EditorTheme::default()
                    .base(theme::text().patch(theme::panel()))
                    .cursor_style(theme::caret())
                    .selection_style(theme::selected_row())
                    .line_numbers_style(theme::muted().patch(theme::panel()))
                    .hide_status_line(),
            )
            .line_numbers(LineNumbers::Absolute)
            .tab_width(4)
            .render(area, buf);
    }
}

fn forge_highlights_to_edtui(
    source: &str,
    spans: &[forge_syntax::HighlightSpan],
    syntax_theme: &forge_syntax::HighlightTheme,
) -> Vec<Highlight> {
    let line_offsets: Vec<usize> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(offset, _)| offset + 1))
        .collect();
    let mut highlights = Vec::new();

    for span in spans {
        let start = span.range.start.min(source.len());
        let end = span.range.end.min(source.len());
        if start >= end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            continue;
        }

        let first_line = line_offsets
            .partition_point(|offset| *offset <= start)
            .saturating_sub(1);
        let last_line = line_offsets
            .partition_point(|offset| *offset < end)
            .saturating_sub(1);
        let rgb = span.style.rgb(syntax_theme);
        let mut style = theme::syntax_segment(rgb, theme::panel().bg);
        if span.style.is_bold() {
            style = style.add_modifier(ratatui::style::Modifier::BOLD);
        }
        if span.style.is_italic() {
            style = style.add_modifier(ratatui::style::Modifier::ITALIC);
        }

        for line in first_line..=last_line.min(line_offsets.len().saturating_sub(1)) {
            let line_start = line_offsets[line];
            let line_content_end = line_offsets
                .get(line + 1)
                .copied()
                .map(|offset| offset.saturating_sub(1))
                .unwrap_or(source.len());
            let segment_start = start.max(line_start).min(line_content_end);
            let segment_end = end.min(line_content_end).max(segment_start);
            if segment_start >= segment_end {
                continue;
            }

            let start_col = source[line_start..segment_start].chars().count();
            let end_col = source[line_start..segment_end].chars().count();
            if end_col > start_col {
                highlights.push(Highlight::new(
                    Index2::new(line, start_col),
                    Index2::new(line, end_col - 1),
                    style,
                ));
            }
        }
    }

    highlights
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

    #[test]
    fn forge_highlights_convert_utf8_byte_ranges_to_character_columns() {
        let source = "α let value";
        let start = source.find("let").unwrap();
        let end = source.find("value").unwrap();
        let spans = vec![forge_syntax::HighlightSpan {
            range: start..end,
            style: forge_syntax::HighlightStyle {
                class: forge_syntax::HighlightClass::Keyword,
            },
        }];

        let highlights =
            forge_highlights_to_edtui(source, &spans, &forge_syntax::HighlightTheme::default());

        assert_eq!(highlights.len(), 1);
        assert_eq!(highlights[0].start, Index2::new(0, 2));
        assert_eq!(highlights[0].end, Index2::new(0, 5));
    }

    #[test]
    fn forge_highlights_split_ranges_across_lines() {
        let source = "fn α() {\n  value\n}";
        let spans = vec![forge_syntax::HighlightSpan {
            range: 0..source.len(),
            style: forge_syntax::HighlightStyle {
                class: forge_syntax::HighlightClass::Default,
            },
        }];

        let highlights =
            forge_highlights_to_edtui(source, &spans, &forge_syntax::HighlightTheme::default());

        assert_eq!(highlights.len(), 3);
        assert_eq!(highlights[0].start, Index2::new(0, 0));
        assert_eq!(highlights[0].end, Index2::new(0, 7));
        assert_eq!(highlights[1].start, Index2::new(1, 0));
        assert_eq!(highlights[1].end, Index2::new(1, 6));
    }

    #[test]
    fn syntax_highlights_rebuild_after_edit_and_can_fall_back_to_plain_text() {
        let mut session = EditorSession::new("fn main() {}");
        session.set_syntax_language(Some("rust"));
        assert!(!session.state.highlights.is_empty());

        session.set_syntax_language(None);
        assert!(session.state.highlights.is_empty());
    }
}
