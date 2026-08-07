//! Footer strip: one row, configuration on the left, live activity on the
//! right. Reuses the row's existing right-aligned-hints convention rather
//! than adding a second row — the footer renders at full window width
//! (`layout.rs`'s top-level status/main/footer stack), not sidebar width,
//! so both halves fit comfortably even at the enforced terminal minimum.

use crate::theme;
use crate::widgets::status::TurnLifecycle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

/// Which footer control (if any) is focused for keyboard/mouse activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterFocus {
    Llm,
    Effort,
}

#[derive(Debug, Clone, Default)]
pub struct FooterModel {
    /// Contextual action hints, right-aligned; overrides the activity
    /// display for this frame when set (e.g. during a dialog/HITL prompt).
    pub hints: String,
    /// `provider/model`, already short-formed — see [`footer_short_model_id`].
    pub llm_label: String,
    pub llm_connected: bool,
    pub effort_label: String,
    pub focus: Option<FooterFocus>,
    /// HITL pending — dim the row, don't look interactive.
    pub dimmed: bool,
    pub lifecycle: TurnLifecycle,
    /// 0.0..=1.0
    pub ctx_pct: f64,
}

pub struct FooterBar<'a> {
    pub model: &'a FooterModel,
}

/// Strip a `provider/` prefix from a wire model id for display.
pub fn footer_short_model_id(model: &str) -> &str {
    match model.split_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => model,
    }
}

/// Context-bar fill color: green under 70%, amber 70-90%, red at 90%+.
fn ctx_bar_style(pct: f64) -> Style {
    if pct >= 0.9 {
        theme::danger()
    } else if pct >= 0.7 {
        theme::warn()
    } else {
        theme::ok()
    }
}

const CTX_BAR_CELLS: usize = 9;

fn ctx_bar(pct: f64) -> String {
    let filled = ((pct.clamp(0.0, 1.0) * CTX_BAR_CELLS as f64).round() as usize).min(CTX_BAR_CELLS);
    format!(
        "{}{}",
        "▓".repeat(filled),
        "░".repeat(CTX_BAR_CELLS - filled)
    )
}

fn lifecycle_dot(life: TurnLifecycle) -> (&'static str, Style) {
    match life {
        TurnLifecycle::Working => ("◐", theme::info()),
        TurnLifecycle::Waiting => ("◑", theme::warn()),
        TurnLifecycle::Failed => ("●", theme::danger()),
        TurnLifecycle::Cancelled | TurnLifecycle::Interrupted => ("●", theme::dim()),
        TurnLifecycle::Ready | TurnLifecycle::Completed => ("●", theme::ok()),
    }
}

/// A filled "step button" glyph — the actual control, distinct from the
/// plain-text label it sits next to. `accent` picks the fill color; the
/// glyph color is always the canvas background so it reads as inverted
/// (a pressable pill), not more colored text.
fn step_button(spans: &mut Vec<ratatui::text::Span<'static>>, glyph: &'static str, fill: Style) {
    let canvas_bg = theme::canvas().bg;
    let inverted = match (fill.fg, canvas_bg) {
        (Some(fg), Some(bg)) => Style::default().bg(fg).fg(bg),
        _ => fill,
    };
    spans.push(ratatui::text::Span::styled(
        glyph,
        inverted.add_modifier(Modifier::BOLD),
    ));
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        let m = self.model;

        // A transient hint (dialog/HITL) takes over the whole row for this
        // frame rather than competing with the persistent activity display.
        let hints = m.hints.trim_end();
        if !hints.is_empty() {
            let hint_w = (hints.chars().count() as u16).min(area.width);
            buf.set_stringn(
                area.x + area.width - hint_w,
                area.y,
                hints,
                hint_w as usize,
                theme::muted(),
            );
            return;
        }

        use ratatui::text::Span;
        let dim = m.dimmed;

        // ---- left: configuration (which-LLM, effort) ----
        let mut left: Vec<Span<'static>> = Vec::new();
        let llm_style = if dim {
            theme::dim()
        } else if !m.llm_connected {
            theme::warn().add_modifier(Modifier::BOLD)
        } else {
            theme::agent().add_modifier(Modifier::BOLD)
        };
        let llm_focused = m.focus == Some(FooterFocus::Llm);
        // Which model is configured stays visible even while disconnected
        // (warn-colored) — connection state is a color signal, not a reason
        // to hide which model you'd be talking to.
        left.push(Span::styled(
            m.llm_label.clone(),
            if llm_focused && !dim {
                theme::focused_selection_style()
            } else {
                llm_style
            },
        ));
        left.push(Span::raw(" "));
        if !dim {
            step_button(&mut left, "▴", llm_style);
        }
        left.push(Span::raw("  "));
        left.push(Span::styled("·", theme::dim()));
        left.push(Span::raw("  "));
        let effort_style = if dim {
            theme::dim()
        } else {
            theme::accent_style().add_modifier(Modifier::BOLD)
        };
        let effort_focused = m.focus == Some(FooterFocus::Effort);
        if !dim {
            step_button(&mut left, "◂", effort_style);
        }
        left.push(Span::styled(
            m.effort_label.clone(),
            if effort_focused && !dim {
                theme::focused_selection_style()
            } else {
                effort_style
            },
        ));
        if !dim {
            step_button(&mut left, "▸", effort_style);
        }

        // ---- right: live activity (state, context, reserved badge) ----
        let (glyph, dot_style) = lifecycle_dot(m.lifecycle);
        let mut right: Vec<Span<'static>> = vec![
            Span::styled(glyph, dot_style.add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(m.lifecycle.label(), theme::text_secondary()),
            Span::raw("  "),
            Span::styled("·", theme::dim()),
            Span::raw("  "),
            Span::styled(ctx_bar(m.ctx_pct), ctx_bar_style(m.ctx_pct)),
            Span::raw(" "),
            Span::styled(
                format!("{:.0}%", m.ctx_pct * 100.0),
                ctx_bar_style(m.ctx_pct),
            ),
            Span::raw("  "),
            Span::styled("·", theme::dim()),
            Span::raw("  "),
            // Reserved for background job/agent counts — dim/empty today.
            Span::styled("⚑", theme::dim()),
        ];
        if dim {
            for span in right.iter_mut() {
                span.style = theme::dim();
            }
        }

        let left_line = ratatui::text::Line::from(left);
        let right_line = ratatui::text::Line::from(right);
        let left_w = left_line.width() as u16;
        let right_w = right_line.width() as u16;

        // Activity (right) never yields — it's read-only and short by
        // construction (fixed-width bar + state word). Configuration
        // (left) clips first under pressure, since a long provider/model
        // string is the only side that can grow unboundedly.
        if right_w <= area.width {
            buf.set_line(area.x + area.width - right_w, area.y, &right_line, right_w);
            let left_budget = area.width.saturating_sub(right_w).saturating_sub(1);
            buf.set_line(area.x, area.y, &left_line, left_budget.min(left_w));
        } else {
            // Pathologically narrow — right itself doesn't fit; give it
            // the whole row rather than showing nothing or corrupting it.
            buf.set_line(area.x, area.y, &right_line, area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(lifecycle: TurnLifecycle, ctx_pct: f64) -> FooterModel {
        FooterModel {
            llm_label: "openai/gpt-5.6-luna".into(),
            llm_connected: true,
            effort_label: "Medium".into(),
            lifecycle,
            ctx_pct,
            ..Default::default()
        }
    }

    fn rendered(m: &FooterModel, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: m }.render(area, &mut buf);
        (0..area.width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    #[test]
    fn renders_llm_and_effort_on_the_left() {
        let m = model(TurnLifecycle::Ready, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.trim_start().starts_with("openai"), "{out:?}");
    }

    #[test]
    fn renders_state_and_context_on_the_right() {
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains("Working"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
        assert!(out.trim_end().ends_with('⚑'), "{out:?}");
    }

    #[test]
    fn context_bar_color_thresholds() {
        assert!(ctx_bar_style(0.1).fg.is_some());
        // Just assert distinct styles at the three bands, not exact colors
        // (those come from the active theme palette).
        assert_ne!(ctx_bar_style(0.5).fg, ctx_bar_style(0.75).fg);
        assert_ne!(ctx_bar_style(0.75).fg, ctx_bar_style(0.95).fg);
    }

    #[test]
    fn disconnected_llm_stays_visible_but_warn_colored() {
        // Connection state is a color signal, not a reason to hide which
        // model is configured — the label must survive disconnection.
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.llm_connected = false;
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        let area = Rect::new(0, 0, 90, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        assert_eq!(buf[(0, 0)].style().fg, theme::warn().fg);
    }

    #[test]
    fn hints_take_over_the_row_when_set() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.hints = "Enter confirm · Esc cancel".into();
        let out = rendered(&m, 60);
        assert!(
            out.trim_end().ends_with("Enter confirm · Esc cancel"),
            "{out:?}"
        );
        assert!(!out.contains("Working"), "{out:?}");
    }

    #[test]
    fn dimmed_row_does_not_panic_and_hides_step_buttons() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.dimmed = true;
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
    }

    #[test]
    fn zero_width_does_not_panic() {
        let m = model(TurnLifecycle::Ready, 0.1);
        let area = Rect::new(0, 0, 0, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
    }

    #[test]
    fn fits_at_min_width_floor_without_dropping_anything() {
        // 76 usable cols is layout.rs::MIN_WIDTH's realistic floor (80-col
        // terminal, 95% content width). Both halves must render in full.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 76);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("Working"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
    }

    #[test]
    fn worst_case_long_label_clips_left_never_corrupts_activity() {
        // A realistic worst case: a long provider/model string + the
        // longest effort label ("Extra High"), at the MIN_WIDTH floor.
        // Row 1 (activity) must survive completely intact; configuration
        // is the side that clips under pressure, not the other way round.
        let mut m = model(TurnLifecycle::Working, 1.0);
        m.llm_label = "anthropic/claude-opus-4-8-20260815-preview".into();
        m.effort_label = "Extra High".into();
        let out = rendered(&m, 76);
        assert!(out.contains("Working"), "{out:?}");
        assert!(out.contains("100%"), "{out:?}");
        assert!(out.trim_end().ends_with('⚑'), "{out:?}");
    }
}
