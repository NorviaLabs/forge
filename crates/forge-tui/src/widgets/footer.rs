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
    /// dialog/HITL states) the hint takes over the whole row; otherwise it
    /// shares the row with the chips, swapping out only the right-side
    /// activity so the chips stay visible — the task strip's session hint
    /// (bold keys) or the focused chip's own action.
    pub hints: String,
    /// Blocking hints replace the entire row; the footer's per-chip hint and
    /// the task strip's session hint share it with the chips.
    pub hint_replaces_row: bool,
    /// Session hint (task-strip bindings) sharing the row with the chips:
    /// rendered in the `key verb` grammar with bold keys, degraded to fit.
    pub hint_bold_keys: bool,
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
    /// Hovered left chip (0 = model, 1 = effort, 2 = notes); tint only, never
    /// focus.
    pub hover_chip: Option<usize>,
    /// Session scratchpad state for the third chip. `None` hides the chip
    /// entirely; `Some` shows a line count, and the word `unsaved` when the
    /// buffer has edits since the last write. An empty saved buffer is hidden.
    pub scratchpad: Option<ScratchpadChip>,
}

/// Line count plus unsaved state for the footer's scratchpad chip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScratchpadChip {
    pub lines: usize,
    pub dirty: bool,
}

pub struct FooterBar<'a> {
    pub model: &'a FooterModel,
    /// Optional sink receiving the two left-chip x-ranges `(model, effort)`
    /// from this paint, for pointer hit-testing. `None` in tests.
    pub chip_sink: Option<&'a ChipSink<'a>>,
}

/// x-ranges `(model, effort)` of the footer's left chips, published by a
/// paint into a [`ChipSink`] for pointer hit-testing.
pub type ChipSink<'a> = std::cell::RefCell<Option<[(u16, u16); 3]>>;

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
/// to the terminal border. Matches `PANE_PAD_X`; the round-2 inset costs the
/// 78-col MIN_WIDTH floor two columns, so a long provider/model label keeps
/// its vendor dropped and middle-truncates the model id instead of clipping.
const PAD: u16 = crate::design::COMPOSER_PAD_X;

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

/// Render a `key verb · key verb` hint with bold keys.
///
/// Parses the ` · `-joined pairs the app hands over (see
/// `TuiApp::contextual_hint`): the key is the pair's first token, bold; the
/// rest of the pair is the verb, kept byte-identical so the text never drifts
/// from what the key does. Degrades like [`crate::hints::hint_spans`] — verbs
/// drop first, then trailing pairs — and never wraps; callers clip with
/// `set_line`.
fn styled_hint_spans(hints: &str, budget: usize) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    let pairs: Vec<(&str, &str)> = hints
        .split(" · ")
        .filter_map(|pair| {
            let key = pair.split_whitespace().next()?;
            Some((key, &pair[key.len()..]))
        })
        .collect();
    fn build(pairs: &[(&str, &str)], verbs: bool) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (key, rest) in pairs {
            if !spans.is_empty() {
                spans.push(Span::styled(
                    if verbs {
                        " · ".to_string()
                    } else {
                        " ".to_string()
                    },
                    theme::metadata_style(),
                ));
            }
            spans.push(Span::styled(
                (*key).to_string(),
                theme::metadata_style().add_modifier(Modifier::BOLD),
            ));
            if verbs && !rest.trim().is_empty() {
                spans.push(Span::styled((*rest).to_string(), theme::metadata_style()));
            }
        }
        spans
    }
    let width = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();

    let full = build(&pairs, true);
    if width(&full) <= budget {
        return full;
    }
    let keys_only = build(&pairs, false);
    if width(&keys_only) <= budget {
        return keys_only;
    }
    for take in (1..pairs.len()).rev() {
        let trimmed = build(&pairs[..take], false);
        if width(&trimmed) <= budget {
            return trimmed;
        }
    }
    Vec::new()
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

        // A blocking hint (HITL/dialog/transient) takes over the whole row for
        // this frame — the chips are dimmed and irrelevant then. The focused
        // footer's per-chip hint and the task strip's session hint are
        // non-blocking: they share the row, replacing only the right-side
        // activity.
        let hints = m.hints.trim_end();
        if m.hint_replaces_row && !hints.is_empty() {
            let line = ratatui::text::Line::from(styled_hint_spans(hints, area.width as usize));
            let hint_w = (line.width() as u16).min(area.width);
            if hint_w > 0 {
                buf.set_line(area.x + area.width - hint_w, area.y, &line, hint_w);
            }
            return;
        }

        // ---- left: configuration chips (which-LLM, effort) ----
        let dim = m.dimmed;
        let connection = if m.llm_connected {
            ""
        } else {
            " · disconnected"
        };
        let config_chrome = 2 // dot + " " before the model label
            + 1 + 1 + 1 // " │ " separator before effort
            + connection.chars().count();
        let effort_chars = m.effort_label.chars().count() as u16;
        let notes_label = m
            .scratchpad
            .filter(|chip| chip.lines > 0 || chip.dirty)
            .map(|chip| {
                format!(
                    "notes {}{}",
                    chip.lines,
                    if chip.dirty { " · unsaved" } else { "" }
                )
            });
        let notes_chars = notes_label
            .as_ref()
            .map_or(0, |label| label.chars().count() as u16 + 3);

        // ---- right: live activity, or the custom hint when one is set ----
        use ratatui::text::Span;
        let right = if hints.is_empty() {
            // Spelling out the token unit costs columns the narrowest frames
            // don't have. The model label shrinks first; the effort chip
            // stays fully visible; the unit label drops before anything on
            // the left. Take the labeled form only when the chips and a
            // still-recognisable model id survive it.
            let min_left = config_chrome as u16 + effort_chars + notes_chars + MIN_MODEL_CHARS;
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
        } else if m.hint_bold_keys {
            // Session hint: the chips keep full width; the hint degrades
            // into the remainder. Built after the left below.
            ratatui::text::Line::default()
        } else {
            // The focused footer's per-chip hint reads as a whisper: dimmed
            // and italic, clearly secondary to the chips it describes.
            let hint_style = theme::dim().add_modifier(Modifier::ITALIC);
            ratatui::text::Line::from(Span::styled(hints.to_string(), hint_style))
        };
        let right_w = right.width() as u16;

        let left_budget = area.width.saturating_sub(right_w).saturating_sub(1);
        let model_max = left_budget
            .saturating_sub(config_chrome as u16 + effort_chars + notes_chars)
            .min(left_budget);
        let llm_label = fit_model_label(&m.llm_label, model_max as usize);
        let llm_label_w = llm_label.chars().count() as u16 + connection.chars().count() as u16;

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
        let llm_style = if m.hover_chip == Some(0) && !llm_focused {
            llm_style.patch(theme::surface_hover())
        } else {
            llm_style
        };
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
        left.push(Span::styled(
            connection,
            if dim { theme::dim() } else { theme::warn() },
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
        let effort_style = if m.hover_chip == Some(1) && !effort_focused {
            effort_style.patch(theme::surface_hover())
        } else {
            effort_style
        };
        left.push(Span::styled(
            m.effort_label.clone(),
            if effort_focused && !dim {
                effort_style.add_modifier(Modifier::UNDERLINED)
            } else {
                effort_style
            },
        ));

        // The scratchpad is an ordinary third chip: same separator, same
        // secondary weight, no new visual class for one feature. The count is a
        // word (`9 lines`), never a meter, and unsaved state is spelled out so
        // colour never travels alone.
        let notes_range = notes_label.map(|label| {
            left.push(Span::raw(" "));
            left.push(Span::styled("\u{2502}", theme::border_muted()));
            left.push(Span::raw(" "));
            let notes_x = area.x
                + left
                    .iter()
                    .map(|span| span.content.chars().count() as u16)
                    .sum::<u16>();
            let base = if dim {
                theme::dim()
            } else {
                theme::text_secondary()
            };
            let style = if m.hover_chip == Some(2) {
                base.patch(theme::surface_hover())
            } else {
                base
            };
            let width = label.chars().count() as u16;
            left.push(Span::styled(
                label,
                if m.scratchpad.is_some_and(|chip| chip.dirty) {
                    theme::warn().add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            ));
            (notes_x, notes_x + width)
        });

        let left_line = ratatui::text::Line::from(left);
        let left_w = left_line.width() as u16;

        // Session hint shares the row: degrade into what's left of the chips.
        let (right, right_w) = if m.hint_bold_keys && !hints.is_empty() {
            let budget = area.width.saturating_sub(left_w).saturating_sub(1) as usize;
            let line = ratatui::text::Line::from(styled_hint_spans(hints, budget));
            let w = line.width() as u16;
            (line, w)
        } else {
            (right, right_w)
        };

        if let Some(sink) = self.chip_sink {
            let model_x = area.x + 2;
            let model_end = model_x + llm_label_w;
            let effort_x = model_end + 3;
            let effort_end = effort_x + m.effort_label.chars().count() as u16;
            let (notes_x, notes_end) = notes_range.unwrap_or((0, 0));
            *sink.borrow_mut() = Some([
                (model_x, model_end),
                (effort_x, effort_end),
                (notes_x, notes_end),
            ]);
        }

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
        ]);
        if m.prompt_tokens > 0 || m.completion_tokens > 0 || m.prompt_cache_reads > 0 {
            right.extend([
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
        }
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

    #[test]
    fn compact_token_count_uses_k_and_m() {
        assert_eq!(compact_token_count(0), "0");
        assert_eq!(compact_token_count(999), "999");
        assert_eq!(compact_token_count(1_000), "1k");
        assert_eq!(compact_token_count(12_400), "12.4k");
        assert_eq!(compact_token_count(1_200_000), "1.2M");
    }
}
