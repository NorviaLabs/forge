//! Footer strip: steady-state control chips + contextual hints on one row.

use crate::theme;
use crate::widgets::input::{fit_composer_chips, ComposerChip, ComposerChipKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

#[derive(Debug, Clone, Default)]
pub struct FooterModel {
    /// Contextual action hints, right-aligned; empty when none.
    pub hints: String,
    /// Steady-state control chips (mode / connect / model / effort).
    pub chips: Vec<ComposerChip>,
    /// When set, that chip index is focused (`F3`).
    pub chip_focus: Option<usize>,
    /// HITL pending — dim chips, don't look interactive.
    pub chips_dimmed: bool,
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

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        let hints = self.model.hints.trim_end();
        let hint_w = hints.chars().count() as u16;
        // Chips fill the row left of the right-aligned hints.
        let chips_width = area.width.saturating_sub(hint_w);
        let chips = fit_composer_chips(self.model.chips.clone(), chips_width);
        let focus = self
            .model
            .chip_focus
            .map(|idx| idx.min(chips.len().saturating_sub(1)));
        let mut x = area.x;
        for (i, chip) in chips.iter().enumerate() {
            let label = format!("[{}]", chip.label);
            let w = label.chars().count() as u16;
            if x >= area.x + chips_width {
                break;
            }
            let draw_w = w.min(area.x + chips_width - x);
            let focused = focus == Some(i);
            let style = if self.model.chips_dimmed {
                theme::dim()
            } else if focused {
                theme::focused_selection_style()
            } else if chip.kind == ComposerChipKind::Connect
                && chip.label.eq_ignore_ascii_case("not connected")
            {
                theme::warn()
            } else {
                theme::muted()
            };
            buf.set_stringn(x, area.y, &label, draw_w as usize, style);
            x = x.saturating_add(draw_w);
            if i + 1 < chips.len() && x < area.x + chips_width {
                buf.set_stringn(x, area.y, " ", 1, theme::dim());
                x = x.saturating_add(1);
            }
        }
        if hint_w > 0 {
            buf.set_stringn(
                area.x + area.width - hint_w,
                area.y,
                hints,
                hint_w as usize,
                theme::muted(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::input::ComposerChipKind;

    fn chip(kind: ComposerChipKind, label: &str) -> ComposerChip {
        ComposerChip {
            kind,
            label: label.to_string(),
        }
    }

    fn rendered(model: &FooterModel, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model }.render(area, &mut buf);
        (0..area.width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    #[test]
    fn renders_chips_as_brackets() {
        let model = FooterModel {
            chips: vec![
                chip(ComposerChipKind::Mode, "build"),
                chip(ComposerChipKind::Model, "claude-sonnet-4"),
            ],
            ..Default::default()
        };
        let out = rendered(&model, 40);
        assert!(out.contains("[build]"), "{out:?}");
        assert!(out.contains("[claude-sonnet-4]"), "{out:?}");
    }

    #[test]
    fn renders_nothing_when_no_chips_or_hints() {
        let model = FooterModel::default();
        let out = rendered(&model, 40);
        assert!(out.trim().is_empty(), "{out:?}");
    }

    #[test]
    fn hints_are_right_aligned_after_chips() {
        let model = FooterModel {
            hints: "Enter confirm · Esc cancel".into(),
            chips: vec![chip(ComposerChipKind::Mode, "build")],
            ..Default::default()
        };
        let out = rendered(&model, 60);
        let trimmed = out.trim_end();
        assert!(trimmed.ends_with("Enter confirm · Esc cancel"), "{out:?}");
        assert!(trimmed.starts_with("[build]"), "{out:?}");
    }

    #[test]
    fn focused_chip_gets_selection_style() {
        let model = FooterModel {
            chips: vec![
                chip(ComposerChipKind::Mode, "build"),
                chip(ComposerChipKind::Model, "claude-sonnet-4"),
            ],
            chip_focus: Some(1),
            ..Default::default()
        };
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        FooterBar { model: &model }.render(area, &mut buf);
        let focus_style = crate::theme::focused_selection_style();
        // Second bracket row is the focused model chip.
        assert_eq!(
            buf[(12, 0)].style().fg,
            focus_style.fg,
            "focused chip should be highlighted"
        );
        assert_ne!(
            buf[(1, 0)].style().fg,
            focus_style.fg,
            "unfocused chip should not be highlighted"
        );
    }

    #[test]
    fn hints_take_priority_over_chips_on_tight_width() {
        let model = FooterModel {
            hints: "Enter confirm · Esc cancel".into(),
            chips: vec![
                chip(ComposerChipKind::Mode, "build"),
                chip(ComposerChipKind::Model, "claude-sonnet-4-6"),
            ],
            ..Default::default()
        };
        let out = rendered(&model, 30);
        assert!(
            out.trim_end().ends_with("Enter confirm · Esc cancel"),
            "{out:?}"
        );
    }
}
