use crate::metrics::MetricsSnapshot;
use crate::span::SpanRecord;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: Option<String>,
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

pub fn export_jsonl(
    path: &Path,
    spans: &[SpanRecord],
    metrics: &MetricsSnapshot,
) -> std::io::Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let s = serde_json::to_string(&json!({
        "spans": spans,
        "metrics": metrics,
    }))
    .map_err(|e| std::io::Error::other(e))?;
    writeln!(f, "{s}")?;
    Ok(1 + spans.len())
}
