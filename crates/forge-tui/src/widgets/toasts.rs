//! Toast overlay backed by [`ratatui_toaster`](https://crates.io/crates/ratatui-toaster).
//!
//! The home-grown [`ToastState`](crate::app::ToastState) only ever mirrored
//! its text into the feedback strip — nothing painted it as an overlay, so a
//! success notice and a chat error looked identical: one persistent line.
//! This stack renders the same push as a positioned, auto-expiring toast on
//! top of everything else, while the feedback strip keeps its persistent
//! latest-status role. Toasts are notification, not interaction: they never
//! take focus and never block input.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui_toaster::{
    ToastBuilder, ToastConstraint, ToastEngine, ToastEngineBuilder, ToastPosition, ToastType,
};

use crate::widgets::feedback::FeedbackSeverity;

/// How long a toast stays on screen. Matches the previous `tick_toast`
/// 2-second expiry so existing timing expectations survive the move from
/// strip-mirror to overlay.
pub const TOAST_TIMEOUT: Duration = Duration::from_secs(2);

/// Map forge severity onto the toast engine's types. One-to-one: both
/// vocabularies have info/warn/error plus an ok/success state.
pub fn toast_type_for(severity: FeedbackSeverity) -> ToastType {
    match severity {
        FeedbackSeverity::Info => ToastType::Info,
        FeedbackSeverity::Warn => ToastType::Warning,
        FeedbackSeverity::Error => ToastType::Error,
        FeedbackSeverity::Ok => ToastType::Success,
    }
}

/// Auto-expiring single-toast overlay. The engine holds at most one current
/// toast, which matches the old single-slot behaviour — a new push replaces
/// the old one instead of stacking clutter.
pub struct ToastStack {
    engine: ToastEngine<()>,
    shown_at: Option<Instant>,
    timeout: Duration,
}

impl Default for ToastStack {
    fn default() -> Self {
        Self {
            engine: ToastEngineBuilder::new(Rect::default())
                .default_duration(TOAST_TIMEOUT)
                .build(),
            shown_at: None,
            timeout: TOAST_TIMEOUT,
        }
    }
}

impl ToastStack {
    /// Short timeout for tests that must observe expiry without sleeping.
    #[cfg(test)]
    fn with_timeout(timeout: Duration) -> Self {
        Self {
            engine: ToastEngineBuilder::new(Rect::new(0, 0, 80, 24))
                .default_duration(timeout)
                .build(),
            shown_at: None,
            timeout,
        }
    }

    /// Show a toast, replacing any currently visible one.
    pub fn push(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.engine.show_toast(
            ToastBuilder::new(Cow::Owned(text))
                .toast_type(toast_type_for(severity))
                .position(ToastPosition::BottomRight)
                .constraint(ToastConstraint::Auto),
        );
        self.shown_at = Some(Instant::now());
    }

    /// Hide the current toast immediately.
    pub fn clear(&mut self) {
        self.engine.hide_toast();
        self.shown_at = None;
    }

    /// Expire the toast once it outlives its timeout. Called from the
    /// event-loop tick — never from `draw`, which must not mutate state.
    pub fn tick(&mut self) {
        if self
            .shown_at
            .is_some_and(|shown_at| shown_at.elapsed() >= self.timeout)
        {
            self.clear();
        }
    }

    pub fn has_toast(&self) -> bool {
        self.engine.has_toast()
    }

    /// Paint the overlay. Refreshes the engine area every frame so a resize
    /// between push and paint still positions the toast correctly.
    pub fn render_overlay(&mut self, frame_area: Rect, buf: &mut Buffer) {
        if !self.has_toast() {
            return;
        }
        self.engine.set_area(frame_area);
        (&self.engine).render(frame_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_one_to_one() {
        assert!(matches!(
            toast_type_for(FeedbackSeverity::Info),
            ToastType::Info
        ));
        assert!(matches!(
            toast_type_for(FeedbackSeverity::Warn),
            ToastType::Warning
        ));
        assert!(matches!(
            toast_type_for(FeedbackSeverity::Error),
            ToastType::Error
        ));
        assert!(matches!(
            toast_type_for(FeedbackSeverity::Ok),
            ToastType::Success
        ));
    }

    #[test]
    fn empty_text_pushes_nothing() {
        let mut stack = ToastStack::default();
        stack.push(FeedbackSeverity::Info, "   ");
        assert!(!stack.has_toast());
    }

    #[test]
    fn push_replaces_and_clear_hides() {
        let mut stack = ToastStack::default();
        stack.push(FeedbackSeverity::Info, "first");
        assert!(stack.has_toast());
        stack.push(FeedbackSeverity::Error, "second");
        assert!(stack.has_toast());
        stack.clear();
        assert!(!stack.has_toast());
    }

    #[test]
    fn zero_timeout_expires_on_tick() {
        let mut stack = ToastStack::with_timeout(Duration::ZERO);
        stack.push(FeedbackSeverity::Ok, "saved");
        stack.tick();
        assert!(!stack.has_toast());
    }

    #[test]
    fn fresh_toast_survives_tick() {
        let mut stack = ToastStack::default();
        stack.push(FeedbackSeverity::Ok, "saved");
        stack.tick();
        assert!(stack.has_toast());
    }

    #[test]
    fn overlay_paints_over_a_filled_buffer() {
        let mut stack = ToastStack::with_timeout(Duration::from_secs(60));
        stack.push(FeedbackSeverity::Error, "boom");
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        for x in 0..area.width {
            for y in 0..area.height {
                buf[(x, y)].set_symbol("·");
            }
        }
        stack.render_overlay(area, &mut buf);
        // The toast draws a bordered box: at least one cell must differ
        // from the fill, and the message text must appear.
        let cells = &buf;
        let flat: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| cells[(x, y)].symbol().to_string()))
            .collect();
        assert!(flat.contains("boom"), "toast text should render");
        assert!(
            flat.chars().any(|c| c != '·'),
            "toast should paint over the fill"
        );
    }

    #[test]
    fn no_toast_leaves_buffer_untouched() {
        let mut stack = ToastStack::default();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let before = buf.clone();
        stack.render_overlay(area, &mut buf);
        assert_eq!(format!("{buf:?}"), format!("{before:?}"));
    }
}
