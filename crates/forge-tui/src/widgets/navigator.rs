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
use ratatui::widgets::{Block, BorderType, Borders, Widget};

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

/// Width shared by tab painting and pointer routing.
pub(crate) const SESSIONS_TAB_WIDTH: u16 = 12;

/// Framed tabs across the top of the navigator column.
pub struct NavigatorTabs {
    pub tab: NavigatorTab,
    pub needs_you: usize,
    /// The row itself holds the keyboard (`↑` at the top of either tab's list,
    /// `FORGE-DESIGN §8.3`). Both outlines then take the L3 accent step while
    /// the active tab keeps its `accent_soft` ground, so the row reads as the
    /// thing being driven without impersonating a different active tab.
    pub focused: bool,
    /// Tab under the pointer. The hovered *inactive* tab gets a raised ground
    /// and a weight step, so a pointer user can tell it is clickable; the
    /// active tab keeps its own treatment and focus never moves on hover.
    /// Neither state moves the label: it is centred in the tab box either way.
    pub hover: Option<NavigatorTab>,
}

impl Widget for NavigatorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let inactive = theme::metadata_style();
        for (index, tab) in [NavigatorTab::Sessions, NavigatorTab::Files]
            .into_iter()
            .enumerate()
        {
            let split = SESSIONS_TAB_WIDTH.min(area.width);
            let tab_area = if index == 0 {
                Rect::new(area.x, area.y, split, area.height)
            } else {
                Rect::new(
                    area.x + split.saturating_sub(1),
                    area.y,
                    area.width.saturating_sub(split.saturating_sub(1)),
                    area.height,
                )
            };
            let is_active = tab == self.tab;
            let hovered = !is_active && self.hover == Some(tab);
            // The tab strip is the navigator panel's own top edge, not a small
            // box inside it: one frame for the active and inactive tabs alike,
            // neutral until the row itself holds the keyboard. The active tab
            // fills its inner row edge to edge —
            // an unmistakable full-width signal that still stays inside the
            // frame, never flowing over or under the text — and the label
            // keeps a weight step and the accent hue for terminals that
            // render no colour (FORGE-DESIGN §5 rule 7).
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if self.focused {
                    theme::accent_style()
                } else {
                    theme::panel_border()
                })
                .style(theme::panel());
            let inner = if area.height >= 3 {
                block.inner(tab_area)
            } else {
                tab_area
            };
            if area.height >= 3 {
                block.render(tab_area, buf);
            }
            let label_style = if is_active {
                theme::accent_style()
                    .add_modifier(Modifier::BOLD)
                    .bg(theme::accent_soft_bg())
            } else if hovered {
                // Ground plus weight, and the label keeps its column.
                inactive
                    .patch(theme::surface_hover())
                    .add_modifier(Modifier::BOLD)
            } else {
                inactive
            };
            if inner.width == 0 || inner.height == 0 {
                continue;
            }
            // Ground first, edge to edge across the inner row; the label and
            // badge repaint their own cells on top of it.
            if is_active {
                fill_inner_row(buf, inner, Some(theme::accent_soft_bg()));
            } else if hovered {
                fill_inner_row(buf, inner, theme::surface_hover().bg);
            }
            let label = truncate(tab.label(), inner.width as usize);
            let label_width = label.chars().count() as u16;
            // Centred in the tab box, not in the space a badge would leave:
            // the two tabs own different widths, so the latter would centre
            // them on different axes.
            let label_x = inner.x + inner.width.saturating_sub(label_width) / 2;
            buf.set_string(label_x, inner.y, &label, label_style);
            if index == 1 && self.tab == NavigatorTab::Files && self.needs_you > 0 {
                // The Sessions tab (12 wide, 8-cell label) cannot hold a
                // badge beside its label, so the wide Files tab hosts it —
                // shown only while viewing Files, when the session list's
                // own need states are out of sight.
                let badge = format!("{} need", self.needs_you);
                let badge_width = badge.chars().count() as u16;
                let badge_x = inner.right().saturating_sub(badge_width);
                // Dropped rather than crowding the label when the tab cannot
                // hold both.
                if badge_width <= inner.width && badge_x > label_x + label_width {
                    buf.set_string(
                        badge_x,
                        inner.y,
                        &badge,
                        theme::warn().bg(theme::accent_soft_bg()),
                    );
                }
            }
        }
        if area.height >= 3 && area.width > SESSIONS_TAB_WIDTH {
            buf[(area.x + SESSIONS_TAB_WIDTH - 1, area.y)].set_symbol("┬");
            buf[(area.x + SESSIONS_TAB_WIDTH - 1, area.y + area.height - 1)].set_symbol("┴");
        }
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
            // Selection owns the accent edge even without cursor: the
            // workspace-visible session stays identifiable while browsing.
            let cursor = if (row.focused && self.focused) || row.selected {
                Span::styled("›", theme::active_panel_border())
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
            if (row.focused && self.focused) || row.selected {
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

/// Paint the tab's inner row edge to edge. The frame rows above and below
/// keep the panel ground; only this row carries the tab's signal.
fn fill_inner_row(buf: &mut Buffer, inner: Rect, bg: Option<ratatui::style::Color>) {
    let Some(bg) = bg else {
        return;
    };
    for col in inner.x..inner.right() {
        buf[(col, inner.y)].set_bg(bg);
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

    /// Render the tab bar at an explicit width and hand back the buffer.
    fn render_tabs(
        width: u16,
        tab: NavigatorTab,
        needs_you: usize,
        hover: Option<NavigatorTab>,
    ) -> ratatui::buffer::Buffer {
        render_tabs_with_focus(width, tab, needs_you, hover, false)
    }

    /// Same, with the row itself holding the keyboard.
    fn render_tabs_with_focus(
        width: u16,
        tab: NavigatorTab,
        needs_you: usize,
        hover: Option<NavigatorTab>,
        focused: bool,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    NavigatorTabs {
                        tab,
                        needs_you,
                        focused,
                        hover,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// First column carrying `symbol` on row `y`.
    fn cell_x(buffer: &ratatui::buffer::Buffer, symbol: &str, y: u16) -> u16 {
        let width = buffer.area.width as usize;
        buffer
            .content()
            .iter()
            .enumerate()
            .find(|(index, cell)| index / width == y as usize && cell.symbol() == symbol)
            .map(|(index, _)| (index % width) as u16)
            .unwrap_or_else(|| panic!("no {symbol:?} on row {y}"))
    }

    #[test]
    fn tabs_show_both_names_and_the_needs_you_count() {
        let buffer = render_tabs(40, NavigatorTab::Files, 2, None);
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains("Sessions"), "{text:?}");
        assert!(text.contains("Files"), "{text:?}");
        assert!(text.contains("2 need"), "{text:?}");
    }

    #[test]
    fn the_badge_hides_while_viewing_sessions() {
        // The list already carries the need states; a badge on top would
        // pull the eye to the inactive tab.
        let buffer = render_tabs(40, NavigatorTab::Sessions, 2, None);
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(text.contains("Sessions"), "{text:?}");
        assert!(!text.contains("2 need"), "{text:?}");
    }

    #[test]
    fn the_shared_edge_takes_tee_joints_top_and_bottom() {
        let buffer = render_tabs(40, NavigatorTab::Sessions, 0, None);
        assert_eq!(
            buffer[(SESSIONS_TAB_WIDTH - 1, 0)].symbol(),
            "┬",
            "top joint must join the two tab frames"
        );
        assert_eq!(
            buffer[(SESSIONS_TAB_WIDTH - 1, 2)].symbol(),
            "┴",
            "bottom joint must join the two tab frames"
        );
    }

    /// While the row holds the keyboard both outlines take the L3 accent step
    /// (`FORGE-DESIGN §8.3`, §9.6). It is a border-level signal only: the active
    /// tab keeps its ground and hue, so the row never displaces the active-tab
    /// signal with its own.
    #[test]
    fn a_focused_row_steps_both_outlines_to_the_accent() {
        let idle = render_tabs(40, NavigatorTab::Files, 0, None);
        let focused = render_tabs_with_focus(40, NavigatorTab::Files, 0, None, true);

        assert_ne!(
            idle[(0u16, 0u16)].fg,
            focused[(0u16, 0u16)].fg,
            "the inactive tab's outline takes the accent too"
        );
        assert_ne!(
            idle[(39u16, 0u16)].fg,
            focused[(39u16, 0u16)].fg,
            "the active tab's outline steps with it"
        );

        let label = cell_x(&idle, "F", 1);
        assert_eq!(
            idle[(label, 1)].bg,
            focused[(label, 1)].bg,
            "the active tab keeps its ground"
        );
        assert_eq!(
            idle[(label, 1)].fg,
            focused[(label, 1)].fg,
            "and its label treatment"
        );
    }

    /// Selection is a full-width chip: the active tab's inner row carries the
    /// soft ground edge to edge, so the active tab is unmistakable, while the
    /// frame rows above and below stay neutral — no underline, no reserved
    /// `>` cell, and the label stays centred in every state.
    #[test]
    fn the_active_tab_fills_its_inner_row_edge_to_edge() {
        let buffer = render_tabs(40, NavigatorTab::Files, 0, None);
        let accent = theme::accent_color();
        let soft = theme::accent_soft_bg();
        let files = cell_x(&buffer, "F", 1);

        let label = buffer[(files, 1)].style();
        assert_eq!(label.fg, Some(accent), "active label lost the accent");
        assert!(
            label.add_modifier.contains(Modifier::BOLD),
            "active label lost its weight step"
        );
        assert_eq!(label.bg, Some(soft), "active label lost the ground");
        assert!(
            !label.add_modifier.contains(Modifier::UNDERLINED),
            "the active tab must not be underlined"
        );

        // The chip fits the tab completely: every inner cell of the row —
        // padding included — carries the ground.
        assert_eq!(
            buffer[(35, 1)].style().bg,
            Some(soft),
            "active tab ground stops short of the tab edge"
        );
        assert_eq!(
            buffer[(12, 1)].style().bg,
            Some(soft),
            "active tab ground stops short of the tab edge"
        );

        // ...and nothing beyond the row: both border rows stay neutral.
        assert_eq!(
            buffer[(files, 0)].style().bg,
            theme::panel().bg,
            "active tab ground leaks onto the top border"
        );
        assert_eq!(
            buffer[(files, 2)].style().bg,
            theme::panel().bg,
            "active tab ground leaks onto the bottom border"
        );

        // The inactive tab keeps the panel ground and gains nothing.
        let sessions = cell_x(&buffer, "S", 1);
        assert_eq!(buffer[(sessions, 1)].style().bg, theme::panel().bg);
        assert_eq!(buffer[(1, 1)].style().bg, theme::panel().bg);
        assert!(
            !buffer[(sessions, 1)]
                .style()
                .add_modifier
                .contains(Modifier::UNDERLINED),
            "no tab is underlined any more"
        );
    }

    /// Each label sits in the middle of its own tab. The two tabs own
    /// different widths — Sessions is fixed, Files takes the remainder — and
    /// the label is centred in that width rather than in the space a badge
    /// would leave, so the axes stay the tab's own.
    #[test]
    fn each_label_is_centred_in_its_own_tab() {
        let buffer = render_tabs(40, NavigatorTab::Sessions, 0, None);
        // Sessions: x 0..12, inner 1..10, eight cells of label -> one pad each side.
        assert_eq!(cell_x(&buffer, "S", 1), 2);
        // Files: x 11..40, inner 12..38, five cells of label -> eleven spaces.
        assert_eq!(cell_x(&buffer, "F", 1), 23);
    }

    /// The needs-you badge keeps its right edge and the label keeps its
    /// centre; a tab too narrow for both drops the badge rather than letting
    /// it run into the label.
    #[test]
    fn the_badge_holds_the_right_edge_and_yields_to_a_narrow_tab() {
        let wide = render_tabs(40, NavigatorTab::Files, 2, None);
        assert_eq!(cell_x(&wide, "2", 1), 33);
        assert_eq!(cell_x(&wide, "F", 1), 23, "the badge moved the label");

        let narrow = render_tabs(20, NavigatorTab::Files, 2, None);
        assert_eq!(cell_x(&narrow, "F", 1), 13, "label is not centred");
        assert!(
            !narrow.content().iter().any(|cell| cell.symbol() == "2"),
            "the badge should be dropped when it cannot clear the label"
        );
    }

    /// A hovered inactive tab must visibly differ from an unhovered one —
    /// ground plus a weight step — so a pointer user can tell it is clickable.
    /// The active tab's treatment is untouched by hover.
    #[test]
    fn hovered_inactive_tab_takes_the_hover_ground_without_moving_the_label() {
        let base = render_tabs(40, NavigatorTab::Files, 0, None);
        let hovered = render_tabs(40, NavigatorTab::Files, 0, Some(NavigatorTab::Sessions));
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
            sessions_label(&hovered)
                .add_modifier
                .contains(Modifier::BOLD),
            "hovered tab lost its weight step"
        );
        // Hover fills the tab's inner row edge to edge, matching the active
        // chip: the padding cell past the label carries it too.
        assert_eq!(
            hovered[(10, 1)].style().bg,
            hover_bg,
            "hovered tab ground stops short of the tab edge"
        );
        // Hover is ground and weight only: the centred label keeps its column.
        let label_x = |buffer: &ratatui::buffer::Buffer| {
            buffer
                .content()
                .iter()
                .position(|cell| cell.symbol() == "S")
                .expect("Sessions label")
        };
        assert_eq!(label_x(&base), label_x(&hovered));
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
