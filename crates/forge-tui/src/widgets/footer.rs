//! Footer strip: row 0 is configuration on the left and live turn activity on
//! the right; row 1 (when the layout reserves it) is the background activity
//! line — counts-only chips for jobs, agents and queued prompts. The footer
//! renders at full window width (`layout.rs`'s top-level status/main/footer
//! stack), not sidebar width, so both halves fit at the enforced minimum.

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

/// Live background activity for the footer's reserved second row (design A3:
/// segmented count chips). Deliberately counts-only — the per-item list,
/// with commands and elapsed time, lives in the task view. All-zero means the
/// row stays blank, so the footer looks exactly as it did before anything runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FooterActivity {
    /// Shell/terminal jobs queued or running.
    pub jobs_active: usize,
    /// Shell/terminal jobs that exited with a failure.
    pub jobs_failed: usize,
    /// Jobs blocked on an operator decision (subagent approvals).
    pub jobs_need: usize,
    /// Shell/terminal jobs that finished successfully or were cancelled.
    pub jobs_done: usize,
    /// Agents/subagents queued or running.
    pub agents_active: usize,
    /// Subagents blocked on an operator decision.
    pub agents_need: usize,
    /// Subagents that ended in failure.
    pub agents_failed: usize,
    /// Subagents that finished successfully or were cancelled.
    pub agents_done: usize,
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
    /// Spinner frames while a turn runs. Advanced once per event-loop tick
    /// by the app busy state, so motion pauses with work instead of
    /// free-running on the wall clock.
    pub throbber: throbber_widgets_tui::ThrobberState,
    /// Session API-reported prompt/input tokens.
    pub prompt_tokens: u64,
    /// Session API-reported completion/output tokens.
    pub completion_tokens: u64,
    /// Session API-reported cached prompt-read tokens.
    pub prompt_cache_reads: u64,
    /// Live background activity for the second row. All-zero renders nothing.
    pub activity: FooterActivity,
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

/// Context pressure, as a word.
///
/// This was a nine-cell `▓░` bar. At the percentages that dominate a session —
/// single digits — every cell was `░`, and a row of 25%-density shade blocks
/// reads as stipple texture rather than as a meter. The colour already carries
/// the warning; a label says what the colour means, in the space the bar took.
fn ctx_label(pct: f64) -> &'static str {
    if pct >= 0.9 {
        "context full"
    } else if pct >= 0.7 {
        "context high"
    } else {
        "context"
    }
}

fn lifecycle_label(life: TurnLifecycle) -> (&'static str, Style) {
    match life {
        TurnLifecycle::Working => ("running", theme::info()),
        TurnLifecycle::Waiting => ("waiting", theme::warn()),
        TurnLifecycle::Failed => ("err", theme::danger()),
        TurnLifecycle::Cancelled | TurnLifecycle::Interrupted => ("stopped", theme::dim()),
        TurnLifecycle::Ready | TurnLifecycle::Completed => ("ready", theme::ok()),
    }
}

/// Pulsing dot for the `running` state, stepped once per event-loop tick
/// via `BusyState::tick`. The glyph never changes — only its brightness —
/// so the row keeps a fixed width and reads calm next to the live turn line.
/// The pattern follows OpenCode/Gemini: one dot breathing on a ~800ms cycle
/// (two ticks bright, two ticks dim at the 200ms event-loop cadence).
/// The plan checklist's active `[>]` marker shares this rhythm via
/// [`crate::conversation::plan_pulse_dim`].
pub(crate) fn running_dot_bright(state: &throbber_widgets_tui::ThrobberState) -> bool {
    (state.index() % 4) < 2
}

fn running_dot_style(
    base: ratatui::style::Style,
    state: &throbber_widgets_tui::ThrobberState,
) -> ratatui::style::Style {
    if running_dot_bright(state) {
        base
    } else {
        // Pulse in brightness within the same hue: dimming the base color
        // via the terminal DIM modifier rather than swapping to the muted
        // gray. The gray swap is low-contrast against light backgrounds,
        // so the dot read as static in `forge-light`.
        base.remove_modifier(Modifier::BOLD)
            .add_modifier(Modifier::DIM)
    }
}

/// Horizontal inset applied to the content row so the footer's text aligns.
/// (The live turn line above the composer owns phase detail; the footer
/// keeps the state word for every lifecycle, animated only while running.)
/// with the composer's left/right edges above it, rather than running flush
/// to the terminal border. Kept to 1 cell — the 76-col MIN_WIDTH floor must
/// still fit the full model label plus every chip.
const PAD: u16 = 1;

/// Columns the model id needs to stay recognisable once middle-truncated
/// (e.g. `…-luna`). Below this the footer drops the token unit label rather
/// than the chips.
const MIN_MODEL_CHARS: u16 = 6;

/// Build the second-row activity line from live counts (design A3). Returns
/// `None` when nothing is running so the reserved row stays blank.
fn activity_chips(a: &FooterActivity) -> Option<ratatui::text::Line<'static>> {
    use ratatui::text::Span;
    let mut spans: Vec<Span<'static>> = Vec::new();
    push_count_chip(
        &mut spans,
        "⟳",
        "jobs",
        a.jobs_active,
        a.jobs_need,
        a.jobs_failed,
        a.jobs_done,
    );
    push_count_chip(
        &mut spans,
        "◆",
        "agents",
        a.agents_active,
        a.agents_need,
        a.agents_failed,
        a.agents_done,
    );
    if spans.is_empty() {
        return None;
    }
    Some(ratatui::text::Line::from(spans))
}

/// One `[glyph noun N · qualifiers]` chip. The glyph and colour carry state;
/// the bracket is shared chrome, so the chips read as a segmented strip rather
/// than a sentence. Returns without drawing when the group is empty.
fn push_count_chip(
    spans: &mut Vec<ratatui::text::Span<'static>>,
    glyph: &'static str,
    noun: &'static str,
    active: usize,
    need: usize,
    failed: usize,
    done: usize,
) {
    use ratatui::text::Span;
    let (glyph, label, style) = if active > 0 {
        let mut label = format!("{noun} {active}");
        if need > 0 {
            label.push_str(&format!(" · {need} need"));
        }
        if failed > 0 {
            label.push_str(&format!(" · {failed} failed"));
        }
        let style = if need > 0 {
            theme::warn()
        } else if failed > 0 {
            theme::danger()
        } else {
            theme::info()
        };
        (glyph, label, style)
    } else if failed > 0 {
        ("✕", format!("{noun} {failed} failed"), theme::danger())
    } else if done > 0 {
        ("✓", format!("{noun} {done} done"), theme::ok())
    } else {
        return;
    };
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("[", theme::border_muted()));
    spans.push(Span::styled(glyph, style.add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(label, style));
    spans.push(Span::styled("]", theme::border_muted()));
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        let m = self.model;

        // Row 0 carries the interactive controls; row 1 (when the layout
        // reserves it) carries live background activity. Both share one inset
        // so text aligns with the composer's edges, not the terminal border.
        let inner = Rect::new(
            area.x + PAD.min(area.width),
            area.y,
            area.width.saturating_sub(2 * PAD),
            area.height.min(2),
        );
        if inner.width == 0 {
            return;
        }
        if inner.height > 1 {
            if let Some(line) = activity_chips(&m.activity) {
                buf.set_line(inner.x, inner.y + 1, &line, inner.width);
            }
        }

        // From here the renderer is single-row: row 0 only.
        let area = Rect::new(inner.x, inner.y, inner.width, 1);

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
            let min_left = config_chrome as u16 + effort_chars + MIN_MODEL_CHARS;
            let fits = |line: &ratatui::text::Line<'static>| {
                area.width
                    .saturating_sub(line.width() as u16)
                    .saturating_sub(1)
                    >= min_left
            };
            // Degrade in order: the token unit label drops first. The chips
            // never drop.
            //
            // The live turn line owns phase detail; the footer keeps the
            // state word beside context and usage, animated only while
            // running (one fixed-width spinner cell, so width never shifts).
            let labeled = self.activity_line(true);
            if fits(&labeled) {
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
        let llm_label = fit_model_label(&m.llm_label, model_max as usize);

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
        // short by construction (fixed-width meter + state word). Configuration
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
        // State word for every lifecycle; `running` breathes through one
        // fixed-width dot (bright/dim only, never a new glyph) so the row
        // never shifts width.
        let (label, dot_style) = lifecycle_label(m.lifecycle);
        let label_style = dot_style.add_modifier(Modifier::BOLD);
        let mut right: Vec<Span<'static>> = Vec::new();
        if m.lifecycle == TurnLifecycle::Working {
            right.push(Span::styled(
                "●",
                running_dot_style(label_style, &m.throbber),
            ));
            right.push(Span::raw(" "));
        }
        right.extend([Span::styled(label, label_style), Span::raw(" ")]);
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
            Span::styled(ctx_label(m.ctx_pct), theme::dim()),
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
/// `0 tokens` until any prompt tokens are reported (including "the model ran
/// but the provider sent no usage"). After that: `12.4k tokens · 81.35% cache`.
pub(crate) fn format_footer_usage_slot(
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_reads: u64,
    labeled: bool,
) -> String {
    // `labeled` spells out the unit: a bare "0" (and even a populated
    // "125k · 35.42% cache") tells a first-time reader nothing about what is being
    // counted, and the idle state — the very first thing they see — carried no
    // clue at all. The caller drops the label only when the row is too narrow
    // to afford it (see `MIN_MODEL_CHARS`).
    let unit = if labeled { " tokens" } else { "" };
    if prompt_tokens == 0 {
        // No cache rate exists yet, so no slot for one. It used to print an em
        // dash — a separator and a placeholder standing in for a value that has
        // no label, which reads as something failing rather than as nothing
        // having happened yet.
        return format!("0{unit}");
    }
    let total = prompt_tokens.saturating_add(completion_tokens);
    let rate = ((cache_reads as f64 / prompt_tokens as f64) * 100.0).clamp(0.0, 100.0);
    format!("{}{unit} · {rate:.2}% cache", compact_token_count(total))
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

/// Fit `Vendor/model` into `max` columns, sacrificing the vendor first.
///
/// Middle-truncating the whole label kept the vendor's first letters and the
/// model's last ones — `OpenAI/gpt-5.6-sol` became `Open…-sol`, which names
/// neither. The vendor is the recoverable half (it is in `/status`, and rarely
/// changes mid-session); the model name is what the footer is for.
fn fit_model_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    if let Some((_, model)) = label.split_once('/') {
        if model.chars().count() <= max {
            return model.to_string();
        }
        return truncate_middle(model, max);
    }
    truncate_middle(label, max)
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

    /// Only `running` breathes: one fixed-width `●` beside the state word,
    /// alternating bright and dim on the event-loop tick. The glyph row is
    /// identical on every frame — only the dot's style pulses — while every
    /// other lifecycle renders fully static output.
    #[test]
    fn busy_footer_pulses_only_while_running() {
        let m = model(TurnLifecycle::Working, 0.34);
        let first = rendered(&m, 90);
        let mut advanced = m.clone();
        advanced.throbber.calc_next();
        advanced.throbber.calc_next();
        let second = rendered(&advanced, 90);
        assert_eq!(first, second, "running text must not shift width");
        assert!(first.contains("running"), "{first:?}");
        assert!(!first.contains("Working"), "{first:?}");
        assert!(
            !first.contains('◐')
                && !first.contains('◑')
                && !first.contains('◒')
                && !first.contains('◓'),
            "{first:?}"
        );
        assert_eq!(
            running_dot_style(theme::info().add_modifier(Modifier::BOLD), &m.throbber),
            theme::info().add_modifier(Modifier::BOLD),
            "pulse starts bright"
        );
        assert_ne!(
            activity_styled(&m)[0].style,
            activity_styled(&advanced)[0].style,
            "dot brightness must pulse"
        );
        // The dim phase stays in the same hue (terminal DIM on the base
        // color), never the muted gray — the gray is low-contrast against
        // light backgrounds, so the dot read as static in `forge-light`.
        assert_eq!(
            activity_styled(&advanced)[0].style,
            theme::info()
                .remove_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
            "dim phase must keep its hue"
        );

        for life in [
            TurnLifecycle::Ready,
            TurnLifecycle::Completed,
            TurnLifecycle::Waiting,
            TurnLifecycle::Failed,
            TurnLifecycle::Cancelled,
            TurnLifecycle::Interrupted,
        ] {
            let m = model(life, 0.34);
            assert_eq!(rendered(&m, 90), rendered(&m, 90));
        }
    }

    #[test]
    fn footer_state_labels() {
        for (life, label) in [
            (TurnLifecycle::Working, "running"),
            (TurnLifecycle::Waiting, "waiting"),
            (TurnLifecycle::Failed, "err"),
            (TurnLifecycle::Cancelled, "stopped"),
            (TurnLifecycle::Interrupted, "stopped"),
            (TurnLifecycle::Ready, "ready"),
            (TurnLifecycle::Completed, "ready"),
        ] {
            let out = rendered(&model(life, 0.34), 90);
            assert!(out.contains(label), "{life:?}: {out:?}");
        }
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
        // DESIGN-012: single content row, no separator row.
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: m }.render(area, &mut buf);
        (0..area.width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    fn activity_styled(m: &FooterModel) -> Vec<ratatui::text::Span<'static>> {
        FooterBar { model: m }.activity_line(true).spans.to_vec()
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
        assert!(out.contains("ready"), "{out:?}");
    }

    #[test]
    fn footer_is_a_single_row_with_no_separator() {
        // DESIGN-012: no rule row of its own — the composer's top rule
        // bounds the zone. Content sits on row 0 at any height.
        let m = model(TurnLifecycle::Ready, 0.34);
        for height in [1, 2] {
            let area = Rect::new(0, 0, 90, height);
            let mut buf = Buffer::empty(area);
            FooterBar { model: &m }.render(area, &mut buf);
            let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
            assert!(row0.contains("Medium"), "{row0:?}");
            assert!(!row0.chars().all(|c| c == '─'), "{row0:?}");
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
        assert!(out.contains("running"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
        assert!(out.contains("0 tokens"), "{out:?}");
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
        assert_eq!(buf[(PAD, 0)].style().fg, theme::warn().fg);
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
        assert!(!out.contains("running"), "{out:?}");
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
            !out.contains('◑') && !out.contains('◒') && !out.contains('◐') && !out.contains('◓'),
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

    /// `Open…-sol` named neither the vendor nor the model. The vendor goes
    /// first, whole, so the model name survives intact.
    #[test]
    fn a_tight_footer_drops_the_vendor_not_the_model_name() {
        assert_eq!(
            fit_model_label("OpenAI/gpt-5.6-sol", 40),
            "OpenAI/gpt-5.6-sol"
        );
        assert_eq!(fit_model_label("OpenAI/gpt-5.6-sol", 14), "gpt-5.6-sol");
        // Only when even the bare model will not fit does it get elided.
        let tiny = fit_model_label("OpenAI/gpt-5.6-sol", 8);
        assert!(tiny.chars().count() <= 8, "{tiny}");
        assert!(!tiny.starts_with("Open"), "vendor should be gone: {tiny}");
    }

    /// No cache rate exists before the first turn, so no slot for one.
    #[test]
    fn the_idle_usage_slot_has_no_placeholder() {
        let out = format_footer_usage_slot(0, 0, 0, true);
        assert_eq!(out, "0 tokens");
        assert!(!out.contains('—'), "{out}");
    }

    /// The bar was nine shade cells that read as stipple at the percentages a
    /// session actually spends its time at.
    #[test]
    fn context_pressure_reads_as_a_word() {
        assert_eq!(ctx_label(0.01), "context");
        assert_eq!(ctx_label(0.75), "context high");
        assert_eq!(ctx_label(0.95), "context full");
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
        // The vendor goes first and goes whole, so the model name stays
        // readable instead of becoming `Open…free`.
        assert!(
            out.contains("deepseek-v4-flash-free"),
            "model name should survive: {out:?}"
        );
        assert!(!out.contains("OpenCode/"), "{out:?}");
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
        // that may shrink; effort and the right-side activity (meter,
        // lifecycle, context bar, usage) must render in full.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 76);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(!out.contains("Auto") && !out.contains("Manual"), "{out:?}");
        assert!(out.contains("running"), "{out:?}");
        assert!(out.contains("34%"), "{out:?}");
        assert!(out.contains("0 tokens"), "{out:?}");
    }

    #[test]
    fn usage_slot_names_its_unit_when_the_row_can_afford_it() {
        // "0" told a first-time reader nothing about what was being
        // counted, and the idle state is the first thing they see.
        let m = model(TurnLifecycle::Ready, 0.1);
        assert!(rendered(&m, 120).contains("0 tokens"));
    }

    #[test]
    fn a_cramped_row_drops_the_unit_label_before_the_effort_chip() {
        // The effort chip stays fully visible; the `tokens` unit drops first.
        // Narrower than it used to be: retiring the nine-cell context bar and
        // the idle `· —` gave the row back eleven columns.
        let m = model(TurnLifecycle::Working, 0.34);
        let out = rendered(&m, 48);
        assert!(out.contains("Medium"), "{out:?}");
        assert!(out.contains("0"), "{out:?}");
        assert!(!out.contains("0 tokens"), "{out:?}");
    }

    #[test]
    fn usage_slot_replaces_the_reserved_job_flag() {
        let mut m = model(TurnLifecycle::Ready, 0.34);
        m.prompt_tokens = 6_094;
        m.completion_tokens = 36;
        m.prompt_cache_reads = 5_504;
        let out = rendered(&m, 90);
        assert!(out.contains("6.1k tokens · 90.32% cache"), "{out:?}");
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
        assert!(out.contains("context full"), "{out:?}");
        assert!(out.contains("0 tokens"), "{out:?}");
    }

    #[test]
    fn format_footer_usage_slot_stays_blank_until_prompt_tokens() {
        assert_eq!(format_footer_usage_slot(0, 0, 0, true), "0 tokens");
        assert_eq!(format_footer_usage_slot(0, 500, 0, true), "0 tokens");
        assert_eq!(
            format_footer_usage_slot(100, 0, 0, true),
            "100 tokens · 0.00% cache"
        );
        assert_eq!(
            format_footer_usage_slot(6_094, 36, 5_504, true),
            "6.1k tokens · 90.32% cache"
        );
        assert_eq!(
            format_footer_usage_slot(100, 0, 200, true),
            "100 tokens · 100.00% cache"
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

    #[test]
    fn second_row_renders_a_jobs_chip_only_when_work_is_present() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        let idle = rows(&m, 90, 2);
        assert!(idle[1].trim().is_empty(), "idle second row: {:?}", idle[1]);

        m.activity.jobs_active = 2;
        m.activity.jobs_need = 1;
        let busy = rows(&m, 90, 2);
        assert!(busy[1].contains("⟳ jobs 2"), "{:?}", busy[1]);
        assert!(busy[1].contains("1 need"), "{:?}", busy[1]);
        // The activity row never disturbs the config/state row.
        assert!(busy[0].contains("Medium"), "{:?}", busy[0]);
        assert!(busy[0].contains("ready"), "{:?}", busy[0]);
    }

    #[test]
    fn finished_jobs_collapse_into_a_state_chip() {
        let mut failed = model(TurnLifecycle::Ready, 0.1);
        failed.activity.jobs_failed = 3;
        let out = rows(&failed, 90, 2);
        assert!(
            out[1].contains('✕') && out[1].contains("3 failed"),
            "{:?}",
            out[1]
        );

        let mut done = model(TurnLifecycle::Ready, 0.1);
        done.activity.jobs_done = 4;
        let out = rows(&done, 90, 2);
        assert!(
            out[1].contains('✓') && out[1].contains("4 done"),
            "{:?}",
            out[1]
        );
    }

    #[test]
    fn agents_get_their_own_chip_after_jobs() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.activity.jobs_active = 1;
        m.activity.agents_active = 2;
        m.activity.agents_need = 1;
        let out = rows(&m, 90, 2);
        assert!(
            out[1].contains("[⟳ jobs 1] [◆ agents 2 · 1 need]"),
            "{:?}",
            out[1]
        );
    }

    #[test]
    fn agents_chip_survives_when_only_finished_subagents_remain() {
        let mut m = model(TurnLifecycle::Ready, 0.1);
        m.activity.agents_done = 2;
        let out = rows(&m, 90, 2);
        assert!(
            out[1].contains('✓') && out[1].contains("agents 2 done"),
            "{:?}",
            out[1]
        );
    }

    /// Render `height` rows and return each row's text.
    fn rows(m: &FooterModel, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        FooterBar { model: m }.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }
}
