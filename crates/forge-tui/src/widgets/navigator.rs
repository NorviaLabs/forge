//! The left **navigator**: a two-tab column, `Sessions | Files` (`FORGE-DESIGN
//! §7.7`). This module owns the tab bar and the session list; the file tree is
//! the existing explorer widget, composed by the renderer under the tab bar.
//!
//! Every session state is user-facing here — `● needs you`, `⣾ working`,
//! `⇥ queued`, `○ idle`, `✓ completed`, `✗ failed`, `■ cancelled`,
//! `∅ interrupted` ([`SessionRowState`]). Branch, worktree and ownership never
//! appear.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
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

/// The stop the navigator tab row's cursor rests on. `Sessions` and `Files` are
/// the tab stops; `NewSession` is the row's `+` cell, which creates a session
/// and is deliberately never a tab (`FORGE-DESIGN §7.7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavigatorRowStop {
    #[default]
    Sessions,
    Files,
    NewSession,
}

impl NavigatorRowStop {
    /// The tab stop that carries `tab`. `NewSession` is tab-independent, so it
    /// is never returned here.
    pub fn for_tab(tab: NavigatorTab) -> Self {
        match tab {
            NavigatorTab::Sessions => Self::Sessions,
            NavigatorTab::Files => Self::Files,
        }
    }
}

/// One session row's state, as the list renders it.
///
/// The four states whose marker already means the same thing here and in the
/// status bar borrow it from [`TurnLifecycle`], so a change to that table
/// reaches both surfaces at once. The other four are the list's own:
/// `Waiting` and `Working` because the list is where the operator scans them
/// and it carries a mark for each (`●`, the turn line's spinner) where the
/// status bar carries a word; `Idle` because a row's state cell is never
/// blank; and `Queued` because a queued prompt exists while the task itself is
/// still `Ready`, so it has no `TaskLifecycle` equivalent at all.
///
/// Every state has a different glyph: two states sharing a marker is how this
/// list came to render `failed` as `○`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRowState {
    /// No active turn.
    Idle,
    /// A turn is running.
    Working,
    /// A turn is running but stopped for the operator.
    Waiting,
    /// A prompt waiting for the current turn to finish.
    Queued,
    /// Turn finished with a final answer.
    Completed,
    /// Turn finished in failure, including retry exhaustion.
    Failed,
    /// Operator cancelled the in-flight turn.
    Cancelled,
    /// Persisted active task with no recoverable runtime.
    Interrupted,
}

impl SessionRowState {
    /// Map the session's lifecycle. Unknown future lifecycles read as
    /// `Interrupted` — never as idle or working, which would claim a runtime
    /// Forge cannot confirm.
    ///
    /// The live-turn flags outrank this: see the renderer, which prefers
    /// `Waiting` and `Working` over whatever the lifecycle last recorded.
    pub fn from_lifecycle(lifecycle: forge_types::TaskLifecycle) -> Self {
        match crate::widgets::TurnLifecycle::from_task_lifecycle(lifecycle) {
            crate::widgets::TurnLifecycle::Ready => Self::Idle,
            crate::widgets::TurnLifecycle::Working => Self::Working,
            crate::widgets::TurnLifecycle::Waiting => Self::Waiting,
            crate::widgets::TurnLifecycle::Completed => Self::Completed,
            crate::widgets::TurnLifecycle::Failed => Self::Failed,
            crate::widgets::TurnLifecycle::Cancelled => Self::Cancelled,
            crate::widgets::TurnLifecycle::Interrupted => Self::Interrupted,
        }
    }

    /// The one-cell marker for this state.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Idle => "○",
            // The turn line's spinner, held at its first frame. The widget
            // steps it per row; see `SessionList::step`.
            Self::Working => Self::spinner_frame(0),
            Self::Waiting => "●",
            Self::Queued => "⇥",
            Self::Completed => crate::widgets::TurnLifecycle::Completed.symbol(),
            Self::Failed => crate::widgets::TurnLifecycle::Failed.symbol(),
            Self::Cancelled => crate::widgets::TurnLifecycle::Cancelled.symbol(),
            Self::Interrupted => crate::widgets::TurnLifecycle::Interrupted.symbol(),
        }
    }

    /// The running marker's frame `step` steps into, from the turn line's own
    /// set — the row and the live turn it is running speak one vocabulary.
    pub fn spinner_frame(step: usize) -> &'static str {
        let frames = crate::widgets::turn_line::SPINNER_FRAMES;
        frames[step % frames.len()]
    }

    /// The marker's style. Selection overrides this — see `SessionList`.
    pub fn style(self) -> ratatui::style::Style {
        match self {
            // The turn line's active-work hue, so the row and the turn it is
            // running read as the same activity.
            Self::Working => theme::activity().add_modifier(Modifier::BOLD),
            Self::Idle => crate::widgets::TurnLifecycle::Ready.style(),
            Self::Waiting => crate::widgets::TurnLifecycle::Waiting.style(),
            Self::Queued => theme::info(),
            Self::Completed => crate::widgets::TurnLifecycle::Completed.style(),
            Self::Failed => crate::widgets::TurnLifecycle::Failed.style(),
            Self::Cancelled => crate::widgets::TurnLifecycle::Cancelled.style(),
            Self::Interrupted => crate::widgets::TurnLifecycle::Interrupted.style(),
        }
    }

    /// Whether this state is asking the operator for something.
    pub fn needs_you(self) -> bool {
        self == Self::Waiting
    }
}

/// One session's row in the list. Two visual lines: identity, then qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// What this row is doing; carries the marker and its style.
    pub state: SessionRowState,
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

/// Width of the navigator row's `+` cell: one border column either side of the
/// single glyph cell.
pub(crate) const NEW_SESSION_CELL_WIDTH: u16 = 3;

/// The navigator column is laid out at 28–37 columns (`layout.rs`), so the `+`
/// cell always fits in a real frame. This floor only keeps a degenerate test
/// geometry from squeezing the `Files` tab, where the cell is then dropped
/// rather than drawn cramped.
pub(crate) const MIN_NEW_SESSION_ROW_WIDTH: u16 = 28;

/// The `+` cell's rect on the navigator tab row, or `None` when the row cannot
/// carry it. The cell sits directly beside the `Sessions` tab, sharing its
/// edge with it, so the create verb reads as acting on sessions. Painting,
/// keyboard stops and pointer routing all read this, so the three can never
/// disagree about whether the cell exists or where it is.
pub(crate) fn new_session_cell(area: Rect) -> Option<Rect> {
    if area.width < MIN_NEW_SESSION_ROW_WIDTH || area.height == 0 {
        return None;
    }
    Some(Rect::new(
        area.x + SESSIONS_TAB_WIDTH.saturating_sub(1),
        area.y,
        NEW_SESSION_CELL_WIDTH,
        area.height,
    ))
}

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
    /// The row's cursor. The two tab stops keep today's treatment; `NewSession`
    /// adds the accent step to the `+` glyph so the cell reads as the selected
    /// one without ever taking the active tab's ground.
    pub row_stop: NavigatorRowStop,
    /// `+` cell under the pointer. Like a tab, hover is a ground plus a weight
    /// step and never moves focus.
    pub hover_new_session: bool,
}

impl Widget for NavigatorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let inactive = theme::metadata_style();
        // The `+` cell sits directly beside the `Sessions` tab, sharing its
        // edge with it, so the create verb reads as acting on sessions.
        // One source of truth for painting, keyboard stops and pointer routing.
        let new_session = new_session_cell(area);
        let split = SESSIONS_TAB_WIDTH.min(area.width);
        let sessions_area = Rect::new(area.x, area.y, split, area.height);
        let files_area = if let Some(cell) = new_session {
            let x = cell.x + cell.width.saturating_sub(1);
            Rect::new(x, area.y, area.right().saturating_sub(x), area.height)
        } else {
            let x = area.x + split.saturating_sub(1);
            Rect::new(x, area.y, area.right().saturating_sub(x), area.height)
        };
        for (index, (tab, tab_area)) in [
            (NavigatorTab::Sessions, sessions_area),
            (NavigatorTab::Files, files_area),
        ]
        .into_iter()
        .enumerate()
        {
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
        if let Some(cell) = new_session {
            // A segment of the row's frame beside the `Sessions` tab, sharing
            // its edge with it. It is not a tab: it never takes the
            // `accent_soft` ground, so it cannot be mistaken for one. It takes
            // the row's focus step with the outlines (`§9.6`) and adds the
            // accent to the glyph only while the row's cursor rests here.
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
                block.inner(cell)
            } else {
                cell
            };
            if area.height >= 3 {
                block.render(cell, buf);
            }
            if inner.width > 0 && inner.height > 0 {
                if self.hover_new_session {
                    fill_inner_row(buf, inner, theme::surface_hover().bg);
                }
                let selected = self.focused && self.row_stop == NavigatorRowStop::NewSession;
                let glyph_style = if selected {
                    theme::accent_style().add_modifier(Modifier::BOLD)
                } else if self.hover_new_session {
                    inactive
                        .patch(theme::surface_hover())
                        .add_modifier(Modifier::BOLD)
                } else {
                    inactive
                };
                let glyph_x = inner.x + inner.width.saturating_sub(1) / 2;
                buf.set_string(glyph_x, inner.y, "+", glyph_style);
            }
        }
        if area.height >= 3 {
            if let Some(cell) = new_session {
                // The cell sits directly after the `Sessions` tab, so the
                // shared edge is the Sessions tab's right edge; the `Files`
                // tab starts on the cell's own right edge. Re-stamp both
                // joints the tab corners would otherwise round off.
                buf[(cell.x, area.y)].set_symbol("┬");
                buf[(cell.x, area.y + area.height - 1)].set_symbol("┴");
                let right = cell.x + cell.width.saturating_sub(1);
                buf[(right, area.y)].set_symbol("┬");
                buf[(right, area.y + area.height - 1)].set_symbol("┴");
            } else if area.width > SESSIONS_TAB_WIDTH {
                buf[(area.x + SESSIONS_TAB_WIDTH - 1, area.y)].set_symbol("┬");
                buf[(area.x + SESSIONS_TAB_WIDTH - 1, area.y + area.height - 1)].set_symbol("┴");
            }
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
    /// How far the running rows' spinner has stepped. Advanced by the event
    /// loop, never by the wall clock, so pausing work pauses the motion.
    pub step: usize,
}

/// Where a row's text begins: the prefix is `bar marker space glyph space`,
/// all five cells reserved on every row, selected or not (`§604`'s
/// pre-reservation rule, which hover already follows). Only the label is
/// elastic, so no state — selection, hover, expansion — can move another row's
/// label column.
pub(crate) const TEXT_COL: usize = 5;

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
            // The peek hangs under the row it belongs to. Its height is decided
            // here, before anything is painted, because a frame costs a line
            // above the row and a line below it — and a half-drawn box is worse
            // than no box, so the frame is drawn only when the whole block fits.
            let peek = self.peek.filter(|_| row.focused && self.focused);
            let peek_height = peek.map_or(0usize, |panel| panel.lines.len() + 3);
            let framed = peek.is_some() && (y as usize) + 4 + peek_height <= bottom as usize;
            let frame_top = y;
            if framed {
                y += 1;
            }
            // The active block owns the selection treatment; a selection that
            // lost the keyboard keeps the bar and the weight step and gives up
            // the ground (`§8.5`). An expanded row's frame replaces both.
            let active_selection = row.selected && self.focused && !framed;
            // Selection is the bar plus the ground. The cursor cell is
            // disclosure, not selection (`D3`): `›` collapsed, `⌄` expanded,
            // and `›` again under the pointer, which is `§8.6`'s non-colour
            // signal for a hover.
            let expanded = row.focused && self.focused && self.peek.is_some();
            let marker = if expanded {
                Span::styled("⌄", theme::active_panel_border())
            } else if (row.focused && self.focused) || hovered {
                Span::styled("›", theme::active_panel_border())
            } else {
                Span::raw(" ")
            };
            // The bar yields the gutter to the frame when the row is expanded;
            // two signals for one row would fight in the same cell.
            let bar = if row.selected && !framed {
                Span::styled("▌", theme::accent_style())
            } else {
                Span::raw(" ")
            };
            let label_style = if row.state.needs_you() && !row.selected {
                theme::text()
            } else {
                theme::text_secondary()
            };
            let label_style = if row.selected {
                // The viewed session stays the loudest label in the column even
                // without the keyboard: accent while the list is focused, a
                // weight step alone when it is not.
                if self.focused {
                    theme::accent_style().add_modifier(Modifier::BOLD)
                } else {
                    theme::text().add_modifier(Modifier::BOLD)
                }
            } else if hovered {
                // Hover is a pointer affordance: ground plus a weight step,
                // never the selection treatment.
                label_style.add_modifier(Modifier::BOLD)
            } else {
                label_style
            };
            // Selection owns the marker's hue while it is on screen; the state
            // is still readable from the glyph and from the qualifier.
            let glyph_style = if row.selected {
                theme::accent_style()
            } else {
                row.state.style()
            };
            // Running rows turn: the frame is offset by the row so a column of
            // running sessions does not blink as one blanket, and every frame
            // is one cell wide so the labels stay in their column.
            let glyph = match row.state {
                SessionRowState::Working => SessionRowState::spinner_frame(self.step + index),
                state => state.glyph(),
            };
            // One ground per row, covering both of its lines, so a selected
            // row reads as one band instead of a name on a strip with its
            // qualifier left floating below it. Applied as a background at the
            // end, so the bar, the glyph and the label keep the hues that
            // carry their state.
            let ground = if active_selection {
                theme::focused_selection_style().bg
            } else if hovered && !framed {
                theme::surface_hover().bg
            } else {
                None
            };
            // Inside a frame the label needs its own cell back for the frame's
            // right edge.
            let room = text_room(area).saturating_sub(usize::from(framed));
            let mut spans = vec![
                bar,
                marker,
                Span::raw(" "),
                Span::styled(glyph, glyph_style),
            ];
            pad_to_label(&mut spans);
            spans.push(Span::styled(
                truncate(&row.label, text_room(area)),
                label_style,
            ));
            let line = Line::from(spans);
            buf.set_line(area.x, y, &line, area.width);
            y += 1;
            if y >= bottom {
                break;
            }
            let qual_style = if active_selection {
                theme::text_secondary()
            } else {
                theme::metadata_style()
            };
            let qual = Line::from(vec![
                Span::raw(" ".repeat(TEXT_COL.min(area.width as usize))),
                Span::styled(truncate(&row.qualifier, room), qual_style),
            ]);
            buf.set_line(area.x, y, &qual, area.width);
            if let Some(bg) = ground {
                fill_row_ground(buf, area, y.saturating_sub(1), 2, bg);
            }
            y += 1;
            if framed {
                draw_expanded_frame(buf, area, frame_top, y, theme::accent_style());
                // The peek hangs below the frame's bottom edge.
                y += 1;
            }
            if let Some(peek) = peek {
                for line in peek.lines {
                    if y >= bottom {
                        break;
                    }
                    paint_peek_line(buf, area, y, peek_body_spans(line, text_room(area)));
                    y += 1;
                }
                if y < bottom {
                    let room = text_room(area).saturating_sub(2);
                    let reply = if peek.reply.is_empty() {
                        Span::styled(truncate(peek.placeholder, room), theme::muted())
                    } else {
                        Span::styled(truncate(peek.reply, room), theme::text())
                    };
                    paint_peek_line(
                        buf,
                        area,
                        y,
                        vec![Span::styled("› ", theme::accent_style()), reply],
                    );
                    y += 1;
                }
                // The reply box stops reading as transcript behind a rule, and
                // the hints come from the one hint grammar every other surface
                // uses, so the verbs cannot drift apart.
                if y < bottom {
                    paint_peek_line(
                        buf,
                        area,
                        y,
                        vec![Span::styled(
                            "─".repeat(text_room(area)),
                            theme::border_muted(),
                        )],
                    );
                    y += 1;
                }
                if y < bottom {
                    paint_peek_line(buf, area, y, peek_hints(text_room(area)));
                    y += 1;
                }
            }
        }
    }
}

/// Frame an expanded row's two lines: a rounded box whose left edge occupies
/// the reserved gutter and whose right edge the list's last column. Both edges
/// land in cells no label ever reaches, so expanding a row moves nothing.
fn draw_expanded_frame(buf: &mut Buffer, area: Rect, top: u16, bottom: u16, style: Style) {
    if area.width == 0 || top >= bottom {
        return;
    }
    let rule = "─".repeat(area.width.saturating_sub(2) as usize);
    let edge = |left: &str, right: &str| {
        vec![
            Span::styled(left.to_string(), style),
            Span::styled(rule.clone(), style),
            Span::styled(right.to_string(), style),
        ]
    };
    buf.set_line(area.x, top, &Line::from(edge("╭", "╮")), area.width);
    buf.set_line(area.x, bottom, &Line::from(edge("╰", "╯")), area.width);
    let right = area.right().saturating_sub(1);
    for y in top.saturating_add(1)..bottom {
        for col in [area.x, right] {
            if let Some(cell) = buf.cell_mut((col, y)) {
                cell.set_symbol("│");
                cell.set_style(style);
            }
        }
    }
}

/// Paint one line of the peek: the guide rule in the cell left of
/// [`TEXT_COL`], then the spans, so the whole peek hangs off one rule instead
/// of reading as transcript that leaked into the column.
fn paint_peek_line(buf: &mut Buffer, area: Rect, y: u16, spans: Vec<Span<'static>>) {
    let col = TEXT_COL.min(area.width as usize);
    let mut all: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    if col > 1 {
        all.push(Span::raw(" ".repeat(col - 1)));
        all.push(Span::styled("│", theme::border_muted()));
    }
    all.extend(spans);
    buf.set_line(area.x, y, &Line::from(all), area.width);
}

/// A peek body line. The one marker that says a turn died takes the danger hue
/// so it reads as a failure rather than as prose; the rest stays secondary.
fn peek_body_spans(line: &str, room: usize) -> Vec<Span<'static>> {
    let marker = forge_core::TURN_FAILED_MARKER;
    match line.strip_prefix(marker) {
        Some(rest) => {
            let marker_room = marker.chars().count().min(room);
            vec![
                Span::styled(marker.to_string(), theme::danger()),
                Span::styled(
                    truncate(rest, room.saturating_sub(marker_room)),
                    theme::text_secondary(),
                ),
            ]
        }
        None => vec![Span::styled(truncate(line, room), theme::text_secondary())],
    }
}

/// The peek's hints, degraded for the navigator's width. The shared grammar
/// drops verbs before it drops pairs, and the navigator is narrow enough that
/// both pairs never fit — leaving `Enter Esc`, which says nothing about what
/// either key does. Sending is what the reply box is for, so its pair is the
/// one asked for when only one fits.
fn peek_hints(room: usize) -> Vec<Span<'static>> {
    let pairs: [crate::hints::Hint; 2] = [("Enter", "send"), ("Esc", "collapse")];
    if crate::hints::hint_text(&pairs).chars().count() <= room {
        return crate::hints::hint_spans(&pairs, room);
    }
    crate::hints::hint_spans(&pairs[..1], room)
}

/// Pad a row's fixed prefix out to [`TEXT_COL`], so every label starts in the
/// same cell whatever the row's state draws in the prefix.
fn pad_to_label(spans: &mut Vec<Span<'static>>) {
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::raw(" ".repeat(TEXT_COL.saturating_sub(used))));
}

/// Width left for a row's text once the reserved columns are taken; the label,
/// the qualifier and the peek all share it.
fn text_room(area: Rect) -> usize {
    (area.width as usize).saturating_sub(TEXT_COL)
}

/// Ground a row's two lines edge to edge. Background only, so the bar, the
/// glyph and the label keep the hues that carry their state.
fn fill_row_ground(buf: &mut Buffer, area: Rect, y: u16, height: u16, bg: ratatui::style::Color) {
    for row_y in y..y.saturating_add(height).min(area.bottom()) {
        for col in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((col, row_y)) {
                cell.set_bg(bg);
            }
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
            state: SessionRowState::Waiting,
            label: label.into(),
            qualifier: qualifier.into(),
            selected: false,
            focused: false,
        }
    }

    /// The cells of a row, in order: the selection bar, the cursor/disclosure
    /// marker, a space, the state glyph, a space, then the label at
    /// [`TEXT_COL`]. Assertions name them so a layout change is one edit.
    const BAR_COL: u16 = 0;
    const MARKER_COL: u16 = 1;
    const GLYPH_COL: u16 = 3;

    /// A row in a named state, for the marker tests.
    fn state_row(state: SessionRowState, label: &str) -> SessionRow {
        SessionRow {
            state,
            label: label.into(),
            qualifier: String::new(),
            selected: false,
            focused: false,
        }
    }

    /// Render the session list at an explicit geometry and hand back the
    /// buffer.
    fn render_list(rows: &[SessionRow], width: u16, height: u16) -> ratatui::buffer::Buffer {
        render_list_at(rows, width, height, 0)
    }

    /// The list at a pinned spinner step, so a frame is asserted exactly.
    fn render_list_at(
        rows: &[SessionRow],
        width: u16,
        height: u16,
        step: usize,
    ) -> ratatui::buffer::Buffer {
        render_list_with(rows, width, height, step, true, None, None)
    }

    /// The list with the block focus, the pointer and the peek under test.
    fn render_list_with(
        rows: &[SessionRow],
        width: u16,
        height: u16,
        step: usize,
        focused: bool,
        hover: Option<usize>,
        peek: Option<&PeekPanel<'_>>,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    SessionList {
                        rows,
                        focused,
                        peek,
                        hover,
                        step,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
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
        // The cursor rests on the tab on screen unless a test says otherwise.
        let row_stop = match tab {
            NavigatorTab::Sessions => NavigatorRowStop::Sessions,
            NavigatorTab::Files => NavigatorRowStop::Files,
        };
        render_row(width, tab, needs_you, hover, row_stop, false, focused)
    }

    /// Render the row with an explicit cursor stop and `+` hover state.
    fn render_row(
        width: u16,
        tab: NavigatorTab,
        needs_you: usize,
        hover: Option<NavigatorTab>,
        row_stop: NavigatorRowStop,
        hover_new_session: bool,
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
                        row_stop,
                        hover_new_session,
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

    /// The `+` cell is its own segment of the row: its own frame, its own
    /// joints, and a glyph — but never the active tab's `accent_soft` ground,
    /// so it cannot be read as a third tab (`FORGE-DESIGN §7.7`, §9.6). It sits
    /// directly beside the `Sessions` tab, sharing that tab's right edge, so
    /// the create verb reads as acting on sessions.
    #[test]
    fn the_row_carries_a_new_session_cell_that_is_not_a_tab() {
        let buffer = render_tabs(40, NavigatorTab::Sessions, 0, None);
        let cell = new_session_cell(buffer.area).expect("the row is wide enough");
        assert_eq!(
            cell.x,
            SESSIONS_TAB_WIDTH - 1,
            "the cell shares the `Sessions` tab's right edge"
        );

        // Joined to the `Sessions` tab's frame on its own left edge, and to the
        // `Files` tab's on its right.
        assert_eq!(buffer[(cell.x, 0)].symbol(), "┬");
        assert_eq!(buffer[(cell.x, 2)].symbol(), "┴");
        assert_eq!(buffer[(cell.x + 1, 1)].symbol(), "+");
        assert_eq!(
            buffer[(cell.x + 1, 1)].bg,
            theme::panel().bg.expect("panel ground"),
            "the cell is unfilled: it never takes a tab's active ground"
        );
        assert!(
            !buffer[(cell.x + 1, 1)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "an idle glyph carries no weight step"
        );
    }

    /// The row's cursor is a glyph-level step on the `+` cell, on top of the
    /// accent outline the whole row takes while it holds the keyboard.
    #[test]
    fn the_new_session_glyph_steps_to_the_accent_only_as_the_rows_cursor() {
        let on_tab = render_row(
            40,
            NavigatorTab::Sessions,
            0,
            None,
            NavigatorRowStop::Sessions,
            false,
            true,
        );
        let on_plus = render_row(
            40,
            NavigatorTab::Sessions,
            0,
            None,
            NavigatorRowStop::NewSession,
            false,
            true,
        );
        let cell = new_session_cell(on_plus.area).expect("the row is wide enough");
        let glyph = (cell.x + 1, 1);

        assert_ne!(
            on_tab[glyph].fg, on_plus[glyph].fg,
            "the row's cursor must be visible on the glyph"
        );
        assert!(
            on_plus[glyph].style().add_modifier.contains(Modifier::BOLD),
            "the selected cell takes the weight step with the accent"
        );
        assert_eq!(
            on_tab[(cell.x, 0)].fg,
            on_plus[(cell.x, 0)].fg,
            "the outline steps for the row, not for the cursor"
        );
    }

    /// Hover is the pointer's focus ring: ground plus weight, never focus.
    #[test]
    fn hovering_the_new_session_cell_takes_the_hover_ground() {
        let idle = render_row(
            40,
            NavigatorTab::Sessions,
            0,
            None,
            NavigatorRowStop::Sessions,
            false,
            false,
        );
        let hovered = render_row(
            40,
            NavigatorTab::Sessions,
            0,
            None,
            NavigatorRowStop::Sessions,
            true,
            false,
        );
        let cell = new_session_cell(hovered.area).expect("the row is wide enough");
        let glyph = (cell.x + 1, 1);
        assert_eq!(hovered[glyph].style().bg, theme::surface_hover().bg);
        assert_ne!(idle[glyph].style().bg, theme::surface_hover().bg);
        assert!(hovered[glyph].style().add_modifier.contains(Modifier::BOLD));
    }

    /// Below the layout's navigator width the cell is dropped rather than
    /// squeezing the `Files` tab, so no frame can draw a cramped row.
    #[test]
    fn a_row_too_narrow_for_the_cell_drops_it() {
        let narrow = render_tabs(
            MIN_NEW_SESSION_ROW_WIDTH - 1,
            NavigatorTab::Sessions,
            0,
            None,
        );
        assert!(new_session_cell(narrow.area).is_none());
        assert!(
            !narrow.content().iter().any(|cell| cell.symbol() == "+"),
            "no cell, no glyph"
        );

        let wide = render_tabs(MIN_NEW_SESSION_ROW_WIDTH, NavigatorTab::Sessions, 0, None);
        assert!(new_session_cell(wide.area).is_some());
        assert!(wide.content().iter().any(|cell| cell.symbol() == "+"));
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
        let cell = new_session_cell(buffer.area).expect("the row is wide enough");
        let files_left = cell.x + cell.width - 1;
        assert_eq!(
            buffer[(files_left + 1, 1)].style().bg,
            Some(soft),
            "active tab ground stops short of the tab edge"
        );
        assert_eq!(
            buffer[(buffer.area.right() - 2, 1)].style().bg,
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
    /// different widths — Sessions is fixed, Files takes the remainder minus
    /// the `+` cell — and the label is centred in that width rather than in the
    /// space a badge would leave, so the axes stay the tab's own.
    #[test]
    fn each_label_is_centred_in_its_own_tab() {
        let buffer = render_tabs(40, NavigatorTab::Sessions, 0, None);
        // Sessions: x 0..12, inner 1..10, eight cells of label -> one pad each side.
        assert_eq!(cell_x(&buffer, "S", 1), 2);
        // The `+` cell owns x 11..14, so Files runs x 13..40 — inner 14..39,
        // five cells of label -> ten spaces either side.
        assert_eq!(cell_x(&buffer, "F", 1), 24);
    }

    /// The needs-you badge keeps its right edge and the label keeps its
    /// centre; a tab too narrow for both drops the badge rather than letting
    /// it run into the label.
    #[test]
    fn the_badge_holds_the_right_edge_and_yields_to_a_narrow_tab() {
        let wide = render_tabs(40, NavigatorTab::Files, 2, None);
        // The badge keeps the right edge of the `Files` tab, which the column
        // edge bounds.
        assert_eq!(cell_x(&wide, "2", 1), 33);
        assert_eq!(cell_x(&wide, "F", 1), 24, "the badge moved the label");

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
            state: SessionRowState::Waiting,
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
                        step: 0,
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
                        step: 0,
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
                        step: 0,
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

    /// Every state the list can render, in the order the enum declares them.
    /// [`SessionRowState::glyph`] and [`SessionRowState::style`] match on the
    /// enum exhaustively, so a new state cannot reach a row without a marker
    /// and a style — but add it here too, or it escapes the tests below.
    const ALL_STATES: [SessionRowState; 8] = [
        SessionRowState::Idle,
        SessionRowState::Working,
        SessionRowState::Waiting,
        SessionRowState::Queued,
        SessionRowState::Completed,
        SessionRowState::Failed,
        SessionRowState::Cancelled,
        SessionRowState::Interrupted,
    ];

    /// Two states sharing a marker is the bug this table replaced: `failed`,
    /// `completed` and `idle` all rendered `○`, so a turn that died and a turn
    /// that finished read the same in the one column an operator scans.
    #[test]
    fn no_two_states_share_a_glyph() {
        let mut seen: Vec<(&'static str, SessionRowState)> = Vec::new();
        for state in ALL_STATES {
            if let Some((glyph, other)) = seen.iter().find(|(glyph, _)| *glyph == state.glyph()) {
                panic!("{state:?} and {other:?} both render {glyph:?}");
            }
            seen.push((state.glyph(), state));
        }
    }

    /// The marker sits in a fixed cell, so it has to be exactly one column —
    /// a two-column glyph would shift the label for that row alone.
    #[test]
    fn every_marker_occupies_one_column() {
        for state in ALL_STATES {
            assert_eq!(
                Span::raw(state.glyph()).width(),
                1,
                "{state:?} renders {:?}",
                state.glyph()
            );
        }
    }

    /// The marker's style, one state at a time. Selection overrides this in
    /// the widget, but the table itself is what a row falls back to.
    #[test]
    fn each_state_carries_its_own_style() {
        for (state, expected) in [
            (SessionRowState::Idle, theme::muted()),
            (
                SessionRowState::Working,
                theme::activity().add_modifier(Modifier::BOLD),
            ),
            (
                SessionRowState::Waiting,
                theme::warn().add_modifier(Modifier::BOLD),
            ),
            (SessionRowState::Queued, theme::info()),
            (SessionRowState::Completed, theme::ok()),
            (SessionRowState::Failed, theme::danger()),
            (SessionRowState::Cancelled, theme::muted()),
            (SessionRowState::Interrupted, theme::warn()),
        ] {
            assert_eq!(state.style(), expected, "{state:?}");
        }
    }

    /// Colour is the second channel. The pairs an operator must never confuse
    /// — a failed turn against a completed one, a turn stopped for them
    /// against a turn that is running — differ in hue as well as in shape.
    #[test]
    fn the_states_that_must_not_be_confused_do_not_share_a_hue() {
        assert_ne!(
            SessionRowState::Failed.style().fg,
            SessionRowState::Completed.style().fg
        );
        assert_ne!(
            SessionRowState::Failed.style().fg,
            SessionRowState::Idle.style().fg
        );
        assert_ne!(
            SessionRowState::Waiting.style().fg,
            SessionRowState::Working.style().fg
        );
    }

    /// A running row carries the turn line's spinner, so the row and the turn
    /// it is running read as the same activity rather than two temperatures.
    #[test]
    fn a_working_row_borrows_the_turn_lines_spinner() {
        assert_eq!(
            SessionRowState::Working.glyph(),
            crate::widgets::turn_line::SPINNER_FRAMES[0]
        );
        assert_eq!(
            SessionRowState::Working.style().fg,
            theme::activity().fg,
            "the running row left the activity token"
        );
    }

    /// The marker column carries the state: a failed row renders `✗` and a
    /// completed one `✓`, and neither renders the `○` idle owns.
    #[test]
    fn a_failed_row_renders_the_failure_marker_and_not_an_idle_ring() {
        let rows = vec![
            state_row(SessionRowState::Failed, "Fix login redirect"),
            state_row(SessionRowState::Completed, "Add the guard"),
            state_row(SessionRowState::Idle, "Ship the release"),
        ];
        let buffer = render_list(&rows, 30, 6);

        // Each row's marker cell: cursor, one space, then the glyph.
        for (y, expected, style) in [
            (0u16, "✗", theme::danger()),
            (2, "✓", theme::ok()),
            (4, "○", theme::muted()),
        ] {
            assert_eq!(buffer[(GLYPH_COL, y)].symbol(), expected, "row {y}");
            assert_eq!(buffer[(GLYPH_COL, y)].style().fg, style.fg, "row {y}");
        }
    }

    /// The list's states follow the task's lifecycle; the live-turn flags the
    /// renderer layers on top are tested at the app level.
    #[test]
    fn the_row_state_follows_the_task_lifecycle() {
        for (lifecycle, expected) in [
            (forge_types::TaskLifecycle::Ready, SessionRowState::Idle),
            (
                forge_types::TaskLifecycle::Working,
                SessionRowState::Working,
            ),
            (
                forge_types::TaskLifecycle::Waiting,
                SessionRowState::Waiting,
            ),
            (
                forge_types::TaskLifecycle::Completed,
                SessionRowState::Completed,
            ),
            (forge_types::TaskLifecycle::Failed, SessionRowState::Failed),
            (
                forge_types::TaskLifecycle::Cancelled,
                SessionRowState::Cancelled,
            ),
            (
                forge_types::TaskLifecycle::Interrupted,
                SessionRowState::Interrupted,
            ),
        ] {
            assert_eq!(
                SessionRowState::from_lifecycle(lifecycle),
                expected,
                "{lifecycle:?}"
            );
        }
    }

    /// Only a turn stopped for the operator asks for the operator.
    #[test]
    fn only_the_waiting_state_needs_you() {
        for state in ALL_STATES {
            assert_eq!(
                state.needs_you(),
                state == SessionRowState::Waiting,
                "{state:?}"
            );
        }
    }

    /// Every frame a running row can show is one column wide and none of them
    /// collides with a state's own marker — the spinner turns through the same
    /// cell the other states sit in.
    #[test]
    fn every_spinner_frame_is_one_column_and_unclaimed() {
        for frame in crate::widgets::turn_line::SPINNER_FRAMES {
            assert_eq!(Span::raw(frame).width(), 1, "{frame:?}");
            for state in ALL_STATES
                .into_iter()
                .filter(|state| *state != SessionRowState::Working)
            {
                assert_ne!(frame, state.glyph(), "{state:?} collides with {frame:?}");
            }
        }
    }

    /// The running row turns: consecutive steps show consecutive frames, the
    /// frame is the running state's, and a row that is not running holds still
    /// while the running ones move around it.
    #[test]
    fn a_running_row_steps_while_the_others_hold_still() {
        let rows = vec![
            state_row(SessionRowState::Working, "one"),
            state_row(SessionRowState::Working, "two"),
            state_row(SessionRowState::Idle, "three"),
        ];
        let frames = crate::widgets::turn_line::SPINNER_FRAMES;
        for step in 0..frames.len() {
            let buffer = render_list_at(&rows, 30, 8, step);
            assert_eq!(buffer[(GLYPH_COL, 0)].symbol(), frames[step % frames.len()]);
            // The second row is one frame ahead: a column of running sessions
            // must not blink as one blanket.
            assert_eq!(
                buffer[(GLYPH_COL, 2)].symbol(),
                frames[(step + 1) % frames.len()]
            );
            assert_eq!(buffer[(GLYPH_COL, 4)].symbol(), "○", "the idle row moved");
        }
    }

    /// A step past the end of the set wraps rather than panicking; the step is
    /// a running counter, never index-bounded.
    #[test]
    fn the_spinner_step_wraps() {
        let rows = vec![state_row(SessionRowState::Working, "one")];
        let frames = crate::widgets::turn_line::SPINNER_FRAMES;
        let far = render_list_at(&rows, 30, 4, frames.len() + 3);
        assert_eq!(far[(GLYPH_COL, 0)].symbol(), frames[3]);
    }

    /// The regression this phase exists to prevent: no state — selection,
    /// hover, expansion — may move a row's label column, because the bar and
    /// the marker are reserved on every row.
    #[test]
    fn no_state_moves_the_label_column() {
        let plain = |label: &str| state_row(SessionRowState::Idle, label);
        let mut selected = plain("second");
        selected.selected = true;

        let buffer = render_list_at(&[plain("first"), selected], 30, 6, 0);
        assert_eq!(buffer[(TEXT_COL as u16, 0)].symbol(), "f", "plain row");
        assert_eq!(buffer[(TEXT_COL as u16, 2)].symbol(), "s", "selected row");

        let hovered = render_list_with(&[plain("first")], 30, 4, 0, true, Some(0), None);
        assert_eq!(hovered[(TEXT_COL as u16, 0)].symbol(), "f", "hovered row");

        let mut cursor = plain("first");
        cursor.focused = true;
        let lines = vec!["I added the guard in src/auth.rs.".to_string()];
        let peek = PeekPanel {
            lines: &lines,
            reply: "",
            placeholder: "reply…",
        };
        // Expanded, the label sits inside the frame's top edge — same column.
        let expanded = render_list_with(&[cursor], 30, 8, 0, true, None, Some(&peek));
        assert_eq!(expanded[(TEXT_COL as u16, 1)].symbol(), "f", "expanded row");
    }

    /// Selection is the bar in the reserved gutter plus the ground; hover is
    /// the ground plus the marker. Neither borrows the other's signal.
    #[test]
    fn selection_paints_the_bar_and_hover_never_does() {
        let mut selected = state_row(SessionRowState::Idle, "viewed");
        selected.selected = true;
        let plain = state_row(SessionRowState::Idle, "other");

        let buffer = render_list_at(&[selected, plain.clone()], 30, 6, 0);
        assert_eq!(buffer[(BAR_COL, 0)].symbol(), "▌");
        assert_eq!(buffer[(BAR_COL, 0)].style().fg, theme::accent_style().fg);
        assert_eq!(
            buffer[(BAR_COL, 2)].symbol(),
            " ",
            "an idle row took the bar"
        );

        let hovered = render_list_with(&[plain], 30, 4, 0, true, Some(0), None);
        assert_eq!(hovered[(BAR_COL, 0)].symbol(), " ", "hover took the bar");
        assert_eq!(
            hovered[(MARKER_COL, 0)].symbol(),
            "›",
            "hover lost its shape"
        );
        assert_eq!(hovered[(BAR_COL, 0)].style().bg, theme::surface_hover().bg);
    }

    /// The ground covers both lines of the row, so a selected row reads as one
    /// band instead of a name on a strip with its qualifier floating below it.
    #[test]
    fn the_selection_ground_covers_both_lines() {
        let ground = theme::focused_selection_style().bg;
        let mut selected = state_row(SessionRowState::Working, "viewed");
        selected.selected = true;
        selected.qualifier = "running · 2m".into();
        let mut plain = state_row(SessionRowState::Idle, "other");
        plain.qualifier = "idle · 4m".into();

        let buffer = render_list_at(&[selected, plain], 30, 6, 0);
        assert_eq!(buffer[(BAR_COL, 0)].style().bg, ground, "label line");
        assert_eq!(buffer[(BAR_COL, 1)].style().bg, ground, "qualifier line");
        assert_ne!(
            buffer[(BAR_COL, 3)].style().bg,
            ground,
            "an idle row took the ground"
        );
    }

    /// `§8.5`: a selection that lost the keyboard stays visible but muted. The
    /// bar and the weight step stay; the ground and the cursor go.
    #[test]
    fn an_unfocused_selection_keeps_the_bar_and_gives_up_the_ground() {
        let mut selected = state_row(SessionRowState::Idle, "viewed");
        selected.selected = true;
        selected.focused = true;

        let buffer = render_list_with(&[selected], 30, 4, 0, false, None, None);
        assert_eq!(buffer[(BAR_COL, 0)].symbol(), "▌");
        assert_eq!(
            buffer[(MARKER_COL, 0)].symbol(),
            " ",
            "the cursor survived losing the keyboard"
        );
        assert_ne!(
            buffer[(BAR_COL, 0)].style().bg,
            theme::focused_selection_style().bg
        );
        assert!(
            buffer[(TEXT_COL as u16, 0)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the label lost its weight step"
        );
    }

    /// The expanded row is framed instead of grounded: the frame's left edge
    /// takes the reserved gutter and its right edge the list's last column, so
    /// expanding a row moves neither the bar's cell nor the label's.
    #[test]
    fn the_expanded_row_is_framed_in_the_reserved_gutter() {
        let mut cursor = state_row(SessionRowState::Working, "peeked");
        cursor.focused = true;
        cursor.selected = true;
        cursor.qualifier = "running · 2m".into();
        let lines = vec!["the last answer".to_string()];
        let peek = PeekPanel {
            lines: &lines,
            reply: "",
            placeholder: "reply…",
        };
        let buffer = render_list_with(&[cursor], 30, 10, 0, true, None, Some(&peek));

        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(29, 0)].symbol(), "╮");
        assert_eq!(buffer[(0, 3)].symbol(), "╰");
        assert_eq!(buffer[(29, 3)].symbol(), "╯");
        assert_eq!(buffer[(0, 1)].symbol(), "│");
        assert_eq!(buffer[(29, 1)].symbol(), "│");
        assert_eq!(buffer[(0, 0)].style().fg, theme::accent_style().fg);
        // The frame replaces the bar and flips the cursor to disclosure.
        assert_eq!(buffer[(MARKER_COL, 1)].symbol(), "⌄");
        assert_eq!(
            buffer[(TEXT_COL as u16, 1)].symbol(),
            "p",
            "the label moved"
        );
        // The peek hangs under the frame behind one guide rule.
        assert_eq!(buffer[((TEXT_COL - 1) as u16, 4)].symbol(), "│");
        assert_eq!(buffer[(TEXT_COL as u16, 4)].symbol(), "t");
    }

    /// A frame costs the block two lines. At a height where it does not fit it
    /// is dropped whole — a half-drawn box is worse than none — and the reply
    /// box still renders.
    #[test]
    fn a_frame_that_does_not_fit_is_dropped_whole() {
        let mut cursor = state_row(SessionRowState::Waiting, "peeked");
        cursor.focused = true;
        cursor.selected = true;
        let lines = vec!["done".to_string()];
        let peek = PeekPanel {
            lines: &lines,
            reply: "push it",
            placeholder: "reply…",
        };
        let buffer = render_list_with(&[cursor], 30, 6, 0, true, None, Some(&peek));
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(!text.contains('╭'), "a half-drawn frame: {text:?}");
        assert!(!text.contains('╰'), "a stray edge: {text:?}");
        assert!(text.contains("push it"), "the reply box went missing");
        // The same block one line taller takes the frame.
        let mut cursor = state_row(SessionRowState::Waiting, "peeked");
        cursor.focused = true;
        cursor.selected = true;
        let buffer = render_list_with(&[cursor], 30, 8, 0, true, None, Some(&peek));
        let text: String = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(
            text.contains('╭'),
            "the frame was dropped with room to spare"
        );
    }

    /// A peek line that opens with the failure marker takes the danger hue for
    /// the marker; the detail behind it stays secondary, so the one line that
    /// says a turn died reads as an error rather than as prose.
    #[test]
    fn a_failed_peek_line_colours_its_marker() {
        let mut cursor = state_row(SessionRowState::Failed, "peeked");
        cursor.focused = true;
        let lines = vec![format!(
            "{}model call failed",
            forge_core::TURN_FAILED_MARKER
        )];
        let peek = PeekPanel {
            lines: &lines,
            reply: "",
            placeholder: "reply…",
        };
        let buffer = render_list_with(&[cursor], 40, 10, 0, true, None, Some(&peek));
        let marker_end = forge_core::TURN_FAILED_MARKER.chars().count() as u16;
        assert_eq!(buffer[(TEXT_COL as u16, 4)].symbol(), "[");
        assert_eq!(buffer[(TEXT_COL as u16, 4)].style().fg, theme::danger().fg);
        assert_eq!(buffer[(TEXT_COL as u16 + marker_end, 4)].symbol(), "m");
        assert_eq!(
            buffer[(TEXT_COL as u16 + marker_end, 4)].style().fg,
            theme::text_secondary().fg
        );
    }

    /// The shared grammar drops verbs before pairs, which at the navigator's
    /// width would leave `Enter Esc`. The reply is what this box is for, so its
    /// pair is the one that keeps its verb.
    #[test]
    fn the_peek_hints_keep_the_reply_verb_when_only_one_pair_fits() {
        let text = |room: usize| -> String {
            peek_hints(room)
                .iter()
                .map(|span| span.content.to_string())
                .collect()
        };
        assert_eq!(text(40), "Enter send · Esc collapse");
        assert_eq!(text(20), "Enter send");
    }

    /// The cursor cell is disclosure, not selection (`D3`): `›` collapsed,
    /// `⌄` expanded, blank on a row the keyboard is not on.
    #[test]
    fn the_cursor_cell_shows_disclosure() {
        let mut cursor = state_row(SessionRowState::Idle, "first");
        cursor.focused = true;
        let second = state_row(SessionRowState::Idle, "second");

        let collapsed = render_list_with(
            &[cursor.clone(), second.clone()],
            30,
            6,
            0,
            true,
            None,
            None,
        );
        assert_eq!(collapsed[(MARKER_COL, 0)].symbol(), "›");
        assert_eq!(collapsed[(MARKER_COL, 2)].symbol(), " ");

        let lines = vec!["the last answer".to_string()];
        let peek = PeekPanel {
            lines: &lines,
            reply: "",
            placeholder: "reply…",
        };
        let expanded = render_list_with(&[cursor, second], 30, 8, 0, true, None, Some(&peek));
        assert_eq!(expanded[(MARKER_COL, 1)].symbol(), "⌄");
    }
}
