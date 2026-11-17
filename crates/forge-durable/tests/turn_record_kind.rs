//! Every turn record carries the kind of task that ran it (`main`,
//! `subagent`, `custom:<name>`). Journals written before the kind existed
//! gain it when they are next opened, worked out from the journal itself, and
//! the read-only reader used for reports works it out the same way without
//! changing the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use forge_durable::{new_session_id, read_turn_records, Journal, TurnRecord};
use forge_test_support::turn_records::TurnRecordBuilder;
use forge_types::{SessionId, TaskKind};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection, Row};

fn record(session_id: SessionId, parent: Option<SessionId>, kind: TaskKind) -> TurnRecord {
    TurnRecordBuilder::new(kind, "anthropic/claude-opus-4-1")
        .session_id(session_id)
        .parent(parent)
        .started_at(Some(Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap()))
        .ended_at(Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 5).unwrap())
        .effort(Some("high"))
        .tokens(Some(1_000), Some(200))
        .cache_tokens(Some(0), Some(0))
        .duration_ms(Some(5_000))
        .build()
}

/// The `events` and `turn_records` tables exactly as Forge created them
/// before turn records had a kind.
const OLD_SCHEMA: &[&str] = &[
    r#"CREATE TABLE events (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        ts TEXT NOT NULL,
        event_type TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        payload TEXT NOT NULL,
        trace_id TEXT
    )"#,
    r#"CREATE TABLE turn_records (
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
    )"#,
];

/// Write `<session_id>.db` in the old layout: a `session_created` event, the
/// given `agent_profile` events, and `turns` turn records (without a kind).
async fn old_journal(
    dir: &Path,
    session_id: SessionId,
    parent: Option<SessionId>,
    profiles: &[serde_json::Value],
    turns: usize,
) -> PathBuf {
    let path = dir.join(format!("{session_id}.db"));
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .connect()
        .await
        .unwrap();
    for statement in OLD_SCHEMA {
        sqlx::query(*statement).execute(&mut conn).await.unwrap();
    }
    let mut events = vec![("session_created", json!({}))];
    events.extend(profiles.iter().map(|p| ("agent_profile", p.clone())));
    for (event_type, payload) in events {
        sqlx::query(
            "INSERT INTO events (session_id, ts, event_type, schema_version, payload) \
             VALUES (?, ?, ?, 1, ?)",
        )
        .bind(session_id.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(event_type)
        .bind(payload.to_string())
        .execute(&mut conn)
        .await
        .unwrap();
    }
    for task_id in 0..turns {
        sqlx::query(
            "INSERT INTO turn_records (session_id, parent_session_id, task_id, started_at, \
             ended_at, model, profile_id, reasoning_effort, tier, outcome, completion_reason, \
             escalated, escalation_retry, prompt_tokens, completion_tokens, cache_read_tokens, \
             cache_write_tokens, model_calls, duration_ms, waiting_ms, signals) \
             VALUES (?, ?, ?, NULL, ?, 'anthropic/claude-opus-4-1', 'anthropic', 'high', NULL, \
             'completed', 'edit_verified', 0, 0, 10, 5, 0, 0, 1, 100, 0, '[]')",
        )
        .bind(session_id.to_string())
        .bind(parent.map(|p| p.to_string()))
        .bind(task_id as i64)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut conn)
        .await
        .unwrap();
    }
    conn.close().await.unwrap();
    path
}

/// Four sessions journaled before task kinds existed:
/// - a top-level session (no parent, no agent profile) → `main`;
/// - a child from before agent profiles were journaled (a parent, no
///   profile) → `subagent`;
/// - a read-only built-in child whose parent link was not recorded (a
///   profile without an agent name) → `subagent`;
/// - a child started as the `reviewer` custom agent → `custom:reviewer`.
struct OldJournals {
    dir: tempfile::TempDir,
    sessions: Vec<(SessionId, TaskKind)>,
}

async fn old_journals() -> OldJournals {
    let dir = tempfile::tempdir().unwrap();
    let parent = new_session_id();
    let main = new_session_id();
    let pre_profile_child = new_session_id();
    let plain_child = new_session_id();
    let custom_child = new_session_id();
    old_journal(dir.path(), main, None, &[], 2).await;
    old_journal(dir.path(), pre_profile_child, Some(parent), &[], 1).await;
    old_journal(
        dir.path(),
        plain_child,
        None,
        &[json!({"tool_limit": null, "mode": "read_only", "no_network": false})],
        1,
    )
    .await;
    old_journal(
        dir.path(),
        custom_child,
        Some(parent),
        &[json!({
            "tool_limit": ["read_file"],
            "mode": "read_only",
            "agent": "reviewer",
            "capabilities": "read-only",
            "instructions": "Review the change.",
            "no_network": true
        })],
        3,
    )
    .await;
    OldJournals {
        dir,
        sessions: vec![
            (main, TaskKind::Main),
            (pre_profile_child, TaskKind::Subagent),
            (plain_child, TaskKind::Subagent),
            (custom_child, TaskKind::Custom("reviewer".into())),
        ],
    }
}

/// The `turn_records` columns of the journal at `path`. Opened the way the
/// read-only reader opens a journal, so looking does not itself leave `-wal`
/// and `-shm` files behind.
async fn columns(path: &Path) -> Vec<String> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(!Path::new(&wal).exists())
        .connect()
        .await
        .unwrap();
    let rows = sqlx::query("PRAGMA table_info(turn_records)")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    rows.iter()
        .map(|row| row.get::<String, _>("name"))
        .collect()
}

/// Every file in `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

#[tokio::test]
async fn new_records_keep_their_kind() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let parent = new_session_id();
    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    let kinds = [
        TaskKind::Main,
        TaskKind::Subagent,
        TaskKind::Custom("reviewer".into()),
    ];
    for kind in &kinds {
        journal
            .append_turn_record(&record(session_id, Some(parent), kind.clone()))
            .await
            .unwrap();
    }
    let rows = journal.turn_records(session_id).await.unwrap();
    let read: Vec<TaskKind> = rows.into_iter().map(|row| row.record.task_kind).collect();
    assert_eq!(read, kinds);
}

#[tokio::test]
async fn a_journal_written_before_task_kinds_opens_and_its_rows_read_as_the_kind_the_journal_shows()
{
    let old = old_journals().await;
    for (session_id, kind) in &old.sessions {
        let journal = Journal::open(old.dir.path(), *session_id).await.unwrap();
        let rows = journal.turn_records(*session_id).await.unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            assert_eq!(&row.record.task_kind, kind, "session {session_id}");
        }
        // New turns in the same journal record their own kind.
        journal
            .append_turn_record(&record(*session_id, None, kind.clone()))
            .await
            .unwrap();
        drop(journal);
        let path = old.dir.path().join(format!("{session_id}.db"));
        assert!(
            columns(&path).await.contains(&"task_kind".to_string()),
            "opening the journal adds the column"
        );
    }
}

#[tokio::test]
async fn opening_an_old_journal_again_changes_nothing_more() {
    let old = old_journals().await;
    let (session_id, kind) = old.sessions[3].clone();
    let first: Vec<_> = {
        let journal = Journal::open(old.dir.path(), session_id).await.unwrap();
        journal.turn_records(session_id).await.unwrap()
    };
    let second = {
        let journal = Journal::open(old.dir.path(), session_id).await.unwrap();
        journal.turn_records(session_id).await.unwrap()
    };
    assert_eq!(first, second);
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|row| row.record.task_kind == kind));
}

#[tokio::test]
async fn the_read_only_reader_works_out_old_kinds_without_changing_the_file() {
    let old = old_journals().await;
    let before = snapshot(old.dir.path());
    for (session_id, kind) in &old.sessions {
        let path = old.dir.path().join(format!("{session_id}.db"));
        let rows = read_turn_records(&path).await.unwrap();
        assert!(!rows.is_empty());
        for row in &rows {
            assert_eq!(row.record.session_id, *session_id);
            assert_eq!(&row.record.task_kind, kind, "session {session_id}");
        }
        assert!(
            !columns(&path).await.contains(&"task_kind".to_string()),
            "a read must not migrate the journal"
        );
    }
    assert_eq!(
        snapshot(old.dir.path()),
        before,
        "reading must leave every file in the journal directory as it was"
    );
}

#[tokio::test]
async fn the_read_only_reader_sees_rows_a_live_journal_just_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    journal
        .append_turn_record(&record(session_id, None, TaskKind::Main))
        .await
        .unwrap();
    journal
        .append_turn_record(&record(
            session_id,
            Some(new_session_id()),
            TaskKind::Custom("reviewer".into()),
        ))
        .await
        .unwrap();
    // The journal is still open (WAL not checkpointed) while the report reads.
    let rows = read_turn_records(&dir.path().join(format!("{session_id}.db")))
        .await
        .unwrap();
    let kinds: Vec<_> = rows
        .iter()
        .map(|row| row.record.task_kind.clone())
        .collect();
    assert_eq!(kinds, [TaskKind::Main, TaskKind::Custom("reviewer".into())]);
    assert_eq!(
        rows[0].record,
        record(session_id, None, TaskKind::Main),
        "every field reads back"
    );
    drop(journal);
}

#[tokio::test]
async fn the_read_only_reader_finds_nothing_in_a_journal_without_turn_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{}.db", new_session_id()));
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    sqlx::query(OLD_SCHEMA[0]).execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
    assert!(read_turn_records(&path).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_read_only_reader_does_not_create_a_missing_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{}.db", new_session_id()));
    assert!(read_turn_records(&path).await.is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

// ------------------------------------------------------------ performance

/// `n` old-layout rows for `session_id` in one statement.
async fn bulk_old_rows(path: &Path, session_id: SessionId, parent: Option<SessionId>, n: usize) {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
         INSERT INTO turn_records (session_id, parent_session_id, task_id, ended_at, model, \
         profile_id, outcome, completion_reason, escalated, escalation_retry, signals) \
         SELECT ?, ?, i, '2025-06-01T00:00:00+00:00', 'a/b', 'a', 'completed', 'x', 0, 0, '[]' \
         FROM n",
    )
    .bind(n as i64)
    .bind(session_id.to_string())
    .bind(parent.map(|p| p.to_string()))
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();
}

/// Opening a journal written before task kinds fills them in with a few
/// statements, not one per row: a session with a long history must not
/// take long to reopen (one row at a time, 50,000 rows took about 11 s in a
/// debug build, 20,000 about 4 s).
#[tokio::test]
async fn filling_in_the_kinds_of_a_large_old_journal_is_quick() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let parent = new_session_id();
    let path = old_journal(
        dir.path(),
        session_id,
        None,
        &[json!({"mode": "writer", "agent": "reviewer"})],
        0,
    )
    .await;
    // Rows with and without a parent link: both are the same child.
    bulk_old_rows(&path, session_id, Some(parent), 25_000).await;
    bulk_old_rows(&path, session_id, None, 25_000).await;

    let started = std::time::Instant::now();
    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    let elapsed = started.elapsed();
    let rows = journal.turn_records(session_id).await.unwrap();
    assert_eq!(rows.len(), 50_000);
    assert!(rows
        .iter()
        .all(|row| row.record.task_kind == TaskKind::Custom("reviewer".into())));
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "opening took {elapsed:?}"
    );
}

/// After the one-time fill-in, finding rows without a kind — done on every
/// open — goes through an index that only holds such rows, not a scan of
/// the whole table.
#[tokio::test]
async fn rows_without_a_kind_are_found_through_an_index_not_a_scan() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    journal
        .append_turn_record(&record(session_id, None, TaskKind::Main))
        .await
        .unwrap();
    drop(journal);

    let path = dir.path().join(format!("{session_id}.db"));
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query(
        "EXPLAIN QUERY PLAN SELECT DISTINCT session_id, parent_session_id IS NOT NULL \
         FROM turn_records WHERE task_kind IS NULL",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect();
    let index: Option<String> = sqlx::query(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'turn_records' \
         AND sql LIKE '%WHERE task_kind IS NULL%'",
    )
    .fetch_optional(&mut conn)
    .await
    .unwrap()
    .map(|row| row.get("sql"));
    conn.close().await.unwrap();
    assert!(index.is_some(), "no partial index on rows without a kind");
    // `SCAN turn_records USING INDEX turn_records_without_kind` walks the
    // partial index (empty once filled in); a bare `SCAN turn_records` would
    // read every row of the table.
    assert!(
        plan.iter()
            .any(|step| step.contains("INDEX turn_records_without_kind")),
        "the lookup does not use the index: {plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.trim() == "SCAN turn_records"),
        "the lookup scans the table: {plan:?}"
    );
}
