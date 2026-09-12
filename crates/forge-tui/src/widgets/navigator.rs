//! The left **navigator**: a two-tab column, `Sessions | Files` (`FORGE-DESIGN
//! §7.7`). This module owns the tab bar and the session list; the file tree is
//! the existing explorer widget, composed by the renderer under the tab bar.
//!
//! Only three session states are user-facing here — `● needs you`,
//! `◐ working`, `○ idle`. Branch, worktree and ownership never appear.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigatorTab {
    Sessions,
    Files,
}

impl NavigatorTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Files => "Files",
        }
    }
}

/// One session's row in the list. Two visual lines: identity, then qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// `●` needs you · `◐` working · `○` idle.
    pub glyph: char,
    pub need: bool,
    /// `None` renders a dimmer qualifier (archived/idle).
    pub label: String,
    pub qualifier: String,
    /// The session currently shown in the workspace.
    pub selected: bool,
    /// The navigator cursor rests here.
    pub focused: bool,
}

/// The one-row tab bar across the top of the navigator column.
pub struct NavigatorTabs {
    pub tab: NavigatorTab,
    pub focused: bool,
    pub needs_you: usize,
    /// Tab under the pointer. The hovered *inactive* tab gets a raised ground
    /// and a leading marker, so a pointer user can tell it is clickable; the
    /// active tab keeps its own treatment and focus never moves on hover.
    pub hover: Option<NavigatorTab>,
}

impl Widget for NavigatorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let active = if self.focused {
            theme::brand().add_modifier(Modifier::BOLD)
        } else {
            theme::brand()
        };
        let inactive = theme::metadata_style();
        let mut spans = Vec::new();
        for (index, tab) in [NavigatorTab::Sessions, NavigatorTab::Files]
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                spans.push(Span::styled(" │ ", theme::border_muted()));
            }
            let is_active = tab == self.tab;
            let hovered = !is_active && self.hover == Some(tab);
            if is_active {
                spans.push(Span::styled("▌", theme::accent_style()));
            } else if hovered {
                // A pointer-only affordance: shape (`›`) plus ground, never
                // colour alone, and never on the tab that already owns input.
                spans.push(Span::styled("›", theme::accent_style()));
            } else {
                spans.push(Span::raw(" "));
            }
            let label_style = if is_active {
                active
            } else if hovered {
                inactive.patch(theme::surface_hover())
            } else {
                inactive
            };
            spans.push(Span::styled(tab.label(), label_style));
        }
        if self.needs_you > 0 {
            let label = format!("{} need", self.needs_you);
            let used: usize = spans.iter().map(Span::width).sum();
            let pad = (area.width as usize)
                .saturating_sub(used)
                .saturating_sub(label.chars().count() + 1);
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(label, theme::warn()));
            }
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

/// The expanded peek under the focused row: the session's last answer and an
/// inline reply box (`FORGE-DESIGN §7.7`).
pub struct PeekPanel<'a> {
    /// Pre-wrapped lines of the last answer.
    pub lines: &'a [String],
    /// Current reply buffer.
    pub reply: &'a str,
    /// Placeholder shown when the buffer is empty.
    pub placeholder: &'a str,
}

/// The vertical, attention-ordered session list.
pub struct SessionList<'a> {
    pub rows: &'a [SessionRow],
    pub focused: bool,
    /// Rendered immediately under the focused row when set.
    pub peek: Option<&'a PeekPanel<'a>>,
    /// Inline "new session" task buffer; rendered on the bottom row when set.
    pub new_session: Option<&'a str>,
    /// Session index under the pointer (hover); tints its two lines only.
    pub hover: Option<usize>,
}

impl Widget for SessionList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut y = area.y;
        let bottom = area.bottom();
        for (index, row) in self.rows.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let hovered = self.hover == Some(index);
            let (glyph_style, label_style) = if row.need {
                (theme::warn(), theme::text())
            } else {
                (theme::muted(), theme::text_secondary())
            };
            let label_style = if row.selected {
                theme::text().add_modifier(Modifier::BOLD)
            } else if hovered && !(row.focused && self.focused) {
                // Hover is a pointer affordance: ground plus a weight step,
                // never the selection treatment.
                label_style.add_modifier(Modifier::BOLD)
            } else {
                label_style
            };
            let glyph_style = if row.selected {
                theme::accent_style()
            } else {
                glyph_style
            };
            let cursor = if (row.focused && self.focused) || row.selected {
                Span::styled("›", theme::accent_style())
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![
                cursor,
                Span::raw(" "),
                Span::styled(row.glyph.to_string(), glyph_style),
                Span::raw(" "),
            ];
            let prefix: usize = spans.iter().map(Span::width).sum();
            let room = (area.width as usize).saturating_sub(prefix);
            spans.push(Span::styled(truncate(&row.label, room), label_style));
            let mut line = Line::from(spans);
            if row.focused && self.focused {
                line = line.style(theme::focused_selection_style());
                buf.set_line(area.x, y, &line, area.width);
                fill_selection(buf, area.x, y, area.width);
            } else {
                if hovered {
                    line = line.style(theme::surface_hover());
                }
                buf.set_line(area.x, y, &line, area.width);
            }
            y += 1;
            if y >= bottom {
                break;
            }
            if !row.qualifier.is_empty() {
                let indent = 3usize.min(area.width as usize);
                let room = (area.width as usize).saturating_sub(indent);
                let text = truncate(&row.qualifier, room);
                let mut qual = Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(text, theme::metadata_style()),
                ]);
                if hovered && !(row.focused && self.focused) {
                    qual = qual.style(theme::surface_hover());
                }
                buf.set_line(area.x, y, &qual, area.width);
            }
            y += 1;
            if let Some(peek) = self.peek.filter(|_| row.focused && self.focused) {
                let indent = 3usize.min(area.width as usize);
                for line in peek.lines {
                    if y >= bottom {
                        break;
                    }
                    let room = (area.width as usize).saturating_sub(indent);
                    buf.set_line(
                        area.x,
                        y,
                        &Line::from(vec![
                            Span::raw(" ".repeat(indent)),
                            Span::styled(truncate(line, room), theme::text_secondary()),
                        ]),
                        area.width,
                    );
                    y += 1;
                }
                if y < bottom {
                    let prefix = "› ";
                    let used = indent + prefix.chars().count();
                    let room = (area.width as usize).saturating_sub(used);
                    let reply = if peek.reply.is_empty() {
                        Span::styled(truncate(peek.placeholder, room), theme::muted())
                    } else {
                        Span::styled(truncate(peek.reply, room), theme::text())
                    };
                    buf.set_line(
                        area.x,
                        y,
                        &Line::from(vec![
                            Span::raw(" ".repeat(indent)),
                            Span::styled(prefix, theme::accent_style()),
                            reply,
                        ]),
                        area.width,
                    );
                    y += 1;
                }
                if y < bottom {
                    let hint = "Enter send · Esc collapse";
                    let room = (area.width as usize).saturating_sub(indent);
                    buf.set_line(
                        area.x,
                        y,
                        &Line::from(vec![
                            Span::raw(" ".repeat(indent)),
                            Span::styled(truncate(hint, room), theme::metadata_style()),
                        ]),
                        area.width,
                    );
                    y += 1;
                }
            }
        }
        if let Some(buffer) = self.new_session {
            let row = area.bottom().saturating_sub(1);
            if row >= area.y {
                let prefix = "› ";
                let room = (area.width as usize).saturating_sub(prefix.chars().count());
                let text = if buffer.is_empty() {
                    "type a task to start a session…"
                } else {
                    buffer
                };
                let style = if buffer.is_empty() {
                    theme::muted()
                } else {
                    theme::text()
                };
                buf.set_line(
                    area.x,
                    row,
                    &Line::from(vec![
                        Span::styled(prefix, theme::accent_style()),
                        Span::styled(truncate(text, room), style),
                    ]),
                    area.width,
                );
            }
        }
    }
}

/// Extend the selection ground across the row so it reads as one band.
fn fill_selection(buf: &mut Buffer, x: u16, y: u16, width: u16) {
    let style = theme::focused_selection_style();
    for col in x..x.saturating_add(width) {
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_style(style);
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    format!("{}…", text.chars().take(width - 1).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn row(label: &str, qualifier: &str) -> SessionRow {
        SessionRow {
            glyph: '●',
            need: true,
            label: label.into(),
            qualifier: qualifier.into(),
            selected: false,
            focused: false,
        }
    }

    #[test]
    fn tabs_show_both_names_and_the_needs_you_count() {
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    NavigatorTabs {
                        tab: NavigatorTab::Sessions,
                        focused: false,
                        needs_you: 2,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains("Sessions"), "{text:?}");
        assert!(text.contains("Files"), "{text:?}");
        assert!(text.contains("2 need"), "{text:?}");
    }

    #[test]
    fn the_active_tab_carries_the_accent() {
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    NavigatorTabs {
                        tab: NavigatorTab::Files,
                        focused: false,
                        needs_you: 0,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let accent = theme::accent_color();
        let marker_accent = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "▌" && cell.style().fg == Some(accent));
        assert!(marker_accent, "active tab marker should be accented");
    }

    /// A hovered inactive tab must visibly differ from an unhovered one —
    /// ground plus a leading marker — so a pointer user can tell it is
    /// clickable. The active tab's treatment is untouched by hover.
    #[test]
    fn hovered_inactive_tab_takes_the_hover_ground() {
        let render = |hover: Option<NavigatorTab>| {
            let backend = TestBackend::new(40, 1);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        NavigatorTabs {
                            tab: NavigatorTab::Files,
                            focused: false,
                            needs_you: 0,
                            hover,
                        },
                        frame.area(),
                    );
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let base = render(None);
        let hovered = render(Some(NavigatorTab::Sessions));
        let hover_bg = theme::surface_hover().bg;

        let sessions_label = |buffer: &ratatui::buffer::Buffer| {
            buffer
                .content()
                .iter()
                .find(|cell| cell.symbol() == "S")
                .expect("Sessions label")
                .style()
        };
        assert_eq!(
            sessions_label(&hovered).bg,
            hover_bg,
            "hovered tab lost its ground"
        );
        assert_ne!(
            sessions_label(&base).bg,
            hover_bg,
            "unhovered tab must not carry the hover ground"
        );
        assert!(
            hovered.content().iter().any(|cell| cell.symbol() == "›"),
            "hovered tab lost its leading marker"
        );
    }

    #[test]
    fn the_peek_shows_the_last_answer_and_reply_box() {
        let rows = vec![SessionRow {
            glyph: '●',
            need: true,
            label: "Fix login redirect".into(),
            qualifier: "needs you".into(),
            selected: true,
            focused: true,
        }];
        let lines = vec!["I added the guard in src/auth.rs.".to_string()];
        let peek = PeekPanel {
            lines: &lines,
            reply: "push it",
            placeholder: "reply to this session…",
        };
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    SessionList {
                        rows: &rows,
                        focused: true,
                        peek: Some(&peek),
                        new_session: None,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains("I added the guard"), "{text:?}");
        assert!(text.contains("push it"), "{text:?}");
        assert!(text.contains("Enter send"), "{text:?}");
    }

    #[test]
    fn the_list_renders_a_glyph_label_and_qualifier_per_row() {
        let rows = vec![row("Fix login redirect", "needs you · 2m")];
        let backend = TestBackend::new(30, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    SessionList {
                        rows: &rows,
                        focused: true,
                        peek: None,
                        new_session: None,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains('●'), "{text:?}");
        assert!(text.contains("Fix login redirect"), "{text:?}");
        assert!(text.contains("needs you"), "{text:?}");
    }

    #[test]
    fn a_long_label_is_elided_to_the_column() {
        let rows = vec![row(&"x".repeat(80), "idle")];
        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    SessionList {
                        rows: &rows,
                        focused: true,
                        peek: None,
                        new_session: None,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains('…'), "long label should elide: {text:?}");
    }
}
