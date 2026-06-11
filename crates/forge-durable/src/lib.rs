//! Durable execution journal (durable-execution.md) — DUR-01, DUR-02.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use forge_types::{
    JournalEvent, JournalEventType, SessionId, SessionStatus, ToolCall, ToolOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("sql error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Journal {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPayload {
    pub call_id: String,
    pub name: String,
    pub output: ToolOutput,
}

#[derive(Debug, Clone)]
pub struct ReplayState {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub last_seq: u64,
    /// call_id -> tool result for completed tools
    pub tool_results: HashMap<String, ToolResultPayload>,
    /// tool intents without results (fail-safe)
    pub incomplete_intents: Vec<String>,
    pub user_messages: Vec<String>,
    pub events: Vec<JournalEvent>,
}

impl Journal {
    pub async fn open(dir: &Path, session_id: SessionId) -> Result<Self, JournalError> {
        std::fs::create_dir_all(dir)?;
        let db_path = dir.join(format!("{session_id}.db"));
        let opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let j = Self { pool };
        j.migrate().await?;
        Ok(j)
    }

    async fn migrate(&self) -> Result<(), JournalError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                ts TEXT NOT NULL,
                event_type TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL,
                trace_id TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn last_seq(&self) -> Result<u64, JournalError> {
        let row = sqlx::query("SELECT COALESCE(MAX(seq), 0) as m FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("m") as u64)
    }

    /// Append event. Payload is stored as JSON. Returns assigned seq.
    /// **Record-before-side-effect:** callers must await this before tool/model side effects.
    pub async fn append(
        &self,
        session_id: SessionId,
        event_type: JournalEventType,
        payload: Value,
    ) -> Result<u64, JournalError> {
        let ts = Utc::now().to_rfc3339();
        let et = serde_json::to_string(&event_type)?;
        let et = et.trim_matches('"').to_string();
        let payload_s = serde_json::to_string(&payload)?;
        let sid = session_id.to_string();
        let res = sqlx::query(
            r#"
            INSERT INTO events (session_id, ts, event_type, schema_version, payload)
            VALUES (?, ?, ?, 1, ?)
            "#,
        )
        .bind(&sid)
        .bind(&ts)
        .bind(&et)
        .bind(&payload_s)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid() as u64)
    }

    pub async fn append_session_created(&self, session_id: SessionId) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::SessionCreated,
            json!({ "session_id": session_id }),
        )
        .await
    }

    pub async fn append_user_message(
        &self,
        session_id: SessionId,
        text: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::UserMessage,
            json!({ "content": text }),
        )
        .await
    }

    pub async fn append_tool_intent(
        &self,
        session_id: SessionId,
        call: &ToolCall,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::ToolIntent,
            json!({
                "call_id": call.id,
                "name": call.name,
                "arguments": call.arguments,
            }),
        )
        .await
    }

    pub async fn append_tool_result(
        &self,
        session_id: SessionId,
        call: &ToolCall,
        output: &ToolOutput,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::ToolResult,
            json!({
                "call_id": call.id,
                "name": call.name,
                "output": output,
            }),
        )
        .await
    }

    pub async fn append_validation_failed(
        &self,
        session_id: SessionId,
        tool: &str,
        message: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::ToolValidationFailed,
            json!({ "tool": tool, "message": message }),
        )
        .await
    }

    pub async fn append_model_request(
        &self,
        session_id: SessionId,
        meta: Value,
    ) -> Result<u64, JournalError> {
        self.append(session_id, JournalEventType::ModelRequest, meta)
            .await
    }

    pub async fn append_model_response(
        &self,
        session_id: SessionId,
        meta: Value,
    ) -> Result<u64, JournalError> {
        self.append(session_id, JournalEventType::ModelResponse, meta)
            .await
    }

    pub async fn append_status(
        &self,
        session_id: SessionId,
        status: SessionStatus,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::SessionStatus,
            json!({ "status": status }),
        )
        .await
    }

    pub async fn replay(&self, session_id: SessionId) -> Result<ReplayState, JournalError> {
        let sid = session_id.to_string();
        let rows = sqlx::query(
            r#"
            SELECT seq, session_id, ts, event_type, schema_version, payload, trace_id
            FROM events WHERE session_id = ? ORDER BY seq ASC
            "#,
        )
        .bind(&sid)
        .fetch_all(&self.pool)
        .await?;

        let mut state = ReplayState {
            session_id,
            status: SessionStatus::Running,
            last_seq: 0,
            tool_results: HashMap::new(),
            incomplete_intents: Vec::new(),
            user_messages: Vec::new(),
            events: Vec::new(),
        };

        let mut open_intents: HashMap<String, String> = HashMap::new();

        for row in rows {
            let seq = row.get::<i64, _>("seq") as u64;
            state.last_seq = seq;
            let et_s: String = row.get("event_type");
            let payload_s: String = row.get("payload");
            let payload: Value = serde_json::from_str(&payload_s)?;
            let event_type: JournalEventType = serde_json::from_str(&format!("\"{et_s}\""))
                .unwrap_or(JournalEventType::StatePatch);
            let ts_s: String = row.get("ts");
            let ts = chrono::DateTime::parse_from_rfc3339(&ts_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            let ev = JournalEvent {
                seq,
                session_id,
                ts,
                event_type,
                schema_version: row.get::<i64, _>("schema_version") as u32,
                payload: payload.clone(),
                trace_id: row.try_get("trace_id").ok(),
            };

            match event_type {
                JournalEventType::UserMessage => {
                    if let Some(c) = payload.get("content").and_then(|v| v.as_str()) {
                        state.user_messages.push(c.to_string());
                    }
                }
                JournalEventType::ToolIntent => {
                    if let Some(id) = payload.get("call_id").and_then(|v| v.as_str()) {
                        open_intents.insert(id.to_string(), id.to_string());
                    }
                }
                JournalEventType::ToolResult => {
                    if let Ok(p) = serde_json::from_value::<ToolResultPayload>(payload.clone()) {
                        open_intents.remove(&p.call_id);
                        state.tool_results.insert(p.call_id.clone(), p);
                    }
                }
                JournalEventType::SessionStatus => {
                    if let Ok(s) = serde_json::from_value::<SessionStatus>(
                        payload.get("status").cloned().unwrap_or(Value::Null),
                    ) {
                        state.status = s;
                    }
                }
                _ => {}
            }

            state.events.push(ev);
        }

        state.incomplete_intents = open_intents.into_keys().collect();
        Ok(state)
    }

    /// Cached result if tool already completed (DUR-02).
    pub fn cached_tool_result<'a>(
        state: &'a ReplayState,
        call_id: &str,
    ) -> Option<&'a ToolResultPayload> {
        state.tool_results.get(call_id)
    }
}

pub fn new_session_id() -> SessionId {
    Uuid::new_v4()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::ToolCall;
    use tempfile::tempdir;

    #[tokio::test]
    async fn record_before_result_and_replay() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_session_created(sid).await.unwrap();
        j.append_user_message(sid, "hi").await.unwrap();

        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a"}),
        };
        j.append_tool_intent(sid, &call).await.unwrap();
        // Simulate crash before result — incomplete intent
        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.incomplete_intents, vec!["c1".to_string()]);
        assert!(Journal::cached_tool_result(&state, "c1").is_none());

        j.append_tool_result(
            sid,
            &call,
            &ToolOutput {
                content: "ok".into(),
                is_error: false,
            },
        )
        .await
        .unwrap();

        let state2 = j.replay(sid).await.unwrap();
        assert!(state2.incomplete_intents.is_empty());
        let cached = Journal::cached_tool_result(&state2, "c1").unwrap();
        assert_eq!(cached.output.content, "ok");
        assert_eq!(state2.user_messages, vec!["hi".to_string()]);
    }

    #[tokio::test]
    async fn append_is_ordered() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        let a = j.append_user_message(sid, "1").await.unwrap();
        let b = j.append_user_message(sid, "2").await.unwrap();
        assert!(b > a);
        assert_eq!(j.last_seq().await.unwrap(), b);
    }
}
