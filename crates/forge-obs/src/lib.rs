//! Observability (observability.md) — OBS-01. Phase 3 only.
//!
//! In-process OTEL-compatible span/metric recorder with optional JSONL export.
//! When disabled, APIs are zero-cost no-ops for the caller.

mod export;
mod metrics;
mod redact;
mod span;

pub use export::{export_jsonl, OtelConfig};
pub use metrics::Metrics;
pub use redact::Redactor;
pub use span::{SpanKind, SpanRecord, SpanStatus, Tracer};

use std::sync::Arc;

/// Initialize observability from config. Safe to call with enabled=false.
pub fn init(config: &OtelConfig) -> ObsHandle {
    ObsHandle {
        enabled: config.enabled,
        config: config.clone(),
        tracer: Arc::new(Tracer::new(config.enabled)),
        metrics: Arc::new(Metrics::default()),
        redactor: Redactor::default(),
    }
}

#[derive(Clone)]
pub struct ObsHandle {
    pub enabled: bool,
    pub config: OtelConfig,
    pub tracer: Arc<Tracer>,
    pub metrics: Arc<Metrics>,
    pub redactor: Redactor,
}

impl ObsHandle {
    pub fn noop() -> Self {
        init(&OtelConfig {
            enabled: false,
            ..Default::default()
        })
    }

    pub fn start_session(&self, session_id: &str, surface: &str, model: &str) -> String {
        if !self.enabled {
            return String::new();
        }
        self.metrics.inc_sessions();
        self.tracer.start(
            SpanKind::Session,
            None,
            &[
                ("session_id", session_id),
                ("surface", surface),
                ("model", model),
            ],
        )
    }

    pub fn start_turn(&self, parent: &str, turn_index: u32) -> String {
        if !self.enabled {
            return String::new();
        }
        self.tracer.start(
            SpanKind::Turn,
            Some(parent),
            &[("turn_index", &turn_index.to_string())],
        )
    }

    pub fn start_model(&self, parent: &str, provider: &str, model: &str) -> String {
        if !self.enabled {
            return String::new();
        }
        self.tracer.start(
            SpanKind::ModelComplete,
            Some(parent),
            &[("provider", provider), ("model", model)],
        )
    }

    pub fn end_model(&self, span_id: &str, latency_ms: u64, in_tok: u32, out_tok: u32) {
        if !self.enabled {
            return;
        }
        self.metrics.record_model_latency(latency_ms);
        self.tracer.end(
            span_id,
            SpanStatus::Ok,
            &[
                ("input_tokens", &in_tok.to_string()),
                ("output_tokens", &out_tok.to_string()),
                ("latency_ms", &latency_ms.to_string()),
            ],
        );
    }

    pub fn start_tool(&self, parent: &str, tool: &str, decision: &str) -> String {
        if !self.enabled {
            return String::new();
        }
        self.tracer.start(
            SpanKind::ToolExecute,
            Some(parent),
            &[("tool_name", tool), ("decision", decision)],
        )
    }

    pub fn end_tool(&self, span_id: &str, latency_ms: u64, error: bool) {
        if !self.enabled {
            return;
        }
        self.metrics.record_tool_latency(latency_ms);
        if error {
            self.metrics.inc_tool_errors();
        }
        self.tracer.end(
            span_id,
            if error {
                SpanStatus::Error
            } else {
                SpanStatus::Ok
            },
            &[("latency_ms", &latency_ms.to_string())],
        );
    }

    pub fn record_context_reset(&self, session_span: &str, before: f64, after: f64) {
        if !self.enabled {
            return;
        }
        let id = self.tracer.start(
            SpanKind::ContextReset,
            Some(session_span),
            &[
                ("usage_before", &format!("{before:.4}")),
                ("usage_after", &format!("{after:.4}")),
            ],
        );
        self.tracer.end(&id, SpanStatus::Ok, &[]);
        self.metrics.set_context_usage(after);
    }

    pub fn record_hitl_wait(&self, turn_span: &str, tool: &str) {
        if !self.enabled {
            return;
        }
        let id = self
            .tracer
            .start(SpanKind::HitlWait, Some(turn_span), &[("tool_name", tool)]);
        self.tracer.end(&id, SpanStatus::Ok, &[]);
    }

    pub fn snapshot_spans(&self) -> Vec<SpanRecord> {
        self.tracer.snapshot()
    }

    pub fn export_jsonl_file(&self, path: &std::path::Path) -> std::io::Result<usize> {
        export_jsonl(path, &self.tracer.snapshot(), &self.metrics.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn noop_when_disabled() {
        let obs = ObsHandle::noop();
        let id = obs.start_session("s", "tui", "m");
        assert!(id.is_empty());
        assert!(obs.snapshot_spans().is_empty());
    }

    #[test]
    fn spans_cover_model_tool_step() {
        let obs = init(&OtelConfig {
            enabled: true,
            endpoint: None,
            export_path: None,
        });
        let sess = obs.start_session("sess-1", "cli", "mock");
        let turn = obs.start_turn(&sess, 0);
        let model = obs.start_model(&turn, "mock", "mock");
        obs.end_model(&model, 12, 10, 5);
        let tool = obs.start_tool(&turn, "read_file", "allow");
        obs.end_tool(&tool, 3, false);
        obs.record_context_reset(&sess, 0.9, 0.1);
        obs.record_hitl_wait(&turn, "bash");

        let spans = obs.snapshot_spans();
        let kinds: Vec<_> = spans.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&SpanKind::Session));
        assert!(kinds.contains(&SpanKind::Turn));
        assert!(kinds.contains(&SpanKind::ModelComplete));
        assert!(kinds.contains(&SpanKind::ToolExecute));
        assert!(kinds.contains(&SpanKind::ContextReset));
        assert!(kinds.contains(&SpanKind::HitlWait));
        assert!(obs.metrics.snapshot().session_count >= 1);
    }

    #[test]
    fn redactor_strips_secrets() {
        let r = Redactor::default();
        let v = r.redact_value(
            "body",
            &json!({"api_key": "sk-x", "path": "a", "authorization": "Bearer z"}),
        );
        assert_eq!(v["api_key"], "[REDACTED]");
        assert_eq!(v["authorization"], "[REDACTED]");
        assert_eq!(v["path"], "a");
    }

    #[test]
    fn export_jsonl_writes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("otel.jsonl");
        let obs = init(&OtelConfig {
            enabled: true,
            endpoint: None,
            export_path: Some(path.clone()),
        });
        let s = obs.start_session("s", "cli", "m");
        obs.tracer.end(&s, SpanStatus::Ok, &[]);
        let n = obs.export_jsonl_file(&path).unwrap();
        assert!(n > 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("session"));
    }
}
