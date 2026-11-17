//! One row per finished turn, kept beside the session's event journal.
//!
//! Every turn — a top-level session's or a subagent's — that reaches a
//! terminal state (completed, failed, cancelled, interrupted) writes one
//! record: which model, effort and tier ran it, what kind of task it was
//! (`main`, `subagent`, `custom:<name>`), how it ended and why, whether it
//! was escalated, what it cost in tokens and time, and the signals observed
//! along the way (approvals, verification commands, cancellation…).
//!
//! Like `audit_events`, this is a local record, not replay input: resume never
//! reads it, and a journal written before the table existed simply gains an
//! empty one when it is next opened. A journal written before turn records
//! had a task kind gains the column when it is next opened, and its rows get
//! the kind the journal itself shows (see [`kind_from_journal`]); the
//! read-only reader works it out the same way without writing. The names
//! of `outcome` and of the `signals` kinds are `forge_types::TurnOutcome`
//! and `forge_types::TurnSignalKind`; `completion_reason` and `tier` belong
//! to `forge-core`. This crate stores them as text.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use forge_types::TaskKind;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteRow};
use sqlx::{ConnectOptions, Connection, Row};

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
    /// What ran the turn: the top-level session, a built-in subagent, or a
    /// named custom agent.
    pub task_kind: TaskKind,
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

/// A column of a stored row. A missing column or a value of the wrong type
/// (a corrupted or hand-made journal) is an error, never a panic.
fn column<'r, T>(row: &'r SqliteRow, name: &str) -> Result<T, JournalError>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    Ok(row.try_get(name)?)
}

fn parse_session(value: &str) -> Result<SessionId, JournalError> {
    value
        .parse()
        .map_err(|error: uuid::Error| JournalError::Other(error.to_string()))
}

/// The kind of task a session is, from what its journal shows: a session
/// whose journal has an `AgentProfile` event, or that has a parent, is a
/// child; a child started as a named custom agent (its latest profile names
/// the agent) is `custom:<agent>`, any other child `subagent`; everything
/// else `main`. `agent_profile` is the latest `AgentProfile` payload.
///
/// The profile is checked first because it is in the child's own journal
/// from its creation, while the parent link of a turn record depends on the
/// parent being known when the turn ended.
pub fn kind_from_journal(agent_profile: Option<&Value>, has_parent: bool) -> TaskKind {
    match agent_profile {
        Some(profile) => profile
            .get("agent")
            .and_then(Value::as_str)
            .and_then(|agent| format!("custom:{agent}").parse().ok())
            .unwrap_or(TaskKind::Subagent),
        None if has_parent => TaskKind::Subagent,
        None => TaskKind::Main,
    }
}

async fn table_exists(conn: &mut SqliteConnection, table: &str) -> Result<bool, JournalError> {
    let row =
        sqlx::query("SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_one(&mut *conn)
            .await?;
    Ok(column::<i64>(&row, "n")? > 0)
}

async fn has_task_kind_column(conn: &mut SqliteConnection) -> Result<bool, JournalError> {
    let rows = sqlx::query("PRAGMA table_info(turn_records)")
        .fetch_all(&mut *conn)
        .await?;
    for row in &rows {
        if column::<String>(row, "name")? == "task_kind" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The latest `AgentProfile` payload of each session, looked up once per
/// session. A journal without an `events` table has none.
#[derive(Default)]
struct Profiles {
    /// Whether the journal has an `events` table; checked the first time a
    /// row needs its kind worked out (most journals never do).
    has_events: Option<bool>,
    /// Keyed by the session id as stored.
    cache: HashMap<String, Option<Value>>,
}

impl Profiles {
    fn new() -> Self {
        Self::default()
    }

    async fn kind(
        &mut self,
        conn: &mut SqliteConnection,
        session_id: &str,
        has_parent: bool,
    ) -> Result<TaskKind, JournalError> {
        if !self.cache.contains_key(session_id) {
            let has_events = match self.has_events {
                Some(has_events) => has_events,
                None => {
                    let has_events = table_exists(conn, "events").await?;
                    self.has_events = Some(has_events);
                    has_events
                }
            };
            let profile = if has_events {
                let row = sqlx::query(
                    "SELECT payload FROM events WHERE session_id = ? AND event_type = 'agent_profile' \
                     ORDER BY seq DESC LIMIT 1",
                )
                .bind(session_id)
                .fetch_optional(&mut *conn)
                .await?;
                row.and_then(|row| column::<String>(&row, "payload").ok())
                    .and_then(|payload| serde_json::from_str(&payload).ok())
            } else {
                None
            };
            self.cache.insert(session_id.to_string(), profile);
        }
        let profile = self.cache.get(session_id).and_then(Option::as_ref);
        Ok(kind_from_journal(profile, has_parent))
    }
}

/// A stored row, with the kind it was written with when it has one. A row
/// without one (written before kinds existed, and not yet migrated) gets the
/// kind the journal shows.
async fn read_row(
    conn: &mut SqliteConnection,
    profiles: &mut Profiles,
    row: &SqliteRow,
    with_kind_column: bool,
) -> Result<TurnRecordRow, JournalError> {
    let session_id = parse_session(&column::<String>(row, "session_id")?)?;
    let started_at: Option<String> = column(row, "started_at")?;
    let ended_at: String = column(row, "ended_at")?;
    let parent: Option<String> = column(row, "parent_session_id")?;
    let parent_session_id = parent.as_deref().map(parse_session).transpose()?;
    let task_kind = kind_of_row(conn, profiles, row, with_kind_column, || {
        Ok((
            Cow::Owned(session_id.to_string()),
            parent_session_id.is_some(),
        ))
    })
    .await?;
    let view = view_of(row, &task_kind, parse_ts(&ended_at)?)?;
    Ok(TurnRecordRow {
        seq: column::<i64>(row, "seq")? as u64,
        record: TurnRecord {
            session_id,
            parent_session_id,
            task_id: column::<i64>(row, "task_id")? as u64,
            started_at: started_at.as_deref().map(parse_ts).transpose()?,
            ended_at: view.ended_at,
            model: view.model.to_string(),
            profile_id: view.profile_id.to_string(),
            reasoning_effort: view.reasoning_effort.map(str::to_string),
            tier: column(row, "tier")?,
            outcome: view.outcome.to_string(),
            completion_reason: view.completion_reason.to_string(),
            escalated: view.escalated,
            escalation_retry: column(row, "escalation_retry")?,
            prompt_tokens: view.prompt_tokens,
            completion_tokens: view.completion_tokens,
            cache_read_tokens: opt_u64(column(row, "cache_read_tokens")?),
            cache_write_tokens: opt_u64(column(row, "cache_write_tokens")?),
            model_calls: opt_u64(column(row, "model_calls")?)
                .map(|calls| u32::try_from(calls).unwrap_or(u32::MAX)),
            duration_ms: view.duration_ms,
            waiting_ms: view.waiting_ms,
            signals: serde_json::from_str(view.signals)?,
            task_kind,
        },
    })
}

/// A row's task kind: as stored; for a row without one, the kind the
/// journal shows (`session`: the row's session id and whether it has a
/// parent, asked for only then). The one rule both readers use.
async fn kind_of_row<'r>(
    conn: &mut SqliteConnection,
    profiles: &mut Profiles,
    row: &'r SqliteRow,
    with_kind_column: bool,
    session: impl FnOnce() -> Result<(Cow<'r, str>, bool), JournalError>,
) -> Result<TaskKind, JournalError> {
    let stored: Option<&str> = if with_kind_column {
        column(row, "task_kind")?
    } else {
        None
    };
    match stored.and_then(|kind| kind.parse().ok()) {
        Some(kind) => Ok(kind),
        None => {
            let (session_id, has_parent) = session()?;
            profiles.kind(conn, &session_id, has_parent).await
        }
    }
}

/// The columns of a row both readers decode (see [`SCAN_COLUMNS`]),
/// borrowed from it: one decoder, so they cannot read a row differently.
fn view_of<'r>(
    row: &'r SqliteRow,
    task_kind: &'r TaskKind,
    ended_at: DateTime<Utc>,
) -> Result<TurnRecordView<'r>, JournalError> {
    Ok(TurnRecordView {
        task_kind,
        ended_at,
        model: column(row, "model")?,
        profile_id: column(row, "profile_id")?,
        reasoning_effort: column(row, "reasoning_effort")?,
        outcome: column(row, "outcome")?,
        completion_reason: column(row, "completion_reason")?,
        escalated: column(row, "escalated")?,
        prompt_tokens: opt_u64(column(row, "prompt_tokens")?),
        completion_tokens: opt_u64(column(row, "completion_tokens")?),
        duration_ms: opt_u64(column(row, "duration_ms")?),
        waiting_ms: opt_u64(column(row, "waiting_ms")?),
        signals: column(row, "signals")?,
    })
}

async fn read_rows(
    conn: &mut SqliteConnection,
    rows: Vec<SqliteRow>,
    with_kind_column: bool,
) -> Result<Vec<TurnRecordRow>, JournalError> {
    let mut profiles = Profiles::new();
    let mut records = Vec::with_capacity(rows.len());
    for row in &rows {
        records.push(read_row(conn, &mut profiles, row, with_kind_column).await?);
    }
    Ok(records)
}

/// How long a read waits for a journal a running session is writing.
const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Open the journal at `db_path` for reading only, creating nothing (see
/// [`read_turn_records`] for why an idle journal is opened immutable).
async fn open_read_only(db_path: &Path) -> Result<SqliteConnection, JournalError> {
    if !db_path.is_file() {
        return Err(JournalError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no journal at {}", db_path.display()),
        )));
    }
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    let in_use = Path::new(&wal).exists();
    Ok(SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true)
        .immutable(!in_use)
        .create_if_missing(false)
        .busy_timeout(READ_BUSY_TIMEOUT)
        .connect()
        .await?)
}

/// Every turn record in the journal at `db_path`, oldest first, read without
/// writing anything: no migration, no file created (a missing journal is an
/// error), safe beside a session that has the journal open. Rows written
/// before turn records had a kind get the kind the journal shows.
///
/// Journals are in WAL mode, and a read-only SQLite connection to a WAL
/// database creates the `-wal` and `-shm` files when they are missing — and,
/// being read-only, cannot remove them again. So a journal no session has
/// open (no `-wal` file: SQLite removes it when the last connection closes)
/// is read as immutable, which takes no locks and creates nothing. A journal
/// in use already has both files; it is read through them, under the usual
/// locks, so the rows a running session has written are seen.
///
/// The one gap: a session that opens an idle journal in the instant it is
/// being read immutably is not seen by the read, which may then return
/// stale rows or an error for that journal. The read never writes, so the
/// journal itself cannot be harmed; a report read again is right.
pub async fn read_turn_records(db_path: &Path) -> Result<Vec<TurnRecordRow>, JournalError> {
    let mut conn = open_read_only(db_path).await?;
    let result = async {
        if !table_exists(&mut conn, "turn_records").await? {
            return Ok(Vec::new());
        }
        let with_kind_column = has_task_kind_column(&mut conn).await?;
        let rows = sqlx::query("SELECT * FROM turn_records ORDER BY seq")
            .fetch_all(&mut conn)
            .await?;
        read_rows(&mut conn, rows, with_kind_column).await
    }
    .await;
    conn.close().await?;
    result
}

/// Which turn records [`scan_turn_records`] visits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnRecordFilter {
    /// Only turns that ended at or after this time.
    pub ended_since: Option<DateTime<Utc>>,
    /// Only turns of this kind (worked out from the journal for a row
    /// without one).
    pub task_kind: Option<TaskKind>,
}

/// What a report needs of one turn record, borrowed from the stored row.
/// `signals` is the stored JSON array, for the caller to interpret: the
/// vocabulary belongs to `forge-core`.
#[derive(Debug, Clone, Copy)]
pub struct TurnRecordView<'a> {
    pub task_kind: &'a TaskKind,
    pub ended_at: DateTime<Utc>,
    pub model: &'a str,
    pub profile_id: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub outcome: &'a str,
    pub completion_reason: &'a str,
    pub escalated: bool,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub waiting_ms: Option<u64>,
    pub signals: &'a str,
}

/// Rows fetched per query: memory stays bounded by one page, however long
/// the journal.
const SCAN_PAGE: i64 = 512;

/// The columns a [`TurnRecordView`] is built from.
const SCAN_COLUMNS: &str = "seq, session_id, parent_session_id, ended_at, model, profile_id, \
     reasoning_effort, outcome, completion_reason, escalated, prompt_tokens, \
     completion_tokens, duration_ms, waiting_ms, signals";

/// Visit every turn record in the journal at `db_path` that `filter` keeps,
/// oldest first, read the way [`read_turn_records`] reads (nothing written
/// or created) but streamed: rows are fetched a page at a time, only the
/// columns a view needs, and the filters are applied in the query — rows
/// they exclude are never handed over — then checked exactly (a timestamp
/// stored with an offset other than UTC compares correctly). A row whose
/// scanned columns do not decode is an error; rows visited before it have
/// been visited. Only those columns are read and checked, and session ids
/// are used as stored (to look up agent profiles), not parsed; use
/// [`read_turn_records`] to check every column of every row.
pub async fn scan_turn_records(
    db_path: &Path,
    filter: &TurnRecordFilter,
    mut visit: impl FnMut(TurnRecordView<'_>),
) -> Result<(), JournalError> {
    let mut conn = open_read_only(db_path).await?;
    let result = scan_rows(&mut conn, filter, &mut visit).await;
    conn.close().await?;
    result
}

async fn scan_rows(
    conn: &mut SqliteConnection,
    filter: &TurnRecordFilter,
    visit: &mut impl FnMut(TurnRecordView<'_>),
) -> Result<(), JournalError> {
    // One query for the table's shape: no columns means no table.
    let columns: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info('turn_records')")
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|row| column(row, "name"))
        .collect::<Result<_, _>>()?;
    if columns.is_empty() {
        return Ok(());
    }
    let with_kind_column = columns.iter().any(|name| name == "task_kind");

    // Built only from the constant fragments below; values are bound.
    let mut sql = format!("SELECT {SCAN_COLUMNS}");
    if with_kind_column {
        sql.push_str(", task_kind");
    }
    sql.push_str(" FROM turn_records WHERE seq > ?");
    // Stored timestamps are RFC 3339, UTC for every row Forge writes, so
    // they compare as text. A day of margin keeps a row stored with another
    // offset (at most a day) in the query; the exact check is below.
    let since_text = filter
        .ended_since
        .map(|since| (since - chrono::Duration::days(1)).to_rfc3339());
    if since_text.is_some() {
        sql.push_str(" AND ended_at >= ?");
    }
    // A row without a kind has its kind worked out below, so it is kept.
    let kind_text = filter
        .task_kind
        .as_ref()
        .filter(|_| with_kind_column)
        .map(TaskKind::to_string);
    if kind_text.is_some() {
        sql.push_str(" AND (task_kind = ? OR task_kind IS NULL)");
    }
    sql.push_str(" ORDER BY seq LIMIT ?");

    let mut profiles = Profiles::new();
    let mut after = i64::MIN;
    loop {
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(after);
        if let Some(since) = &since_text {
            query = query.bind(since);
        }
        if let Some(kind) = &kind_text {
            query = query.bind(kind);
        }
        let rows = query.bind(SCAN_PAGE).fetch_all(&mut *conn).await?;
        for row in &rows {
            after = column(row, "seq")?;
            let ended_at = parse_ts(column(row, "ended_at")?)?;
            if filter.ended_since.is_some_and(|since| ended_at < since) {
                continue;
            }
            // Session ids as stored: only to look up agent profiles.
            let task_kind = kind_of_row(conn, &mut profiles, row, with_kind_column, || {
                let session_id: &str = column(row, "session_id")?;
                let parent: Option<&str> = column(row, "parent_session_id")?;
                Ok((Cow::Borrowed(session_id), parent.is_some()))
            })
            .await?;
            if filter
                .task_kind
                .as_ref()
                .is_some_and(|kind| kind != &task_kind)
            {
                continue;
            }
            visit(view_of(row, &task_kind, ended_at)?);
        }
        if (rows.len() as i64) < SCAN_PAGE {
            return Ok(());
        }
    }
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
                signals TEXT NOT NULL,
                task_kind TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        self.migrate_turn_record_kinds().await
    }

    /// Give a table from before task kinds the `task_kind` column, and every
    /// row without a kind the kind the journal shows. Safe to run again: it
    /// only touches rows that still have no kind, in one transaction.
    ///
    /// This runs on every open, so it must cost nothing once done: rows
    /// without a kind are found through a partial index that holds only
    /// them — empty after the fill-in, and never entered by a row written
    /// with a kind — rather than by scanning the table. A row an older Forge
    /// writes without a kind lands in it and is filled in on the next open.
    /// The fill-in itself is one `UPDATE` per (session, has a parent), not
    /// one per row: its kind depends on nothing else.
    async fn migrate_turn_record_kinds(&self) -> Result<(), JournalError> {
        let mut conn = self.pool.acquire().await?;
        // One query for what the table already has, so a journal that needs
        // nothing costs two statements: this and the probe below.
        let schema = sqlx::query(
            "SELECT \
               (SELECT COUNT(*) FROM pragma_table_info('turn_records') \
                WHERE name = 'task_kind') AS has_column, \
               (SELECT COUNT(*) FROM sqlite_master \
                WHERE type = 'index' AND name = 'turn_records_without_kind') AS has_index",
        )
        .fetch_one(&mut *conn)
        .await?;
        if column::<i64>(&schema, "has_column")? == 0 {
            sqlx::query("ALTER TABLE turn_records ADD COLUMN task_kind TEXT")
                .execute(&mut *conn)
                .await?;
        }
        if column::<i64>(&schema, "has_index")? == 0 {
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS turn_records_without_kind \
                 ON turn_records(session_id, parent_session_id) WHERE task_kind IS NULL",
            )
            .execute(&mut *conn)
            .await?;
        }
        let any_pending = sqlx::query("SELECT 1 FROM turn_records WHERE task_kind IS NULL LIMIT 1")
            .fetch_optional(&mut *conn)
            .await?;
        if any_pending.is_none() {
            return Ok(());
        }
        let pending = sqlx::query(
            "SELECT DISTINCT session_id, parent_session_id IS NOT NULL AS has_parent \
             FROM turn_records WHERE task_kind IS NULL",
        )
        .fetch_all(&mut *conn)
        .await?;
        let mut tx = conn.begin().await?;
        let mut profiles = Profiles::new();
        for group in pending {
            let session_id: String = column(&group, "session_id")?;
            let has_parent: bool = column(&group, "has_parent")?;
            let kind = profiles.kind(&mut tx, &session_id, has_parent).await?;
            sqlx::query(
                "UPDATE turn_records SET task_kind = ? \
                 WHERE task_kind IS NULL AND session_id = ? \
                 AND (parent_session_id IS NOT NULL) = ?",
            )
            .bind(kind.to_string())
            .bind(&session_id)
            .bind(has_parent)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Append one turn record. Returns its sequence number.
    pub async fn append_turn_record(&self, record: &TurnRecord) -> Result<u64, JournalError> {
        let result = sqlx::query(
            r#"
            INSERT INTO turn_records (
                session_id, parent_session_id, task_id, started_at, ended_at,
                model, profile_id, reasoning_effort, tier, task_kind, outcome,
                completion_reason, escalated, escalation_retry, prompt_tokens,
                completion_tokens, cache_read_tokens, cache_write_tokens,
                model_calls, duration_ms, waiting_ms, signals
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(record.task_kind.to_string())
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
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(
            r#"
            SELECT * FROM turn_records WHERE session_id = ? ORDER BY seq
            "#,
        )
        .bind(session_id.to_string())
        .fetch_all(&mut *conn)
        .await?;
        read_rows(&mut conn, rows, true).await
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
            task_kind: if parent.is_some() {
                TaskKind::Subagent
            } else {
                TaskKind::Main
            },
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
