use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Session,
    Turn,
    ModelComplete,
    ToolExecute,
    ContextReset,
    ContextOffload,
    HitlWait,
    JournalAppend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Ok,
    Error,
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub kind: SpanKind,
    pub name: String,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub status: SpanStatus,
    pub attributes: HashMap<String, String>,
}

pub struct Tracer {
    enabled: bool,
    // span_id -> record
    spans: Mutex<HashMap<String, SpanRecord>>,
    // span_id -> trace_id for parent lookup
    traces: Mutex<HashMap<String, String>>,
}

impl Tracer {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            spans: Mutex::new(HashMap::new()),
            traces: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        kind: SpanKind,
        parent_span_id: Option<&str>,
        attrs: &[(&str, &str)],
    ) -> String {
        if !self.enabled {
            return String::new();
        }
        let span_id = Uuid::new_v4().to_string();
        let trace_id = {
            let traces = self.traces.lock().unwrap();
            if let Some(p) = parent_span_id {
                traces
                    .get(p)
                    .cloned()
                    .unwrap_or_else(|| Uuid::new_v4().to_string())
            } else {
                Uuid::new_v4().to_string()
            }
        };
        {
            let mut traces = self.traces.lock().unwrap();
            traces.insert(span_id.clone(), trace_id.clone());
        }
        let mut attributes = HashMap::new();
        for (k, v) in attrs {
            attributes.insert((*k).into(), (*v).into());
        }
        let name = match kind {
            SpanKind::Session => "session",
            SpanKind::Turn => "turn",
            SpanKind::ModelComplete => "model.complete",
            SpanKind::ToolExecute => "tool.execute",
            SpanKind::ContextReset => "context.reset",
            SpanKind::ContextOffload => "context.offload",
            SpanKind::HitlWait => "hitl.wait",
            SpanKind::JournalAppend => "journal.append",
        };
        let rec = SpanRecord {
            trace_id,
            span_id: span_id.clone(),
            parent_span_id: parent_span_id.map(|s| s.to_string()),
            kind,
            name: name.into(),
            start: Utc::now(),
            end: None,
            status: SpanStatus::Unset,
            attributes,
        };
        self.spans.lock().unwrap().insert(span_id.clone(), rec);
        span_id
    }

    pub fn end(&self, span_id: &str, status: SpanStatus, attrs: &[(&str, &str)]) {
        if !self.enabled || span_id.is_empty() {
            return;
        }
        let mut spans = self.spans.lock().unwrap();
        if let Some(s) = spans.get_mut(span_id) {
            s.end = Some(Utc::now());
            s.status = status;
            for (k, v) in attrs {
                s.attributes.insert((*k).into(), (*v).into());
            }
        }
    }

    pub fn snapshot(&self) -> Vec<SpanRecord> {
        self.spans.lock().unwrap().values().cloned().collect()
    }
}
