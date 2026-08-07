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
    Mode,
}

#[derive(Debug, Clone, Default)]
pub struct FooterModel {
    /// Contextual action hints. When `hint_replaces_row` is set (blocking
    /// dialog/HITL states) the hint takes over the whole row; the focused
    /// footer's per-chip hint instead shares the row, swapping out only the
    /// right-side activity so the chips stay visible.
    pub hints: String,
    /// Blocking hints replace the entire row; footer-focus hints don't.
    pub hint_replaces_row: bool,
    /// `provider/model`, already short-formed — see [`footer_short_model_id`].
    pub llm_label: String,
    pub llm_connected: bool,
    pub effort_label: String,
    /// Permission-mode label (Manual / Auto) for the footer's mode chip.
    pub mode_label: String,
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

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        let m = self.model;

        // A blocking hint (HITL/dialog) takes over the whole row for this
        // frame — the chips are dimmed and irrelevant then. The focused
        // footer's per-chip hint is non-blocking: it shares the row,
        // replacing only the right-side activity.
        let hints = m.hints.trim_end();
        if m.hint_replaces_row && !hints.is_empty() {
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

        // ---- right: live activity, or the custom hint when one is set ----
        use ratatui::text::Span;
        let right = if hints.is_empty() {
            self.activity_line()
        } else {
            // The focused footer's per-chip hint reads as a whisper: dimmed
            // and italic, clearly secondary to the chips it describes.
            let hint_style = theme::dim().add_modifier(Modifier::ITALIC);
            ratatui::text::Line::from(Span::styled(hints.to_string(), hint_style))
        };
        let right_w = right.width() as u16;

        // ---- left: configuration chips (which-LLM, effort, mode) ----
        let dim = m.dimmed;
        let mode_display = m.mode_label.clone();
        let config_chrome = 1 // " " after the model label
            + 2 + 1 + 2 // "  ·  " separator
            + 2 + 1 + 2 // "  ·  " before the mode chip
            + mode_display.chars().count();
        let effort_chars = m.effort_label.chars().count() as u16;
        let left_budget = area.width.saturating_sub(right_w).saturating_sub(1);
        let model_max = left_budget
            .saturating_sub(config_chrome as u16 + effort_chars)
            .min(left_budget);
        let llm_label = truncate_middle(&m.llm_label, model_max as usize);

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
            llm_label,
            if llm_focused && !dim {
                theme::focused_selection_style()
            } else {
                llm_style
            },
        ));
        left.push(Span::raw("  "));
        left.push(Span::styled("·", theme::dim()));
        left.push(Span::raw("  "));
        let effort_style = if dim {
            theme::dim()
        } else {
            theme::accent_style().add_modifier(Modifier::BOLD)
        };
        let effort_focused = m.focus == Some(FooterFocus::Effort);
        left.push(Span::styled(
            m.effort_label.clone(),
            if effort_focused && !dim {
                theme::focused_selection_style()
            } else {
                effort_style
            },
        ));
        left.push(Span::raw("  "));
        left.push(Span::styled("·", theme::dim()));
        left.push(Span::raw("  "));
        let mode_style = if dim {
            theme::dim()
        } else {
            theme::accent_style().add_modifier(Modifier::BOLD)
        };
        let mode_focused = m.focus == Some(FooterFocus::Mode);
        left.push(Span::styled(
            mode_display,
            if mode_focused && !dim {
                theme::focused_selection_style()
            } else {
                mode_style
            },
        ));

        let left_line = ratatui::text::Line::from(left);
        let left_w = left_line.width() as u16;

        // Activity (right) never yields when it's the read-only state — it's
        // short by construction (fixed-width bar + state word). Configuration
        // (left) clips first under pressure, since a long provider/model
        // string is the only side that can grow unboundedly.
        if right_w <= area.width {
            buf.set_line(area.x + area.width - right_w, area.y, &right, right_w);
            let left_rend_budget = area.width.saturating_sub(right_w).saturating_sub(1);
            buf.set_line(area.x, area.y, &left_line, left_rend_budget.min(left_w));
        } else {
            // Pathologically narrow — right itself doesn't fit; give it
            // the whole row rather than showing nothing or corrupting it.
            buf.set_line(area.x, area.y, &right, area.width);
        }
    }
}

impl FooterBar<'_> {
    fn activity_line(&self) -> ratatui::text::Line<'static> {
        use ratatui::text::Span;
        let m = self.model;
        let dim = m.dimmed;
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
        ratatui::text::Line::from(right)
    }
}

/// Middle-truncate `text` to at most `max` chars, keeping both ends (the
/// vendor prefix and the model id stay recognizable); drops the ellipsis
/// entirely when `max` is too small to afford one.
fn truncate_middle(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    if max < 5 {
        return text.chars().take(max).collect();
    }
    let keep = (max - 1) / 2;
    let start: String = text.chars().take(keep).collect();
    let end: String = text
        .chars()
        .rev()
        .take(max - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(lifecycle: TurnLifecycle, ctx_pct: f64) -> FooterModel {
        FooterModel {
            llm_label: "openai/gpt-5.6-luna".into(),
            llm_connected: true,
            effort_label: "Medium".into(),
            mode_label: "Auto".into(),
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
    fn renders_llm_effort_and_mode_as_plain_labels() {
        // Chips are plain text — no glyphs (▴/⏎/>>) — the interactions are
        // taught by the hint row, not by decorating the labels.
        let m = model(TurnLifecycle::Ready, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("Auto"), "{out:?}");
        assert!(
            !out.contains('▴') && !out.contains('⏎') && !out.contains(">>"),
            "{out:?}"
        );
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
    fn blocking_hint_takes_over_the_row_when_set() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.hint_replaces_row = true;
        m.hints = "Enter confirm · Esc cancel".into();
        let out = rendered(&m, 60);
        assert!(
            out.trim_end().ends_with("Enter confirm · Esc cancel"),
            "{out:?}"
        );
        assert!(!out.contains("Working"), "{out:?}");
        assert!(!out.contains("openai/gpt-5.6-luna"), "chips yield: {out:?}");
    }

    #[test]
    fn footer_hint_shares_the_row_with_the_chips() {
        // The focused footer's per-chip hint is non-blocking: it swaps out
        // only the right-side activity, never the chips, and never needs a
        // second row. It reads "Hit Enter ⏎ to <action>" in dimmed italics.
        let mut m = model(TurnLifecycle::Working, 0.34);
        m.hints = "Hit Enter ⏎ to open model".into();
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("Hit Enter ⏎ to open model"), "{out:?}");
        assert!(
            !out.contains("Working"),
            "activity yields to the hint: {out:?}"
        );
        let area = Rect::new(0, 0, 90, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        let hint_cell = (0..area.width)
            .find(|&x| buf[(x, 0)].symbol() == "⏎")
            .expect("hint glyph should render");
        let style = buf[(hint_cell, 0)].style();
        assert!(
            style.add_modifier.contains(Modifier::ITALIC),
            "hint should be italic: {style:?}"
        );
    }

    #[test]
    fn dimmed_row_does_not_panic() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.dimmed = true;
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
    }

    #[test]
    fn long_model_truncates_but_keeps_the_effort_control() {
        // The read-only model label is the side that shrinks under pressure;
        // the effort and mode chips must stay fully visible.
        let mut m = model(TurnLifecycle::Working, 0.34);
        m.llm_label = "OpenCode/deepseek-v4-flash-free".into();
        let out = rendered(&m, 76);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("Auto"), "{out:?}");
        assert!(
            out.contains('…'),
            "long model should middle-truncate: {out:?}"
        );
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
        // terminal, 95% content width). Plain chips (no glyphs) free enough
        // budget for the full model label here; every control and both
        // activity halves must render in full.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 76);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("Auto"), "{out:?}");
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
