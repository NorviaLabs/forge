//! Input bar — multi-line paste / Shift+Enter newline, visible caret.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, Default)]
pub struct InputModel {
    pub text: String,
    pub cursor: usize,
    pub dimmed: bool,
    pub hint: String,
    /// When true, text uses history_active background (Phase 7 browse).
    pub history_browse: bool,
}

impl InputModel {
    pub fn insert(&mut self, c: char) {
        let i = self.cursor.min(self.text.len());
        self.text.insert(i, c);
        self.cursor = i + c.len_utf8();
    }

    /// Insert a newline at the cursor (Shift+Enter / paste).
    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        let start = self.cursor - prev;
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.cursor -= prev;
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.cursor += next;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_browse = false;
    }

    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_browse = false;
        t
    }

    /// Replace buffer (e.g. from history recall); cursor moves to end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Number of visual lines for layout (capped).
    pub fn visual_lines(&self) -> u16 {
        let n = self.text.lines().count().max(1) as u16;
        n.min(6)
    }
}

pub struct InputBar<'a> {
    pub model: &'a InputModel,
}

impl Widget for InputBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let base = if self.model.dimmed {
            theme::dim()
        } else if self.model.history_browse {
            theme::history_active()
        } else {
            theme::text()
        };

        // Block cursor: solid cell at caret (█ at EOL, inverted char mid-text).
        let lines: Vec<Line> = if self.model.text.is_empty() && !self.model.hint.is_empty() {
            vec![Line::from(vec![
                Span::styled(" ❯ ", theme::brand()),
                Span::styled(self.model.hint.as_str(), theme::dim()),
                Span::styled(" ", theme::caret()), // block cell
            ])]
        } else if self.model.text.is_empty() {
            vec![Line::from(vec![
                Span::styled(" ❯ ", theme::brand()),
                Span::styled(" ", theme::caret()),
            ])]
        } else {
            let t = &self.model.text;
            let cur = self.model.cursor.min(t.len());
            let before = &t[..cur];
            let line_idx = before.matches('\n').count();
            let mut out = Vec::new();
            for (i, raw) in t.split('\n').enumerate() {
                let prefix = if i == 0 { " ❯ " } else { "   " };
                if i == line_idx {
                    let line_start = if i == 0 {
                        0
                    } else {
                        before.rfind('\n').map(|p| p + 1).unwrap_or(0)
                    };
                    let col = cur.saturating_sub(line_start).min(raw.len());
                    let (left, right) = raw.split_at(col);
                    if right.is_empty() {
                        // EOL: solid block cell
                        out.push(Line::from(vec![
                            Span::styled(prefix, theme::brand()),
                            Span::styled(left, base),
                            Span::styled(" ", theme::caret()),
                        ]));
                    } else {
                        // Mid-line: invert the character under the cursor (block cursor)
                        let ch = right.chars().next().unwrap();
                        let n = ch.len_utf8();
                        let under = &right[..n];
                        // Non-space under cursor stays readable; space becomes full block cell
                        let under_disp = if under == " " { " " } else { under };
                        out.push(Line::from(vec![
                            Span::styled(prefix, theme::brand()),
                            Span::styled(left, base),
                            Span::styled(under_disp, theme::caret()),
                            Span::styled(&right[n..], base),
                        ]));
                    }
                } else {
                    out.push(Line::from(vec![
                        Span::styled(prefix, theme::brand()),
                        Span::styled(raw, base),
                    ]));
                }
            }
            if t.ends_with('\n') && cur == t.len() {
                out.push(Line::from(vec![
                    Span::styled("   ", theme::brand()),
                    Span::styled(" ", theme::caret()),
                ]));
            }
            out
        };

        let border = if self.model.history_browse {
            theme::brand()
        } else {
            theme::border()
        };
        let title = if self.model.history_browse {
            " input · history "
        } else if self.model.text.contains('\n') {
            " input · multi-line · Enter send · Shift+Enter newline "
        } else {
            " input "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(Span::styled(title, theme::muted()));
        Paragraph::new(lines)
            .style(Style::default().add_modifier(if self.model.dimmed {
                Modifier::DIM
            } else {
                Modifier::empty()
            }))
            .block(block)
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn insert_and_backspace() {
        let mut m = InputModel::default();
        m.insert('a');
        m.insert('b');
        assert_eq!(m.text, "ab");
        m.backspace();
        assert_eq!(m.text, "a");
        assert_eq!(m.cursor, 1);
    }

    #[test]
    fn newline_insert() {
        let mut m = InputModel::default();
        m.insert('a');
        m.insert_newline();
        m.insert('b');
        assert_eq!(m.text, "a\nb");
        assert_eq!(m.visual_lines(), 2);
    }

    #[test]
    fn cursor_moves() {
        let mut m = InputModel {
            text: "hi".into(),
            cursor: 2,
            ..Default::default()
        };
        m.move_left();
        assert_eq!(m.cursor, 1);
        m.insert('X');
        assert_eq!(m.text, "hXi");
    }

    #[test]
    fn take_clears() {
        let mut m = InputModel {
            text: "cmd".into(),
            cursor: 3,
            ..Default::default()
        };
        assert_eq!(m.take(), "cmd");
        assert!(m.text.is_empty());
    }

    #[test]
    fn caret_cell_has_background() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 2; // EOL — block cell after text
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(InputBar { model: &m }, f.size());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = buf.get(x, y);
                if cell.style().bg == Some(theme::TEXT) {
                    found = true;
                }
            }
        }
        assert!(found, "expected block cursor cell with solid background");
    }

    #[test]
    fn mid_line_block_cursor_inverts_char() {
        let mut m = InputModel::default();
        m.set_text("ab");
        m.cursor = 0; // over 'a'
        let backend = TestBackend::new(40, 5);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(InputBar { model: &m }, f.size());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found_a = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = buf.get(x, y);
                if cell.symbol() == "a" && cell.style().bg == Some(theme::TEXT) {
                    found_a = true;
                }
            }
        }
        assert!(found_a, "expected inverted block cursor on character under caret");
    }
}
