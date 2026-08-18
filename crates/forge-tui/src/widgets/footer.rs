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
use ratatui::text::Span;
use ratatui::widgets::Widget;
use std::time::{SystemTime, UNIX_EPOCH};

/// Which footer control (if any) is focused for keyboard/mouse activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterFocus {
    Llm,
    Effort,
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
    pub focus: Option<FooterFocus>,
    /// HITL pending — dim the row, don't look interactive.
    pub dimmed: bool,
    pub lifecycle: TurnLifecycle,
    /// Short qualifier shown after the lifecycle label, e.g. naming a check
    /// that didn't finish on an otherwise completed turn. Styled as secondary
    /// text, never as failure — the lifecycle glyph alone carries severity.
    pub lifecycle_detail: Option<String>,
    /// 0.0..=1.0
    pub ctx_pct: f64,
    /// Session API-reported prompt/input tokens.
    pub prompt_tokens: u64,
    /// Session API-reported completion/output tokens.
    pub completion_tokens: u64,
    /// Session API-reported cached prompt-read tokens.
    pub prompt_cache_reads: u64,
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
        TurnLifecycle::Working => ("●", theme::info()),
        TurnLifecycle::Waiting => ("◑", theme::warn()),
        TurnLifecycle::Failed => ("●", theme::danger()),
        TurnLifecycle::Cancelled | TurnLifecycle::Interrupted => ("●", theme::dim()),
        TurnLifecycle::Ready | TurnLifecycle::Completed => ("●", theme::ok()),
    }
}

fn shimmer_phase_at(text_len: usize, millis: u128) -> usize {
    if text_len == 0 {
        return 0;
    }
    ((millis / 180) as usize) % text_len
}

fn shimmer_phase(text_len: usize) -> usize {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    shimmer_phase_at(text_len, millis)
}

fn shimmer_label(label: &'static str, base: Style) -> Vec<Span<'static>> {
    let phase = shimmer_phase(label.chars().count());
    label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let style = if index == phase {
                theme::accent_style().add_modifier(Modifier::BOLD)
            } else {
                base
            };
            Span::styled(character.to_string(), style)
        })
        .collect()
}

/// Horizontal inset applied to the content row so the footer's text aligns
/// with the composer's left/right edges above it, rather than running flush
/// to the terminal border. Kept to 1 cell — the 76-col MIN_WIDTH floor must
/// still fit the full model label plus every chip.
const PAD: u16 = 1;

/// Columns the model id needs to stay recognisable once middle-truncated
/// (e.g. `…-luna`). Below this the footer drops the token unit label rather
/// than the chips.
const MIN_MODEL_CHARS: u16 = 6;

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        let m = self.model;

        // A divider rule separates the footer from the composer above it.
        // On a pathologically short area (height 1) there's no room for
        // both the rule and the content — skip the rule and use the whole
        // area as content, same as the pre-two-row behavior.
        let (rule_area, content_area) = if area.height >= 2 {
            (
                Some(Rect::new(area.x, area.y, area.width, 1)),
                Rect::new(area.x, area.y + 1, area.width, area.height - 1),
            )
        } else {
            (None, area)
        };
        if let Some(rule_area) = rule_area {
            buf.set_stringn(
                rule_area.x,
                rule_area.y,
                "─".repeat(rule_area.width as usize),
                rule_area.width as usize,
                theme::border_muted(),
            );
        }
        // Inset the content row so text aligns with the composer's edges
        // instead of running flush to the terminal border.
        let area = Rect::new(
            content_area.x + PAD.min(content_area.width),
            content_area.y,
            content_area.width.saturating_sub(2 * PAD),
            content_area.height,
        );
        if area.width == 0 {
            return;
        }

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

        // ---- left: configuration chips (which-LLM, effort) ----
        let dim = m.dimmed;
        let config_chrome = 2 // dot + " " before the model label
            + 1 + 1 + 1; // " │ " separator before effort
        let effort_chars = m.effort_label.chars().count() as u16;

        // ---- right: live activity, or the custom hint when one is set ----
        use ratatui::text::Span;
        let right = if hints.is_empty() {
            // Spelling out the token unit costs columns the narrowest frames
            // don't have. The model label shrinks first; the effort chip
            // stays fully visible; the unit label drops before anything on
            // the left. Take the labeled form only when the chips and a
            // still-recognisable model id survive it.
            let labeled = self.activity_line(true);
            let min_left = config_chrome as u16 + effort_chars + MIN_MODEL_CHARS;
            let fits = area
                .width
                .saturating_sub(labeled.width() as u16)
                .saturating_sub(1)
                >= min_left;
            if fits {
                labeled
            } else {
                self.activity_line(false)
            }
        } else {
            // The focused footer's per-chip hint reads as a whisper: dimmed
            // and italic, clearly secondary to the chips it describes.
            let hint_style = theme::dim().add_modifier(Modifier::ITALIC);
            ratatui::text::Line::from(Span::styled(hints.to_string(), hint_style))
        };
        let right_w = right.width() as u16;

        let left_budget = area.width.saturating_sub(right_w).saturating_sub(1);
        let model_max = left_budget
            .saturating_sub(config_chrome as u16 + effort_chars)
            .min(left_budget);
        let llm_label = truncate_middle(&m.llm_label, model_max as usize);

        let mut left: Vec<Span<'static>> = Vec::new();
        let dot_style = if dim {
            theme::dim()
        } else if !m.llm_connected {
            theme::warn()
        } else {
            theme::accent_style()
        };
        // Which model is configured stays visible even while disconnected
        // (warn-colored) — connection state is a color signal, not a reason
        // to hide which model you'd be talking to.
        let llm_style = if dim {
            theme::dim()
        } else if !m.llm_connected {
            theme::warn().add_modifier(Modifier::BOLD)
        } else {
            theme::text_secondary()
        };
        let llm_focused = m.focus == Some(FooterFocus::Llm);
        left.push(Span::styled("●", dot_style));
        left.push(Span::raw(" "));
        left.push(Span::styled(
            llm_label,
            if llm_focused && !dim {
                llm_style.add_modifier(Modifier::UNDERLINED)
            } else {
                llm_style
            },
        ));
        left.push(Span::raw(" "));
        left.push(Span::styled("│", theme::border_muted()));
        left.push(Span::raw(" "));
        let effort_style = if dim {
            theme::dim()
        } else {
            theme::accent_style().add_modifier(Modifier::BOLD)
        };
        let effort_focused = m.focus == Some(FooterFocus::Effort);
        left.push(Span::styled(
            m.effort_label.clone(),
            if effort_focused && !dim {
                effort_style.add_modifier(Modifier::UNDERLINED)
            } else {
                effort_style
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
    fn activity_line(&self, labeled_usage: bool) -> ratatui::text::Line<'static> {
        use ratatui::text::Span;
        let m = self.model;
        let dim = m.dimmed;
        let (glyph, dot_style) = lifecycle_dot(m.lifecycle);
        let mut right: Vec<Span<'static>> = vec![
            Span::styled(glyph, dot_style.add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ];
        if m.lifecycle == TurnLifecycle::Working && !dim {
            right.extend(shimmer_label(m.lifecycle.label(), theme::text_secondary()));
        } else {
            right.push(Span::styled(m.lifecycle.label(), theme::text_secondary()));
        }
        if let Some(detail) = m
            .lifecycle_detail
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
        {
            right.push(Span::styled(format!(" · {detail}"), theme::dim()));
        }
        right.extend([
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
        ]);
        right.push(Span::styled(
            format_footer_usage_slot(
                m.prompt_tokens,
                m.completion_tokens,
                m.prompt_cache_reads,
                labeled_usage,
            ),
            theme::text_secondary(),
        ));
        if dim {
            for span in right.iter_mut() {
                span.style = theme::dim();
            }
        }
        ratatui::text::Line::from(right)
    }
}

/// Last footer segment: session total + cache hit rate.
///
/// `0 tokens · —` until any prompt tokens are reported (including "the model ran
/// but the provider sent no usage"). After that: `12.4k tokens · 81% cache`.
pub(crate) fn format_footer_usage_slot(
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_reads: u64,
    labeled: bool,
) -> String {
    // `labeled` spells out the unit: a bare "0 · —" (and even a populated
    // "125k · 35% cache") tells a first-time reader nothing about what is being
    // counted, and the idle state — the very first thing they see — carried no
    // clue at all. The caller drops the label only when the row is too narrow
    // to afford it (see `MIN_MODEL_CHARS`).
    let unit = if labeled { " tokens" } else { "" };
    if prompt_tokens == 0 {
        return format!("0{unit} · —");
    }
    let total = prompt_tokens.saturating_add(completion_tokens);
    let rate = ((cache_reads as f64 / prompt_tokens as f64) * 100.0)
        .clamp(0.0, 100.0)
        .round() as u32;
    format!("{}{unit} · {rate}% cache", compact_token_count(total))
}

/// Compact count for the footer: `999`, `1.2k`, `12k`, `1.2M`.
pub(crate) fn compact_token_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        compact_scaled(n, 1_000, "k")
    } else if n < 1_000_000_000 {
        compact_scaled(n, 1_000_000, "M")
    } else {
        compact_scaled(n, 1_000_000_000, "B")
    }
}

fn compact_scaled(n: u64, scale: u64, suffix: &str) -> String {
    let tenths = n.saturating_add(scale / 20) / (scale / 10);
    if tenths.is_multiple_of(10) {
        format!("{}{suffix}", tenths / 10)
    } else {
        format!("{}.{}{suffix}", tenths / 10, tenths % 10)
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

    #[test]
    fn shimmer_moves_left_to_right_at_a_slow_cadence() {
        assert_eq!(shimmer_phase_at(7, 0), 0);
        assert_eq!(shimmer_phase_at(7, 179), 0);
        assert_eq!(shimmer_phase_at(7, 180), 1);
        assert_eq!(shimmer_phase_at(7, 1_260), 0);
    }

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

    /// Renders at the standard two-row height (rule + content) and returns
    /// the content row's text.
    fn rendered(m: &FooterModel, width: u16) -> String {
        let area = Rect::new(0, 0, width, 2);
        let mut buf = Buffer::empty(area);
        FooterBar { model: m }.render(area, &mut buf);
        (0..area.width).map(|x| buf[(x, 1)].symbol()).collect()
    }

    #[test]
    fn renders_llm_and_effort_as_plain_labels() {
        // Chips are plain text — no glyphs (▴/⏎/>>) — the interactions are
        // taught by the hint row, not by decorating the labels.
        let m = model(TurnLifecycle::Ready, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
        assert!(out.contains("Medium"), "{out:?}");
        assert!(!out.contains("Auto") && !out.contains("Manual"), "{out:?}");
        assert!(
            !out.contains('▴') && !out.contains('⏎') && !out.contains(">>"),
            "{out:?}"
        );
        assert!(out.trim_start().starts_with('●'), "{out:?}");
    }

    #[test]
    fn renders_a_divider_rule_above_the_content_row() {
        let m = model(TurnLifecycle::Ready, 0.34);
        let area = Rect::new(0, 0, 90, 2);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), "─", "rule row should be solid");
        }
    }

    #[test]
    fn content_row_is_inset_from_both_edges() {
        let m = model(TurnLifecycle::Ready, 0.34);
        let out = rendered(&m, 90);
        assert_eq!(&out[..PAD as usize], " ", "left inset: {out:?}");
        let trailing: String = out.chars().rev().take(PAD as usize).collect();
        assert_eq!(trailing, " ", "right inset: {out:?}");
    }

    #[test]
    fn config_chips_use_pipe_separators() {
        // Only the left-side config chips switch to `│`; the right-side
        // activity line keeps its own `·` separators unchanged.
        let m = model(TurnLifecycle::Ready, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains('│'), "{out:?}");
        let effort_end = out.find("Medium").expect("effort chip renders") + "Medium".len();
        assert!(
            !out[..effort_end].contains('·'),
            "chip cluster should use │, not ·: {out:?}"
        );
    }

    #[test]
    fn height_one_degrades_to_single_row_without_a_rule() {
        // A pathologically short area (no room for rule + content) falls
        // back to rendering content directly in the one row available,
        // matching the pre-two-row behavior rather than panicking.
        let m = model(TurnLifecycle::Ready, 0.34);
        let area = Rect::new(0, 0, 90, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        let out: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(out.contains("openai/gpt-5.6-luna"), "{out:?}");
    }

    #[test]
    fn renders_state_and_context_on_the_right() {
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 90);
        assert!(out.contains("Working"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
        assert!(out.contains("0 tokens · —"), "{out:?}");
        assert!(!out.contains('⚑'), "{out:?}");
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
        let area = Rect::new(0, 0, 90, 2);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        assert_eq!(buf[(PAD, 1)].style().fg, theme::warn().fg);
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
        let area = Rect::new(0, 0, 90, 2);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &m }.render(area, &mut buf);
        let hint_cell = (0..area.width)
            .find(|&x| buf[(x, 1)].symbol() == "⏎")
            .expect("hint glyph should render");
        let style = buf[(hint_cell, 1)].style();
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
        // the effort chip must stay fully visible.
        let mut m = model(TurnLifecycle::Working, 0.34);
        m.llm_label = "OpenCode/deepseek-v4-flash-free".into();
        let out = rendered(&m, 76);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(!out.contains("Auto") && !out.contains("Manual"), "{out:?}");
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
        // terminal, 95% content width). The model label is the only side
        // that may shrink; effort and the right-side activity
        // (lifecycle, context bar, usage) must render in full.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 76);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(!out.contains("Auto") && !out.contains("Manual"), "{out:?}");
        assert!(out.contains("Working"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
        assert!(out.contains("0 tokens · —"), "{out:?}");
    }

    #[test]
    fn usage_slot_names_its_unit_when_the_row_can_afford_it() {
        // "0 · —" told a first-time reader nothing about what was being
        // counted, and the idle state is the first thing they see.
        let m = model(TurnLifecycle::Ready, 0.1);
        assert!(rendered(&m, 120).contains("0 tokens · —"));
    }

    #[test]
    fn a_cramped_row_drops_the_unit_label_before_the_effort_chip() {
        // The effort chip stays fully visible; the `tokens` unit drops first.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 60);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("0 · —"), "{out:?}");
        assert!(!out.contains("0 tokens"), "{out:?}");
    }

    #[test]
    fn usage_slot_replaces_the_reserved_job_flag() {
        let mut m = model(TurnLifecycle::Ready, 0.34);
        m.prompt_tokens = 6_094;
        m.completion_tokens = 36;
        m.prompt_cache_reads = 5_504;
        let out = rendered(&m, 90);
        assert!(out.contains("6.1k tokens · 90% cache"), "{out:?}");
        assert!(!out.contains("2 changes"), "{out:?}");
        assert!(!out.contains('⚑'), "{out:?}");
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
        assert!(out.contains("0 tokens · —"), "{out:?}");
    }

    #[test]
    fn format_footer_usage_slot_stays_blank_until_prompt_tokens() {
        assert_eq!(format_footer_usage_slot(0, 0, 0, true), "0 tokens · —");
        assert_eq!(format_footer_usage_slot(0, 500, 0, true), "0 tokens · —");
        assert_eq!(
            format_footer_usage_slot(100, 0, 0, true),
            "100 tokens · 0% cache"
        );
        assert_eq!(
            format_footer_usage_slot(6_094, 36, 5_504, true),
            "6.1k tokens · 90% cache"
        );
        assert_eq!(
            format_footer_usage_slot(100, 0, 200, true),
            "100 tokens · 100% cache"
        );
    }

    #[test]
    fn compact_token_count_uses_k_and_m() {
        assert_eq!(compact_token_count(0), "0");
        assert_eq!(compact_token_count(999), "999");
        assert_eq!(compact_token_count(1_000), "1k");
        assert_eq!(compact_token_count(12_400), "12.4k");
        assert_eq!(compact_token_count(1_200_000), "1.2M");
    }
}
