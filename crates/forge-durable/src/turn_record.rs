//! One row per finished turn, kept beside the session's event journal.
//!
//! Every turn — a top-level session's or a subagent's — that reaches a
//! terminal state (completed, failed, cancelled, interrupted) writes one
//! record: which model, effort and tier ran it, how it ended and why, whether
//! it was escalated, what it cost in tokens and time, and the signals
//! observed along the way (approvals, verification commands, cancellation…).
//!
//! Like `audit_events`, this is a local record, not replay input: resume never
//! reads it, and a journal written before the table existed simply gains an
//! empty one when it is next opened. The vocabulary of `outcome`,
//! `completion_reason`, `tier` and `signals` belongs to `forge-core`; this
//! crate stores it.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;

use crate::{Journal, JournalError, SessionId};

/// What one finished turn did. `None` means the value is not known — e.g.
/// the duration and tokens of a turn found interrupted by a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRecord {
    pub session_id: SessionId,
    /// The session that spawned this one, for a subagent.
    pub parent_session_id: Option<SessionId>,
    /// The session's task id for this turn.
    pub task_id: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: DateTime<Utc>,
    /// `<provider>/<model>` the turn ended on.
    pub model: String,
    pub profile_id: String,
    pub reasoning_effort: Option<String>,
    /// The model tier in force, when one was asked for.
    pub tier: Option<String>,
    /// `completed`, `failed`, `cancelled` or `interrupted`.
    pub outcome: String,
    /// Why it ended that way (the completion evaluator's category, or the
    /// runtime's: `user_cancelled`, `model_call_failed`, …).
    pub completion_reason: String,
    /// This (failed) turn was re-run on the strong tier.
    pub escalated: bool,
    /// This turn is that strong-tier re-run.
    pub escalation_retry: bool,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub model_calls: Option<u32>,
    /// Wall-clock time from the turn's start to its end, waits included.
    pub duration_ms: Option<u64>,
    /// The part of `duration_ms` spent waiting on the user (approvals,
    /// questions).
    pub waiting_ms: Option<u64>,
    /// The signals observed during the turn, in order (JSON array).
    pub signals: Value,
}

/// A stored turn record.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRecordRow {
    pub seq: u64,
    pub record: TurnRecord,
}

fn opt_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn to_i64(value: Option<u64>) -> Option<i64> {
    value.map(|value| i64::try_from(value).unwrap_or(i64::MAX))
}

fn parse_ts(value: &str) -> Result<DateTime<Utc>, JournalError> {
    DateTime::parse_from_rfc3339(value)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|error| JournalError::Other(error.to_string()))
}

fn parse_session(value: &str) -> Result<SessionId, JournalError> {
    value
        .parse()
        .map_err(|error: uuid::Error| JournalError::Other(error.to_string()))
}

impl Journal {
    pub(crate) async fn migrate_turn_records(&self) -> Result<(), JournalError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS turn_records (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                parent_session_id TEXT,
                task_id INTEGER NOT NULL,
                started_at TEXT,
                ended_at TEXT NOT NULL,
                model TEXT NOT NULL,
                profile_id TEXT NOT NULL,
                reasoning_effort TEXT,
                tier TEXT,
                outcome TEXT NOT NULL,
                completion_reason TEXT NOT NULL,
                escalated INTEGER NOT NULL,
                escalation_retry INTEGER NOT NULL,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                model_calls INTEGER,
                duration_ms INTEGER,
                waiting_ms INTEGER,
                signals TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append one turn record. Returns its sequence number.
    pub async fn append_turn_record(&self, record: &TurnRecord) -> Result<u64, JournalError> {
        let result = sqlx::query(
            r#"
            INSERT INTO turn_records (
                session_id, parent_session_id, task_id, started_at, ended_at,
                model, profile_id, reasoning_effort, tier, outcome,
                completion_reason, escalated, escalation_retry, prompt_tokens,
                completion_tokens, cache_read_tokens, cache_write_tokens,
                model_calls, duration_ms, waiting_ms, signals
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(record.session_id.to_string())
        .bind(record.parent_session_id.map(|id| id.to_string()))
        .bind(i64::try_from(record.task_id).unwrap_or(i64::MAX))
        .bind(record.started_at.map(|ts| ts.to_rfc3339()))
        .bind(record.ended_at.to_rfc3339())
        .bind(&record.model)
        .bind(&record.profile_id)
        .bind(record.reasoning_effort.as_deref())
        .bind(record.tier.as_deref())
        .bind(&record.outcome)
        .bind(&record.completion_reason)
        .bind(record.escalated)
        .bind(record.escalation_retry)
        .bind(to_i64(record.prompt_tokens))
        .bind(to_i64(record.completion_tokens))
        .bind(to_i64(record.cache_read_tokens))
        .bind(to_i64(record.cache_write_tokens))
        .bind(record.model_calls.map(i64::from))
        .bind(to_i64(record.duration_ms))
        .bind(to_i64(record.waiting_ms))
        .bind(serde_json::to_string(&record.signals)?)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid() as u64)
    }

    /// Every turn record for `session_id`, oldest first.
    pub async fn turn_records(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnRecordRow>, JournalError> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM turn_records WHERE session_id = ? ORDER BY seq
            "#,
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let started_at: Option<String> = row.get("started_at");
                let ended_at: String = row.get("ended_at");
                let parent: Option<String> = row.get("parent_session_id");
                let signals: String = row.get("signals");
                Ok(TurnRecordRow {
                    seq: row.get::<i64, _>("seq") as u64,
                    record: TurnRecord {
                        session_id,
                        parent_session_id: parent.as_deref().map(parse_session).transpose()?,
                        task_id: row.get::<i64, _>("task_id") as u64,
                        started_at: started_at.as_deref().map(parse_ts).transpose()?,
                        ended_at: parse_ts(&ended_at)?,
                        model: row.get("model"),
                        profile_id: row.get("profile_id"),
                        reasoning_effort: row.get("reasoning_effort"),
                        tier: row.get("tier"),
                        outcome: row.get("outcome"),
                        completion_reason: row.get("completion_reason"),
                        escalated: row.get("escalated"),
                        escalation_retry: row.get("escalation_retry"),
                        prompt_tokens: opt_u64(row.get("prompt_tokens")),
                        completion_tokens: opt_u64(row.get("completion_tokens")),
                        cache_read_tokens: opt_u64(row.get("cache_read_tokens")),
                        cache_write_tokens: opt_u64(row.get("cache_write_tokens")),
                        model_calls: opt_u64(row.get("model_calls"))
                            .map(|calls| u32::try_from(calls).unwrap_or(u32::MAX)),
                        duration_ms: opt_u64(row.get("duration_ms")),
                        waiting_ms: opt_u64(row.get("waiting_ms")),
                        signals: serde_json::from_str(&signals)?,
                    },
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

    fn record(session_id: SessionId, parent: Option<SessionId>) -> TurnRecord {
        TurnRecord {
            session_id,
            parent_session_id: parent,
            task_id: 3,
            started_at: Some(Utc::now()),
            ended_at: Utc::now(),
            model: "anthropic/claude-opus-4-1".into(),
            profile_id: "anthropic".into(),
            reasoning_effort: Some("high".into()),
            tier: Some("strong".into()),
            outcome: "completed".into(),
            completion_reason: "edit_verified".into(),
            escalated: false,
            escalation_retry: true,
            prompt_tokens: Some(1_200),
            completion_tokens: Some(300),
            cache_read_tokens: Some(1_000),
            cache_write_tokens: Some(0),
            model_calls: Some(2),
            duration_ms: Some(4_500),
            waiting_ms: Some(1_000),
            signals: json!([{"kind": "verification_passed", "detail": "cargo test"}]),
        }
    }

    #[tokio::test]
    async fn turn_records_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = new_session_id();
        let parent = new_session_id();
        let journal = Journal::open(dir.path(), session_id).await.unwrap();
        assert!(journal.turn_records(session_id).await.unwrap().is_empty());

        let first = record(session_id, Some(parent));
        let mut second = record(session_id, Some(parent));
        second.task_id = 4;
        second.tier = None;
        second.started_at = None;
        second.duration_ms = None;
        second.prompt_tokens = None;
        second.outcome = "interrupted".into();
        let a = journal.append_turn_record(&first).await.unwrap();
        let b = journal.append_turn_record(&second).await.unwrap();
        assert!(b > a);

        let rows = journal.turn_records(session_id).await.unwrap();
        assert_eq!(rows.len(), 2);
        // RFC 3339 keeps sub-second precision, so timestamps round-trip.
        assert_eq!(rows[0].record, first);
        assert_eq!(rows[1].record, second);
        assert!(journal.turn_records(parent).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_table_survives_reopening_and_does_not_touch_replay() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = new_session_id();
        let journal = Journal::open(dir.path(), session_id).await.unwrap();
        journal.append_session_created(session_id).await.unwrap();
        journal
            .append_turn_record(&record(session_id, None))
            .await
            .unwrap();
        let events_before = journal.replay(session_id).await.unwrap().last_seq;
        drop(journal);

        let reopened = Journal::open(dir.path(), session_id).await.unwrap();
        assert_eq!(reopened.turn_records(session_id).await.unwrap().len(), 1);
        assert_eq!(
            reopened.replay(session_id).await.unwrap().last_seq,
            events_before
        );
    }
}
