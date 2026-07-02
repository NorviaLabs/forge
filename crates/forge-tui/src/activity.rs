//! Activity feed ring buffer (Phase 10 / TUI-10).

use crate::widgets::FeedbackSeverity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Model,
    Tool,
    Connect,
    Slash,
    System,
    Error,
    Hitl,
    Context,
}

#[derive(Debug, Clone)]
pub struct ActivityItem {
    pub kind: ActivityKind,
    pub summary: String,
    pub severity: FeedbackSeverity,
}

#[derive(Debug, Clone)]
pub struct ActivityFeed {
    items: Vec<ActivityItem>,
    max: usize,
}

impl Default for ActivityFeed {
    fn default() -> Self {
        Self::with_capacity(50)
    }
}

impl ActivityFeed {
    pub fn with_capacity(max: usize) -> Self {
        Self {
            items: Vec::new(),
            max: max.max(1),
        }
    }

    pub fn push(&mut self, kind: ActivityKind, severity: FeedbackSeverity, summary: impl Into<String>) {
        let summary = summary.into();
        // Redact obvious secret-like tokens
        let summary = if summary.to_ascii_lowercase().contains("api_key")
            || summary.contains("sk-")
            || summary.contains("Bearer ")
        {
            "[redacted activity]".into()
        } else {
            summary
        };
        self.items.push(ActivityItem {
            kind,
            summary,
            severity,
        });
        while self.items.len() > self.max {
            self.items.remove(0);
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest last; returns last `n` items.
    pub fn recent(&self, n: usize) -> &[ActivityItem] {
        let start = self.items.len().saturating_sub(n);
        &self.items[start..]
    }

    pub fn all(&self) -> &[ActivityItem] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_caps() {
        let mut f = ActivityFeed::with_capacity(5);
        for i in 0..12 {
            f.push(ActivityKind::System, FeedbackSeverity::Info, format!("e{i}"));
        }
        assert_eq!(f.len(), 5);
        assert_eq!(f.recent(5)[0].summary, "e7");
        assert_eq!(f.recent(5)[4].summary, "e11");
    }

    #[test]
    fn redacts_api_keyish() {
        let mut f = ActivityFeed::default();
        f.push(
            ActivityKind::Error,
            FeedbackSeverity::Error,
            "failed api_key=secret",
        );
        assert!(f.recent(1)[0].summary.contains("redacted"));
    }

    #[test]
    fn busy_phase_label_format() {
        use crate::widgets::BusyPhase;
        assert_eq!(BusyPhase::Model.label(), "running · model");
        assert_eq!(
            BusyPhase::Tool {
                name: "web_search".into()
            }
            .label(),
            "running · tool:web_search"
        );
    }
}
