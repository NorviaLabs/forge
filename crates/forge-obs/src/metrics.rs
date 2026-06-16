use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    pub session_count: AtomicU64,
    pub tool_errors: AtomicU64,
    pub model_latency_sum_ms: AtomicU64,
    pub model_latency_count: AtomicU64,
    pub tool_latency_sum_ms: AtomicU64,
    pub tool_latency_count: AtomicU64,
    pub journal_latency_sum_ms: AtomicU64,
    pub journal_latency_count: AtomicU64,
    pub context_usage_x1000: AtomicU64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub session_count: u64,
    pub tool_errors: u64,
    pub model_latency_avg_ms: f64,
    pub tool_latency_avg_ms: f64,
    pub journal_latency_avg_ms: f64,
    pub context_usage_ratio: f64,
}

impl Metrics {
    pub fn inc_sessions(&self) {
        self.session_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_tool_errors(&self) {
        self.tool_errors.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_model_latency(&self, ms: u64) {
        self.model_latency_sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.model_latency_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_tool_latency(&self, ms: u64) {
        self.tool_latency_sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.tool_latency_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_journal_latency(&self, ms: u64) {
        self.journal_latency_sum_ms
            .fetch_add(ms, Ordering::Relaxed);
        self.journal_latency_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn set_context_usage(&self, ratio: f64) {
        self.context_usage_x1000
            .store((ratio * 1000.0) as u64, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let mc = self.model_latency_count.load(Ordering::Relaxed).max(1);
        let tc = self.tool_latency_count.load(Ordering::Relaxed).max(1);
        let jc = self.journal_latency_count.load(Ordering::Relaxed).max(1);
        MetricsSnapshot {
            session_count: self.session_count.load(Ordering::Relaxed),
            tool_errors: self.tool_errors.load(Ordering::Relaxed),
            model_latency_avg_ms: self.model_latency_sum_ms.load(Ordering::Relaxed) as f64
                / mc as f64,
            tool_latency_avg_ms: self.tool_latency_sum_ms.load(Ordering::Relaxed) as f64
                / tc as f64,
            journal_latency_avg_ms: self.journal_latency_sum_ms.load(Ordering::Relaxed) as f64
                / jc as f64,
            context_usage_ratio: self.context_usage_x1000.load(Ordering::Relaxed) as f64 / 1000.0,
        }
    }
}
