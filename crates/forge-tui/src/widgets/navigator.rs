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
use ratatui::widgets::Widget;

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigatorTab {
    Sessions,
    Files,
    /// Working-tree review. The tab only exists while the workspace is inside a
    /// Git repository, and its column is the explorer filtered to the changed
    /// files — the patch itself renders in the Workspace pane.
    Git,
}

impl NavigatorTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Files => "Files",
            Self::Git => "Git",
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
    Git,
    NewSession,
}

impl NavigatorRowStop {
    /// The tab stop that carries `tab`. `NewSession` is tab-independent, so it
    /// is never returned here.
    pub fn for_tab(tab: NavigatorTab) -> Self {
        match tab {
            NavigatorTab::Sessions => Self::Sessions,
            NavigatorTab::Files => Self::Files,
            NavigatorTab::Git => Self::Git,
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
    /// The qualifier names the same state as the marker, including finished
    /// turns whose runtime has returned to idle.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Waiting => "needs you",
            Self::Queued => "queued",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

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
        crate::widgets::turn_line::running_marker(step, false)
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
pub(crate) const SESSIONS_TAB_WIDTH: u16 = 10;

/// Width of the navigator row's `+` action, with breathing room around its glyph.
pub(crate) const NEW_SESSION_CELL_WIDTH: u16 = 3;

/// Keep creation reachable at the navigator's normal minimum width.
pub(crate) const MIN_NEW_SESSION_ROW_WIDTH: u16 = 22;

/// The `+` cell's rect on the navigator tab row, or `None` when the row cannot
/// carry it. It sits flush against the Sessions tab, so the selected ground
/// reaches the tile edge instead of stopping at a blank gutter. Painting,
/// keyboard stops and pointer routing all read this, so the three can never
/// disagree about whether the cell exists or where it is.
pub(crate) fn new_session_cell(area: Rect) -> Option<Rect> {
    if area.width < MIN_NEW_SESSION_ROW_WIDTH || area.height == 0 {
        return None;
    }
    Some(Rect::new(
        area.x + SESSIONS_TAB_WIDTH,
        area.y,
        NEW_SESSION_CELL_WIDTH,
        area.height,
    ))
}

/// The navigator tab row's tab rects, left to right. `git` adds the third tab,
/// which splits the space the `Sessions` tab and the `+` cell leave with
/// `Files`; without it the two-tab layout is exactly as it was, so nothing
/// moves in a workspace that is not a repository.
///
/// Painting, keyboard stops and pointer routing all read this, so the three can
/// never disagree about where a tab is.
pub(crate) fn navigator_tab_rects(area: Rect, git: bool) -> Vec<(NavigatorTab, Rect)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let split = SESSIONS_TAB_WIDTH.min(area.width);
    let sessions = Rect::new(area.x, area.y, split, area.height);
    let new_session = new_session_cell(area);
    let start = match new_session {
        Some(cell) => cell.right(),
        None => area.x + split,
    };
    let remaining = area.right().saturating_sub(start);
    if !git || remaining < 2 {
        return vec![
            (NavigatorTab::Sessions, sessions),
            (
                NavigatorTab::Files,
                Rect::new(start, area.y, remaining, area.height),
            ),
        ];
    }
    // Tiles sit flush: each owns its cells edge-to-edge so the selected ground
    // reaches the tile boundary instead of stopping short at a blank gutter.
    let files_width = remaining.div_ceil(2);
    let git_x = start + files_width;
    vec![
        (NavigatorTab::Sessions, sessions),
        (
            NavigatorTab::Files,
            Rect::new(start, area.y, files_width, area.height),
        ),
        (
            NavigatorTab::Git,
            Rect::new(
                git_x,
                area.y,
                area.right().saturating_sub(git_x),
                area.height,
            ),
        ),
    ]
}

/// The tab whose tile covers `col`, if any. Tiles sit flush, so the only
/// columns that return `None` are the `+` cell's, which is routed separately.
pub(crate) fn navigator_tab_at(col: u16, area: Rect, git: bool) -> Option<NavigatorTab> {
    navigator_tab_rects(area, git)
        .into_iter()
        .find(|(_, rect)| col >= rect.x && col < rect.right())
        .map(|(tab, _)| tab)
}

/// Single-row tiles across the top of the navigator column.
pub struct NavigatorTabs {
    pub tab: NavigatorTab,
    pub needs_you: usize,
    /// Whether the workspace is a repository, which is the only case the `Git`
    /// tab exists in.
    pub git: bool,
    /// Dedicated-sessions sidebar: render only the `Sessions` heading and the
    /// `+` cell. Files and Git are main-workspace tabs, not navigator tabs.
    pub sessions_only: bool,
    /// The row itself holds the keyboard (`↑` at the top of either tab's list,
    /// `FORGE-DESIGN §8.3`). Only the cursor tile takes the neutral selection
    /// ground and `>` marker; the selected tab remains independently marked.
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
        theme::fill(area, buf, theme::panel());
        let inactive = theme::metadata_style();
        // Keep the creation action beside Sessions, flush with it.
        // One source of truth for painting, keyboard stops and pointer routing.
        let new_session = new_session_cell(area);
        let rects = if self.sessions_only {
            vec![(
                NavigatorTab::Sessions,
                Rect::new(
                    area.x,
                    area.y,
                    SESSIONS_TAB_WIDTH.min(area.width),
                    area.height,
                ),
            )]
        } else {
            navigator_tab_rects(area, self.git)
        };
        // Grounds first, edge to edge across each tab; the labels and the `+`
        // glyph repaint their own cells on top of them.
        for (tab, rect) in &rects {
            theme::fill(*rect, buf, theme::panel());
            if *tab == self.tab {
                fill_tab_background(buf, *rect, Some(theme::accent_soft_bg()));
            } else if self.hover == Some(*tab) {
                fill_tab_background(buf, *rect, theme::surface_hover().bg);
            }
        }
        if let Some(cell) = new_session {
            theme::fill(cell, buf, theme::panel());
            if self.hover_new_session {
                fill_tab_background(buf, cell, theme::surface_hover().bg);
            }
        }
        for (index, (tab, tab_area)) in rects.into_iter().enumerate() {
            let is_active = tab == self.tab;
            let hovered = !is_active && self.hover == Some(tab);
            // Keep labels centered in equal-height, borderless tiles.
            let inner = Rect::new(
                tab_area.x,
                tab_area.y + tab_area.height.saturating_sub(1) / 2,
                tab_area.width,
                1,
            );
            let owner = self.focused && self.row_stop == NavigatorRowStop::for_tab(tab);
            if owner {
                // The keyboard cursor ground fills the whole tile, not just the
                // label row, so a taller tab band stays covered.
                theme::fill(tab_area, buf, theme::focused_selection_style());
            }
            let label_style = if owner {
                theme::focused_selection_style().add_modifier(Modifier::BOLD)
            } else if is_active {
                theme::text()
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
            let label = truncate(tab.label(), inner.width as usize);
            let label_width = label.chars().count() as u16;
            // Centred in the tab tile, both axes: the label sits on the band's
            // centre row and is centred across the full tile width.
            let mut label_x = inner.x + inner.width.saturating_sub(label_width) / 2;
            // A focused stop needs a two-cell marker slot (`>` plus a space) to
            // the left; nudge the label right only when centring would not leave
            // room, so the unfocused row stays truly centred.
            if owner && label_x < inner.x + 2 {
                label_x = inner.x + 2;
            }
            buf.set_string(label_x, inner.y, &label, label_style);
            if self.focused && self.row_stop == NavigatorRowStop::for_tab(tab) && label_x > inner.x
            {
                buf.set_string(
                    label_x.saturating_sub(2).max(inner.x),
                    inner.y,
                    theme::FOCUS_MARKER,
                    theme::accent_style().add_modifier(Modifier::BOLD),
                );
            }
            if index == 1 && self.tab != NavigatorTab::Sessions && self.needs_you > 0 {
                // The Sessions tab (10 wide, 8-cell label) cannot hold a
                // badge beside its label, so the wide Files tab hosts it —
                // shown whenever the session list's own need states are out
                // of sight, which is also true while the Git tab is up.
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
            let inner = Rect::new(
                cell.x,
                cell.y + cell.height.saturating_sub(1) / 2,
                cell.width,
                1,
            );
            if inner.width > 0 && inner.height > 0 {
                let selected = self.focused && self.row_stop == NavigatorRowStop::NewSession;
                let glyph_style = if selected {
                    theme::fill(cell, buf, theme::focused_selection_style());
                    theme::focused_selection_style().add_modifier(Modifier::BOLD)
                } else if self.hover_new_session {
                    inactive
                        .patch(theme::surface_hover())
                        .add_modifier(Modifier::BOLD)
                } else {
                    inactive
                };
                let mut glyph_x = inner.x + inner.width / 2;
                if selected && glyph_x < inner.x + 2 {
                    glyph_x = inner.x + 2;
                }
                buf.set_string(glyph_x, inner.y, "+", glyph_style);
                if selected {
                    buf.set_string(
                        glyph_x.saturating_sub(2).max(inner.x),
                        inner.y,
                        theme::FOCUS_MARKER,
                        glyph_style,
                    );
                }
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
    pub reduced_motion: bool,
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
            let label_style = if row.state.needs_you() {
                theme::text().add_modifier(Modifier::BOLD)
            } else {
                theme::text()
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
                SessionRowState::Working => crate::widgets::turn_line::running_marker(
                    self.step + index,
                    self.reduced_motion,
                ),
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

/// Paint the supplied tab interior without changing its glyphs.
fn fill_tab_background(buf: &mut Buffer, area: Rect, bg: Option<ratatui::style::Color>) {
    let Some(bg) = bg else {
        return;
    };
    for row in area.y..area.bottom() {
        for col in area.x..area.right() {
            buf[(col, row)].set_bg(bg);
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

    #[test]
    fn tab_row_marks_only_the_keyboard_cursor() {
        let area = Rect::new(0, 0, 46, 1);
        for focused in [false, true] {
            for stop in [
                NavigatorRowStop::Sessions,
                NavigatorRowStop::Files,
                NavigatorRowStop::Git,
                NavigatorRowStop::NewSession,
            ] {
                let mut buf = Buffer::empty(area);
                NavigatorTabs {
                    tab: NavigatorTab::Files,
                    needs_you: 0,
                    git: true,
                    sessions_only: false,
                    focused,
                    hover: None,
                    row_stop: stop,
                    hover_new_session: false,
                }
                .render(area, &mut buf);
                let row: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
                assert_eq!(row.matches('>').count(), usize::from(focused), "{row}");
                for label in ["Sessions", "Files", "Git", "+"] {
                    assert!(row.contains(label), "{row}");
                }
                if focused {
                    let label = match stop {
                        NavigatorRowStop::Sessions => "Sessions",
                        NavigatorRowStop::Files => "Files",
                        NavigatorRowStop::Git => "Git",
                        NavigatorRowStop::NewSession => "+",
                    };
                    assert!(row.contains(&format!("> {label}")), "{row}");
                }
            }
        }
    }

    #[test]
    fn tiles_are_flush_and_cover_every_column() {
        for width in [30, 40, 60] {
            let area = Rect::new(2, 0, width, 1);
            let tabs = navigator_tab_rects(area, true);
            let plus = new_session_cell(area).unwrap();
            let sessions = tabs[0].1;
            let files = tabs[1].1;
            let git = tabs[2].1;
            assert_eq!(plus.x, sessions.right(), "plus flush after Sessions");
            assert_eq!(files.x, plus.right(), "Files flush after the + cell");
            assert_eq!(git.x, files.right(), "Git flush after Files");
            assert_eq!(git.right(), area.right(), "Git reaches the row edge");
            for col in area.x..area.right() {
                let inside_plus = col >= plus.x && col < plus.right();
                if inside_plus {
                    assert_eq!(
                        navigator_tab_at(col, area, true),
                        None,
                        "the + cell is not a tab"
                    );
                } else {
                    assert!(
                        navigator_tab_at(col, area, true).is_some(),
                        "column {col} maps to no tab"
                    );
                }
            }
            for (tab, rect) in tabs {
                assert_eq!(navigator_tab_at(rect.x, area, true), Some(tab));
                assert_eq!(navigator_tab_at(rect.right() - 1, area, true), Some(tab));
                assert!(rect.right() <= area.right());
            }
        }
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
}
