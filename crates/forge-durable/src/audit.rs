//! Append-only audit trail kept beside the session's event journal.
//!
//! The governance `AuditLog` is in-memory and lost with the process; this
//! table keeps the same decisions, plus what happened to them (approval
//! resolutions, grants, tool results, gateway egress), durably in the
//! session's own database. Rows are correlated by tool call id. It is a
//! record, not replay input: resume never reads it, so a journal written
//! before the table existed resumes unchanged and simply gains an empty table
//! when it is next opened.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::{Journal, JournalError, SessionId};

/// What an audit row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// A governance decision on a tool call, as the in-memory log records it.
    Authorize,
    /// The session paused the call for an operator decision.
    HitlRequest,
    /// The operator's decision on a paused call.
    HitlResolution,
    /// A network host permitted for the session or written persistently.
    HostGrant,
    /// A command pattern remembered for the session or written persistently.
    PatternGrant,
    /// Session approve-all turned on from an approval.
    ApproveAll,
    /// A tool execution finished.
    ToolResult,
    /// One connection decision from the session's egress gateway.
    Egress,
    /// A kind written by a newer build.
    #[serde(other)]
    Unknown,
}

impl AuditKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::HitlRequest => "hitl_request",
            Self::HitlResolution => "hitl_resolution",
            Self::HostGrant => "host_grant",
            Self::PatternGrant => "pattern_grant",
            Self::ApproveAll => "approve_all",
            Self::ToolResult => "tool_result",
            Self::Egress => "egress",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Self {
        serde_json::from_value(Value::String(value.to_string())).unwrap_or(Self::Unknown)
    }
}

/// A row to append. `detail` must already be redacted: arguments go in as
/// governance redacted them, never raw.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub kind: AuditKind,
    pub call_id: Option<String>,
    pub tool: Option<String>,
    pub detail: Value,
}

/// A stored audit row.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditRow {
    pub seq: u64,
    pub session_id: SessionId,
    pub ts: DateTime<Utc>,
    pub kind: AuditKind,
    pub call_id: Option<String>,
    pub tool: Option<String>,
    pub detail: Value,
}

impl Journal {
    pub(crate) async fn migrate_audit(&self) -> Result<(), JournalError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS audit_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                ts TEXT NOT NULL,
                kind TEXT NOT NULL,
                call_id TEXT,
                tool TEXT,
                detail TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS audit_events_call ON audit_events (session_id, call_id)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append `entries` in order, in one transaction.
    pub async fn append_audit(
        &self,
        session_id: SessionId,
        entries: &[AuditEntry],
    ) -> Result<(), JournalError> {
        if entries.is_empty() {
            return Ok(());
        }
        let ts = Utc::now().to_rfc3339();
        let sid = session_id.to_string();
        let mut tx = self.pool.begin().await?;
        for entry in entries {
            sqlx::query(
                r#"
                INSERT INTO audit_events (session_id, ts, kind, call_id, tool, detail)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&sid)
            .bind(&ts)
            .bind(entry.kind.as_str())
            .bind(entry.call_id.as_deref())
            .bind(entry.tool.as_deref())
            .bind(serde_json::to_string(&entry.detail)?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every audit row for `session_id`, oldest first.
    pub async fn audit_events(&self, session_id: SessionId) -> Result<Vec<AuditRow>, JournalError> {
        let rows = sqlx::query(
            r#"
            SELECT seq, ts, kind, call_id, tool, detail
            FROM audit_events WHERE session_id = ? ORDER BY seq
            "#,
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let ts: String = row.get("ts");
                let detail: String = row.get("detail");
                Ok(AuditRow {
                    seq: row.get::<i64, _>("seq") as u64,
                    session_id,
                    ts: DateTime::parse_from_rfc3339(&ts)
                        .map(|ts| ts.with_timezone(&Utc))
                        .map_err(|error| JournalError::Other(error.to_string()))?,
                    kind: AuditKind::parse(row.get::<String, _>("kind").as_str()),
                    call_id: row.get("call_id"),
                    tool: row.get("tool"),
                    detail: serde_json::from_str(&detail)?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_session_id;
    use serde_json::json;

    #[tokio::test]
    async fn audit_rows_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = new_session_id();
        let journal = Journal::open(dir.path(), session_id).await.unwrap();
        assert!(journal.audit_events(session_id).await.unwrap().is_empty());
        journal
            .append_audit(
                session_id,
                &[
                    AuditEntry {
                        kind: AuditKind::Authorize,
                        call_id: Some("c1".into()),
                        tool: Some("bash".into()),
                        detail: json!({"decision": "Hitl"}),
                    },
                    AuditEntry {
                        kind: AuditKind::Egress,
                        call_id: Some("c1".into()),
                        tool: None,
                        detail: json!({"host": "example.com"}),
                    },
                ],
            )
            .await
            .unwrap();
        let rows = journal.audit_events(session_id).await.unwrap();
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            [AuditKind::Authorize, AuditKind::Egress]
        );
        assert!(rows[0].seq < rows[1].seq);
        assert_eq!(rows[0].tool.as_deref(), Some("bash"));
        assert_eq!(rows[1].detail["host"], "example.com");
        // Other sessions' rows are not mixed in.
        assert!(journal
            .audit_events(new_session_id())
            .await
            .unwrap()
            .is_empty());
    }

    /// A journal written before the audit table existed opens, replays, and
    /// gains an empty table.
    #[tokio::test]
    async fn a_journal_without_the_audit_table_opens_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = new_session_id();
        {
            let journal = Journal::open(dir.path(), session_id).await.unwrap();
            journal.append_session_created(session_id).await.unwrap();
            journal
                .append_user_message(session_id, "hello")
                .await
                .unwrap();
            sqlx::query("DROP INDEX audit_events_call")
                .execute(&journal.pool)
                .await
                .unwrap();
            sqlx::query("DROP TABLE audit_events")
                .execute(&journal.pool)
                .await
                .unwrap();
            journal.pool.close().await;
        }
        let journal = Journal::open(dir.path(), session_id).await.unwrap();
        let state = journal.replay(session_id).await.unwrap();
        assert_eq!(state.session_id, session_id);
        assert!(journal.audit_events(session_id).await.unwrap().is_empty());
        assert_eq!(AuditKind::parse("from_the_future"), AuditKind::Unknown);
    }
}
