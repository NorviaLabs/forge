//! Durable execution journal (durable-execution.md) — DUR-01, DUR-02.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use forge_types::{
    BackgroundTaskId, JournalEvent, JournalEventType, Message, MessageRole, ModelResponse,
    QueueItemId, QueueItemStatus, SessionId, TaskLifecycle, ToolCall, ToolOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
#[non_exhaustive]
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
    db_path: PathBuf,
}

/// Return the most recently modified session journal in `dir`.
///
/// Session journals are one database per session, and SQLite updates the
/// database file whenever a journal event is written. Invalid or unrelated
/// files are ignored so a stale runtime artifact cannot prevent startup.
pub fn latest_session_id(dir: &Path) -> Result<Option<SessionId>, JournalError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut latest: Option<(std::time::SystemTime, SessionId)> = None;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("db") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(session_id) = Uuid::parse_str(stem) else {
            continue;
        };
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        if latest.as_ref().is_none_or(|(time, _)| modified > *time) {
            latest = Some((modified, session_id));
        }
    }
    Ok(latest.map(|(_, session_id)| session_id))
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
    pub status: TaskLifecycle,
    pub last_seq: u64,
    /// call_id -> tool result for completed tools
    pub tool_results: HashMap<String, ToolResultPayload>,
    /// tool intents without results (fail-safe)
    pub incomplete_intents: Vec<String>,
    pub user_messages: Vec<String>,
    /// Ordered active conversation reconstructed from journal events.
    pub messages: Vec<Message>,
    /// Completed model responses, used to restore cumulative usage metrics.
    pub model_responses: Vec<ModelResponse>,
    pub events: Vec<JournalEvent>,
    /// Phase 2: pending HITL payload if status is AwaitingHitl
    pub pending_hitl: Option<serde_json::Value>,
    /// Future-task queue items reconstructed from `Queue*` events. A
    /// `Promoting` item with no later `Promoted`/`Removed` event (a crash
    /// mid-promotion) is left as `Promoting` here — the caller (forge-core's
    /// `TaskQueue::from_restored`) is responsible for reconciling it back to
    /// `Queued`, since that reconciliation policy belongs with the queue's
    /// own invariants, not journal replay.
    pub queue_items: Vec<RestoredQueueItem>,
    /// Background tasks (shell jobs, subagents) reconstructed from
    /// `BackgroundTaskStarted`/`BackgroundTaskFinished` pairs. A task with
    /// `finished: false` was still in flight when the journal ends —
    /// reconciliation policy (mark cancelled vs. auto-resume) belongs to the
    /// caller (`forge-core`'s `background` module), same division of
    /// responsibility as `queue_items`.
    pub background_tasks: Vec<RestoredBackgroundTask>,
    /// `BackgroundTaskId` -> worktree path, from `SubagentSpawned` payloads
    /// (`Journal::append_subagent_spawned`'s `workspace` argument) — the
    /// piece `background_tasks` alone can't carry, since a subagent's
    /// worktree location is only known once the worktree is actually
    /// created, after the generic `BackgroundTaskStarted` is journaled.
    pub subagent_workspaces: HashMap<u64, PathBuf>,
}

/// One queue item as reconstructed from the journal — a lightweight mirror
/// of forge-core's `QueuedTask` that doesn't require forge-durable to depend
/// on forge-core (dependency direction is the other way around).
#[derive(Debug, Clone)]
pub struct RestoredQueueItem {
    pub id: QueueItemId,
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub status: QueueItemStatus,
}

/// One background task as reconstructed from the journal — a lightweight
/// mirror of forge-core's `BackgroundTaskHandle` (no `CancellationToken`,
/// which isn't durable and doesn't need to be).
#[derive(Debug, Clone)]
pub struct RestoredBackgroundTask {
    pub id: BackgroundTaskId,
    /// `"shell"` or `"subagent"`, from `BackgroundTaskKind::tag()`.
    pub kind: String,
    pub label: String,
    pub child_session_id: Option<SessionId>,
    pub finished: bool,
}

impl Journal {
    pub fn directory(&self) -> &Path {
        self.db_path.parent().unwrap_or_else(|| Path::new("."))
    }

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
        let j = Self { pool, db_path };
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
        status: TaskLifecycle,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::SessionStatus,
            json!({ "status": status }),
        )
        .await
    }

    /// Phase 2 DUR-03: durable HITL wait (record before releasing compute).
    pub async fn append_hitl_wait(
        &self,
        session_id: SessionId,
        payload: &serde_json::Value,
    ) -> Result<u64, JournalError> {
        self.append(session_id, JournalEventType::HitlWait, payload.clone())
            .await
    }

    pub async fn append_hitl_resume(
        &self,
        session_id: SessionId,
        decision: &str,
        actor: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::HitlResume,
            json!({ "decision": decision, "actor": actor }),
        )
        .await
    }

    pub async fn append_context_reset(
        &self,
        session_id: SessionId,
        meta: Value,
    ) -> Result<u64, JournalError> {
        self.append(session_id, JournalEventType::ContextReset, meta)
            .await
    }

    /// Unified task/queue lifecycle: durable future-task queue events.
    /// **Record-before-side-effect** applies here too — callers must await
    /// each of these before claiming the corresponding queue mutation
    /// succeeded (e.g. before telling the user a message was queued).
    pub async fn append_queue_enqueued(
        &self,
        session_id: SessionId,
        item_id: QueueItemId,
        text: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::QueueEnqueued,
            json!({ "item_id": item_id.0, "text": text }),
        )
        .await
    }

    pub async fn append_queue_promoting(
        &self,
        session_id: SessionId,
        item_id: QueueItemId,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::QueuePromoting,
            json!({ "item_id": item_id.0 }),
        )
        .await
    }

    pub async fn append_queue_promoted(
        &self,
        session_id: SessionId,
        item_id: QueueItemId,
        task_id: u64,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::QueuePromoted,
            json!({ "item_id": item_id.0, "task_id": task_id }),
        )
        .await
    }

    pub async fn append_queue_removed(
        &self,
        session_id: SessionId,
        item_id: QueueItemId,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::QueueRemoved,
            json!({ "item_id": item_id.0 }),
        )
        .await
    }

    /// `kind` is `"shell"` or `"subagent"` — informational only, not used by
    /// replay. `child_session_id` is `Some` for a subagent (its own journal
    /// records the rest), `None` for a shell job.
    pub async fn append_background_task_started(
        &self,
        session_id: SessionId,
        task_id: BackgroundTaskId,
        kind: &str,
        label: &str,
        child_session_id: Option<SessionId>,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::BackgroundTaskStarted,
            json!({
                "task_id": task_id.0,
                "kind": kind,
                "label": label,
                "child_session_id": child_session_id,
            }),
        )
        .await
    }

    pub async fn append_background_task_finished(
        &self,
        session_id: SessionId,
        task_id: BackgroundTaskId,
        status: &str,
        summary: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::BackgroundTaskFinished,
            json!({ "task_id": task_id.0, "status": status, "summary": summary }),
        )
        .await
    }

    /// `workspace` is the subagent's dedicated git worktree path — recorded
    /// so a restart can find the same checkout again (`forge-core`'s
    /// `reconcile_orphaned_background_tasks` reads it back via
    /// `ReplayState::subagent_workspaces` to auto-resume an orphaned
    /// subagent without recomputing/guessing the worktree location).
    pub async fn append_subagent_spawned(
        &self,
        session_id: SessionId,
        task_id: BackgroundTaskId,
        child_session_id: SessionId,
        role: &str,
        workspace: &Path,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::SubagentSpawned,
            json!({
                "task_id": task_id.0,
                "child_session_id": child_session_id,
                "role": role,
                "workspace": workspace.display().to_string(),
            }),
        )
        .await
    }

    pub async fn append_subagent_finished(
        &self,
        session_id: SessionId,
        task_id: BackgroundTaskId,
        child_session_id: SessionId,
        status: &str,
        summary: &str,
    ) -> Result<u64, JournalError> {
        self.append(
            session_id,
            JournalEventType::SubagentFinished,
            json!({
                "task_id": task_id.0,
                "child_session_id": child_session_id,
                "status": status,
                "summary": summary,
            }),
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
            status: TaskLifecycle::Working,
            last_seq: 0,
            tool_results: HashMap::new(),
            incomplete_intents: Vec::new(),
            user_messages: Vec::new(),
            messages: Vec::new(),
            model_responses: Vec::new(),
            events: Vec::new(),
            pending_hitl: None,
            queue_items: Vec::new(),
            background_tasks: Vec::new(),
            subagent_workspaces: HashMap::new(),
        };

        let mut open_intents: HashMap<String, String> = HashMap::new();
        // Tracks a promotion currently "in flight" per the event stream: set
        // on `QueuePromoting`, cleared on the matching `QueuePromoted`/
        // `QueueRemoved`. If a `UserMessage` event lands while a promotion is
        // in flight, the task it belongs to was actually created — a crash
        // before the confirming `QueuePromoted` event must not then re-queue
        // the item (that would create a second task from the same
        // instruction). Only an item with no `UserMessage` following its
        // `QueuePromoting` is safe to revert to `Queued`.
        let mut promoting_item: Option<u64> = None;
        let mut promoted_without_confirmation: std::collections::HashSet<u64> =
            std::collections::HashSet::new();

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
                        state.messages.push(Message::new(MessageRole::User, c));
                    }
                    if let Some(id) = promoting_item {
                        promoted_without_confirmation.insert(id);
                    }
                }
                JournalEventType::ModelResponse => {
                    // Older journals only contain response metadata and remain resumable with
                    // partial history; current journals persist the complete response.
                    if let Ok(response) = serde_json::from_value::<ModelResponse>(payload.clone()) {
                        let has_thinking = response
                            .thinking
                            .as_ref()
                            .is_some_and(|thinking| !thinking.trim().is_empty());
                        if !response.text.is_empty()
                            || has_thinking
                            || !response.tool_calls.is_empty()
                        {
                            state.messages.push(Message {
                                outcome: Default::default(),
                                role: MessageRole::Assistant,
                                content: response.text.clone(),
                                tool_call_id: None,
                                name: None,
                                thinking: response
                                    .thinking
                                    .clone()
                                    .filter(|thinking| !thinking.trim().is_empty()),
                                thinking_duration_secs: None,
                                tool_calls: response.tool_calls.clone(),
                            });
                        }
                        state.model_responses.push(response);
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
                        state.messages.push(Message {
                            outcome: p.output.effective_outcome(),
                            role: MessageRole::Tool,
                            content: p.output.content.clone(),
                            tool_call_id: Some(p.call_id.clone()),
                            name: Some(p.name.clone()),
                            thinking: None,
                            thinking_duration_secs: None,
                            tool_calls: vec![],
                        });
                        state.tool_results.insert(p.call_id.clone(), p);
                    }
                }
                JournalEventType::SessionStatus => {
                    if let Ok(s) = serde_json::from_value::<TaskLifecycle>(
                        payload.get("status").cloned().unwrap_or(Value::Null),
                    ) {
                        state.status = s;
                    }
                }
                JournalEventType::HitlWait => {
                    state.pending_hitl = Some(payload.clone());
                    state.status = TaskLifecycle::Waiting;
                }
                JournalEventType::HitlResume => {
                    state.pending_hitl = None;
                    if state.status == TaskLifecycle::Waiting {
                        state.status = TaskLifecycle::Working;
                    }
                }
                JournalEventType::ContextReset => {
                    if let Some(messages) = payload.get("messages") {
                        if let Ok(messages) =
                            serde_json::from_value::<Vec<Message>>(messages.clone())
                        {
                            state.messages = messages;
                        }
                    }
                }
                JournalEventType::QueueEnqueued => {
                    if let (Some(id), Some(text)) = (
                        payload.get("item_id").and_then(|v| v.as_u64()),
                        payload.get("text").and_then(|v| v.as_str()),
                    ) {
                        state.queue_items.push(RestoredQueueItem {
                            id: QueueItemId(id),
                            text: text.to_string(),
                            created_at: ts,
                            status: QueueItemStatus::Queued,
                        });
                    }
                }
                JournalEventType::QueuePromoting => {
                    if let Some(id) = payload.get("item_id").and_then(|v| v.as_u64()) {
                        promoting_item = Some(id);
                        if let Some(item) =
                            state.queue_items.iter_mut().find(|item| item.id.0 == id)
                        {
                            item.status = QueueItemStatus::Promoting;
                        }
                    }
                }
                JournalEventType::QueuePromoted => {
                    if let Some(id) = payload.get("item_id").and_then(|v| v.as_u64()) {
                        promoting_item = None;
                        promoted_without_confirmation.remove(&id);
                        if let Some(item) =
                            state.queue_items.iter_mut().find(|item| item.id.0 == id)
                        {
                            item.status = QueueItemStatus::Promoted;
                        }
                    }
                }
                JournalEventType::QueueRemoved => {
                    if let Some(id) = payload.get("item_id").and_then(|v| v.as_u64()) {
                        promoting_item = None;
                        promoted_without_confirmation.remove(&id);
                        if let Some(item) =
                            state.queue_items.iter_mut().find(|item| item.id.0 == id)
                        {
                            item.status = QueueItemStatus::Removed;
                        }
                    }
                }
                JournalEventType::BackgroundTaskStarted => {
                    if let (Some(id), Some(kind), Some(label)) = (
                        payload.get("task_id").and_then(|v| v.as_u64()),
                        payload.get("kind").and_then(|v| v.as_str()),
                        payload.get("label").and_then(|v| v.as_str()),
                    ) {
                        let child_session_id = payload
                            .get("child_session_id")
                            .and_then(|v| serde_json::from_value::<SessionId>(v.clone()).ok());
                        state.background_tasks.push(RestoredBackgroundTask {
                            id: BackgroundTaskId(id),
                            kind: kind.to_string(),
                            label: label.to_string(),
                            child_session_id,
                            finished: false,
                        });
                    }
                }
                JournalEventType::BackgroundTaskFinished => {
                    if let Some(id) = payload.get("task_id").and_then(|v| v.as_u64()) {
                        if let Some(task) = state.background_tasks.iter_mut().find(|t| t.id.0 == id)
                        {
                            task.finished = true;
                        }
                    }
                }
                JournalEventType::SubagentSpawned => {
                    if let (Some(id), Some(workspace)) = (
                        payload.get("task_id").and_then(|v| v.as_u64()),
                        payload.get("workspace").and_then(|v| v.as_str()),
                    ) {
                        state
                            .subagent_workspaces
                            .insert(id, PathBuf::from(workspace));
                    }
                }
                _ => {}
            }

            state.events.push(ev);
        }

        // A `Promoting` item with no confirming `QueuePromoted`/`QueueRemoved`
        // is a crash mid-promotion. If a `UserMessage` was journaled after it
        // started promoting, the task was already created — reconcile
        // forward to `Promoted` so restart can't create a second task from
        // the same instruction. Otherwise it's safe to leave as `Promoting`
        // (the caller, `TaskQueue::from_restored`, reverts that to `Queued`).
        for item in state.queue_items.iter_mut() {
            if item.status == QueueItemStatus::Promoting
                && promoted_without_confirmation.contains(&item.id.0)
            {
                item.status = QueueItemStatus::Promoted;
            }
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
                outcome: Default::default(),
                content: "ok".into(),
                is_error: false,
                exit_code: None,
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
    async fn replay_restores_ordered_conversation_and_reset_context() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let journal = Journal::open(dir.path(), sid).await.unwrap();
        journal
            .append_user_message(sid, "inspect it")
            .await
            .unwrap();
        let response = ModelResponse {
            text: "checking".into(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a"}),
            }],
            usage: Some(forge_types::Usage {
                prompt_tokens: 12,
                completion_tokens: 3,
                ..Default::default()
            }),
            thinking: Some("need the file".into()),
        };
        journal
            .append_model_response(sid, serde_json::to_value(&response).unwrap())
            .await
            .unwrap();
        journal
            .append_tool_result(
                sid,
                &response.tool_calls[0],
                &ToolOutput {
                    outcome: Default::default(),
                    content: "contents".into(),
                    is_error: false,
                    exit_code: None,
                },
            )
            .await
            .unwrap();

        let state = journal.replay(sid).await.unwrap();
        assert_eq!(state.messages.len(), 3);
        assert_eq!(state.messages[0].role, MessageRole::User);
        assert_eq!(state.messages[1].role, MessageRole::Assistant);
        assert_eq!(state.messages[1].tool_calls[0].id, "c1");
        assert_eq!(state.messages[2].role, MessageRole::Tool);
        assert_eq!(
            state.model_responses[0]
                .usage
                .as_ref()
                .unwrap()
                .prompt_tokens,
            12
        );

        let reset_messages = vec![Message::new(MessageRole::User, "handoff context")];
        journal
            .append_context_reset(sid, json!({"messages": reset_messages}))
            .await
            .unwrap();
        let reset_state = journal.replay(sid).await.unwrap();
        assert_eq!(reset_state.messages.len(), 1);
        assert_eq!(reset_state.messages[0].content, "handoff context");
    }

    #[tokio::test]
    async fn hitl_wait_resume_replay() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_session_created(sid).await.unwrap();
        j.append_hitl_wait(sid, &json!({"call_id": "c1", "tool": "bash"}))
            .await
            .unwrap();
        let st = j.replay(sid).await.unwrap();
        assert_eq!(st.status, TaskLifecycle::Waiting);
        assert!(st.pending_hitl.is_some());
        j.append_hitl_resume(sid, "approve", "tui:test")
            .await
            .unwrap();
        let st2 = j.replay(sid).await.unwrap();
        assert_eq!(st2.status, TaskLifecycle::Working);
        assert!(st2.pending_hitl.is_none());
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

    /// `directory()` must return the directory the journal's db file lives in,
    /// not the db file path itself — callers use it to colocate sibling
    /// session artifacts (e.g. offload files) with the journal.
    #[tokio::test]
    async fn directory_returns_the_parent_of_the_db_file() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        assert_eq!(j.directory(), dir.path());
    }

    #[tokio::test]
    async fn append_validation_failed_is_recorded_and_replayed_as_an_event() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_validation_failed(sid, "bash", "missing required argument: command")
            .await
            .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.events.len(), 1);
        let ev = &state.events[0];
        assert_eq!(ev.event_type, JournalEventType::ToolValidationFailed);
        assert_eq!(ev.payload["tool"], "bash");
        assert_eq!(ev.payload["message"], "missing required argument: command");
    }

    #[tokio::test]
    async fn append_model_request_is_recorded_verbatim() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        let meta = json!({"model": "mock-large", "prompt_tokens": 42});
        j.append_model_request(sid, meta.clone()).await.unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].event_type, JournalEventType::ModelRequest);
        assert_eq!(state.events[0].payload, meta);
    }

    /// Default (no events) replays as `Running`; each explicit status must
    /// round-trip to the matching `TaskLifecycle` variant, not just "some
    /// status changed" — an N-arm enum written through one JSON shape, so
    /// every variant gets its own journal to avoid later events masking an
    /// earlier mis-wired arm.
    #[tokio::test]
    async fn append_status_updates_replayed_session_status() {
        for status in [
            TaskLifecycle::Completed,
            TaskLifecycle::Failed,
            TaskLifecycle::Cancelled,
            TaskLifecycle::Interrupted,
        ] {
            let dir = tempdir().unwrap();
            let sid = new_session_id();
            let j = Journal::open(dir.path(), sid).await.unwrap();
            j.append_status(sid, status).await.unwrap();
            let state = j.replay(sid).await.unwrap();
            assert_eq!(
                state.status, status,
                "status did not round-trip through replay"
            );
        }
    }

    #[tokio::test]
    async fn queue_events_round_trip_through_replay() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_queue_enqueued(sid, QueueItemId(1), "do the thing")
            .await
            .unwrap();
        j.append_queue_enqueued(sid, QueueItemId(2), "do another thing")
            .await
            .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items.len(), 2);
        assert_eq!(state.queue_items[0].id, QueueItemId(1));
        assert_eq!(state.queue_items[0].status, QueueItemStatus::Queued);
        assert_eq!(state.queue_items[0].text, "do the thing");

        j.append_queue_promoting(sid, QueueItemId(1)).await.unwrap();
        j.append_queue_promoted(sid, QueueItemId(1), 7)
            .await
            .unwrap();
        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items[0].status, QueueItemStatus::Promoted);
        // Untouched item stays Queued.
        assert_eq!(state.queue_items[1].status, QueueItemStatus::Queued);

        j.append_queue_removed(sid, QueueItemId(2)).await.unwrap();
        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items[1].status, QueueItemStatus::Removed);
    }

    #[tokio::test]
    async fn background_task_events_round_trip_through_replay() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_background_task_started(sid, BackgroundTaskId(1), "shell", "cargo check", None)
            .await
            .unwrap();
        j.append_background_task_started(sid, BackgroundTaskId(2), "shell", "cargo test", None)
            .await
            .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.background_tasks.len(), 2);
        assert_eq!(state.background_tasks[0].id, BackgroundTaskId(1));
        assert_eq!(state.background_tasks[0].kind, "shell");
        assert_eq!(state.background_tasks[0].label, "cargo check");
        assert!(!state.background_tasks[0].finished);
        assert!(!state.background_tasks[1].finished);

        j.append_background_task_finished(sid, BackgroundTaskId(1), "succeeded", "ok")
            .await
            .unwrap();
        let state = j.replay(sid).await.unwrap();
        assert!(state.background_tasks[0].finished);
        // Untouched task stays unfinished — this is exactly the "orphaned on
        // restart" case forge-core's reconciliation acts on.
        assert!(!state.background_tasks[1].finished);
    }

    #[tokio::test]
    async fn subagent_background_task_carries_child_session_id_through_replay() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let child_sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_background_task_started(
            sid,
            BackgroundTaskId(1),
            "subagent",
            "test-fixer",
            Some(child_sid),
        )
        .await
        .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.background_tasks[0].kind, "subagent");
        assert_eq!(state.background_tasks[0].child_session_id, Some(child_sid));
    }

    #[tokio::test]
    async fn subagent_spawned_records_the_worktree_path_for_resume() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let child_sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        let worktree = std::path::Path::new("/repo/.forge/local/worktrees/subagent-1-fixer");
        j.append_subagent_spawned(sid, BackgroundTaskId(1), child_sid, "fixer", worktree)
            .await
            .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(
            state.subagent_workspaces.get(&1),
            Some(&worktree.to_path_buf())
        );
    }

    #[tokio::test]
    async fn queue_item_stuck_in_promoting_with_no_task_created_stays_promoting_through_replay() {
        // A crash between `QueuePromoting` and `append_user_message` means no
        // task was ever created for this item — replay leaves it `Promoting`
        // (not reconciled forward), and `TaskQueue::from_restored` in
        // forge-core is the one that reverts it to `Queued` as safe to retry.
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_queue_enqueued(sid, QueueItemId(1), "stuck")
            .await
            .unwrap();
        j.append_queue_promoting(sid, QueueItemId(1)).await.unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items.len(), 1);
        assert_eq!(state.queue_items[0].status, QueueItemStatus::Promoting);
    }

    #[tokio::test]
    async fn queue_item_promoted_without_confirmation_reconciles_to_promoted_not_queued() {
        // A crash *after* the task was actually created (its UserMessage is
        // journaled) but *before* the confirming `QueuePromoted` event must
        // not re-queue the item — replaying it back to `Queued` here would
        // let a subsequent promotion create a second task from the same
        // instruction. Reconciling forward to `Promoted` prevents that.
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_queue_enqueued(sid, QueueItemId(1), "do the thing")
            .await
            .unwrap();
        j.append_queue_promoting(sid, QueueItemId(1)).await.unwrap();
        // Mirrors what `AgentSession::promote_next_queued` -> `append_user_message`
        // journals next, in the real pipeline — no `QueuePromoted` follows.
        j.append_user_message(sid, "do the thing").await.unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items.len(), 1);
        assert_eq!(state.queue_items[0].status, QueueItemStatus::Promoted);
    }

    #[tokio::test]
    async fn user_message_before_any_promotion_does_not_affect_unrelated_queue_items() {
        // A `UserMessage` journaled with no promotion in flight (the normal,
        // non-queue dispatch path) must not spuriously mark some other,
        // still-`Queued` item as `Promoted`.
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();
        j.append_user_message(sid, "direct dispatch, no queue involved")
            .await
            .unwrap();
        j.append_queue_enqueued(sid, QueueItemId(1), "still waiting")
            .await
            .unwrap();

        let state = j.replay(sid).await.unwrap();
        assert_eq!(state.queue_items.len(), 1);
        assert_eq!(state.queue_items[0].status, QueueItemStatus::Queued);
    }

    /// `ModelResponse::text` may be empty while the turn still produced
    /// content worth keeping in the reconstructed transcript — either
    /// "thinking" text, tool calls, or both. Each shape below is a case in
    /// the same `if` that decides whether to push an assistant `Message`,
    /// so it must be tested with distinct, unambiguous fixtures: one where
    /// only the `has_thinking` disjunct is true, one where only the
    /// tool-calls disjunct is true, and one where the whole response is
    /// empty and no message must be pushed at all.
    #[tokio::test]
    async fn replay_pushes_assistant_message_for_thinking_only_and_tool_calls_only_responses() {
        let dir = tempdir().unwrap();
        let sid = new_session_id();
        let j = Journal::open(dir.path(), sid).await.unwrap();

        let thinking_only = ModelResponse {
            text: String::new(),
            tool_calls: vec![],
            usage: None,
            thinking: Some("mulling it over".into()),
        };
        let tool_calls_only = ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c-tc".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a"}),
            }],
            usage: None,
            thinking: None,
        };
        let fully_empty = ModelResponse {
            text: String::new(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        };
        // Whitespace-only thinking must be treated the same as absent
        // thinking (trimmed empty), not as "has content".
        let whitespace_thinking = ModelResponse {
            text: String::new(),
            tool_calls: vec![],
            usage: None,
            thinking: Some("   \n  ".into()),
        };

        for response in [
            &thinking_only,
            &tool_calls_only,
            &fully_empty,
            &whitespace_thinking,
        ] {
            j.append_model_response(sid, serde_json::to_value(response).unwrap())
                .await
                .unwrap();
        }

        let state = j.replay(sid).await.unwrap();
        // All four ModelResponse events are always recorded for usage-metric
        // bookkeeping, regardless of whether a message was pushed.
        assert_eq!(state.model_responses.len(), 4);
        // Only the thinking-only and tool-calls-only responses push a
        // Message; fully-empty and whitespace-only-thinking do not.
        assert_eq!(state.messages.len(), 2);

        let thinking_message = &state.messages[0];
        assert_eq!(thinking_message.role, MessageRole::Assistant);
        assert_eq!(thinking_message.content, "");
        assert_eq!(
            thinking_message.thinking.as_deref(),
            Some("mulling it over")
        );
        assert!(thinking_message.tool_calls.is_empty());

        let tool_calls_message = &state.messages[1];
        assert_eq!(tool_calls_message.role, MessageRole::Assistant);
        assert!(tool_calls_message.thinking.is_none());
        assert_eq!(tool_calls_message.tool_calls.len(), 1);
        assert_eq!(tool_calls_message.tool_calls[0].id, "c-tc");
    }
}
