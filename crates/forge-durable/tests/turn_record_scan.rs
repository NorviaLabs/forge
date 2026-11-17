//! `scan_turn_records`: the turn records of a journal as a report needs them
//! — streamed, filtered in the query, read-only.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use forge_durable::{
    new_session_id, read_turn_records, scan_turn_records, Journal, TurnRecord, TurnRecordFilter,
};
use forge_types::{SessionId, TaskKind};
use serde_json::json;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};

fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 6, 1, hour, 0, 0).unwrap()
}

fn record(session_id: SessionId, kind: TaskKind, ended_at: DateTime<Utc>) -> TurnRecord {
    TurnRecord {
        session_id,
        parent_session_id: None,
        task_id: 1,
        started_at: Some(ended_at - chrono::Duration::seconds(5)),
        ended_at,
        model: "anthropic/claude-opus-4-1".into(),
        profile_id: "anthropic".into(),
        reasoning_effort: Some("high".into()),
        tier: Some("strong".into()),
        task_kind: kind,
        outcome: "completed".into(),
        completion_reason: "edit_verified".into(),
        escalated: true,
        escalation_retry: false,
        prompt_tokens: Some(1_000),
        completion_tokens: Some(200),
        cache_read_tokens: Some(5),
        cache_write_tokens: Some(0),
        model_calls: Some(2),
        duration_ms: Some(5_000),
        waiting_ms: Some(1_000),
        signals: json!([{"kind": "verification_passed", "detail": "cargo test", "at_ms": 3}]),
    }
}

async fn journal_with(dir: &Path, records: &[TurnRecord]) -> PathBuf {
    let session_id = records[0].session_id;
    let journal = Journal::open(dir, session_id).await.unwrap();
    for record in records {
        journal.append_turn_record(record).await.unwrap();
    }
    drop(journal);
    dir.join(format!("{session_id}.db"))
}

/// What a scan visits, owned: (kind, ended_at, model, profile, effort,
/// outcome, completion_reason, escalated, (prompt, completion, duration,
/// waiting), signals).
type Seen = (
    TaskKind,
    DateTime<Utc>,
    String,
    String,
    Option<String>,
    String,
    String,
    bool,
    (Option<u64>, Option<u64>, Option<u64>, Option<u64>),
    serde_json::Value,
);

async fn scan(path: &Path, filter: &TurnRecordFilter) -> Vec<Seen> {
    let mut seen = Vec::new();
    scan_turn_records(path, filter, |view| {
        seen.push((
            view.task_kind.clone(),
            view.ended_at,
            view.model.to_string(),
            view.profile_id.to_string(),
            view.reasoning_effort.map(str::to_string),
            view.outcome.to_string(),
            view.completion_reason.to_string(),
            view.escalated,
            (
                view.prompt_tokens,
                view.completion_tokens,
                view.duration_ms,
                view.waiting_ms,
            ),
            serde_json::from_str(view.signals).unwrap(),
        ));
    })
    .await
    .unwrap();
    seen
}

fn expected(record: &TurnRecord) -> Seen {
    (
        record.task_kind.clone(),
        record.ended_at,
        record.model.clone(),
        record.profile_id.clone(),
        record.reasoning_effort.clone(),
        record.outcome.clone(),
        record.completion_reason.clone(),
        record.escalated,
        (
            record.prompt_tokens,
            record.completion_tokens,
            record.duration_ms,
            record.waiting_ms,
        ),
        record.signals.clone(),
    )
}

#[tokio::test]
async fn a_scan_sees_what_the_full_reader_reads_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let records: Vec<TurnRecord> = (0..1_200)
        .map(|i| {
            let mut r = record(session_id, TaskKind::Subagent, at(1));
            r.task_id = i;
            r.prompt_tokens = Some(i);
            r.reasoning_effort = (i % 2 == 0).then(|| "low".to_string());
            r
        })
        .collect();
    let path = journal_with(dir.path(), &records).await;
    let scanned = scan(&path, &TurnRecordFilter::default()).await;
    let read: Vec<Seen> = read_turn_records(&path)
        .await
        .unwrap()
        .iter()
        .map(|row| expected(&row.record))
        .collect();
    assert_eq!(scanned.len(), 1_200, "more rows than one page");
    assert_eq!(scanned, read);
}

#[tokio::test]
async fn since_keeps_turns_that_ended_at_or_after_it_whatever_their_offset() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let path = journal_with(
        dir.path(),
        &[
            record(session_id, TaskKind::Main, at(9)),
            record(session_id, TaskKind::Main, at(10)),
            record(session_id, TaskKind::Main, at(11)),
        ],
    )
    .await;
    // Two rows a writer stored with a +02:00 offset: 01:00+02:00 is 23:00 the
    // day before, 12:30+02:00 is 10:30 — textually both sort after 10:00Z.
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    for ended in ["2025-06-01T01:00:00+02:00", "2025-06-01T12:30:00+02:00"] {
        sqlx::query(
            "INSERT INTO turn_records (session_id, task_id, ended_at, model, profile_id, \
             outcome, completion_reason, escalated, escalation_retry, signals, task_kind) \
             VALUES (?, 9, ?, 'a/b', 'a', 'completed', 'x', 0, 0, '[]', 'main')",
        )
        .bind(session_id.to_string())
        .bind(ended)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    conn.close().await.unwrap();

    let filter = TurnRecordFilter {
        ended_since: Some(at(10)),
        ..TurnRecordFilter::default()
    };
    let mut ended: Vec<DateTime<Utc>> = scan(&path, &filter).await.iter().map(|s| s.1).collect();
    ended.sort();
    assert_eq!(
        ended,
        [
            at(10),
            Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
            at(11)
        ]
    );
}

#[tokio::test]
async fn a_kind_filter_applies_to_kinds_worked_out_for_old_rows_too() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let path = journal_with(
        dir.path(),
        &[
            record(session_id, TaskKind::Main, at(1)),
            record(session_id, TaskKind::Custom("reviewer".into()), at(2)),
        ],
    )
    .await;
    // A row with no kind (written by an older Forge after this journal was
    // last opened): the journal has no agent profile and the row no parent,
    // so it is `main`.
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO turn_records (session_id, task_id, ended_at, model, profile_id, \
         outcome, completion_reason, escalated, escalation_retry, signals) \
         VALUES (?, 9, '2025-06-01T03:00:00+00:00', 'a/b', 'a', 'completed', 'x', 0, 0, '[]')",
    )
    .bind(session_id.to_string())
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();

    let kinds = |kind: TaskKind| {
        let path = path.clone();
        async move {
            let filter = TurnRecordFilter {
                task_kind: Some(kind),
                ..TurnRecordFilter::default()
            };
            scan(&path, &filter)
                .await
                .into_iter()
                .map(|s| (s.0, s.1))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        kinds(TaskKind::Main).await,
        [(TaskKind::Main, at(1)), (TaskKind::Main, at(3))]
    );
    assert_eq!(
        kinds(TaskKind::Custom("reviewer".into())).await,
        [(TaskKind::Custom("reviewer".into()), at(2))]
    );
    assert!(kinds(TaskKind::Subagent).await.is_empty());
}

#[tokio::test]
async fn a_row_that_does_not_decode_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let path = journal_with(dir.path(), &[record(session_id, TaskKind::Main, at(1))]).await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO turn_records (session_id, task_id, ended_at, model, profile_id, \
         outcome, completion_reason, escalated, escalation_retry, signals, task_kind) \
         VALUES (?, 2, '2025-06-01T02:00:00+00:00', 'a/b', 'a', 'completed', 'x', 'yes', 0, \
         '[]', 'main')",
    )
    .bind(session_id.to_string())
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();
    let result = scan_turn_records(&path, &TurnRecordFilter::default(), |_| {}).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn a_scan_neither_creates_nor_changes_files() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let path = journal_with(dir.path(), &[record(session_id, TaskKind::Main, at(1))]).await;
    // Let SQLite close the journal and remove its -wal and -shm files.
    for _ in 0..500 {
        if std::fs::read_dir(dir.path()).unwrap().count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let before = std::fs::read(&path).unwrap();
    assert_eq!(scan(&path, &TurnRecordFilter::default()).await.len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let missing = dir.path().join(format!("{}.db", new_session_id()));
    assert!(
        scan_turn_records(&missing, &TurnRecordFilter::default(), |_| {})
            .await
            .is_err()
    );
    assert!(!missing.exists());
}

/// The filters are applied in the query: rows they exclude are not handed
/// over one by one to be dropped. A filter that excludes every row of a
/// large journal takes a small fraction of the time of reading it all.
#[tokio::test]
async fn filters_are_applied_in_the_query() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    let path = journal_with(dir.path(), &[record(session_id, TaskKind::Main, at(1))]).await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30000) \
         INSERT INTO turn_records (session_id, task_id, ended_at, model, profile_id, outcome, \
         completion_reason, escalated, escalation_retry, signals, task_kind) \
         SELECT ?, i, '2025-06-01T01:00:00+00:00', 'a/b', 'a', 'completed', 'x', 0, 0, \
         '[{\"kind\":\"verification_passed\",\"detail\":\"cargo test\"}]', 'main' FROM n",
    )
    .bind(session_id.to_string())
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();

    let time = |filter: TurnRecordFilter| {
        let path = path.clone();
        async move {
            let started = Instant::now();
            let mut rows = 0usize;
            scan_turn_records(&path, &filter, |_| rows += 1)
                .await
                .unwrap();
            (started.elapsed(), rows)
        }
    };
    let (everything, all_rows) = time(TurnRecordFilter::default()).await;
    // Two days after every row: beyond the day of margin the query allows
    // for offsets, so the query itself excludes them.
    let (by_since, since_rows) = time(TurnRecordFilter {
        ended_since: Some(Utc.with_ymd_and_hms(2025, 6, 3, 0, 0, 0).unwrap()),
        ..TurnRecordFilter::default()
    })
    .await;
    let (by_kind, kind_rows) = time(TurnRecordFilter {
        task_kind: Some(TaskKind::Subagent),
        ..TurnRecordFilter::default()
    })
    .await;
    assert_eq!((all_rows, since_rows, kind_rows), (30_001, 0, 0));
    assert!(
        by_since * 4 < everything && by_kind * 4 < everything,
        "everything {everything:?}, since {by_since:?}, kind {by_kind:?}"
    );
}
