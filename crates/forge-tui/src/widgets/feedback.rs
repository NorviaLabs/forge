//! Feedback strip (Phase 10 / TUI-08) — always-visible latest status/error.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedbackSeverity {
    #[default]
    Info,
    Warn,
    Error,
    Ok,
}

#[derive(Debug, Clone, Default)]
pub struct FeedbackModel {
    pub text: String,
    pub severity: FeedbackSeverity,
}

impl FeedbackModel {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Info,
        }
    }

    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Warn,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Error,
        }
    }

    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Ok,
        }
    }
}

pub struct FeedbackBar<'a> {
    pub model: &'a FeedbackModel,
}

impl Widget for FeedbackBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 || self.model.is_empty() {
            return;
        }
        let (prefix, style) = match self.model.severity {
            FeedbackSeverity::Info => (" ", theme::info()),
            FeedbackSeverity::Warn => ("! ", theme::warn().add_modifier(Modifier::BOLD)),
            FeedbackSeverity::Error => ("! ", theme::danger().add_modifier(Modifier::BOLD)),
            FeedbackSeverity::Ok => ("✓ ", theme::ok()),
        };
        let max = area.width as usize;
        let raw = format!("{prefix}{}", self.model.text);
        let mut shown: String = raw.chars().take(max.saturating_sub(1)).collect();
        if raw.chars().count() > max.saturating_sub(1) && max > 2 {
            shown = format!(
                "{}…",
                raw.chars().take(max.saturating_sub(2)).collect::<String>()
            );
        }
        buf.set_line(
            area.x,
            area.y,
            &Line::from(Span::styled(shown, style)),
            area.width,
        );
    }
}

/// Map raw errors to operator-facing copy (TUI-08).
pub fn classify_operator_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("429") || lower.contains("rate limit") || lower.contains("rate_limit") {
        return "Model error: rate limited (HTTP 429). Wait and retry, or /model.".into();
    }
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("api key")
        || lower.contains("unauthenticated")
        || lower.contains("no credentials")
        || lower.contains("fixture token")
        || lower.contains("fixture-")
    {
        return "Model error: authentication failed. Run /connect xai and finish real OAuth \
(not fixture), or set XAI_API_KEY.".into();
    }
    if lower.contains("eof while parsing") || lower.contains("stdout polluted") {
        return "Model error: worker protocol noise (update forge-litellm-worker). Restart forge."
            .into();
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return "Model error: request timed out. Retry or check network/worker.".into();
    }
    if lower.contains("worker")
        && (lower.contains("unavailable")
            || lower.contains("failed")
            || lower.contains("closed stdout")
            || lower.contains("no module named"))
    {
        return "Model error: LiteLLM worker unavailable. Install with: \
cd workers/forge-litellm-worker && pip install -e .  (then restart forge)".into();
    }
    if lower.contains("closed stdout") {
        return "Model error: LiteLLM worker crashed. Install with: \
cd workers/forge-litellm-worker && pip install -e .  (then restart forge)".into();
    }
    let trimmed: String = raw.chars().take(200).collect();
    if trimmed.is_empty() {
        "Operation failed.".into()
    } else {
        format!("Model error: {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rate_limit() {
        let s = classify_operator_error("upstream returned 429 rate limit exceeded");
        assert!(s.contains("429"));
        assert!(s.contains("rate limited"));
    }

    #[test]
    fn classify_auth() {
        assert!(classify_operator_error("401 unauthorized").contains("authentication"));
    }

    #[test]
    fn empty_feedback() {
        assert!(FeedbackModel::default().is_empty());
        assert!(!FeedbackModel::error("x").is_empty());
    }
}
