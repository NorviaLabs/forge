use crate::metrics::MetricsSnapshot;
use crate::span::SpanRecord;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Optional OTLP HTTP endpoint (export left as config for deploy; JSONL used in-process).
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Write spans/metrics as JSON lines (OTEL-compatible field names).
    #[serde(default)]
    pub export_path: Option<std::path::PathBuf>,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: std::env::var("FORGE_OTEL_ENDPOINT")
                .ok()
                .filter(|s| !s.is_empty()),
            export_path: None,
        }
    }
}

#[derive(Serialize)]
struct ExportLine<'a> {
    resource_spans: bool,
    spans: &'a [SpanRecord],
    metrics: &'a MetricsSnapshot,
}

pub fn export_jsonl(
    path: &Path,
    spans: &[SpanRecord],
    metrics: &MetricsSnapshot,
) -> std::io::Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = ExportLine {
        resource_spans: true,
        spans,
        metrics,
    };
    let s = serde_json::to_string(&line).map_err(|e| std::io::Error::other(e))?;
    writeln!(f, "{s}")?;
    Ok(1 + spans.len())
}
